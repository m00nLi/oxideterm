//! Serialized command execution over one persistent shell channel.
//!
//! Single-channel SSH servers kill the transport when a second channel opens,
//! so monitoring commands cannot use per-command exec channels. This module
//! multiplexes commands over one shell channel with echo-proof marker framing
//! and client-side timeout resynchronization.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};
use tracing::{debug, warn};

use crate::{SshConfig, SshTransportClient, SshTransportError};

/// Final output marker. It is assembled by the remote shell from fragments so
/// that shells which echo their input never emit the literal marker text as
/// part of the echoed command line.
const MARKER_BEGIN: &str = "__OXIDE_MON_BEGIN__";
const MARKER_END: &str = "__OXIDE_MON_END__";
const IDLE_WINDOW_CAP: usize = 256;
const MONITOR_SHELL_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// Build the shell command line for one monitored command.
///
/// The command runs inside a subshell so `cd`/environment changes cannot leak
/// into later commands. Markers are produced with `printf '%s'` so the echoed
/// command contains `%s` instead of the literal marker.
pub(crate) fn monitor_command_line(command: &str) -> String {
    format!(
        "printf '\\n__OXIDE_MON_%s__\\n' BEGIN; ( {command} ); printf '\\n__OXIDE_MON_%s__\\n' END\n"
    )
}

#[derive(Debug, PartialEq)]
pub(crate) struct MonitorCommandResult {
    pub(crate) output: Vec<u8>,
    pub(crate) truncated: bool,
}

/// Stream parser that splits one serial shell stream into per-command outputs.
#[derive(Debug, Default)]
pub(crate) struct MonitorShellFraming {
    current_output: Vec<u8>,
    current_max: usize,
    current_truncated: bool,
    pending_cr: bool,
    skip_leading_newline: bool,
    collecting: bool,
    /// Uncommitted bytes while a command is collecting.
    collect_window: Vec<u8>,
    /// Uncommitted noise while waiting for the next begin marker.
    idle_window: Vec<u8>,
    completed: VecDeque<MonitorCommandResult>,
}

impl MonitorShellFraming {
    /// Mark a command as in flight. Call after writing its command line.
    pub(crate) fn begin_command(&mut self, max_output: usize) {
        self.current_output.clear();
        self.current_max = max_output;
        self.current_truncated = false;
        self.pending_cr = false;
        self.skip_leading_newline = false;
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
        self.current_output.clear();
        self.current_truncated = false;
        self.pending_cr = false;
        self.skip_leading_newline = false;
        self.collecting = false;
        self.collect_window.clear();
    }

    pub(crate) fn take_completed(&mut self) -> VecDeque<MonitorCommandResult> {
        std::mem::take(&mut self.completed)
    }

    fn scan_collecting(&mut self) {
        let Some(marker_pos) = find_subslice(&self.collect_window, MARKER_END.as_bytes()) else {
            // Everything except a potential split marker suffix is now safe.
            let keep = MARKER_END
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
            output,
            truncated: self.current_truncated,
        });
        self.collect_window.drain(..marker_pos + MARKER_END.len());
        self.collecting = false;
        self.current_truncated = false;

        if !self.collect_window.is_empty() {
            let remainder = std::mem::take(&mut self.collect_window);
            push_capped(&mut self.idle_window, &remainder, IDLE_WINDOW_CAP);
            self.scan_idle();
        }
    }

    fn scan_idle(&mut self) {
        let Some(marker_pos) = find_subslice(&self.idle_window, MARKER_BEGIN.as_bytes()) else {
            return;
        };
        // Drop everything through the marker and its trailing line break.
        self.idle_window.drain(..marker_pos + MARKER_BEGIN.len());

        self.current_output.clear();
        self.current_truncated = false;
        self.skip_leading_newline = true;
        self.collecting = true;
        self.collect_window = std::mem::take(&mut self.idle_window);
        self.scan_collecting();
    }

    fn append_output(&mut self, bytes: &[u8]) {
        if self.current_truncated {
            return;
        }
        for &byte in bytes {
            if self.pending_cr {
                self.pending_cr = false;
                if byte == b'\n' {
                    // Collapse CRLF into one newline before byte counting.
                    if self.skip_leading_newline {
                        self.skip_leading_newline = false;
                        continue;
                    }
                    if self.current_output.len() >= self.current_max {
                        self.current_truncated = true;
                        return;
                    }
                    self.current_output.push(b'\n');
                    continue;
                }
            }
            if byte == b'\r' {
                self.pending_cr = true;
                continue;
            }
            if byte == b'\n' && self.skip_leading_newline {
                self.skip_leading_newline = false;
                continue;
            }
            if self.current_output.len() >= self.current_max {
                self.current_truncated = true;
                return;
            }
            self.current_output.push(byte);
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

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum MonitorShellError {
    #[error("monitor command timed out")]
    Timeout,
    #[error("monitor shell channel closed")]
    ChannelClosed,
    #[error("monitor shell write failed: {0}")]
    Write(String),
}

#[derive(Debug, Default)]
struct MonitorShellState {
    framing: MonitorShellFraming,
    waiters: VecDeque<oneshot::Sender<Result<(Vec<u8>, bool), MonitorShellError>>>,
}

/// Serialized command runner over one persistent shell channel.
pub struct SingleChannelShellSession<R> {
    writer: Arc<Mutex<tokio::io::WriteHalf<R>>>,
    command_lock: Mutex<()>,
    state: Arc<Mutex<MonitorShellState>>,
    command_in_flight: Arc<AtomicBool>,
    reader_task: tokio::task::JoinHandle<()>,
    keepalive_task: tokio::task::JoinHandle<()>,
}

/// Concrete monitor session over a russh channel stream.
pub type MonitorShellSession = SingleChannelShellSession<russh::ChannelStream<russh::client::Msg>>;

/// Open one dedicated SSH connection and its single shell channel.
///
/// `keepalive_interval_secs > 0` with non-empty `keepalive_data` keeps the
/// otherwise idle transport alive on servers that require channel data.
pub async fn connect_monitor_shell(
    config: SshConfig,
    keepalive_interval_secs: u32,
    keepalive_data: Vec<u8>,
) -> Result<MonitorShellSession, SshTransportError> {
    let mut client = SshTransportClient::new(config);
    if keepalive_interval_secs > 0 && !keepalive_data.is_empty() {
        client = client.with_keepalive(keepalive_interval_secs, keepalive_data);
    }
    let shell = client.connect_for_monitor_channel().await?;
    Ok(SingleChannelShellSession::new(shell.into_raw_stream()))
}

/// Wrap an existing shell channel in a serialized monitor session.
///
/// Used when the monitor shell rides the node's existing transport so
/// single-channel servers never see an additional connection.
pub fn monitor_shell_session(shell: crate::SshShellChannel) -> MonitorShellSession {
    SingleChannelShellSession::new(shell.into_raw_stream())
}

/// Open a dedicated sampler connection for profiler/GPU sampling.
///
/// Returns a type-erased sampler so callers never see the internal dedicated
/// connection type. The same channel-data keepalive guard applies.
pub async fn connect_monitor_sampler(
    config: SshConfig,
    keepalive_interval_secs: u32,
    keepalive_data: Vec<u8>,
) -> Result<std::sync::Arc<dyn oxideterm_connection_monitor::ResourceSampler>, SshTransportError> {
    let mut client = SshTransportClient::new(config);
    if keepalive_interval_secs > 0 && !keepalive_data.is_empty() {
        client = client.with_keepalive(keepalive_interval_secs, keepalive_data);
    }
    client.connect_for_monitor().await
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
        let writer = Arc::new(Mutex::new(writer));
        let command_in_flight = Arc::new(AtomicBool::new(false));
        let keepalive_writer = writer.clone();
        let keepalive_busy = command_in_flight.clone();
        let keepalive_task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(MONITOR_SHELL_KEEPALIVE_INTERVAL).await;
                if keepalive_busy.load(Ordering::Relaxed) {
                    // A command is reading from the shell; an injected
                    // newline would be echoed into its output.
                    continue;
                }
                if keepalive_writer
                    .lock()
                    .await
                    .write_all(b"\n")
                    .await
                    .is_err()
                {
                    warn!("monitor shell keepalive write failed, stopping");
                    break;
                }
            }
        });
        Self {
            writer,
            command_lock: Mutex::new(()),
            state,
            command_in_flight,
            reader_task,
            keepalive_task,
        }
    }

    pub async fn run_command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<(Vec<u8>, bool), MonitorShellError> {
        let _command_guard = self.command_lock.lock().await;
        let _in_flight_guard = InFlightGuard::new(self.command_in_flight.clone());
        let receiver = {
            let mut state = self.state.lock().await;
            state.framing.begin_command(max_output);
            let (sender, receiver) = oneshot::channel();
            state.waiters.push_back(sender);
            receiver
        };

        let line = monitor_command_line(command);
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
                // The timed-out command is the oldest waiter because command
                // submission is fully serialized.
                state.waiters.pop_front();
                Err(MonitorShellError::Timeout)
            }
        }
    }
}

struct InFlightGuard {
    flag: Arc<AtomicBool>,
}

impl InFlightGuard {
    fn new(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self { flag }
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}

impl<R> Drop for SingleChannelShellSession<R> {
    fn drop(&mut self) {
        // Aborting the reader drops the read half, whose close-on-drop closes
        // the channel and releases the dedicated transport.
        self.reader_task.abort();
        self.keepalive_task.abort();
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
            Err(error) => {
                warn!(error = %error, "monitor shell reader failed");
                fail_all_waiters(&state).await;
                return;
            }
        };
        if read == 0 {
            debug!("monitor shell reader reached EOF");
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
            if let Some(sender) = guard.waiters.pop_front() {
                let _ = sender.send(Ok((result.output, result.truncated)));
            }
        }
    }
}

async fn fail_all_waiters(state: &Arc<Mutex<MonitorShellState>>) {
    let mut guard = state.lock().await;
    for sender in guard.waiters.drain(..) {
        let _ = sender.send(Err(MonitorShellError::ChannelClosed));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncBufReadExt;

    #[test]
    fn command_line_wraps_command_in_subshell_without_literal_markers() {
        let line = monitor_command_line("cat /proc/loadavg");

        assert!(line.contains("( cat /proc/loadavg )"));
        // Echo-proofing: the echoed command must never contain the literal
        // marker text, only the printf fragments that assemble it.
        assert!(!line.contains(MARKER_BEGIN));
        assert!(!line.contains(MARKER_END));
        assert!(line.contains("%s"));
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn framing_extracts_output_between_markers() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        framing.feed(b"\n__OXIDE_MON_BEGIN__\n");
        framing.feed(b"load average: 0.1\n");
        framing.feed(b"\n__OXIDE_MON_END__\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(
            completed[0],
            MonitorCommandResult {
                output: b"load average: 0.1".to_vec(),
                truncated: false,
            }
        );
    }

    #[test]
    fn framing_ignores_echoed_command_lines() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        // A shell that echoes its input replays the command line, which only
        // contains the printf fragments, before the real markers appear.
        framing.feed(
            b"printf '\\n__OXIDE_MON_%s__\\n' BEGIN; ( echo probe-ok ); printf '\\n__OXIDE_MON_%s__\\n' END\n",
        );
        framing.feed(b"\n__OXIDE_MON_BEGIN__\nprobe-ok\n");
        framing.feed(b"\n__OXIDE_MON_END__\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"probe-ok");
    }

    #[test]
    fn framing_tolerates_markers_split_across_chunks() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        framing.feed(b"\n__OXIDE_MON_BEGIN__\nout");
        framing.feed(b"put\n__OXIDE_MON_END");
        framing.feed(b"__\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"output");
    }

    #[test]
    fn framing_normalizes_crlf_output() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        framing.feed(b"\r\n__OXIDE_MON_BEGIN__\r\nprobe-ok\r\n\r\n__OXIDE_MON_END__\r\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"probe-ok");
    }

    #[test]
    fn framing_normalizes_crlf_split_across_chunks() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        framing.feed(b"\r\n__OXIDE_MON_BEGIN__\r");
        framing.feed(b"\nprobe-ok\r");
        framing.feed(b"\n\r\n__OXIDE_MON_END__\r\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"probe-ok");
    }

    #[test]
    fn framing_normalizes_multiline_crlf_output() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);

        framing.feed(b"\r\n__OXIDE_MON_BEGIN__\r\na b\r\nc\r\n__OXIDE_MON_END__\r\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"a b\nc");
    }

    #[test]
    fn framing_recovers_after_timeout_and_skips_stale_output() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(4096);
        framing.feed(b"\n__OXIDE_MON_BEGIN__\npartial");

        framing.fail_current();
        // Late output from the hung command must be discarded.
        framing.feed(b"stale bytes that never terminate\n");

        framing.begin_command(4096);
        framing.feed(b"\n__OXIDE_MON_BEGIN__\nfresh\n__OXIDE_MON_END__\n");

        let completed = framing.take_completed();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].output, b"fresh");
    }

    #[test]
    fn framing_truncates_oversized_output() {
        let mut framing = MonitorShellFraming::default();
        framing.begin_command(8);

        framing.feed(b"\n__OXIDE_MON_BEGIN__\n0123456789\n__OXIDE_MON_END__\n");

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
                .write_all(b"\n__OXIDE_MON_BEGIN__\none\n\n__OXIDE_MON_END__\n")
                .await
                .unwrap();
            line.clear();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN__\ntwo\n\n__OXIDE_MON_END__\n")
                .await
                .unwrap();
        });

        let first = session
            .run_command("echo one", Duration::from_secs(2), 1024)
            .await
            .unwrap()
            .0;
        let second = session
            .run_command("echo two", Duration::from_secs(2), 1024)
            .await
            .unwrap()
            .0;

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
                .write_all(b"\n__OXIDE_MON_BEGIN__\nstuck\n")
                .await
                .unwrap();
            line.clear();
            reader.read_until(b'\n', &mut line).await.unwrap();
            reader
                .get_mut()
                .write_all(b"\n__OXIDE_MON_BEGIN__\nok\n\n__OXIDE_MON_END__\n")
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
            .unwrap()
            .0;
        assert_eq!(recovered, b"ok");
        responder.await.unwrap();
    }
}
