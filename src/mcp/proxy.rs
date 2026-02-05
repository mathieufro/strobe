use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use crate::Result;

/// Stdio proxy that connects MCP clients to the daemon.
/// Launches daemon if not running.
pub async fn stdio_proxy() -> Result<()> {
    let strobe_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".strobe");

    let socket_path = strobe_dir.join("strobe.sock");
    let pid_path = strobe_dir.join("strobe.pid");

    // Check if daemon is running, start if not
    if !is_daemon_running(&pid_path, &socket_path).await {
        start_daemon().await?;
        // Wait for socket to be available
        for _ in 0..50 {
            if socket_path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Connect to daemon
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut daemon_reader = BufReader::new(reader);

    // Read from stdin, write to daemon
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stdin_reader = BufReader::new(stdin);

    let mut stdin_line = String::new();
    let mut daemon_line = String::new();

    loop {
        tokio::select! {
            // Read from stdin -> send to daemon
            result = stdin_reader.read_line(&mut stdin_line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        writer.write_all(stdin_line.as_bytes()).await?;
                        writer.flush().await?;
                        stdin_line.clear();
                    }
                    Err(e) => {
                        eprintln!("stdin error: {}", e);
                        break;
                    }
                }
            }
            // Read from daemon -> send to stdout
            result = daemon_reader.read_line(&mut daemon_line) => {
                match result {
                    Ok(0) => break, // Daemon disconnected
                    Ok(_) => {
                        stdout.write_all(daemon_line.as_bytes()).await?;
                        stdout.flush().await?;
                        daemon_line.clear();
                    }
                    Err(e) => {
                        eprintln!("daemon error: {}", e);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn is_daemon_running(pid_path: &PathBuf, socket_path: &PathBuf) -> bool {
    if !pid_path.exists() || !socket_path.exists() {
        return false;
    }

    // Read PID and check if process exists
    if let Ok(pid_str) = std::fs::read_to_string(pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Check if process exists (Unix-specific)
            unsafe {
                return libc::kill(pid, 0) == 0;
            }
        }
    }

    false
}

async fn start_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;

    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
