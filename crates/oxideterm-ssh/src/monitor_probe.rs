//! Real-server probe for the single-channel monitor shell.
//!
//! Exposed only behind the `monitor-probe` feature. The probe trusts the host
//! key automatically and reads its target from environment variables; never
//! commit real credentials or enable this feature in production builds.

use std::time::Duration;

use crate::{
    SshConfig, SshTransportError,
    monitor_shell::{MonitorShellError, MonitorShellSession, connect_monitor_shell},
};

/// Public mirror of the internal command error for probe callers.
#[derive(Debug)]
pub enum ProbeCommandError {
    Timeout,
    ChannelClosed,
    Write(String),
}

impl From<MonitorShellError> for ProbeCommandError {
    fn from(error: MonitorShellError) -> Self {
        match error {
            MonitorShellError::Timeout => Self::Timeout,
            MonitorShellError::ChannelClosed => Self::ChannelClosed,
            MonitorShellError::Write(detail) => Self::Write(detail),
        }
    }
}

pub struct ProbeSession {
    session: MonitorShellSession,
}

pub async fn connect_probe(config: SshConfig) -> Result<ProbeSession, SshTransportError> {
    Ok(ProbeSession {
        session: connect_monitor_shell(config, 0, Vec::new()).await?,
    })
}

impl ProbeSession {
    pub async fn run_command(
        &mut self,
        command: &str,
        timeout: Duration,
        max_output: usize,
    ) -> Result<(Vec<u8>, bool), ProbeCommandError> {
        self.session
            .run_command(command, timeout, max_output)
            .await
            .map_err(ProbeCommandError::from)
    }
}
