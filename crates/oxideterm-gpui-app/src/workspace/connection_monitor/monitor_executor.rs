use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use oxideterm_ssh::{
    SshCommandOutput, SshConnectionRegistry,
    monitor_shell::{MonitorShellError, MonitorShellSession, connect_monitor_shell},
};
use tracing::{debug, warn};

const DEFAULT_MONITOR_KEEPALIVE_INTERVAL: u32 = 10;

/// Runs host-tools commands on the right transport for the target node.
///
/// Normal servers keep per-command exec channels on the shared registry
/// connection. Single-channel servers multiplex commands over one dedicated
/// connection with a single persistent shell channel.
#[derive(Clone)]
pub(crate) struct MonitorCommandExecutor {
    registry: SshConnectionRegistry,
    sessions: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<MonitorShellSession>>>>>,
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

        let session = if let Some(session) = self
            .sessions
            .lock()
            .expect("monitor session map poisoned")
            .get(connection_id)
        {
            session.clone()
        } else {
            debug!(
                connection_id,
                "opening dedicated single-channel monitor shell"
            );
            let config = handle.ssh_config();
            let (interval_secs, data) = self.keepalive_snapshot();
            let session = connect_monitor_shell(config, interval_secs, data)
                .await
                .map_err(|error| {
                    warn!(connection_id, error = %error, "single-channel monitor connect failed");
                    MonitorShellError::Write(error.to_string())
                })?;
            let session = Arc::new(tokio::sync::Mutex::new(session));
            self.sessions
                .lock()
                .expect("monitor session map poisoned")
                .insert(connection_id.to_string(), session.clone());
            session
        };

        let result = session
            .lock()
            .await
            .run_command(command, timeout, max_output)
            .await;
        if let Err(error) = &result {
            warn!(connection_id, error = %error, "single-channel monitor command failed");
        }
        if matches!(
            result,
            Err(MonitorShellError::ChannelClosed | MonitorShellError::Write(_))
        ) {
            self.sessions
                .lock()
                .expect("monitor session map poisoned")
                .remove(connection_id);
        }
        result.map(|(stdout, truncated)| SshCommandOutput {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::new(),
            exit_code: None,
            truncated,
        })
    }
}
