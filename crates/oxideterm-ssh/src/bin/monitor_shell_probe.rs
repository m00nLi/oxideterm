//! SPIKE real-server probe binary.
//!
//! Target comes from environment variables only:
//! - `OXIDE_PROBE_HOST` (required)
//! - `OXIDE_PROBE_PORT` (default 22)
//! - `OXIDE_PROBE_USER` (required)
//! - `OXIDE_PROBE_IDENTITY_FILE` (key auth, optional)
//! - `OXIDE_PROBE_PASSPHRASE` (optional)
//! - `OXIDE_PROBE_PASSWORD` (fallback password auth, optional)
//! Otherwise SSH agent authentication is attempted.

use std::{env, time::Duration};

use oxideterm_ssh::{AuthMethod, SshConfig, monitor_probe::connect_probe};

#[tokio::main]
async fn main() {
    match run().await {
        Ok(()) => {}
        Err(error) => {
            eprintln!("PROBE FAILED: {error}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<(), String> {
    let config = probe_config()?;
    println!(
        "connecting to {}@{}:{} (host key auto-trusted for this probe)",
        config.username, config.host, config.port
    );
    let mut session = connect_probe(config)
        .await
        .map_err(|error| error.to_string())?;
    println!("shell channel open");

    check(
        &mut session,
        "single command",
        "echo probe-ok",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"probe-ok"),
    )
    .await?;

    check(
        &mut session,
        "first of two consecutive commands",
        "echo first",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"first"),
    )
    .await?;
    check(
        &mut session,
        "second of two consecutive commands",
        "echo second",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"second"),
    )
    .await?;

    check(
        &mut session,
        "large output truncation",
        "seq 1 200000",
        Duration::from_secs(20),
        64 * 1024,
        Expected::Truncated,
    )
    .await?;

    match session
        .run_command("sleep 30", Duration::from_secs(2), 4096)
        .await
    {
        Err(oxideterm_ssh::monitor_probe::ProbeCommandError::Timeout) => {
            println!("PASS hung-command timeout");
        }
        other => {
            return Err(format!(
                "hung-command timeout: expected Timeout, got {other:?}"
            ));
        }
    }
    check(
        &mut session,
        "recovery after hung command",
        "echo after-hang",
        // The remote `sleep 30` still owns the serial shell; this command
        // only executes after it finishes, so the deadline must exceed it.
        Duration::from_secs(45),
        4096,
        Expected::Output(b"after-hang"),
    )
    .await?;

    check(
        &mut session,
        "multiline output",
        "printf 'a b\\nc'",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"a b\nc"),
    )
    .await?;

    check(
        &mut session,
        "final liveness",
        "echo done",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"done"),
    )
    .await?;

    println!("ALL PROBE SCENARIOS PASSED");
    Ok(())
}

enum Expected {
    Output(&'static [u8]),
    Truncated,
}

async fn check(
    session: &mut oxideterm_ssh::monitor_probe::ProbeSession,
    name: &str,
    command: &str,
    timeout: Duration,
    max_output: usize,
    expected: Expected,
) -> Result<(), String> {
    let result = session.run_command(command, timeout, max_output).await;
    match (&expected, result) {
        (Expected::Output(want), Ok((output, truncated))) => {
            if truncated {
                return Err(format!("{name}: unexpected truncation"));
            }
            if output != *want {
                return Err(format!(
                    "{name}: output mismatch, want {:?}, got {:?}",
                    String::from_utf8_lossy(want),
                    String::from_utf8_lossy(&output)
                ));
            }
            println!("PASS {name}");
            Ok(())
        }
        (Expected::Truncated, Ok((output, truncated))) => {
            if !truncated || output.len() != max_output {
                return Err(format!(
                    "{name}: expected truncation at {max_output} bytes, got {} bytes truncated={truncated}",
                    output.len()
                ));
            }
            println!("PASS {name}");
            Ok(())
        }
        (Expected::Output(_), Err(error)) | (Expected::Truncated, Err(error)) => {
            Err(format!("{name}: {error:?}"))
        }
    }
}

fn probe_config() -> Result<SshConfig, String> {
    let host =
        env::var("OXIDE_PROBE_HOST").map_err(|_| "OXIDE_PROBE_HOST is required".to_string())?;
    let port = env::var("OXIDE_PROBE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(22);
    let username =
        env::var("OXIDE_PROBE_USER").map_err(|_| "OXIDE_PROBE_USER is required".to_string())?;

    let auth = if let Ok(identity_file) = env::var("OXIDE_PROBE_IDENTITY_FILE") {
        AuthMethod::key(identity_file, env::var("OXIDE_PROBE_PASSPHRASE").ok())
    } else if let Ok(password) = env::var("OXIDE_PROBE_PASSWORD") {
        AuthMethod::password(password)
    } else {
        AuthMethod::Agent
    };

    Ok(SshConfig {
        host,
        port,
        username,
        auth,
        trust_host_key: Some(true),
        skip_remote_env_detection: true,
        ..SshConfig::default()
    })
}
