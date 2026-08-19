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

use oxideterm_ssh::{
    AuthMethod, SshConfig,
    monitor_probe::{connect_probe, connect_sampler_probe},
    reconnectable_monitor_sampler,
};

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
    let keepalive_interval = env::var("OXIDE_PROBE_KEEPALIVE_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(5);
    let keepalive_data = vec![b'\n'];
    println!(
        "connecting to {}@{}:{} (host key auto-trusted, keepalive {}s)",
        config.username, config.host, config.port, keepalive_interval
    );
    let mut session = connect_probe(config.clone(), keepalive_interval, keepalive_data.clone())
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

    // A fully quiet dedicated connection must survive idle with channel-data
    // keepalive on servers that otherwise drop idle transports.
    tokio::time::sleep(Duration::from_secs(12)).await;
    check(
        &mut session,
        "idle keepalive survival",
        "echo idle-ok",
        Duration::from_secs(10),
        4096,
        Expected::Output(b"idle-ok"),
    )
    .await?;

    // Dedicated sampler connection: validates the profiler/GPU read path on
    // the same single-channel server with one shell channel.
    let sampler = connect_sampler_probe(config.clone(), keepalive_interval, keepalive_data.clone())
        .await
        .map_err(|error| error.to_string())?;
    let sampled = sampler
        .sample_until(
            "",
            "echo profiler-ok; echo __PROBE_SAMPLE_END__",
            "__PROBE_SAMPLE_END__",
            Duration::from_secs(15),
            64 * 1024,
        )
        .await
        .map_err(|error| format!("profiler sample read: {error}"))?;
    if !sampled.contains("profiler-ok") {
        let preview = String::from_utf8_lossy(&sampled.as_bytes()[..sampled.len().min(128)]);
        return Err(format!(
            "profiler sample read: marker output missing, got {:?}",
            preview
        ));
    }
    println!("PASS profiler sample read ({} bytes)", sampled.len());

    // Appliance diagnosis: run the exact tmux snapshot script used by the
    // host tools page and print its raw output when it reports an error.
    let tmux_command = oxideterm_connection_monitor::build_tmux_snapshot_command("Linux").command;
    let (tmux_output, _) = session
        .run_command(&tmux_command, Duration::from_secs(15), 64 * 1024)
        .await
        .map_err(|error| format!("tmux snapshot probe: {error:?}"))?;
    let tmux_text = String::from_utf8_lossy(&tmux_output);
    println!("TMUX RAW OUTPUT ({} bytes):", tmux_output.len());
    println!("{}", debug_escape(&tmux_text));
    let tmux_snapshot = oxideterm_connection_monitor::parse_tmux_snapshot(&tmux_text);
    println!("TMUX PARSE STATUS: {:?}", tmux_snapshot.status);
    println!(
        "TMUX PARSE ROWS: {} sessions, {} windows, {} panes",
        tmux_snapshot.sessions.len(),
        tmux_snapshot.windows.len(),
        tmux_snapshot.panes.len()
    );
    if matches!(
        tmux_snapshot.status,
        oxideterm_connection_monitor::ResourceTmuxStatus::Error { .. }
    ) {
        return Err("tmux snapshot parsed with an error status".to_string());
    }
    println!("PASS tmux snapshot read ({} bytes)", tmux_output.len());

    // GPU diagnosis: time each vendor tool independently so a single hung
    // probe is visible instead of masking every vendor behind one 30-second
    // page timeout, then run the exact full GPU command through the sampler.
    let vendor_timing = concat!(
        "probe_tool() { name=$1; shift; ",
        "if ! command -v \"$name\" >/dev/null 2>&1; then echo \"$name absent\"; return; fi; ",
        "start=$(date +%s%N); ",
        "if command -v timeout >/dev/null 2>&1; then timeout 8 \"$@\" >/dev/null 2>&1; rc=$?; ",
        "else \"$@\" >/dev/null 2>&1; rc=$?; fi; ",
        "end=$(date +%s%N); echo \"$name rc=$rc wall_ms=$(( (end - start) / 1000000 ))\"; }; ",
        "probe_tool nvidia-smi --query-gpu=index,name --format=csv,noheader,nounits; ",
        "probe_tool nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader,nounits; ",
        "probe_tool npu-smi info; ",
        "probe_tool amd-smi --json; ",
        "probe_tool rocm-smi; ",
        "probe_tool cnmon info; ",
        "probe_tool hy-smi; ",
        "probe_tool xpu-smi discovery -j; ",
        "probe_tool mthreads-gmi"
    );
    let (timing_output, timing_truncated) = session
        .run_command(vendor_timing, Duration::from_secs(100), 64 * 1024)
        .await
        .map_err(|error| format!("vendor timing probe: {error:?}"))?;
    if timing_truncated {
        println!("VENDOR TIMING TRUNCATED");
    }
    println!(
        "VENDOR TIMING:\n{}",
        String::from_utf8_lossy(&timing_output)
    );

    // Replay the exact app path: the app's GPU page uses the reconnecting
    // sampler, which opens a brand-new connection per shell. The persistent
    // probe sampler above would reuse one transport and trigger the server's
    // second-channel kill instead of exercising the vendor command.
    let gpu_command = oxideterm_connection_monitor::build_gpu_sample_command("Linux");
    let gpu_sampler =
        reconnectable_monitor_sampler(config.clone(), keepalive_interval, keepalive_data.clone());
    let mut gpu_shell = gpu_sampler
        .open_shell("", Duration::from_secs(10))
        .await
        .map_err(|error| format!("GPU sampler shell open: {error}"))?;
    let sampled = gpu_shell
        .sample_until(
            &gpu_command,
            oxideterm_connection_monitor::GPU_END_MARKER,
            Duration::from_secs(45),
            512 * 1024,
        )
        .await
        .map_err(|error| format!("full GPU sample: {error}"))?;
    println!(
        "FULL GPU SAMPLE: {} bytes, end marker {}",
        sampled.len(),
        if sampled.contains(oxideterm_connection_monitor::GPU_END_MARKER) {
            "present"
        } else {
            "MISSING"
        }
    );
    if !sampled.contains(oxideterm_connection_monitor::GPU_END_MARKER) {
        println!(
            "FULL GPU SAMPLE TAIL:\n{}",
            String::from_utf8_lossy(&sampled.as_bytes()[sampled.len().saturating_sub(2048)..])
        );
    }

    println!("ALL PROBE SCENARIOS PASSED");
    Ok(())
}

fn debug_escape(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.chars() {
        match character {
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n\n"),
            '\r' => output.push_str("\\r"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{{{:x}}}", character as u32));
            }
            character => output.push(character),
        }
    }
    output
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
