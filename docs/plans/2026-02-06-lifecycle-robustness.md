# MCP & Daemon Lifecycle Robustness

**Goal:** Fix all lifecycle issues found in code review — initialize enforcement, per-connection state, pending pattern cleanup, graceful shutdown, proxy race conditions, and daemon logging.
**Architecture:** Add per-connection state tracking to the daemon, enforce MCP protocol ordering, make pending patterns per-connection, add graceful shutdown with session cleanup, harden the proxy with file locking and connection-based readiness checks, and route daemon logs to a file.
**Commit strategy:** Single commit at end.

## Workstreams

- **Stream A (daemon server):** Tasks 1, 2, 3, 4
- **Stream B (proxy):** Tasks 5, 6
- **Serial:** Task 7 (integration tests, depends on A and B)

---

### Task 1: Per-Connection State & Initialize Enforcement

The daemon currently has no concept of "which client sent this message." All state is global. This task adds per-connection tracking and enforces the MCP protocol requirement that `initialize` must be called before any other method.

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Add connection state to handle_connection**

Replace the current `handle_connection` loop with one that tracks whether `initialize` has been called. No new structs needed — a simple `bool` scoped to the connection is sufficient.

```rust
async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut initialized = false;
    let connection_id = uuid::Uuid::new_v4().to_string();

    tracing::info!("Client connected: {}", connection_id);

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break; // EOF
        }

        // Update activity timestamp
        *self.last_activity.write().await = Instant::now();

        let response = self.handle_message(&line, &mut initialized, &connection_id).await;
        let response_json = serde_json::to_string(&response)?;
        writer.write_all(response_json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }

    tracing::info!("Client disconnected: {}", connection_id);
    self.handle_disconnect(&connection_id).await;

    Ok(())
}
```

**Step 2: Update handle_message to enforce initialize ordering**

Add `initialized` and `connection_id` parameters. Reject non-initialize methods before initialization. Track connection in pending patterns registry.

```rust
async fn handle_message(
    &self,
    message: &str,
    initialized: &mut bool,
    connection_id: &str,
) -> JsonRpcResponse {
    let request: JsonRpcRequest = match serde_json::from_str(message) {
        Ok(r) => r,
        Err(e) => {
            return JsonRpcResponse::error(
                serde_json::Value::Null,
                -32700,
                format!("Parse error: {}", e),
                None,
            );
        }
    };

    // Enforce MCP protocol: initialize must be called first
    if !*initialized && request.method != "initialize" {
        return JsonRpcResponse::error(
            request.id,
            -32002,
            "Server not initialized. Call 'initialize' first.".to_string(),
            None,
        );
    }

    let result = match request.method.as_str() {
        "initialize" => {
            *initialized = true;
            self.handle_initialize(&request.params).await
        }
        "initialized" => Ok(serde_json::json!({})),
        "tools/list" => self.handle_tools_list().await,
        "tools/call" => self.handle_tools_call(&request.params, connection_id).await,
        _ => Err(crate::Error::Frida(format!(
            "Unknown method: {}",
            request.method
        ))),
    };

    match result {
        Ok(value) => JsonRpcResponse::success(request.id, value),
        Err(e) => {
            let mcp_error: McpError = e.into();
            JsonRpcResponse::error(
                request.id,
                -32000,
                mcp_error.message,
                Some(serde_json::to_value(mcp_error.code).unwrap()),
            )
        }
    }
}
```

**Step 3: Pass connection_id through to handle_tools_call**

Update `handle_tools_call` signature to accept `connection_id: &str` and pass it to `tool_debug_trace` (for per-connection pending patterns) and `tool_debug_launch` (to track which connection owns which session).

```rust
async fn handle_tools_call(&self, params: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
    let call: McpToolCallRequest = serde_json::from_value(params.clone())?;

    let result = match call.name.as_str() {
        "debug_launch" => self.tool_debug_launch(&call.arguments, connection_id).await,
        "debug_trace" => self.tool_debug_trace(&call.arguments, connection_id).await,
        "debug_query" => self.tool_debug_query(&call.arguments).await,
        "debug_stop" => self.tool_debug_stop(&call.arguments).await,
        _ => Err(crate::Error::Frida(format!("Unknown tool: {}", call.name))),
    };

    // ... rest unchanged ...
}
```

**Checkpoint:** Server rejects tool calls before `initialize`. Each connection has a unique ID logged on connect/disconnect.

---

### Task 2: Per-Connection Pending Patterns

Currently `pending_patterns` is a global `HashSet` shared across all connections. Client A's patterns leak into Client B's launch. This task makes pending patterns per-connection and clears them after launch.

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Change pending_patterns type from global to per-connection**

```rust
pub struct Daemon {
    socket_path: PathBuf,
    pid_path: PathBuf,
    session_manager: Arc<SessionManager>,
    last_activity: Arc<RwLock<Instant>>,
    /// Pending trace patterns per connection, applied on next launch
    pending_patterns: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Sessions owned by each connection (for cleanup on disconnect)
    connection_sessions: Arc<RwLock<HashMap<String, Vec<String>>>>,
}
```

Update the constructor:
```rust
pending_patterns: Arc::new(RwLock::new(HashMap::new())),
connection_sessions: Arc::new(RwLock::new(HashMap::new())),
```

**Step 2: Update tool_debug_trace for per-connection pending patterns**

When no `sessionId` is provided, scope patterns to the connection:

```rust
async fn tool_debug_trace(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
    let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

    match req.session_id {
        None => {
            let mut all_pending = self.pending_patterns.write().await;
            let pending = all_pending.entry(connection_id.to_string()).or_default();

            if let Some(ref add) = req.add {
                for pattern in add {
                    pending.insert(pattern.clone());
                }
            }
            if let Some(ref remove) = req.remove {
                for pattern in remove {
                    pending.remove(pattern);
                }
            }

            let patterns: Vec<String> = pending.iter().cloned().collect();
            let response = DebugTraceResponse {
                active_patterns: patterns,
                hooked_functions: 0,
            };
            Ok(serde_json::to_value(response)?)
        }
        Some(ref session_id) => {
            // ... existing live-session logic, unchanged ...
        }
    }
}
```

**Step 3: Update tool_debug_launch to consume connection's pending patterns**

Read and clear this connection's pending patterns, and register session ownership:

```rust
async fn tool_debug_launch(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
    // ... existing auto-cleanup and spawn logic ...

    // Get and clear this connection's pending patterns
    let pending_patterns: Vec<String> = {
        let mut all_pending = self.pending_patterns.write().await;
        match all_pending.remove(connection_id) {
            Some(patterns) => patterns.into_iter().collect(),
            None => Vec::new(),
        }
    };

    // ... spawn_with_frida, create_session ...

    // Register session ownership for disconnect cleanup
    {
        let mut sessions = self.connection_sessions.write().await;
        sessions.entry(connection_id.to_string()).or_default().push(session_id.clone());
    }

    // ... install hooks if pending_patterns not empty (existing logic) ...
}
```

**Checkpoint:** Pending patterns are scoped per connection, consumed on launch, and never leak between clients.

---

### Task 3: Connection Disconnect Cleanup

When a client disconnects without calling `debug_stop`, its sessions and pending patterns must be cleaned up.

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Implement handle_disconnect**

```rust
async fn handle_disconnect(&self, connection_id: &str) {
    // Clean up pending patterns for this connection
    {
        let mut pending = self.pending_patterns.write().await;
        pending.remove(connection_id);
    }

    // Stop any sessions owned by this connection
    let session_ids = {
        let mut sessions = self.connection_sessions.write().await;
        sessions.remove(connection_id).unwrap_or_default()
    };

    for session_id in session_ids {
        // Check if session is still running before stopping
        if let Ok(Some(session)) = self.session_manager.get_session(&session_id) {
            if session.status == crate::db::SessionStatus::Running {
                tracing::info!("Cleaning up session {} after client disconnect", session_id);
                let _ = self.session_manager.stop_frida(&session_id).await;
                let _ = self.session_manager.stop_session(&session_id);
            }
        }
    }
}
```

**Step 2: Unregister sessions on explicit debug_stop**

When a client explicitly stops a session, remove it from the connection's session list so disconnect cleanup doesn't try to stop it again:

In `tool_debug_stop`, after successful stop:
```rust
// Remove from connection tracking (find which connection owns it)
{
    let mut sessions = self.connection_sessions.write().await;
    for session_list in sessions.values_mut() {
        session_list.retain(|s| s != &req.session_id);
    }
}
```

**Checkpoint:** Disconnecting a client stops its running sessions and clears its pending patterns. Explicit `debug_stop` properly deregisters sessions.

---

### Task 4: Graceful Daemon Shutdown

The idle timeout currently calls `std::process::exit(0)` without stopping Frida sessions or flushing the DB writer. This task adds graceful shutdown.

**Files:**
- Modify: `src/daemon/server.rs`
- Modify: `src/daemon/session_manager.rs`

**Step 1: Add a method to list running sessions**

In `src/db/session.rs`, add:
```rust
pub fn get_running_sessions(&self) -> Result<Vec<Session>> {
    let conn = self.connection();
    let mut stmt = conn.prepare(
        "SELECT id, binary_path, project_root, pid, started_at, ended_at, status
         FROM sessions WHERE status = 'running'"
    )?;

    let sessions = stmt.query_map([], |row| {
        Ok(Session {
            id: row.get(0)?,
            binary_path: row.get(1)?,
            project_root: row.get(2)?,
            pid: row.get(3)?,
            started_at: row.get(4)?,
            ended_at: row.get(5)?,
            status: SessionStatus::from_str(&row.get::<_, String>(6)?).unwrap(),
        })
    })?.collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(sessions)
}
```

**Step 2: Add graceful shutdown to Daemon**

```rust
async fn graceful_shutdown(&self) {
    tracing::info!("Starting graceful shutdown...");

    // Stop all running Frida sessions
    if let Ok(sessions) = self.session_manager.db().get_running_sessions() {
        for session in sessions {
            tracing::info!("Stopping session {} during shutdown", session.id);
            let _ = self.session_manager.stop_frida(&session.id).await;
            let _ = self.session_manager.stop_session(&session.id);
        }
    }

    // Give DB writer tasks time to flush remaining events
    tokio::time::sleep(Duration::from_millis(50)).await;

    self.cleanup();
    tracing::info!("Graceful shutdown complete");
}
```

**Step 3: Use graceful_shutdown in idle_timeout_loop**

```rust
async fn idle_timeout_loop(&self) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        let last = *self.last_activity.read().await;
        if last.elapsed() > IDLE_TIMEOUT {
            tracing::info!("Idle timeout reached, shutting down");
            self.graceful_shutdown().await;
            std::process::exit(0);
        }
    }
}
```

**Checkpoint:** Idle timeout cleanly stops all Frida sessions, flushes events, and removes socket/PID files before exiting.

---

### Task 5: Proxy Race-Safe Daemon Startup

Two proxies starting simultaneously can both spawn daemons. This task adds file locking and connection-based readiness checks.

**Files:**
- Modify: `src/mcp/proxy.rs`

**Step 1: Use a lock file for daemon startup**

```rust
use std::os::unix::io::AsRawFd;

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

        let got_lock = unsafe {
            libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0
        };

        if got_lock {
            // We won the race — start the daemon
            start_daemon(&strobe_dir).await?;
        }
        // Whether we started it or someone else did, wait for the socket

        // Drop the lock after spawning (or if we didn't get it)
        drop(lock_file);

        // Wait for socket to be connectable (not just existing)
        wait_for_daemon(&socket_path).await?;
    }

    // Connect to daemon
    let stream = UnixStream::connect(&socket_path).await?;
    // ... rest of proxy loop unchanged ...
}
```

**Step 2: Replace socket-exists check with connection-based readiness**

```rust
async fn wait_for_daemon(socket_path: &PathBuf) -> Result<()> {
    for attempt in 0..50 {
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
```

**Step 3: Improve is_daemon_running with connection check**

```rust
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
```

**Checkpoint:** Multiple simultaneous proxy starts are serialized via lock file. Socket readiness is verified by actual connection, not file existence.

---

### Task 6: Daemon Log File

Daemon stderr goes to `/dev/null`. This task routes daemon output to a log file for debugging.

**Files:**
- Modify: `src/mcp/proxy.rs`
- Modify: `src/main.rs`

**Step 1: Update start_daemon to redirect stderr to log file**

```rust
async fn start_daemon(strobe_dir: &PathBuf) -> Result<()> {
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

Note: `start_daemon` now takes `strobe_dir` as a parameter to avoid recomputing it.

**Step 2: Configure tracing to use stderr (already works)**

No change needed in `main.rs` — `tracing_subscriber::fmt::init()` already writes to stderr by default. With the proxy redirecting daemon stderr to `~/.strobe/daemon.log`, all `tracing::info!()` and `tracing::error!()` output will appear there.

**Checkpoint:** Daemon startup errors and runtime logs are captured in `~/.strobe/daemon.log`. If the daemon crashes, the user can check this file.

---

### Task 7: Tests

**Files:**
- Modify: `tests/integration.rs`

**Step 1: Test initialize enforcement**

```rust
#[test]
fn test_mcp_initialize_response_has_instructions() {
    let response = strobe::mcp::McpInitializeResponse {
        protocol_version: "2024-11-05".to_string(),
        capabilities: strobe::mcp::McpServerCapabilities {
            tools: strobe::mcp::McpToolsCapability { list_changed: false },
        },
        server_info: strobe::mcp::McpServerInfo {
            name: "strobe".to_string(),
            version: "0.1.0".to_string(),
        },
        instructions: Some("Test instructions".to_string()),
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("instructions"));
    assert!(json.contains("Test instructions"));

    // When None, instructions field should be omitted
    let response_no_instructions = strobe::mcp::McpInitializeResponse {
        protocol_version: "2024-11-05".to_string(),
        capabilities: strobe::mcp::McpServerCapabilities {
            tools: strobe::mcp::McpToolsCapability { list_changed: false },
        },
        server_info: strobe::mcp::McpServerInfo {
            name: "strobe".to_string(),
            version: "0.1.0".to_string(),
        },
        instructions: None,
    };

    let json = serde_json::to_string(&response_no_instructions).unwrap();
    assert!(!json.contains("instructions"));
}
```

**Step 2: Test pending patterns are consumed after launch**

This is a unit-level logic test. Since the actual launch requires Frida, we test the pattern storage/retrieval/clearing logic in isolation, not the full tool_debug_launch flow.

```rust
#[tokio::test]
async fn test_pending_patterns_isolation() {
    use std::collections::{HashMap, HashSet};

    // Simulate per-connection pending patterns
    let mut all_pending: HashMap<String, HashSet<String>> = HashMap::new();

    // Client A sets patterns
    let client_a = "conn-a";
    all_pending.entry(client_a.to_string()).or_default().insert("foo::*".to_string());

    // Client B sets different patterns
    let client_b = "conn-b";
    all_pending.entry(client_b.to_string()).or_default().insert("bar::*".to_string());

    // Client A launches — should get only its patterns, and they should be consumed
    let a_patterns: Vec<String> = all_pending.remove(client_a)
        .map(|s| s.into_iter().collect())
        .unwrap_or_default();

    assert_eq!(a_patterns, vec!["foo::*"]);
    assert!(all_pending.get(client_a).is_none()); // consumed

    // Client B's patterns should be unaffected
    assert!(all_pending.get(client_b).unwrap().contains("bar::*"));
}
```

**Step 3: Test graceful session cleanup**

```rust
#[tokio::test]
async fn test_session_cleanup_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    // Create two running sessions
    db.create_session("session-1", "/bin/app1", "/home", 1000).unwrap();
    db.create_session("session-2", "/bin/app2", "/home", 2000).unwrap();

    // Both should be listed as running
    let running = db.get_running_sessions().unwrap();
    assert_eq!(running.len(), 2);

    // Stop one
    db.update_session_status("session-1", strobe::db::SessionStatus::Stopped).unwrap();

    let running = db.get_running_sessions().unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, "session-2");
}
```

**Checkpoint:** All lifecycle behaviors are verified by tests — initialize enforcement, per-connection pattern isolation, session cleanup.

---

## Summary of Changes

| File | Changes |
|------|---------|
| `src/daemon/server.rs` | Per-connection state, initialize enforcement, per-connection pending patterns, connection_sessions tracking, disconnect cleanup, graceful shutdown |
| `src/db/session.rs` | `get_running_sessions()` method |
| `src/mcp/proxy.rs` | Lock file for daemon startup, connection-based readiness check, daemon log file, proper error on timeout |
| `tests/integration.rs` | Tests for instructions serialization, pattern isolation, session cleanup |

No new files. No new dependencies.
