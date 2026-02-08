# Daemon Lifecycle Hardening

**Goal:** Make the daemon lifecycle rock-solid: signal handling, lock-based single-instance guarantee, proxy auto-reconnection on daemon death, and graceful error recovery.

**Architecture:**
- Daemon holds an exclusive `flock` for its entire lifetime, preventing duplicate instances
- SIGTERM/SIGINT trigger graceful shutdown via `tokio::select!` (stop Frida, flush DB, clean files)
- Idle timeout uses `Notify` instead of `process::exit(0)` for clean shutdown
- Accept error backoff prevents CPU spin on persistent listener errors
- Proxy wraps relay loop in a reconnection loop: on daemon death, respawn + reconnect + re-initialize MCP session transparently
- Per-session Frida worker threads: hook installation on session A can't block operations on session B

**Tech Stack:** tokio signals (already available via `features = ["full"]`), libc flock, tokio::sync::Notify
**Commit strategy:** Single commit at end.

## Workstreams

- **Stream A (daemon — `server.rs`):** Tasks 1, 2, 3
- **Stream B (proxy — `proxy.rs`):** Task 4
- **Stream C (frida — `spawner.rs`):** Task 6
- **Serial:** Task 5 (tests, depends on A, B, C)

---

### Task 1: Daemon-Side Lock File

The daemon should hold an exclusive flock on `~/.strobe/daemon.lock` for its entire lifetime. This prevents two daemons from running simultaneously even if multiple proxies spawn them concurrently.

**Files:**
- Modify: `src/daemon/server.rs:31-84`

**Step 1: Acquire exclusive lock at daemon startup**

At the top of `Daemon::run()`, after creating the strobe directory but before removing the old socket, acquire an exclusive non-blocking lock:

```rust
use std::os::unix::io::AsRawFd;

// In Daemon::run(), after std::fs::create_dir_all(&strobe_dir)?;

// Acquire exclusive lock — only one daemon can run at a time.
// The lock is held for the daemon's entire lifetime (lock_file lives until run() returns).
let lock_path = strobe_dir.join("daemon.lock");
let lock_file = std::fs::OpenOptions::new()
    .create(true)
    .write(true)
    .open(&lock_path)?;

let lock_result = unsafe {
    libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
};
if lock_result != 0 {
    // Another daemon holds the lock — exit cleanly
    tracing::info!("Another daemon is already running (lock held), exiting");
    return Ok(());
}
// _lock_file kept alive (not dropped) for the rest of this function
```

The `lock_file` variable must remain alive until the end of `run()`. When `run()` returns (graceful shutdown), the file is dropped and the OS releases the flock. If the daemon crashes, the OS also releases the flock.

**Checkpoint:** Only one daemon can run at a time. Extra daemon processes exit immediately and cleanly.

---

### Task 2: Signal Handling + Shutdown Notify + Accept Error Backoff

Three related changes to the daemon accept loop:
1. SIGTERM/SIGINT trigger graceful shutdown
2. Idle timeout uses `Notify` instead of `process::exit(0)` for clean return
3. Accept errors backoff and eventually trigger shutdown

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Add `shutdown_signal` field to Daemon**

```rust
pub struct Daemon {
    socket_path: PathBuf,
    pid_path: PathBuf,
    session_manager: Arc<SessionManager>,
    last_activity: Arc<RwLock<Instant>>,
    pending_patterns: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    connection_sessions: Arc<RwLock<HashMap<String, Vec<String>>>>,
    test_runs: Arc<tokio::sync::RwLock<HashMap<String, crate::test::TestRun>>>,
    /// Signaled by idle_timeout_loop to tell the accept loop to exit
    shutdown_signal: Arc<tokio::sync::Notify>,
}
```

Update constructor in `run()`:
```rust
let daemon = Arc::new(Self {
    // ... existing fields ...
    shutdown_signal: Arc::new(tokio::sync::Notify::new()),
});
```

Update `test_daemon()` helper in `mod tests`:
```rust
shutdown_signal: Arc::new(tokio::sync::Notify::new()),
```

**Step 2: Replace accept loop with signal-aware select!**

Replace `src/daemon/server.rs:69-83`:

```rust
use tokio::signal::unix::{signal, SignalKind};

let mut sigterm = signal(SignalKind::terminate())?;
let shutdown = Arc::clone(&daemon.shutdown_signal);
let mut consecutive_accept_errors: u32 = 0;

loop {
    tokio::select! {
        result = listener.accept() => {
            match result {
                Ok((stream, _)) => {
                    consecutive_accept_errors = 0;
                    let daemon = Arc::clone(&daemon);
                    tokio::spawn(async move {
                        if let Err(e) = daemon.handle_connection(stream).await {
                            tracing::error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    consecutive_accept_errors += 1;
                    tracing::error!("Accept error ({}/10): {}", consecutive_accept_errors, e);
                    if consecutive_accept_errors >= 10 {
                        tracing::error!("Too many consecutive accept errors, shutting down");
                        daemon.graceful_shutdown().await;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(
                        100 * consecutive_accept_errors as u64
                    )).await;
                }
            }
        }
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM, initiating graceful shutdown");
            daemon.graceful_shutdown().await;
            break;
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT, initiating graceful shutdown");
            daemon.graceful_shutdown().await;
            break;
        }
        _ = shutdown.notified() => {
            // Idle timeout already called graceful_shutdown
            break;
        }
    }
}
```

**Step 3: Update idle_timeout_loop to use Notify**

Replace `src/daemon/server.rs:86-97`:

```rust
async fn idle_timeout_loop(&self) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        let last = *self.last_activity.read().await;
        if last.elapsed() > IDLE_TIMEOUT {
            tracing::info!("Idle timeout reached, shutting down");
            self.graceful_shutdown().await;
            self.shutdown_signal.notify_one();
            return;
        }
    }
}
```

No more `std::process::exit(0)`. The notify wakes the accept loop which breaks, `run()` returns normally, tokio runtime shuts down cleanly.

**Checkpoint:** Daemon responds to SIGTERM/SIGINT/idle-timeout by running graceful_shutdown (stops Frida, flushes DB, cleans up socket/PID files), then exits cleanly. Accept errors trigger exponential backoff (100ms, 200ms, ...) and shutdown after 10 consecutive failures.

---

### Task 3: Simplify Proxy Lock Logic

Now that the daemon itself holds the flock, the proxy no longer needs its own lock acquisition. Multiple proxies can safely spawn daemon processes — only one daemon survives (the one that gets the flock).

**Files:**
- Modify: `src/mcp/proxy.rs` (remove the flock logic from the proxy)

The proxy's `ensure_daemon_and_connect` (Task 4) handles this:
1. Try to connect to existing daemon
2. If can't connect, clean stale files, spawn daemon
3. Wait for socket to become connectable

If two proxies both spawn a daemon, one daemon gets the flock and runs, the other exits cleanly. Both proxies eventually connect to the surviving daemon.

**Checkpoint:** Proxy no longer touches the lock file. Daemon-side lock is the single source of truth.

---

### Task 4: Proxy Reconnection with Auto-Respawn

The proxy should automatically reconnect to the daemon when it dies, respawning if necessary. On reconnection, it sends `initialize` + `initialized` to the new daemon to restore the MCP session state transparently.

**Files:**
- Rewrite: `src/mcp/proxy.rs`

**Step 1: Full rewrite of stdio_proxy**

```rust
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

        // Phase 2: On reconnection (not first connect), re-initialize the MCP session
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
                "id": "_proxy_reinit_1",
                "method": "initialized",
                "params": {}
            }));
            writer.write_all(initialized_msg.as_bytes()).await?;
            writer.flush().await?;
            // Read and discard the initialized response
            daemon_line.clear();
            daemon_reader.read_line(&mut daemon_line).await?;

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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log_file))
        .spawn()?;

    Ok(())
}
```

**Key design decisions:**
- `stdin_reader` is created once and persists across reconnections — prevents losing buffered input
- Reconnect counter resets after 60s of stable connection — prevents a single crash 30min ago from counting
- Max 3 rapid reconnects — daemon keeps crashing = give up with actionable error message
- `ensure_daemon_and_connect` tries fast connect first (500ms) before spawning — avoids spawning duplicate daemons for no reason
- On reconnect, proxy privately sends `initialize`/`initialized` to new daemon and discards responses — MCP client sees nothing

**Checkpoint:** Proxy automatically reconnects when daemon dies. Re-initializes MCP session transparently. In-flight MCP call is lost (client retries after timeout). Reconnection limit prevents infinite crash loops.

---

### Task 5: Tests

**Files:**
- Modify: `src/daemon/server.rs` (add to existing `mod tests` block)

**Step 1: Test lock file prevents duplicate daemons**

```rust
#[test]
fn test_daemon_lock_prevents_duplicates() {
    use std::os::unix::io::AsRawFd;

    let dir = tempdir().unwrap();
    let lock_path = dir.path().join("daemon.lock");

    // First lock acquisition should succeed
    let lock_file1 = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .unwrap();

    let result1 = unsafe {
        libc::flock(lock_file1.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
    };
    assert_eq!(result1, 0, "First lock should succeed");

    // Second lock acquisition should fail
    let lock_file2 = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .unwrap();

    let result2 = unsafe {
        libc::flock(lock_file2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
    };
    assert_ne!(result2, 0, "Second lock should fail while first is held");

    // After dropping first lock, acquisition should succeed
    drop(lock_file1);

    let result3 = unsafe {
        libc::flock(lock_file2.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
    };
    assert_eq!(result3, 0, "Lock should succeed after release");
}
```

**Step 2: Test graceful shutdown cleans up socket and PID files**

```rust
#[tokio::test]
async fn test_graceful_shutdown_cleans_files() {
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("test.sock");
    let pid_path = dir.path().join("test.pid");

    // Create the files
    std::fs::write(&socket_path, "").unwrap();
    std::fs::write(&pid_path, "12345").unwrap();
    assert!(socket_path.exists());
    assert!(pid_path.exists());

    let db_path = dir.path().join("test.db");
    let session_manager = Arc::new(SessionManager::new(&db_path).unwrap());

    let daemon = Daemon {
        socket_path: socket_path.clone(),
        pid_path: pid_path.clone(),
        session_manager,
        last_activity: Arc::new(RwLock::new(Instant::now())),
        pending_patterns: Arc::new(RwLock::new(HashMap::new())),
        connection_sessions: Arc::new(RwLock::new(HashMap::new())),
        test_runs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
        shutdown_signal: Arc::new(tokio::sync::Notify::new()),
    };

    daemon.graceful_shutdown().await;

    // Files should be cleaned up
    assert!(!socket_path.exists(), "Socket file should be removed");
    assert!(!pid_path.exists(), "PID file should be removed");
}
```

**Step 3: Test shutdown_signal wakes notify**

```rust
#[tokio::test]
async fn test_shutdown_signal_notify() {
    let (daemon, _dir) = test_daemon();

    // Notify should wake a waiting task
    let signal = Arc::clone(&daemon.shutdown_signal);
    let handle = tokio::spawn(async move {
        signal.notified().await;
        true
    });

    // Small delay to ensure the task is waiting
    tokio::time::sleep(Duration::from_millis(10)).await;
    daemon.shutdown_signal.notify_one();

    let result = tokio::time::timeout(Duration::from_secs(1), handle).await;
    assert!(result.is_ok(), "Notify should wake the waiting task");
    assert!(result.unwrap().unwrap(), "Task should return true");
}
```

Run all: `cargo test`
Expected: All existing tests still pass + 3 new tests pass.

**Checkpoint:** Lock file behavior verified. Graceful shutdown file cleanup verified. Shutdown notify mechanism verified.

---

### Task 6: Per-Session Frida Worker Threads

The current single `frida_worker` thread handles ALL sessions sequentially. When `AddPatterns` blocks for up to 60s waiting for hooks confirmation on session A, every other session is completely stalled. This makes concurrent debug sessions unreliable.

**Solution:** Split the single worker into a **coordinator thread** (device-level operations) + **per-session worker threads** (script-level operations that block).

**Files:**
- Modify: `src/frida_collector/spawner.rs`

**Step 1: Add SendScriptPtr wrapper and new command enums**

Replace `FridaCommand` with two enums and add a `SpawnResult` struct:

```rust
/// Wrapper to move raw script pointer across threads.
/// Safety: each session's script is only accessed by its dedicated worker thread.
struct SendScriptPtr(*mut frida_sys::_FridaScript);
unsafe impl Send for SendScriptPtr {}

/// Result returned by coordinator after spawning a process
struct SpawnResult {
    pid: u32,
    script_ptr: SendScriptPtr,
    hooks_ready: HooksReadySignal,
}

/// Commands for the coordinator thread (device-level operations)
enum CoordinatorCommand {
    Spawn {
        session_id: String,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        image_base: u64,
        event_tx: mpsc::Sender<Event>,
        response: oneshot::Sender<Result<SpawnResult>>,
    },
    StopSession {
        session_id: String,
        response: oneshot::Sender<Result<()>>,
    },
}

/// Commands for per-session worker threads (script-level operations)
enum SessionCommand {
    AddPatterns {
        functions: Vec<FunctionTarget>,
        image_base: u64,
        mode: HookMode,
        serialization_depth: Option<u32>,
        response: oneshot::Sender<Result<u32>>,
    },
    RemovePatterns {
        functions: Vec<FunctionTarget>,
        response: oneshot::Sender<Result<()>>,
    },
    SetWatches {
        watches: Vec<WatchTarget>,
        response: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}
```

Keep the old `FridaCommand` enum deleted — all usages will be replaced.

**Step 2: Extract coordinator_worker from frida_worker**

Rename `frida_worker` to `coordinator_worker`. It keeps: Device init, output_registry, spawn gating, and the command loop. But now it only handles `CoordinatorCommand::Spawn` and `CoordinatorCommand::StopSession`.

```rust
fn coordinator_worker(cmd_rx: std::sync::mpsc::Receiver<CoordinatorCommand>) {
    // ... same device init, output_registry setup, spawn gating setup as before ...

    loop {
        // Check for spawn notifications (non-blocking) — same as before
        while let Ok(child_pid) = spawn_rx.try_recv() {
            handle_child_spawn(&mut device, child_pid, &output_registry);
        }

        let cmd = match cmd_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(cmd) => cmd,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match cmd {
            CoordinatorCommand::Spawn {
                session_id, command, args, cwd, env,
                image_base, event_tx, response,
            } => {
                let result = (|| -> Result<SpawnResult> {
                    // device.spawn(), device.attach(), create_script_raw(),
                    // register output context, device.resume()
                    // ... same spawn logic as current code, minus initial_functions wait ...

                    Ok(SpawnResult {
                        pid,
                        script_ptr: SendScriptPtr(script_ptr),
                        hooks_ready,
                    })
                })();
                let _ = response.send(result);
            }

            CoordinatorCommand::StopSession { session_id, response } => {
                // Kill all PIDs for this session via output_registry + device.kill()
                // ... same kill logic as current Stop handler ...
                let _ = response.send(Ok(()));
            }
        }
    }
}
```

Key change: `handle_child_spawn` no longer needs `&sessions` — it already finds the parent session via `output_registry`. Update its signature to remove the `sessions` parameter.

**Step 3: Create session_worker function**

New function — one thread per session. Handles all script-level operations that may block:

```rust
fn session_worker(
    session_id: String,
    script_ptr: SendScriptPtr,
    hooks_ready: HooksReadySignal,
    pid: u32,
    cmd_rx: std::sync::mpsc::Receiver<SessionCommand>,
) {
    let raw_ptr = script_ptr.0;

    loop {
        let cmd = match cmd_rx.recv() {
            Ok(cmd) => cmd,
            Err(_) => break, // Channel closed (FridaSpawner dropped)
        };

        match cmd {
            SessionCommand::AddPatterns {
                functions, image_base, mode, serialization_depth, response,
            } => {
                // Move the current AddPatterns handler logic here:
                // 1. Build func_list JSON
                // 2. Set up hooks_ready signal sender
                // 3. post_message_raw(raw_ptr, &hooks_msg)
                // 4. Wait on signal_rx.recv_timeout(TIMEOUT_PER_CHUNK_SECS)
                // This blocks THIS session's thread only — other sessions unaffected
                let result = handle_add_patterns(raw_ptr, &hooks_ready, &functions, image_base, mode, serialization_depth);
                let _ = response.send(result);
            }

            SessionCommand::RemovePatterns { functions, response } => {
                // post_message_raw with remove action
                let result = handle_remove_patterns(raw_ptr, &functions);
                let _ = response.send(result);
            }

            SessionCommand::SetWatches { watches, response } => {
                // Same SetWatches logic with post + hooks_ready wait
                let result = handle_set_watches(raw_ptr, &hooks_ready, pid, &watches);
                let _ = response.send(result);
            }

            SessionCommand::Shutdown => {
                tracing::info!("Session worker {} shutting down", session_id);
                break;
            }
        }
    }
}
```

Extract the current command handler bodies into standalone functions (`handle_add_patterns`, `handle_remove_patterns`, `handle_set_watches`) for clarity. These are the same logic from the current `frida_worker` match arms, just factored into functions.

**Step 4: Update FridaSpawner**

Add `session_workers` map to route per-session commands:

```rust
pub struct FridaSpawner {
    sessions: HashMap<String, FridaSession>,
    coordinator_tx: std::sync::mpsc::Sender<CoordinatorCommand>,
    session_workers: HashMap<String, std::sync::mpsc::Sender<SessionCommand>>,
}

impl FridaSpawner {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            coordinator_worker(cmd_rx);
        });

        Self {
            sessions: HashMap::new(),
            coordinator_tx: cmd_tx,
            session_workers: HashMap::new(),
        }
    }
```

**Step 5: Update spawn() method**

After receiving SpawnResult from coordinator, spawn a session worker thread:

```rust
pub async fn spawn(&mut self, ...) -> Result<u32> {
    let (response_tx, response_rx) = oneshot::channel();

    self.coordinator_tx.send(CoordinatorCommand::Spawn {
        session_id: session_id.to_string(),
        command: command.to_string(),
        args: args.to_vec(),
        cwd: cwd.map(|s| s.to_string()),
        env: env.cloned(),
        image_base,
        event_tx: event_sender,
        response: response_tx,
    }).map_err(|_| crate::Error::Frida("Coordinator thread died".to_string()))?;

    let spawn_result = response_rx.await
        .map_err(|_| crate::Error::Frida("Coordinator response lost".to_string()))??;

    // Spawn dedicated worker thread for this session
    let (session_tx, session_rx) = std::sync::mpsc::channel();
    let sid = session_id.to_string();
    let pid = spawn_result.pid;
    thread::spawn(move || {
        session_worker(sid, spawn_result.script_ptr, spawn_result.hooks_ready, pid, session_rx);
    });
    self.session_workers.insert(session_id.to_string(), session_tx);

    // Store session state (same as before)
    self.sessions.insert(session_id.to_string(), FridaSession { ... });

    Ok(pid)
}
```

**Step 6: Update add_patterns/remove_patterns/set_watches methods**

Route to session worker channel instead of coordinator:

```rust
async fn send_add_chunk(&self, session_id: &str, functions: Vec<FunctionTarget>,
    image_base: u64, mode: HookMode, serialization_depth: Option<u32>,
) -> Result<u32> {
    let (response_tx, response_rx) = oneshot::channel();

    let worker_tx = self.session_workers.get(session_id)
        .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

    worker_tx.send(SessionCommand::AddPatterns {
        functions, image_base, mode, serialization_depth,
        response: response_tx,
    }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

    response_rx.await
        .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
}
```

Same pattern for `remove_patterns` and `set_watches` — route to `session_workers[session_id]`.

**Step 7: Update stop() method — two-phase shutdown**

```rust
pub async fn stop(&mut self, session_id: &str) -> Result<()> {
    self.sessions.remove(session_id);

    // Phase 1: Shut down session worker (stops script operations)
    if let Some(worker_tx) = self.session_workers.remove(session_id) {
        let _ = worker_tx.send(SessionCommand::Shutdown);
        // Worker thread will exit its loop and terminate
    }

    // Phase 2: Kill processes via coordinator (device.kill)
    let (response_tx, response_rx) = oneshot::channel();
    self.coordinator_tx.send(CoordinatorCommand::StopSession {
        session_id: session_id.to_string(),
        response: response_tx,
    }).map_err(|_| crate::Error::Frida("Coordinator thread died".to_string()))?;

    response_rx.await
        .map_err(|_| crate::Error::Frida("Coordinator response lost".to_string()))?
}
```

**Why this works:**
- `device` (not Send) stays on the coordinator thread — never moves
- `script_ptr` moves once from coordinator → session worker via `SendScriptPtr`
- Each session worker blocks independently — session A installing 10K hooks doesn't block session B
- `hooks_ready` Arc is shared between session worker and Frida's GLib callback thread (already thread-safe via Mutex)
- `output_registry` stays on coordinator — all pid→session mappings managed there
- Public API (`spawn`, `add_patterns`, `stop`, etc.) is unchanged — callers don't know about the internal split
- DWARF resolution still runs on the async caller thread (already the case) — no change needed

**Checkpoint:** Multiple concurrent sessions can install hooks, set watches, and stop independently without blocking each other. Only device-level operations (spawn, kill) are serialized through the coordinator.

---

## Summary of Changes

| File | Changes |
|------|---------|
| `src/daemon/server.rs` | Add `shutdown_signal: Arc<Notify>` field; acquire flock at startup (prevent duplicates); replace accept loop with `tokio::select!` (signals + shutdown notify + accept with backoff); update `idle_timeout_loop` to use Notify instead of `process::exit(0)`; update `test_daemon()` helper; add 3 new tests |
| `src/mcp/proxy.rs` | Full rewrite: reconnecting relay loop with `ensure_daemon_and_connect`; auto-respawn on daemon death; re-initialize MCP session on reconnect; reconnect limit with time-based reset; remove old `is_daemon_running`/`wait_for_daemon`/flock logic |
| `src/frida_collector/spawner.rs` | Replace single `FridaCommand` enum with `CoordinatorCommand` + `SessionCommand`; split `frida_worker` into `coordinator_worker` + per-session `session_worker` threads; add `SendScriptPtr` wrapper; extract handler functions; add `session_workers` map to `FridaSpawner`; two-phase stop (shutdown worker → kill via coordinator) |

**No new files. No new dependencies.** (`tokio::signal` already available via `features = ["full"]`)

## Architecture After Changes

```
Claude Code (MCP client)
  |
  | stdio (JSON-RPC lines)
  v
strobe mcp (proxy — reconnecting relay)
  |  <- auto-reconnect on daemon death
  |  <- re-initialize MCP session transparently
  |  <- max 3 rapid reconnects, then fail
  |
  | Unix socket (~/.strobe/strobe.sock)
  v
strobe daemon (singleton, flock-protected)
  |-- tokio::select! accept loop
  |     |-- listener.accept() -> spawn connection task
  |     |-- SIGTERM -> graceful_shutdown + break
  |     |-- SIGINT  -> graceful_shutdown + break
  |     |-- shutdown_signal (from idle timeout) -> break
  |     |-- accept error backoff (100ms * n, max 10)
  |
  |-- idle_timeout_loop (30min)
  |     |-- graceful_shutdown() + notify
  |
  |-- Frida (per-session worker architecture)
  |     |-- coordinator_worker thread
  |     |     |-- device.spawn() / device.attach() / device.resume()
  |     |     |-- device.kill() (on StopSession)
  |     |     |-- output_registry (pid → session mapping)
  |     |     |-- spawn gating (child process interception)
  |     |
  |     |-- session_worker thread (session A)
  |     |     |-- AddPatterns: post_message + wait hooks_ready
  |     |     |-- RemovePatterns: post_message
  |     |     |-- SetWatches: post_message + wait confirmation
  |     |
  |     |-- session_worker thread (session B)  ← independent, non-blocking
  |     |     |-- (same as above)
  |     |
  |     |-- session_worker thread (session C) ...
  |
  |-- graceful_shutdown()
        |-- Phase 1: stop_frida on all running sessions
        |-- Phase 2: sleep 100ms for DB flush
        |-- Phase 3: stop_session on all sessions
        |-- Phase 4: remove socket + PID files
        |-- flock released when run() returns
```

| File | Changes |
|------|---------|
| `src/daemon/server.rs` | Add `shutdown_signal: Arc<Notify>` field; acquire flock at startup (prevent duplicates); replace accept loop with `tokio::select!` (signals + shutdown notify + accept with backoff); update `idle_timeout_loop` to use Notify instead of `process::exit(0)`; update `test_daemon()` helper; add 3 new tests |
| `src/mcp/proxy.rs` | Full rewrite: reconnecting relay loop with `ensure_daemon_and_connect`; auto-respawn on daemon death; re-initialize MCP session on reconnect; reconnect limit with time-based reset; remove old `is_daemon_running`/`wait_for_daemon`/flock logic |

**No new files. No new dependencies.** (`tokio::signal` already available via `features = ["full"]`)

## Architecture After Changes

```
Claude Code (MCP client)
  |
  | stdio (JSON-RPC lines)
  v
strobe mcp (proxy — reconnecting relay)
  |  <- auto-reconnect on daemon death
  |  <- re-initialize MCP session transparently
  |  <- max 3 rapid reconnects, then fail
  |
  | Unix socket (~/.strobe/strobe.sock)
  v
strobe daemon (singleton, flock-protected)
  |-- tokio::select! accept loop
  |     |-- listener.accept() -> spawn connection task
  |     |-- SIGTERM -> graceful_shutdown + break
  |     |-- SIGINT  -> graceful_shutdown + break
  |     |-- shutdown_signal (from idle timeout) -> break
  |     |-- accept error backoff (100ms * n, max 10)
  |
  |-- idle_timeout_loop (30min)
  |     |-- graceful_shutdown() + notify
  |
  |-- graceful_shutdown()
        |-- Phase 1: stop_frida on all running sessions
        |-- Phase 2: sleep 100ms for DB flush
        |-- Phase 3: stop_session on all sessions
        |-- Phase 4: remove socket + PID files
        |-- flock released when run() returns
```
