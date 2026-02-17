use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use crate::Result;

/// Max reconnection attempts within the reset window before giving up.
const MAX_RECONNECT_ATTEMPTS: u32 = 3;
/// If the connection was stable for this long, reset the reconnect counter.
const RECONNECT_RESET_SECS: u64 = 60;

enum RelayResult {
    /// MCP client closed stdin — normal exit
    StdinClosed,
    /// Daemon disconnected (crashed or shut down)
    DaemonDisconnected,
}

/// Stdio proxy that connects MCP clients to the daemon.
/// Launches daemon if not running. Reconnects automatically on daemon death.
pub async fn stdio_proxy() -> Result<()> {
    let strobe_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".strobe");

    std::fs::create_dir_all(&strobe_dir)?;

    let socket_path = strobe_dir.join("strobe.sock");

    // Create stdin reader ONCE — persists across reconnections to avoid losing buffered data
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stdin_reader = BufReader::new(stdin);
    let mut stdin_line = String::new();

    let mut first_connect = true;
    let mut reconnect_count: u32 = 0;
    let mut last_connected = std::time::Instant::now();

    loop {
        // Phase 1: Ensure daemon is running and connect
        let stream = ensure_daemon_and_connect(&strobe_dir, &socket_path).await?;
        let (reader, mut writer) = stream.into_split();
        let mut daemon_reader = BufReader::new(reader);
        let mut daemon_line = String::new();

        // Phase 2: On reconnection (not first connect), re-initialize the MCP session.
        // The MCP client already sent initialize on the first connection and won't resend it.
        // The new daemon requires initialize before accepting tool calls.
        if !first_connect {
            // Reset counter if the previous connection was stable
            if last_connected.elapsed() > Duration::from_secs(RECONNECT_RESET_SECS) {
                reconnect_count = 0;
            }
            reconnect_count += 1;
            if reconnect_count > MAX_RECONNECT_ATTEMPTS {
                return Err(crate::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    format!(
                        "Daemon keeps crashing ({} reconnects in {}s). Check ~/.strobe/daemon.log",
                        reconnect_count - 1,
                        RECONNECT_RESET_SECS
                    ),
                )));
            }

            // Send initialize to new daemon
            let init_msg = format!("{}\n", serde_json::json!({
                "jsonrpc": "2.0",
                "id": "_proxy_reinit_0",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "strobe-proxy-reconnect",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }));
            writer.write_all(init_msg.as_bytes()).await?;
            writer.flush().await?;
            // Read and discard the initialize response
            daemon_line.clear();
            daemon_reader.read_line(&mut daemon_line).await?;

            // Send initialized notification
            let initialized_msg = format!("{}\n", serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }));
            writer.write_all(initialized_msg.as_bytes()).await?;
            writer.flush().await?;

            eprintln!("Reconnected to daemon successfully");
        }
        first_connect = false;
        last_connected = std::time::Instant::now();

        // Phase 3: Bidirectional relay (stdin <-> daemon socket)
        let result = loop {
            daemon_line.clear();
            tokio::select! {
                result = stdin_reader.read_line(&mut stdin_line) => {
                    match result {
                        Ok(0) => break RelayResult::StdinClosed,
                        Ok(_) => {
                            if writer.write_all(stdin_line.as_bytes()).await.is_err() {
                                stdin_line.clear();
                                break RelayResult::DaemonDisconnected;
                            }
                            let _ = writer.flush().await;
                            stdin_line.clear();
                        }
                        Err(_) => break RelayResult::StdinClosed,
                    }
                }
                result = daemon_reader.read_line(&mut daemon_line) => {
                    match result {
                        Ok(0) => break RelayResult::DaemonDisconnected,
                        Ok(_) => {
                            let _ = stdout.write_all(daemon_line.as_bytes()).await;
                            let _ = stdout.flush().await;
                        }
                        Err(_) => break RelayResult::DaemonDisconnected,
                    }
                }
            }
        };

        match result {
            RelayResult::StdinClosed => break, // Client exited normally
            RelayResult::DaemonDisconnected => {
                eprintln!("Daemon disconnected, attempting reconnect...");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue; // Loop back to ensure_daemon_and_connect
            }
        }
    }

    Ok(())
}

/// Try to connect to an existing daemon, or spawn one and connect.
async fn ensure_daemon_and_connect(strobe_dir: &Path, socket_path: &Path) -> Result<UnixStream> {
    // Fast path: daemon may already be running
    if let Ok(Ok(stream)) = tokio::time::timeout(
        Duration::from_millis(500),
        UnixStream::connect(socket_path),
    ).await {
        return Ok(stream);
    }

    // Daemon not available — clean stale files, spawn new daemon
    cleanup_stale_files(strobe_dir);
    start_daemon(strobe_dir)?;

    // Wait for daemon to become connectable (50 attempts x 100ms = 5s)
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Ok(Ok(stream)) = tokio::time::timeout(
            Duration::from_millis(100),
            UnixStream::connect(socket_path),
        ).await {
            return Ok(stream);
        }
    }

    Err(crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Daemon failed to start within 5 seconds. Check ~/.strobe/daemon.log",
    )))
}

/// Remove stale PID and socket files if the daemon process is dead.
fn cleanup_stale_files(strobe_dir: &Path) {
    let pid_path = strobe_dir.join("strobe.pid");
    let socket_path = strobe_dir.join("strobe.sock");

    if let Ok(pid_str) = std::fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            if unsafe { libc::kill(pid, 0) } != 0 {
                // Process dead — clean up stale files
                let _ = std::fs::remove_file(&socket_path);
                let _ = std::fs::remove_file(&pid_path);
            }
        }
    }
}

fn start_daemon(strobe_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log_path = strobe_dir.join("daemon.log");

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    std::process::Command::new(exe)
        .arg("daemon")
        .env("RUST_LOG", std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()?;

    Ok(())
}
