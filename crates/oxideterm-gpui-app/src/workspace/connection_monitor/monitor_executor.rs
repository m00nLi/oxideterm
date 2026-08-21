use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use oxideterm_ssh::{
    SshCommandOutput, SshConnectionRegistry,
    monitor_shell::{MonitorShellError, MonitorShellSession, connect_monitor_shell},
};
use tracing::{debug, warn};

const DEFAULT_MONITOR_KEEPALIVE_INTERVAL: u32 = 10;

/// Slot for a dedicated single-channel monitor shell session.
enum SessionSlot<S> {
    /// A creator is establishing the connection; waiters park on this guard
    /// until the slot becomes `Ready` or is removed after a failed connect.
    Connecting(Arc<tokio::sync::Mutex<()>>),
    Ready(S),
}

type MonitorSession = Arc<tokio::sync::Mutex<MonitorShellSession>>;

/// Return the single session for `connection_id`, creating it through
/// `connect` when necessary.
///
/// Creation is serialized per connection: the creator inserts a `Connecting`
/// slot before awaiting `connect` and replaces it with `Ready` on success or
/// removes it on failure. Waiters park on the slot guard and re-check the map
/// after waking, so concurrent first commands share one transport instead of
/// opening duplicate dedicated connections.
async fn acquire_session<S, E, F, Fut>(
    sessions: &Mutex<HashMap<String, SessionSlot<S>>>,
    connection_id: &str,
    connect: F,
) -> Result<S, E>
where
    S: Clone,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<S, E>>,
{
    loop {
        let (guard, creator) = {
            let mut map = sessions.lock().expect("monitor session map poisoned");
            match map.get(connection_id) {
                Some(SessionSlot::Ready(session)) => return Ok(session.clone()),
                Some(SessionSlot::Connecting(guard)) => (guard.clone(), false),
                None => {
                    let guard = Arc::new(tokio::sync::Mutex::new(()));
                    map.insert(
                        connection_id.to_string(),
                        SessionSlot::Connecting(guard.clone()),
                    );
                    (guard, true)
                }
            }
        };

        // The creator holds the guard across the connect await. A waiter
        // blocks here and re-checks the slot after the creator finishes.
        let _held = guard.lock().await;

        if let Some(SessionSlot::Ready(session)) = sessions
            .lock()
            .expect("monitor session map poisoned")
            .get(connection_id)
        {
            return Ok(session.clone());
        }
        if !creator {
            // The previous creator failed and removed its slot; retry as the
            // next creator.
            continue;
        }

        match connect().await {
            Ok(session) => {
                sessions
                    .lock()
                    .expect("monitor session map poisoned")
                    .insert(connection_id.to_string(), SessionSlot::Ready(session.clone()));
                return Ok(session);
            }
            Err(error) => {
                let mut map = sessions.lock().expect("monitor session map poisoned");
                let slot_is_ours = matches!(
                    map.get(connection_id),
                    Some(SessionSlot::Connecting(existing)) if Arc::ptr_eq(existing, &guard)
                );
                if slot_is_ours {
                    map.remove(connection_id);
                }
                return Err(error);
            }
        }
    }
}

/// Remove a `Ready` slot only while it still holds `expected`, so a stale
/// failure path cannot evict a session that a concurrent caller rebuilt.
fn evict_session_if_ready<T>(
    sessions: &Mutex<HashMap<String, SessionSlot<Arc<T>>>>,
    connection_id: &str,
    expected: &Arc<T>,
) -> bool {
    let mut map = sessions.lock().expect("monitor session map poisoned");
    let matches = matches!(
        map.get(connection_id),
        Some(SessionSlot::Ready(existing)) if Arc::ptr_eq(existing, expected)
    );
    if matches {
        map.remove(connection_id);
    }
    matches
}

/// Acquire the cached session for `connection_id` and transparently replace it
/// when `alive` reports it dead, so the first command after a reconnect does
/// not wait out its timeout on a transport whose reader has already exited.
async fn acquire_live_session<T, E, F, Fut, A>(
    sessions: &Mutex<HashMap<String, SessionSlot<Arc<T>>>>,
    connection_id: &str,
    connect: F,
    alive: A,
) -> Result<Arc<T>, E>
where
    F: Fn() -> Fut + Copy,
    Fut: Future<Output = Result<Arc<T>, E>>,
    A: Fn(&T) -> bool,
{
    for _ in 0..2 {
        let session = acquire_session(sessions, connection_id, connect).await?;
        if alive(&session) {
            return Ok(session);
        }
        evict_session_if_ready(sessions, connection_id, &session);
    }
    // Best effort after a failed replacement: a still-dead session fails the
    // command immediately and the existing error path evicts it again.
    acquire_session(sessions, connection_id, connect).await
}

/// Runs host-tools commands on the right transport for the target node.
///
/// Normal servers keep per-command exec channels on the shared registry
/// connection. Single-channel servers multiplex commands over one dedicated
/// connection with a single persistent shell channel.
#[derive(Clone)]
pub(crate) struct MonitorCommandExecutor {
    registry: SshConnectionRegistry,
    sessions: Arc<Mutex<HashMap<String, SessionSlot<MonitorSession>>>>,
    keepalive: Arc<RwLock<(u32, Vec<u8>)>>,
    detected_os: Arc<Mutex<HashMap<String, String>>>,
}

impl MonitorCommandExecutor {
    pub(super) fn new(registry: SshConnectionRegistry, keepalive: (u32, Vec<u8>)) -> Self {
        Self {
            registry,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            keepalive: Arc::new(RwLock::new(keepalive)),
            detected_os: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(super) fn set_keepalive(&self, interval_secs: u32, data: Vec<u8>) {
        if let Ok(mut keepalive) = self.keepalive.write() {
            *keepalive = (interval_secs, data);
        }
    }

    pub(super) fn keepalive_snapshot(&self) -> (u32, Vec<u8>) {
        let keepalive = self.keepalive.read().expect("keepalive lock poisoned");
        let (interval, data) = (keepalive.0, keepalive.1.clone());
        if interval == 0 || data.is_empty() {
            // Monitor connections are dedicated and otherwise idle; keep them
            // alive by default even when the user never configured keepalive.
            (DEFAULT_MONITOR_KEEPALIVE_INTERVAL, vec![b'\n'])
        } else {
            (interval, data)
        }
    }

    pub(super) fn cached_os_type(&self, connection_id: &str) -> Option<String> {
        self.detected_os
            .lock()
            .expect("detected os map poisoned")
            .get(connection_id)
            .cloned()
    }

    pub(super) async fn ensure_os_type(
        &self,
        connection_id: &str,
    ) -> Result<String, MonitorShellError> {
        if let Some(os_type) = self.cached_os_type(connection_id) {
            return Ok(os_type);
        }
        let output = self
            .run(connection_id, "uname -s", Duration::from_secs(10), 128)
            .await?;
        let os_type = output.stdout.trim().to_string();
        let os_type = if os_type.is_empty() {
            "Linux".to_string()
        } else {
            os_type
        };
        self.detected_os
            .lock()
            .expect("detected os map poisoned")
            .insert(connection_id.to_string(), os_type.clone());
        Ok(os_type)
    }

    pub(super) async fn run(
        &self,
        connection_id: &str,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<SshCommandOutput, MonitorShellError> {
        let Some(handle) = self.registry.get(connection_id) else {
            warn!(
                connection_id,
                "monitor command dropped: connection not in registry"
            );
            return Err(MonitorShellError::ChannelClosed);
        };
        if !handle.skip_remote_env_detection() {
            return handle
                .run_command_capture(command, timeout, max_output)
                .await
                .map_err(|error| {
                    warn!(connection_id, error = %error, "registry monitor command failed");
                    MonitorShellError::ChannelClosed
                });
        }

        let connection_id_owned = connection_id.to_string();
        let session = acquire_live_session(
            &self.sessions,
            connection_id,
            || {
                let config = handle.ssh_config();
                let (interval_secs, data) = self.keepalive_snapshot();
                let connection_id = connection_id_owned.clone();
                async move {
                    debug!(
                        connection_id,
                        "opening dedicated single-channel monitor shell"
                    );
                    connect_monitor_shell(config, interval_secs, data)
                        .await
                        .map(|shell| Arc::new(tokio::sync::Mutex::new(shell)))
                        .map_err(|error| {
                            warn!(
                                connection_id,
                                error = %error,
                                "single-channel monitor connect failed"
                            );
                            MonitorShellError::Write(error.to_string())
                        })
                }
            },
            |session: &tokio::sync::Mutex<MonitorShellSession>| {
                session
                    .try_lock()
                    .map(|guard| guard.is_alive())
                    .unwrap_or(true)
            },
        )
        .await?;

        let result = session
            .lock()
            .await
            .run_command(command, timeout, max_output)
            .await;
        if let Err(error) = &result {
            warn!(
                connection_id,
                error = %error,
                command = command,
                "single-channel monitor command failed"
            );
        }
        if matches!(
            result,
            Err(MonitorShellError::Timeout
                | MonitorShellError::ChannelClosed
                | MonitorShellError::Write(_))
        ) {
            // A timed-out remote command still occupies the serial shell, so
            // the session is unusable until the remote process exits. Drop it
            // and let the next command open a fresh connection.
            self.sessions
                .lock()
                .expect("monitor session map poisoned")
                .remove(connection_id);
        }
        result.map(|(stdout, truncated)| monitor_shell_output(stdout, truncated))
    }
}

fn monitor_shell_output(stdout: Vec<u8>, truncated: bool) -> SshCommandOutput {
    SshCommandOutput {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::new(),
        // Receiving the END marker means the command line ran to completion
        // and the serial shell resynchronized. The marker framing cannot carry
        // the remote exit status, and every host-tool snapshot script ends
        // with a successful echo, so report success instead of leaving a
        // `None` that tmux capture strictly rejects.
        exit_code: Some(0),
        truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type TestSession = Arc<tokio::sync::Mutex<u8>>;
    type TestSlot = SessionSlot<TestSession>;

    fn test_sessions() -> Arc<Mutex<HashMap<String, TestSlot>>> {
        Arc::new(Mutex::new(HashMap::new()))
    }

    #[test]
    fn monitor_shell_success_reports_zero_exit_code_for_tmux_parsing() {
        let output = monitor_shell_output(b"===TMUX===...".to_vec(), false);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.stdout, "===TMUX===...");
    }

    #[tokio::test]
    async fn concurrent_first_commands_share_one_session() {
        let sessions = test_sessions();
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect = || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                tokio::task::yield_now().await;
                assert_eq!(attempt, 1, "connect runs only for the creator");
                Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
            }
        };

        let (first, second) = tokio::join!(
            acquire_session(&sessions, "conn-1", connect),
            acquire_session(&sessions, "conn-1", connect),
        );
        let first = first.expect("first caller");
        let second = second.expect("second caller");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn waiter_reuses_the_creators_session() {
        let sessions = test_sessions();
        let attempts = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let creator_sessions = sessions.clone();
        let creator_attempts = attempts.clone();
        let creator_started = started.clone();
        let creator = tokio::spawn(async move {
            acquire_session(&creator_sessions, "conn-1", move || {
                let attempt = creator_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let creator_started = creator_started.clone();
                async move {
                    creator_started.notify_one();
                    tokio::task::yield_now().await;
                    assert_eq!(attempt, 1);
                    Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
                }
            })
            .await
        });
        started.notified().await;

        let waiter = acquire_session(&sessions, "conn-1", || async {
            panic!("the waiter must reuse the creator's session");
            #[allow(unreachable_code)]
            Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(2_u8)))
        });

        let creator_session = creator.await.expect("creator task").expect("creator connect");
        let waiter_session = waiter.await.expect("waiter connect");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&creator_session, &waiter_session));
    }

    #[tokio::test]
    async fn failed_creation_removes_the_slot_and_allows_retry() {
        let sessions = test_sessions();
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect = || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                tokio::task::yield_now().await;
                if attempt == 1 {
                    Err("boom".to_string())
                } else {
                    Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(2_u8)))
                }
            }
        };

        let first = acquire_session(&sessions, "conn-1", connect).await;
        assert!(matches!(first, Err(ref error) if error == "boom"));
        let second = acquire_session(&sessions, "conn-1", connect)
            .await
            .expect("retry succeeds");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let _ = second;
    }

    #[tokio::test]
    async fn waiter_becomes_creator_after_failed_connection_attempt() {
        let sessions = test_sessions();
        let attempts = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let creator_sessions = sessions.clone();
        let creator_attempts = attempts.clone();
        let creator_started = started.clone();
        let creator = tokio::spawn(async move {
            acquire_session(&creator_sessions, "conn-1", move || {
                let attempt = creator_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                let creator_started = creator_started.clone();
                async move {
                    creator_started.notify_one();
                    tokio::task::yield_now().await;
                    if attempt == 1 {
                        Err("boom".to_string())
                    } else {
                        Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
                    }
                }
            })
            .await
        });
        started.notified().await;

        let waiter = acquire_session(&sessions, "conn-1", || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                tokio::task::yield_now().await;
                assert_eq!(attempt, 2, "the waiter becomes the retry creator");
                Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
            }
        });

        let creator_result = creator.await.expect("creator task");
        assert!(creator_result.is_err());
        let waiter_session = waiter.await.expect("waiter retries as creator");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        let _ = waiter_session;
    }

    #[tokio::test]
    async fn ready_session_is_reused_without_reconnecting() {
        let sessions = test_sessions();
        let attempts = Arc::new(AtomicUsize::new(0));
        let connect = || {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                assert_eq!(attempt, 1);
                Ok::<TestSession, String>(Arc::new(tokio::sync::Mutex::new(3_u8)))
            }
        };

        let first = acquire_session(&sessions, "conn-1", connect)
            .await
            .expect("first");
        let second = acquire_session(&sessions, "conn-1", connect)
            .await
            .expect("second");

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dead_cached_session_is_evicted_and_reconnected_once() {
        let sessions = test_sessions();
        sessions
            .lock()
            .expect("session map poisoned")
            .insert(
                "conn-1".to_string(),
                SessionSlot::Ready(Arc::new(tokio::sync::Mutex::new(0_u8))),
            );
        let connects = Arc::new(AtomicUsize::new(0));
        let connect = || {
            let attempt = connects.fetch_add(1, Ordering::SeqCst) + 1;
            async move {
                tokio::task::yield_now().await;
                assert_eq!(attempt, 1, "dead session needs exactly one reconnect");
                Ok::<_, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
            }
        };

        let session = acquire_live_session(&sessions, "conn-1", connect, |slot| {
            slot.try_lock().is_ok_and(|value| *value == 1)
        })
        .await
        .expect("live session");

        assert_eq!(connects.load(Ordering::SeqCst), 1);
        assert_eq!(*session.try_lock().expect("session lock"), 1);
    }

    #[tokio::test]
    async fn live_cached_session_is_reused_without_reconnecting() {
        let sessions = test_sessions();
        sessions
            .lock()
            .expect("session map poisoned")
            .insert(
                "conn-1".to_string(),
                SessionSlot::Ready(Arc::new(tokio::sync::Mutex::new(1_u8))),
            );
        let connects = Arc::new(AtomicUsize::new(0));
        let connect = || {
            let connects = connects.clone();
            async move {
                connects.fetch_add(1, Ordering::SeqCst);
                Ok::<_, String>(Arc::new(tokio::sync::Mutex::new(1_u8)))
            }
        };

        let session = acquire_live_session(&sessions, "conn-1", connect, |slot| {
            slot.try_lock().is_ok_and(|value| *value == 1)
        })
        .await
        .expect("live session");

        assert_eq!(connects.load(Ordering::SeqCst), 0);
        assert_eq!(*session.try_lock().expect("session lock"), 1);
    }

    #[test]
    fn evict_only_removes_the_matching_ready_slot() {
        let sessions = test_sessions();
        let original = Arc::new(tokio::sync::Mutex::new(1_u8));
        sessions
            .lock()
            .expect("session map poisoned")
            .insert(
                "conn-1".to_string(),
                SessionSlot::Ready(original.clone()),
            );

        let other = Arc::new(tokio::sync::Mutex::new(1_u8));
        assert!(!evict_session_if_ready(&sessions, "conn-1", &other));
        assert!(sessions
            .lock()
            .expect("session map poisoned")
            .contains_key("conn-1"));

        assert!(evict_session_if_ready(&sessions, "conn-1", &original));
        assert!(sessions
            .lock()
            .expect("session map poisoned")
            .get("conn-1")
            .is_none());
    }
}
