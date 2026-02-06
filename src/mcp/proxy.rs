use std::os::unix::io::AsRawFd;
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

    std::fs::create_dir_all(&strobe_dir)?;

    let socket_path = strobe_dir.join("strobe.sock");
    let pid_path = strobe_dir.join("strobe.pid");

    // Ensure daemon is running
    if !is_daemon_running(&pid_path, &socket_path).await {
        // Use a lock file to prevent multiple proxies from starting daemons simultaneously
        let lock_path = strobe_dir.join("daemon.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;

        let lock_result = unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
        };

        if lock_result == 0 {
            // We won the race — start the daemon
            start_daemon(&strobe_dir)?;
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK) {
                return Err(crate::Error::Io(err));
            }
            // EWOULDBLOCK: another proxy holds the lock, fall through to wait
        }

        // Drop the lock after spawning (or if we didn't get it)
        drop(lock_file);

        // Wait for socket to be connectable (not just existing)
        wait_for_daemon(&socket_path).await?;
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
            if unsafe { libc::kill(pid, 0) } != 0 {
                // Process doesn't exist — stale files
                let _ = std::fs::remove_file(socket_path);
                let _ = std::fs::remove_file(pid_path);
                return false;
            }
            // Process exists — verify socket is connectable
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                UnixStream::connect(socket_path),
            ).await {
                Ok(Ok(stream)) => {
                    drop(stream);
                    return true;
                }
                _ => {
                    // PID exists but socket isn't accepting — could be stale PID
                    return false;
                }
            }
        }
    }

    false
}

async fn wait_for_daemon(socket_path: &PathBuf) -> Result<()> {
    for _attempt in 0..50 {
        if socket_path.exists() {
            // Try actual connection to verify daemon is accepting
            match tokio::time::timeout(
                std::time::Duration::from_millis(100),
                UnixStream::connect(socket_path),
            ).await {
                Ok(Ok(stream)) => {
                    // Connected successfully — daemon is ready
                    drop(stream);
                    return Ok(());
                }
                _ => {
                    // Socket exists but not ready yet, or timeout
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    Err(crate::Error::Io(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "Daemon failed to start within 5 seconds",
    )))
}

fn start_daemon(strobe_dir: &PathBuf) -> Result<()> {
    let exe = std::env::current_exe()?;
    let log_path = strobe_dir.join("daemon.log");

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()?;

    Ok(())
}
