// Copyright (C) 2026 AnalyseDeCircuit
// SPDX-License-Identifier: GPL-3.0-only

use std::{sync::Arc, time::Duration};

use oxideterm_connection_monitor::{ResourceSampleShell, ResourceSampler, ResourceSamplerFuture};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::warn;

use crate::DedicatedMonitorConnection;
use crate::transport::SamplerStreamDecoder;
use crate::{SshConfig, SshConnectionHandle, SshShellChannel, SshTransportClient};

impl ResourceSampler for SshConnectionHandle {
    fn open_shell<'a>(
        &'a self,
        init_command: &'a str,
        timeout: Duration,
    ) -> ResourceSamplerFuture<'a, Result<Box<dyn ResourceSampleShell>, String>> {
        Box::pin(async move {
            match tokio::time::timeout(timeout, self.open_persistent_shell_channel(init_command))
                .await
            {
                Ok(Ok(shell)) => Ok(Box::new(SshResourceSampleShell {
                    shell,
                    fallback: self.clone(),
                }) as Box<dyn ResourceSampleShell>),
                Ok(Err(_error)) => Ok(Box::new(SshExecResourceSampleShell {
                    connection: self.clone(),
                }) as Box<dyn ResourceSampleShell>),
                Err(_) => Ok(Box::new(SshExecResourceSampleShell {
                    connection: self.clone(),
                }) as Box<dyn ResourceSampleShell>),
            }
        })
    }
}

struct SshResourceSampleShell {
    shell: SshShellChannel,
    fallback: SshConnectionHandle,
}

impl ResourceSampleShell for SshResourceSampleShell {
    fn sample_until<'a>(
        &'a mut self,
        command: &'a str,
        end_marker: &'a str,
        timeout: Duration,
        max_output_size: usize,
    ) -> ResourceSamplerFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match self
                .shell
                .sample_until(command, end_marker, timeout, max_output_size)
                .await
            {
                Ok(output) => Ok(output),
                // Some servers accept exec channels but reject or stall
                // interactive shell channels. Fall back to one-shot exec so
                // the health panel can still collect full metrics.
                Err(_) => self
                    .fallback
                    .run_command(command, timeout, max_output_size)
                    .await
                    .map_err(|error| error.to_string()),
            }
        })
    }

    fn close<'a>(&'a mut self) -> ResourceSamplerFuture<'a, Result<(), String>> {
        Box::pin(async move { self.shell.close().await.map_err(|error| error.to_string()) })
    }
}

struct SshExecResourceSampleShell {
    connection: SshConnectionHandle,
}

impl ResourceSampleShell for SshExecResourceSampleShell {
    fn sample_until<'a>(
        &'a mut self,
        command: &'a str,
        _end_marker: &'a str,
        timeout: Duration,
        max_output_size: usize,
    ) -> ResourceSamplerFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.connection
                .run_command(command, timeout, max_output_size)
                .await
                .map_err(|error| error.to_string())
        })
    }

    fn close<'a>(&'a mut self) -> ResourceSamplerFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// Used by skip_remote_env_detection nodes: opens a single shell channel
/// on an independent SSH connection. No exec fallback — single-channel
/// servers only allow one channel per connection.
impl ResourceSampler for DedicatedMonitorConnection {
    fn open_shell<'a>(
        &'a self,
        init_command: &'a str,
        timeout: Duration,
    ) -> ResourceSamplerFuture<'a, Result<Box<dyn ResourceSampleShell>, String>> {
        Box::pin(async move {
            match tokio::time::timeout(timeout, self.open_shell_channel(init_command)).await {
                Ok(Ok(shell)) => {
                    Ok(Box::new(into_dedicated_monitor_shell(shell))
                        as Box<dyn ResourceSampleShell>)
                }
                Ok(Err(error)) => Err(error.to_string()),
                Err(_) => Err("monitor shell open timed out".to_string()),
            }
        })
    }
}

const DEDICATED_SAMPLER_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);

fn into_dedicated_monitor_shell(shell: SshShellChannel) -> DedicatedMonitorShell {
    let (reader, writer) = shell.into_raw_stream().into_split();
    let writer = Arc::new(tokio::sync::Mutex::new(writer));
    let keepalive_task = tokio::spawn({
        let writer = writer.clone();
        async move {
            loop {
                tokio::time::sleep(DEDICATED_SAMPLER_KEEPALIVE_INTERVAL).await;
                if writer.lock().await.write_all(b"\n").await.is_err() {
                    warn!("sampler shell keepalive write failed, stopping");
                    break;
                }
            }
        }
    });
    DedicatedMonitorShell {
        reader,
        writer,
        keepalive_task: Some(keepalive_task),
    }
}

/// Reconnecting sampler for single-channel nodes.
///
/// The server forbids a second channel per transport, so recovering from a
/// poisoned or closed sampler shell requires a brand-new SSH connection.
/// Every `open_shell` call therefore connects fresh.
pub struct ReconnectingMonitorSampler {
    config: SshConfig,
    keepalive_interval_secs: u32,
    keepalive_data: Vec<u8>,
}

pub fn reconnectable_monitor_sampler(
    config: SshConfig,
    keepalive_interval_secs: u32,
    keepalive_data: Vec<u8>,
) -> Arc<dyn ResourceSampler> {
    Arc::new(ReconnectingMonitorSampler {
        config,
        keepalive_interval_secs,
        keepalive_data,
    })
}

impl ResourceSampler for ReconnectingMonitorSampler {
    fn open_shell<'a>(
        &'a self,
        init_command: &'a str,
        timeout: Duration,
    ) -> ResourceSamplerFuture<'a, Result<Box<dyn ResourceSampleShell>, String>> {
        Box::pin(async move {
            let mut client = SshTransportClient::new(self.config.clone());
            if self.keepalive_interval_secs > 0 && !self.keepalive_data.is_empty() {
                client = client
                    .with_keepalive(self.keepalive_interval_secs, self.keepalive_data.clone());
            }
            let connection = tokio::time::timeout(timeout, client.connect_for_monitor_raw())
                .await
                .map_err(|_| "sampler connect timed out".to_string())?
                .map_err(|error| error.to_string())?;
            let shell = tokio::time::timeout(timeout, connection.open_shell_channel(init_command))
                .await
                .map_err(|_| "sampler shell open timed out".to_string())?
                .map_err(|error| error.to_string())?;
            Ok(Box::new(into_dedicated_monitor_shell(shell)) as Box<dyn ResourceSampleShell>)
        })
    }
}

struct DedicatedMonitorShell {
    reader: russh::ChannelStreamReader<russh::client::Msg>,
    writer: Arc<tokio::sync::Mutex<russh::ChannelStreamWriter<russh::client::Msg>>>,
    keepalive_task: Option<tokio::task::JoinHandle<()>>,
}

impl ResourceSampleShell for DedicatedMonitorShell {
    fn sample_until<'a>(
        &'a mut self,
        command: &'a str,
        end_marker: &'a str,
        timeout: Duration,
        max_output_size: usize,
    ) -> ResourceSamplerFuture<'a, Result<String, String>> {
        Box::pin(async move {
            self.writer
                .lock()
                .await
                .write_all(command.as_bytes())
                .await
                .map_err(|error| error.to_string())?;

            let mut decoder = SamplerStreamDecoder::new(command);
            tokio::time::timeout(timeout, async {
                let mut buffer = [0u8; 4096];
                let mut output = Vec::new();
                loop {
                    let read = self
                        .reader
                        .read(&mut buffer)
                        .await
                        .map_err(|error| error.to_string())?;
                    if read == 0 {
                        warn!("sampler shell channel reached EOF");
                        return Err("persistent shell channel closed".to_string());
                    }
                    decoder.feed(&buffer[..read], &mut output, max_output_size);
                    if let Ok(text) = std::str::from_utf8(&output)
                        && text.contains(end_marker)
                    {
                        break;
                    }
                }
                Ok::<String, String>(String::from_utf8_lossy(&output).into_owned())
            })
            .await
            .map_err(|_| "sampler read timed out".to_string())?
        })
    }

    fn close<'a>(&'a mut self) -> ResourceSamplerFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Some(task) = self.keepalive_task.take() {
                task.abort();
            }
            Ok(())
        })
    }
}
