//! SPIKE prototype for single-channel monitoring over one persistent shell.
//!
//! Throwaway feasibility code: validates marker framing, serialization, and
//! timeout resynchronization against a mock byte stream before wiring a real
//! russh shell channel. Do not merge into production as-is.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};

const MARKER_BEGIN_PREFIX: &str = "__OXIDE_MON_BEGIN_";
const MARKER_END_PREFIX: &str = "__OXIDE_MON_END_";
const IDLE_WINDOW_CAP: usize = 256;

/// Build the shell command line for one monitored command.
///
/// The command runs inside a subshell so `cd`/environment changes cannot leak
/// into later commands. Unique markers delimit the captured output.
pub(crate) fn monitor_command_line(command: &str, token: u64) -> String {
    format!(
        "printf '\\n{begin}{token}\\n'; ( {command} ); printf '\\n{end}{token}\\n'\n",
        begin = MARKER_BEGIN_PREFIX,
        end = MARKER_END_PREFIX,
    )
}

#[derive(Debug, PartialEq)]
pub(crate) struct MonitorCommandResult {
    pub(crate) token: u64,
    pub(crate) output: Vec<u8>,
    pub(crate) truncated: bool,
}

/// Stream parser that splits one serial shell stream into per-command outputs.
#[derive(Debug, Default)]
pub(crate) struct MonitorShellFraming {
    current_token: Option<u64>,
    current_output: Vec<u8>,
    current_max: usize,
    current_truncated: bool,
    collecting: bool,
    /// Uncommitted bytes while a command is collecting.
    collect_window: Vec<u8>,
    /// Uncommitted noise while waiting for the next begin marker.
    idle_window: Vec<u8>,
    completed: VecDeque<MonitorCommandResult>,
}

impl MonitorShellFraming {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Mark a command as in flight. Call after writing its command line.
    pub(crate) fn begin_command(&mut self, token: u64, max_output: usize) {
        self.current_token = Some(token);
        self.current_output.clear();
        self.current_max = max_output;
        self.current_truncated = false;
        self.collecting = false;
    }

    /// Feed raw shell output and complete commands whose end marker arrived.
    pub(crate) fn feed(&mut self, data: &[u8]) {
        if self.collecting {
            self.collect_window.extend_from_slice(data);
            self.scan_collecting();
        } else {
            push_capped(&mut self.idle_window, data, IDLE_WINDOW_CAP);
            self.scan_idle();
        }
    }

    /// Abort the in-flight command and discard its output until the next
    /// begin marker (timeout recovery without killing the channel).
    pub(crate) fn fail_current(&mut self) {
        self.current_token = None;
        self.current_output.clear();
        self.current_truncated = false;
        self.collecting = false;
        self.collect_window.clear();
    }

    pub(crate) fn take_completed(&mut self) -> VecDeque<MonitorCommandResult> {
        std::mem::take(&mut self.completed)
    }

    fn scan_collecting(&mut self) {
        let Some(token) = self.current_token else {
            return;
        };
        let end_marker = format!("{MARKER_END_PREFIX}{token}");
        let Some(marker_pos) = find_subslice(&self.collect_window, end_marker.as_bytes()) else {
            // Everything except a potential split marker suffix is now safe.
            let keep = end_marker
                .len()
                .saturating_sub(1)
                .min(self.collect_window.len());
            let commit_len = self.collect_window.len() - keep;
            if commit_len > 0 {
                let committed = self.collect_window[..commit_len].to_vec();
                self.append_output(&committed);
                self.collect_window.drain(..commit_len);
            }
            return;
        };

        let output_part = self.collect_window[..marker_pos].to_vec();
        self.append_output(&output_part);
        let mut output = std::mem::take(&mut self.current_output);
        trim_trailing_whitespace(&mut output);
        self.completed.push_back(MonitorCommandResult {
            token,
            output,
            truncated: self.current_truncated,
        });
        self.collect_window.drain(..marker_pos + end_marker.len());
        self.current_token = None;
        self.collecting = false;
        self.current_truncated = false;

        if !self.collect_window.is_empty() {
            let remainder = std::mem::take(&mut self.collect_window);
            push_capped(&mut self.idle_window, &remainder, IDLE_WINDOW_CAP);
            self.scan_idle();
        }
    }

    fn scan_idle(&mut self) {
        let Some(prefix_pos) = find_subslice(&self.idle_window, MARKER_BEGIN_PREFIX.as_bytes())
        else {
            return;
        };
        let digits_start = prefix_pos + MARKER_BEGIN_PREFIX.len();
        let digit_len = self.idle_window[digits_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digit_len == 0 {
            self.idle_window.drain(..digits_start);
            self.scan_idle();
            return;
        }
        let token_text =
            std::str::from_utf8(&self.idle_window[digits_start..digits_start + digit_len])
                .unwrap_or("");
        let Ok(token) = token_text.parse::<u64>() else {
            self.idle_window.drain(..digits_start);
            self.scan_idle();
            return;
        };

        // Drop everything through the marker and its trailing line break.
        self.idle_window.drain(..digits_start + digit_len);
        if let Some(stripped) = self.idle_window.strip_prefix(b"\n") {
            let len = self.idle_window.len() - stripped.len();
            self.idle_window.drain(..len);
        }

        self.current_token = Some(token);
        self.current_output.clear();
        self.current_truncated = false;
        self.collecting = true;
        self.collect_window = std::mem::take(&mut self.idle_window);
        self.scan_collecting();
    }

    fn append_output(&mut self, bytes: &[u8]) {
        if self.current_truncated {
            return;
        }
        let remaining = self.current_max.saturating_sub(self.current_output.len());
        if bytes.len() > remaining {
            self.current_output.extend_from_slice(&bytes[..remaining]);
            self.current_truncated = true;
        } else {
            self.current_output.extend_from_slice(bytes);
        }
    }
}

fn trim_trailing_whitespace(output: &mut Vec<u8>) {
    let trimmed_len = output
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    output.truncate(trimmed_len);
}

fn push_capped(target: &mut Vec<u8>, incoming: &[u8], cap: usize) {
    target.extend_from_slice(incoming);
    if target.len() > cap {
        let excess = target.len() - cap;
        target.drain(..excess);
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[derive(Debug, PartialEq)]
pub(crate) enum MonitorShellError {
    Timeout,
    ChannelClosed,
    Write(String),
}

#[derive(Debug, Default)]
struct MonitorShellState {
    framing: MonitorShellFraming,
    next_token: u64,
    waiters: HashMap<u64, oneshot::Sender<Result<Vec<u8>, MonitorShellError>>>,
}

/// Serialized command runner over one persistent shell channel.
pub(crate) struct SingleChannelShellSession<R> {
    writer: Mutex<tokio::io::WriteHalf<R>>,
    state: Arc<Mutex<MonitorShellState>>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl<R> SingleChannelShellSession<R>
where
    R: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub(crate) fn new(stream: R) -> Self {
        let (reader, writer) = tokio::io::split(stream);
        let state = Arc::new(Mutex::new(MonitorShellState::default()));
        let reader_state = state.clone();
        let reader_task = tokio::spawn(async move {
            reader_loop(reader, reader_state).await;
        });
        Self {
            writer: Mutex::new(writer),
            state,
            _reader_task: reader_task,
        }
    }

    pub(crate) async fn run_command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<Vec<u8>, MonitorShellError> {
        let (token, receiver) = {
            let mut state = self.state.lock().await;
            let token = state.next_token;
            state.next_token = state.next_token.wrapping_add(1);
            state.framing.begin_command(token, max_output);
            let (sender, receiver) = oneshot::channel();
            state.waiters.insert(token, sender);
            (token, receiver)
        };

        let line = monitor_command_line(command, token);
        self.writer
            .lock()
            .await
            .write_all(line.as_bytes())
            .await
            .map_err(|error| MonitorShellError::Write(error.to_string()))?;

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(MonitorShellError::ChannelClosed),
            Err(_elapsed) => {
                let mut state = self.state.lock().await;
                state.framing.fail_current();
                state.waiters.remove(&token);
                Err(MonitorShellError::Timeout)
            }
        }
    }
}

async fn reader_loop<R>(mut reader: R, state: Arc<Mutex<MonitorShellState>>)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = vec![0u8; 4096];
    loop {
        let read = match reader.read(&mut buffer).await {
            Ok(read) => read,
            Err(_) => {
                fail_all_waiters(&state).await;
                return;
            }
        };
        if read == 0 {
            fail_all_waiters(&state).await;
            return;
        }

        let completed = {
            let mut guard = state.lock().await;
            guard.framing.feed(&buffer[..read]);
            guard.framing.take_completed()
        };
        let mut guard = state.lock().await;
        for result in completed {
            if let Some(sender) = guard.waiters.remove(&result.token) {
                let _ = sender.send(Ok(result.output));
            }
        }
    }
}

async fn fail_all_waiters(state: &Arc<Mutex<MonitorShellState>>) {
    let mut guard = state.lock().await;
    for (_, sender) in guard.waiters.drain() {
        let _ = sender.send(Err(MonitorShellError::ChannelClosed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn command_line_wraps_command_in_subshell_with_unique_markers() {
        let line = monitor_command_line("cat /proc/loadavg", 42);

        assert!(line.contains("( cat /proc/loadavg )"));
        assert!(line.contains("__OXIDE_MON_BEGIN_42"));
        assert!(line.contains("__OXIDE_MON_END_42"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn framing_extracts_output_between_markers() {
        let mut framing = MonitorShellFraming::new();
        framing.begin_command(1, 4096);

        framing.feed(b"\n__OXIDE_MON_BEGIN_1\n");
        framing.feed(b"load average: 0.1\n");
        framing.feed(b"\n__OXIDE_MON_END_1\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0],
            MonitorCommandResult {
                token: 1,
                output: b"load average: 0.1".to_vec(),
                truncated: false,
            }
        );
    }

    #[test]
    fn framing_tolerates_markers_split_across_chunks() {
        let mut framing = MonitorShellFraming::new();
        framing.begin_command(2, 4096);

        framing.feed(b"\n__OXIDE_MON_BEGIN_2\nout");
        framing.feed(b"put\n__OXIDE_MON_EN");
        framing.feed(b"D_2\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"output");
    }

    #[test]
    fn framing_recovers_after_timeout_and_skips_stale_output() {
        let mut framing = MonitorShellFraming::new();
        framing.begin_command(1, 4096);
        framing.feed(b"\n__OXIDE_MON_BEGIN_1\npartial");

        framing.fail_current();
        // Late output from the hung command must be discarded.
        framing.feed(b"stale bytes that never terminate\n");

        framing.begin_command(2, 4096);
        framing.feed(b"\n__OXIDE_MON_BEGIN_2\nfresh\n__OXIDE_MON_END_2\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].token, 2);
        assert_eq!(completed[0].output, b"fresh");
    }

    #[test]
    fn framing_truncates_oversized_output() {
        let mut framing = MonitorShellFraming::new();
        framing.begin_command(3, 8);

        framing.feed(b"\n__OXIDE_MON_BEGIN_3\n0123456789\n__OXIDE_MON_END_3\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"01234567");
        assert!(completed[0].truncated);
    }

    #[tokio::test]
    async fn session_runs_commands_serially_over_one_stream() {
        let (client, server) = tokio::io::duplex(4096);
        let mut session = SingleChannelShellSession::new(client);
        let responder = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(server);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN_0\none\n\n__OXIDE_MON_END_0\n")
                .await
                .unwrap();
            line.clear();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN_1\ntwo\n\n__OXIDE_MON_END_1\n")
                .await
                .unwrap();
        });

        let first = session
            .run_command("echo one", Duration::from_secs(2), 1024)
            .await
            .unwrap();
        let second = session
            .run_command("echo two", Duration::from_secs(2), 1024)
            .await
            .unwrap();

        assert_eq!(first, b"one");
        assert_eq!(second, b"two");
        responder.await.unwrap();
    }

    #[tokio::test]
    async fn session_times_out_hung_command_and_recovers_next() {
        let (client, server) = tokio::io::duplex(4096);
        let mut session = SingleChannelShellSession::new(client);
        let responder = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(server);
            let mut line = Vec::new();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN_0\nstuck\n")
                .await
                .unwrap();
            line.clear();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN_1\nok\n\n__OXIDE_MON_END_1\n")
                .await
                .unwrap();
        });

        let hung = session
            .run_command("sleep 999", Duration::from_millis(50), 1024)
            .await;
        assert_eq!(hung, Err(MonitorShellError::Timeout));

        let recovered = session
            .run_command("echo ok", Duration::from_secs(2), 1024)
            .await
            .unwrap();
        assert_eq!(recovered, b"ok");
        responder.await.unwrap();
    }
}
