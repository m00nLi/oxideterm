use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use oxideterm_ssh::{
    SshCommandOutput, SshConnectionRegistry,
    monitor_shell::{MonitorShellError, MonitorShellSession, connect_monitor_shell},
};

/// Runs host-tools commands on the right transport for the target node.
///
/// Normal servers keep per-command exec channels on the shared registry
/// connection. Single-channel servers multiplex commands over one dedicated
/// connection with a single persistent shell channel.
#[derive(Clone)]
pub(crate) struct MonitorCommandExecutor {
    registry: SshConnectionRegistry,
    sessions: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<MonitorShellSession>>>>>,
    keepalive: Arc<tokio::sync::RwLock<(u32, Vec<u8>)>>,
}

impl MonitorCommandExecutor {
    pub(super) fn new(registry: SshConnectionRegistry, keepalive: (u32, Vec<u8>)) -> Self {
        Self {
            registry,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            keepalive: Arc::new(tokio::sync::RwLock::new(keepalive)),
        }
    }

    pub(super) fn set_keepalive(&self, interval_secs: u32, data: Vec<u8>) {
        if let Ok(mut keepalive) = self.keepalive.try_write() {
            *keepalive = (interval_secs, data);
        }
    }

    pub(super) fn keepalive_snapshot(&self) -> (u32, Vec<u8>) {
        let keepalive = self.keepalive.blocking_read();
        (keepalive.0, keepalive.1.clone())
    }

    pub(super) async fn run(
        &self,
        connection_id: &str,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<SshCommandOutput, MonitorShellError> {
        let Some(handle) = self.registry.get(connection_id) else {
            return Err(MonitorShellError::ChannelClosed);
        };
        if !handle.skip_remote_env_detection() {
            return handle
                .run_command_capture(command, timeout, max_output)
                .await
                .map_err(|_| MonitorShellError::ChannelClosed);
        }

        let session = if let Some(session) = self
            .sessions
            .lock()
            .expect("monitor session map poisoned")
            .get(connection_id)
        {
            session.clone()
        } else {
            let config = handle.ssh_config();
            let (interval_secs, data) = {
                let keepalive = self.keepalive.read().await;
                (keepalive.0, keepalive.1.clone())
            };
            let session = connect_monitor_shell(config, interval_secs, data)
                .await
                .map_err(|error| MonitorShellError::Write(error.to_string()))?;
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
