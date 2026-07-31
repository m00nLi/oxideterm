use oxideterm_ssh::{SshConfig, SshPtyHandle, SshTransportClient, SshTransportCommand};
use std::time::Duration;
use zeroize::Zeroizing;

#[tokio::main]
async fn main() {
    let host = std::env::var("OXIDETERM_TEST_SSH_HOST")
        .expect("set OXIDETERM_TEST_SSH_HOST before running this test binary");
    let port: u16 = std::env::var("OXIDETERM_TEST_SSH_PORT")
        .unwrap_or_else(|_| "22".to_string())
        .parse()
        .expect("OXIDETERM_TEST_SSH_PORT must be a valid port number");
    let username = std::env::var("OXIDETERM_TEST_SSH_USERNAME")
        .expect("set OXIDETERM_TEST_SSH_USERNAME before running this test binary");
    // Read the password from an environment variable so credentials never
    // enter version control. Set OXIDETERM_TEST_SSH_PASSWORD before running.
    let password = std::env::var("OXIDETERM_TEST_SSH_PASSWORD")
        .expect("set OXIDETERM_TEST_SSH_PASSWORD before running this test binary");

    eprintln!("=== Test: OxideTerm SshTransportClient::connect_shell ===");

    let config = SshConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        auth: oxideterm_ssh::AuthMethod::password_secret(Zeroizing::new(password.to_string())),
        skip_remote_env_detection: true,
        ..SshConfig::default()
    };

    let client = SshTransportClient::new(config);
    eprintln!("=== Connecting ===");

    let pty_handle = client.connect_shell().await.expect("connect_shell failed");
    eprintln!(
        "=== Shell connected, session: {} ===",
        pty_handle.session_id
    );

    // SshPtyHandle implements Drop, so we can't move fields out.
    // Use ManuallyDrop to take ownership of fields.
    let mut pty = std::mem::ManuallyDrop::new(pty_handle);
    let command_tx = pty.command_tx.clone();
    // Safety: we never drop pty (ManuallyDrop), so the fields stay alive.
    // We only access output_rx by mutable reference.
    let output_rx = unsafe { &mut *std::ptr::addr_of_mut!((*pty).output_rx) };

    let start = std::time::Instant::now();

    loop {
        match output_rx.try_recv() {
            Ok(chunk) => {
                if start.elapsed() < Duration::from_secs(5) {
                    let s = String::from_utf8_lossy(&chunk);
                    let preview: String = s.chars().take(60).collect();
                    eprintln!(
                        "=== Output at {:?}: {}",
                        start.elapsed(),
                        preview.replace('\n', "\\n").replace('\r', "\\r")
                    );
                }
            }
            Err(_) => {}
        }

        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}

            _ = tokio::time::sleep(Duration::from_secs(80)) => {
                eprintln!("=== 80s, sending 'l' ===");
                match command_tx.send(SshTransportCommand::Data(b"l".to_vec())).await {
                    Ok(()) => eprintln!("=== Command sent ==="),
                    Err(e) => { eprintln!("=== Command send failed: {}", e); break; }
                }
                for _ in 0..100 {
                    match output_rx.try_recv() {
                        Ok(chunk) => {
                            let s = String::from_utf8_lossy(&chunk);
                            eprintln!("=== Response: {}", s);
                            break;
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                    }
                }
                break;
            }
        }
    }

    eprintln!("=== Done at {:?}", start.elapsed());
    let _ = command_tx.send(SshTransportCommand::Close).await;
}
