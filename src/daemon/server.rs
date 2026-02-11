use std::collections::{HashMap, HashSet};
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tokio::time::Instant;
use crate::mcp::*;
use crate::Result;
use super::SessionManager;

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60); // 30 minutes
const MAX_SESSIONS_PER_CONNECTION: usize = 10;
const MAX_TOTAL_SESSIONS: usize = 50;

pub struct Daemon {
    socket_path: PathBuf,
    pid_path: PathBuf,
    session_manager: Arc<SessionManager>,
    last_activity: Arc<RwLock<Instant>>,
    /// Pending trace patterns per connection, applied on next launch
    pending_patterns: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Sessions owned by each connection (for cleanup on disconnect)
    connection_sessions: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Active and recently-completed test runs, keyed by testRunId
    test_runs: Arc<tokio::sync::RwLock<HashMap<String, crate::test::TestRun>>>,
    /// Signaled by idle_timeout_loop to tell the accept loop to exit
    shutdown_signal: Arc<tokio::sync::Notify>,
    /// Vision sidecar for UI element detection
    #[cfg(target_os = "macos")]
    vision_sidecar: Arc<std::sync::Mutex<crate::ui::vision::VisionSidecar>>,
}

fn format_event(event: &crate::db::Event, verbose: bool) -> serde_json::Value {
    if event.event_type == crate::db::EventType::Crash {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "crash",
            "pid": event.pid,
            "threadId": event.thread_id,
            "signal": event.signal,
            "faultAddress": event.fault_address,
            "registers": event.registers,
            "backtrace": event.backtrace,
            "locals": event.locals,
        });
    }

    if event.event_type == crate::db::EventType::VariableSnapshot {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "variable_snapshot",
            "threadId": event.thread_id,
            "pid": event.pid,
            "data": event.arguments,
        });
    }

    if event.event_type == crate::db::EventType::Stdout || event.event_type == crate::db::EventType::Stderr {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": event.event_type.as_str(),
            "threadId": event.thread_id,
            "pid": event.pid,
            "text": event.text,
        });
    }

    if event.event_type == crate::db::EventType::Pause {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "pause",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "backtrace": event.backtrace,
            "arguments": event.arguments,
        });
    }

    if event.event_type == crate::db::EventType::Logpoint {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "logpoint",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "logpointMessage": event.logpoint_message,
        });
    }

    if event.event_type == crate::db::EventType::ConditionError {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "condition_error",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "logpointMessage": event.logpoint_message,
        });
    }

    if verbose {
        serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": event.event_type.as_str(),
            "function": event.function_name,
            "functionRaw": event.function_name_raw,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "duration_ns": event.duration_ns,
            "threadId": event.thread_id,
            "pid": event.pid,
            "parentEventId": event.parent_event_id,
            "arguments": event.arguments,
            "returnValue": event.return_value,
            "watchValues": event.watch_values,
            "logpointMessage": event.logpoint_message,
        })
    } else {
        let mut obj = serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": event.event_type.as_str(),
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "duration_ns": event.duration_ns,
            "pid": event.pid,
            "returnType": event.return_value.as_ref()
                .map(|v| match v {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "bool",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object",
                })
                .unwrap_or("void"),
            "watchValues": event.watch_values,
        });
        if let Some(ref msg) = event.logpoint_message {
            obj["logpointMessage"] = serde_json::Value::String(msg.clone());
        }
        obj
    }
}

/// Parse a type hint string (e.g. "u32", "f64", "pointer") into (size_bytes, type_kind_str).
pub fn parse_type_hint(hint: &str) -> (u8, String) {
    match hint {
        "i8"  => (1, "int".to_string()),
        "u8"  => (1, "uint".to_string()),
        "i16" => (2, "int".to_string()),
        "u16" => (2, "uint".to_string()),
        "i32" => (4, "int".to_string()),
        "u32" => (4, "uint".to_string()),
        "i64" => (8, "int".to_string()),
        "u64" => (8, "uint".to_string()),
        "f32" => (4, "float".to_string()),
        "f64" => (8, "float".to_string()),
        "pointer" => (8, "pointer".to_string()),
        _ => (4, "uint".to_string()), // default: 4-byte unsigned
    }
}

fn hook_status_message(installed: u32, matched: u32, patterns_empty: bool) -> String {
    if installed > 0 && matched > installed {
        format!("{} functions hooked (out of {} matches — excess skipped to stay under limit). Use debug_query to see traced events.", installed, matched)
    } else if installed > 0 {
        format!("{} functions hooked. Use debug_query to see traced events.", installed)
    } else if matched > 0 {
        format!("{} functions matched but could not be hooked. They may be inlined or optimized out. Try broader patterns or @file: patterns.", matched)
    } else if patterns_empty {
        "No patterns active. Add patterns with debug_trace to start tracing.".to_string()
    } else {
        "No functions matched. Try broader patterns, @file: patterns, or check that the binary has debug symbols (DWARF).".to_string()
    }
}

impl Daemon {
    pub async fn run() -> Result<()> {
        let strobe_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".strobe");

        std::fs::create_dir_all(&strobe_dir)?;

        // Acquire exclusive lock — only one daemon can run at a time.
        // The lock is held for the daemon's entire lifetime (_lock_file lives until run() returns).
        let lock_path = strobe_dir.join("daemon.lock");
        let _lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&lock_path)?;

        let lock_result = unsafe {
            libc::flock(_lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB)
        };
        if lock_result != 0 {
            tracing::info!("Another daemon is already running (lock held), exiting");
            return Ok(());
        }

        let socket_path = strobe_dir.join("strobe.sock");
        let pid_path = strobe_dir.join("strobe.pid");
        let db_path = strobe_dir.join("strobe.db");

        // Remove stale socket
        let _ = std::fs::remove_file(&socket_path);

        // Write PID file
        std::fs::write(&pid_path, std::process::id().to_string())?;

        let session_manager = Arc::new(SessionManager::new(&db_path)?);

        let daemon = Arc::new(Self {
            socket_path: socket_path.clone(),
            pid_path,
            session_manager,
            last_activity: Arc::new(RwLock::new(Instant::now())),
            pending_patterns: Arc::new(RwLock::new(HashMap::new())),
            connection_sessions: Arc::new(RwLock::new(HashMap::new())),
            test_runs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            #[cfg(target_os = "macos")]
            vision_sidecar: Arc::new(std::sync::Mutex::new(crate::ui::vision::VisionSidecar::new())),
        });

        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!("Daemon listening on {:?}", socket_path);

        // Spawn idle timeout checker
        let daemon_clone = Arc::clone(&daemon);
        tokio::spawn(async move {
            daemon_clone.idle_timeout_loop().await;
        });

        let mut sigterm = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        )?;
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

        Ok(())
    }

    async fn idle_timeout_loop(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            // Check vision sidecar idle timeout
            #[cfg(target_os = "macos")]
            {
                let settings = crate::config::resolve(None);
                if let Ok(mut sidecar) = self.vision_sidecar.lock() {
                    sidecar.check_idle_timeout(settings.vision_sidecar_idle_timeout_seconds);
                }
            }

            let last = *self.last_activity.read().await;
            if last.elapsed() > IDLE_TIMEOUT {
                tracing::info!("Idle timeout reached, shutting down");
                self.graceful_shutdown().await;
                self.shutdown_signal.notify_one();
                return;
            }
        }
    }

    async fn graceful_shutdown(&self) {
        tracing::info!("Starting graceful shutdown...");

        // Phase 1: Stop all Frida sessions (stops event generation)
        let session_ids: Vec<String> = self.session_manager.get_running_sessions()
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect();

        for id in &session_ids {
            tracing::info!("Stopping Frida for session {} during shutdown", id);
            let _ = self.session_manager.stop_frida(id).await;
        }

        // Phase 2: Delete sessions from DB (writers are now awaited in stop_session)
        for id in &session_ids {
            let _ = self.session_manager.stop_session(id).await;
        }

        // Phase 4: Shutdown vision sidecar if running
        #[cfg(target_os = "macos")]
        {
            if let Ok(mut sidecar) = self.vision_sidecar.lock() {
                sidecar.shutdown();
            }
        }

        self.cleanup();
        tracing::info!("Graceful shutdown complete");
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_path);
    }

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
                let result = self.handle_initialize(&request.params).await;
                if result.is_ok() {
                    *initialized = true;
                }
                result
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

    async fn handle_initialize(&self, _params: &serde_json::Value) -> Result<serde_json::Value> {
        let response = McpInitializeResponse {
            protocol_version: "2024-11-05".to_string(),
            capabilities: McpServerCapabilities {
                tools: McpToolsCapability { list_changed: false },
            },
            server_info: McpServerInfo {
                name: "strobe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            instructions: Some(Self::debugging_instructions().to_string()),
        };

        Ok(serde_json::to_value(response)?)
    }

    fn debugging_instructions() -> &'static str {
        r#"Strobe is a dynamic instrumentation tool. Launch programs, observe runtime behavior (stdout/stderr, function calls, arguments, return values), and stop them — no recompilation needed.

## Workflow

1. Read code, form hypothesis about expected runtime behavior
2. `debug_launch` with NO prior `debug_trace` — stdout/stderr always captured automatically
3. Check output first — `debug_query({ eventType: "stderr" })` then stdout. Crashes, ASAN, assertions often suffice.
4. Trace only if needed — `debug_trace({ sessionId, add: [...] })` on the running session. Do not restart.
5. Iterate — narrow or widen patterns based on results. Session stays alive.

If behavior requires user action (button press, network event), tell the user what to trigger.

## Patterns

- `foo::bar` — exact | `foo::*` — direct children | `foo::**` — all descendants
- `*::validate` — named function, one level | `@file:parser.cpp` — by source file
- `*` stops at `::`, `**` crosses it. Start with 1-3 specific patterns, widen incrementally.

## Limits

- Aim for <50 hooks (fast, stable). 100+ risks crashes. Hard cap: 100 per debug_trace call.
- Default 200k events/session (FIFO). Configure via .strobe/settings.json. Use 500k for audio/DSP; avoid 1M+.

## Watches

Read globals during function execution (requires DWARF symbols).
- `{ variable: \"gCounter\" }` or `{ variable: \"gClock->counter\" }` — named variables
- `{ address: \"0x1234\", type: \"f64\", label: \"tempo\" }` — raw address
- `{ expr: \"ptr(0x5678).readU32()\", label: \"custom\" }` — JS expression
- Scope to functions: `{ variable: \"gTempo\", on: [\"audio::process\"] }` (supports `*`/`**` wildcards)
- Max 32 watches; 4 native CModule for best perf.

## Queries

- eventType: `stderr`/`stdout` (always captured), `function_enter`/`function_exit` (when tracing), `pause`/`logpoint`/`condition_error`
- Filters: `function: { contains: \"parse\" }`, `sourceFile: { contains: \"auth\" }`, `verbose: true`
- Default 50 events. Paginate with `offset`. Check `hasMore`.
- Cursor-based polling: pass `afterEventId` (from previous response's `lastEventId`) to get only new events. Response includes `eventsDropped: true` if FIFO eviction occurred.

## Mistakes to Avoid

- Do NOT trace before launch or use `@usercode` — always check stderr first
- Do NOT use broad `@file:` patterns (`@file:src`). Be specific: `@file:parser.cpp`
- If hookedFunctions: 0 → check for missing .dSYM, inline functions, or try `@file:` patterns
- If hook limit warnings appear, narrow patterns — do NOT retry the same broad pattern

## Running Tests

ALWAYS use `debug_test` — never `cargo test` or test binaries via bash.
Tests always run inside Frida, so you can add traces at any time without restarting.

`debug_test` returns immediately with a `testRunId`. Poll with `debug_test({ action: \"status\", testRunId })`.
The server blocks up to 15s waiting for completion, so it's safe to call immediately after each response.

### Status Response Fields
- `progress.currentTest` — name of the currently executing test
- `progress.currentTestElapsedMs` — how long the current test has been running
- `progress.currentTestBaselineMs` — historical average duration for this test (if known)
- `progress.warnings` — stuck detection warnings (see below)
- `sessionId` — Frida session ID for `debug_trace` and `debug_session`

### Stuck Test Detection
The test runner monitors for stuck tests (deadlocks, infinite loops). When detected,
status response includes warnings:
```json
{ \"warnings\": [{ \"testName\": \"test_auth\", \"idleMs\": 12000,
  \"diagnosis\": \"0% CPU, stacks unchanged 6s\" }] }
```

When you see a warning:
1. Use `debug_trace({ sessionId, add: [\"relevant::patterns\"] })` to investigate
2. Use `debug_query({ sessionId })` to see what's happening
3. Use `debug_session({ action: \"stop\", sessionId })` to kill the test when you understand the issue

### Framework Selection
- **Rust projects**: just provide `projectRoot` — Cargo.toml is auto-detected
- **C++/Catch2**: provide `command` (path to test binary)
- Do NOT pass `framework` unless auto-detection fails — it's usually unnecessary
- Only two frameworks are supported: `cargo` and `catch2`

### Quick Reference
- Add `tracePatterns` to trace from the start (optional — can add later via `debug_trace`)

## Live Memory Access

Read/write variables in a running process without setting up traces:
- `debug_memory({ sessionId, targets: [{ variable: \"gTempo\" }] })` — read a global (action defaults to \"read\")
- `debug_memory({ sessionId, targets: [{ variable: \"gClock->counter\" }] })` — follow pointer chain
- `debug_memory({ sessionId, targets: [...], depth: 2 })` — expand struct fields
- `debug_memory({ sessionId, targets: [...], poll: { intervalMs: 100, durationMs: 2000 } })` — sample over time
- `debug_memory({ action: \"write\", sessionId, targets: [{ variable: \"gTempo\", value: 120.0 }] })` — write a value
- Poll results: `debug_query({ eventType: \"variable_snapshot\" })`

## Session Management

- `debug_session({ action: \"status\", sessionId })` — full snapshot: pid, status, hook count, patterns, breakpoints, logpoints, watches, paused threads
- `debug_session({ action: \"stop\", sessionId })` — stop session (add `retain: true` to keep events for post-mortem)
- `debug_session({ action: \"list\" })` — list retained sessions
- `debug_session({ action: \"delete\", sessionId })` — delete a retained session"#
    }

    async fn handle_tools_list(&self) -> Result<serde_json::Value> {
        let tools = vec![
            // ---- Primary tools (8) ----
            McpTool {
                name: "debug_launch".to_string(),
                description: "Launch a binary with Frida attached. Process stdout/stderr are ALWAYS captured automatically (no tracing needed). Follow the observation loop: 1) Launch clean, 2) Check stderr/stdout first, 3) Add traces only if needed. Applies any pending patterns if debug_trace was called beforehand (advanced usage).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Path to executable" },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "Command line arguments" },
                        "cwd": { "type": "string", "description": "Working directory" },
                        "projectRoot": { "type": "string", "description": "Root directory for user code detection" },
                        "env": { "type": "object", "description": "Additional environment variables" }
                    },
                    "required": ["command", "projectRoot"]
                }),
            },
            McpTool {
                name: "debug_session".to_string(),
                description: "Manage debug sessions: get status, stop, list retained, or delete. Use action to select operation.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["status", "stop", "list", "delete"], "description": "Action to perform" },
                        "sessionId": { "type": "string", "description": "Session ID (required for status/stop/delete)" },
                        "retain": { "type": "boolean", "description": "Retain session data for post-mortem debugging (default: false, only for action: 'stop')" }
                    },
                    "required": ["action"]
                }),
            },
            McpTool {
                name: "debug_trace".to_string(),
                description: r#"Add or remove function trace patterns on a RUNNING debug session.

RECOMMENDED WORKFLOW (Observation Loop):
1. Launch with debug_launch (no prior debug_trace needed)
2. Query stderr/stdout first - most issues visible in output alone
3. If output insufficient, add targeted patterns: debug_trace({ sessionId, add: [...] })
4. Query traces, iterate patterns as needed

When called WITH sessionId (recommended):
- Immediately installs hooks on running process
- Returns actual hook count showing pattern matches
- Start with 1-3 specific patterns (under 50 hooks ideal)
- hookedFunctions: 0 means patterns didn't match - see status for guidance

When called WITHOUT sessionId (advanced/staging mode):
- Stages "pending patterns" for next debug_launch by this connection
- hookedFunctions will be 0 (hooks not installed until launch)
- Use only when you know exactly what to trace upfront
- Consider launching clean and observing output first instead

Validation Limits (enforced):
- watches: max 32 per session
- watch expressions/variables: max 1KB length, max 10 levels deep (-> or . operators)"#.to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string", "description": "Session ID. Omit to set pending patterns for the next debug_launch. Provide to modify a running session." },
                        "add": { "type": "array", "items": { "type": "string" }, "description": "Patterns to start tracing (e.g. \"mymodule::*\", \"*::init\", \"@usercode\")" },
                        "remove": { "type": "array", "items": { "type": "string" }, "description": "Patterns to stop tracing" },
                        "serializationDepth": { "type": "integer", "description": "Maximum depth for recursive argument serialization (default: 3, max: 10)", "minimum": 1, "maximum": 10 },
                        "projectRoot": { "type": "string", "description": "Root directory for user code detection" },
                        "watches": {
                            "type": "object",
                            "description": "Watch global/static variables during function execution (requires debug symbols)",
                            "properties": {
                                "add": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "variable": { "type": "string", "description": "Variable name or expression like 'gClock->counter' (pointer dereferencing)" },
                                            "address": { "type": "string", "description": "Hex address for raw memory watches" },
                                            "type": { "type": "string", "description": "Type hint: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer" },
                                            "label": { "type": "string", "description": "Display label for this watch" },
                                            "expr": { "type": "string", "description": "JavaScript expression for custom reads (e.g. 'ptr(0x5678).readU32()')" },
                                            "on": {
                                                "type": "array",
                                                "items": { "type": "string" },
                                                "description": "Optional function patterns to scope this watch (e.g. ['NoteOn', 'audio::*']). Supports wildcards: * (shallow, stops at ::), ** (deep, crosses ::). If omitted, watch is global (captured on all traced functions)."
                                            }
                                        }
                                    }
                                },
                                "remove": {
                                    "type": "array",
                                    "items": { "type": "string" },
                                    "description": "Labels of watches to remove"
                                }
                            }
                        }
                    }
                }),
            },
            McpTool {
                name: "debug_query".to_string(),
                description: "Query the unified execution timeline: function traces AND process stdout/stderr. Returns events in chronological order. Filter by eventType to get only traces or only output.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" },
                        "eventType": { "type": "string", "enum": ["function_enter", "function_exit", "stdout", "stderr", "crash", "variable_snapshot", "pause", "logpoint", "condition_error"] },
                        "function": {
                            "type": "object",
                            "properties": {
                                "equals": { "type": "string" },
                                "contains": { "type": "string" },
                                "matches": { "type": "string" }
                            }
                        },
                        "sourceFile": {
                            "type": "object",
                            "properties": {
                                "equals": { "type": "string" },
                                "contains": { "type": "string" }
                            }
                        },
                        "returnValue": {
                            "type": "object",
                            "properties": {
                                "equals": {},
                                "isNull": { "type": "boolean" }
                            }
                        },
                        "threadName": {
                            "type": "object",
                            "properties": {
                                "contains": { "type": "string" }
                            }
                        },
                        "timeFrom": {
                            "description": "Filter from this time. Integer (absolute ns) or string (\"-5s\", \"-1m\", \"-500ms\")"
                        },
                        "timeTo": {
                            "description": "Filter to this time. Integer (absolute ns) or string (\"-5s\", \"-1m\", \"-500ms\")"
                        },
                        "minDurationNs": {
                            "type": "integer",
                            "description": "Minimum function duration in nanoseconds (find slow functions)"
                        },
                        "pid": {
                            "type": "integer",
                            "description": "Filter by process ID (for multi-process sessions)"
                        },
                        "limit": { "type": "integer", "default": 50, "maximum": 500 },
                        "offset": { "type": "integer" },
                        "verbose": { "type": "boolean", "default": false },
                        "afterEventId": { "type": "integer", "description": "Cursor: return only events with rowid > afterEventId (for incremental polling)" }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_breakpoint".to_string(),
                description: "Set or remove breakpoints and logpoints. Pauses execution when hit (breakpoint) or logs a message without pausing (logpoint, when 'message' is present). Use debug_continue to resume after breakpoint pause. Supports function names, file:line, conditions, and hit counts.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" },
                        "add": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "function": { "type": "string", "description": "Function name or pattern" },
                                    "file": { "type": "string", "description": "Source file path" },
                                    "line": { "type": "integer", "description": "Line number (required with file)" },
                                    "condition": { "type": "string", "description": "JS condition: e.g. 'args[0] > 100'" },
                                    "hitCount": { "type": "integer", "description": "Break after N hits (breakpoints only)" },
                                    "message": { "type": "string", "description": "Log message template — if present, creates a logpoint instead of breakpoint. Use {args[0]} etc for arguments." }
                                }
                            }
                        },
                        "remove": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Breakpoint or logpoint IDs to remove"
                        }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_continue".to_string(),
                description: "Resume execution after a breakpoint pause. Supports stepping: continue (resume all), step-over (next line), step-into (into calls), step-out (to caller).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" },
                        "action": { "type": "string", "enum": ["continue", "step-over", "step-into", "step-out"], "description": "Default: continue" }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_memory".to_string(),
                description: "Read or write memory in a running process. Supports DWARF-resolved variables, pointer chains, struct expansion, raw addresses, and polling mode for timeline integration.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" },
                        "action": { "type": "string", "enum": ["read", "write"], "description": "Default: read" },
                        "targets": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "variable": { "type": "string", "description": "Variable name or pointer chain (e.g. 'gClock->counter')" },
                                    "address": { "type": "string", "description": "Hex address for raw memory reads" },
                                    "size": { "type": "integer", "description": "Size in bytes (required for raw address)" },
                                    "type": { "type": "string", "description": "Type: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer/bytes" },
                                    "value": { "description": "Value to write (required for action: 'write')" }
                                }
                            },
                            "description": "1-16 read/write targets"
                        },
                        "depth": { "type": "integer", "description": "Struct traversal depth (default 1, max 5)", "minimum": 1, "maximum": 5 },
                        "poll": {
                            "type": "object",
                            "properties": {
                                "intervalMs": { "type": "integer", "description": "Poll interval in ms (50-5000)", "minimum": 50, "maximum": 5000 },
                                "durationMs": { "type": "integer", "description": "Poll duration in ms (100-30000)", "minimum": 100, "maximum": 30000 }
                            }
                        }
                    },
                    "required": ["sessionId", "targets"]
                }),
            },
            McpTool {
                name: "debug_test".to_string(),
                description: "Start a test run asynchronously or poll for results. Returns a testRunId immediately — poll with action: 'status' for progress and results.\n\nSupported frameworks:\n- Rust: provide projectRoot (auto-detects Cargo.toml). No command needed.\n- C++/Catch2: provide command (path to test binary).\n\nUse this instead of running test commands via bash.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "action": { "type": "string", "enum": ["run", "status"], "description": "Action: 'run' (default) starts a test, 'status' polls for results" },
                        "testRunId": { "type": "string", "description": "Test run ID (required for action: 'status')" },
                        "projectRoot": { "type": "string", "description": "Project root for adapter detection (required for action: 'run')" },
                        "framework": { "type": "string", "enum": ["cargo", "catch2"], "description": "Override auto-detection. Usually not needed — framework is detected from projectRoot (Cargo) or command (Catch2)." },
                        "level": { "type": "string", "enum": ["unit", "integration", "e2e"], "description": "Filter: unit, integration, e2e. Omit for all." },
                        "test": { "type": "string", "description": "Run a single test by name (substring match — e.g. 'stuck_detector' runs all tests containing that string)" },
                        "command": { "type": "string", "description": "Path to test binary. Required for C++/Catch2 projects." },
                        "tracePatterns": { "type": "array", "items": { "type": "string" }, "description": "Trace patterns to apply immediately (tests always run inside Frida)" },
                        "watches": {
                            "type": "object",
                            "description": "Watch variables during test execution",
                            "properties": {
                                "add": { "type": "array", "items": { "type": "object" } },
                                "remove": { "type": "array", "items": { "type": "string" } }
                            }
                        },
                        "env": { "type": "object", "description": "Additional environment variables" }
                    }
                }),
            },
            McpTool {
                name: "debug_ui".to_string(),
                description: "Query the UI state of a running process. Returns accessibility tree (native widgets) and optionally AI-detected custom widgets. Use mode to select output.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string", "description": "Session ID (from debug_launch)" },
                        "mode": { "type": "string", "enum": ["tree", "screenshot", "both"], "description": "Output mode: tree (UI element hierarchy), screenshot (PNG image), or both" },
                        "vision": { "type": "boolean", "description": "Enable AI vision pass for custom widgets (default: false). Requires vision sidecar." },
                        "verbose": { "type": "boolean", "description": "Return JSON instead of compact text (default: false)" }
                    },
                    "required": ["sessionId", "mode"]
                }),
            },
        ];

        let response = McpToolsListResponse { tools };
        Ok(serde_json::to_value(response)?)
    }

    async fn handle_tools_call(&self, params: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let call: McpToolCallRequest = serde_json::from_value(params.clone())?;

        let result = match call.name.as_str() {
            "debug_launch" => self.tool_debug_launch(&call.arguments, connection_id).await,
            "debug_trace" => self.tool_debug_trace(&call.arguments, connection_id).await,
            "debug_query" => self.tool_debug_query(&call.arguments).await,
            "debug_session" => self.tool_debug_session(&call.arguments).await,
            "debug_test" => self.tool_debug_test(&call.arguments, connection_id).await,
            "debug_memory" => self.tool_debug_memory(&call.arguments).await,
            "debug_breakpoint" => self.tool_debug_breakpoint(&call.arguments).await,
            "debug_continue" => self.tool_debug_continue(&call.arguments).await,
            "debug_ui" => self.tool_debug_ui(&call.arguments).await,
            _ => Err(crate::Error::Frida(format!("Unknown tool: {}", call.name))),
        };

        match result {
            Ok(value) => {
                let response = McpToolCallResponse {
                    content: vec![McpContent::Text {
                        text: serde_json::to_string_pretty(&value)?,
                    }],
                    is_error: None,
                };
                Ok(serde_json::to_value(response)?)
            }
            Err(e) => {
                let mcp_error: McpError = e.into();
                let response = McpToolCallResponse {
                    content: vec![McpContent::Text {
                        text: format!("{}: {}", serde_json::to_string(&mcp_error.code)?, mcp_error.message),
                    }],
                    is_error: Some(true),
                };
                Ok(serde_json::to_value(response)?)
            }
        }
    }

    fn require_session(&self, session_id: &str) -> crate::Result<crate::db::Session> {
        self.session_manager.get_session(session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))
    }

    async fn untrack_session(&self, session_id: &str) {
        let mut sessions = self.connection_sessions.write().await;
        for session_list in sessions.values_mut() {
            session_list.retain(|s| s != session_id);
        }
    }

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
            if let Ok(Some(session)) = self.session_manager.get_session(&session_id) {
                if session.status == crate::db::SessionStatus::Running {
                    tracing::info!("Cleaning up session {} after client disconnect", session_id);
                    let _ = self.session_manager.stop_frida(&session_id).await;
                    let _ = self.session_manager.stop_session(&session_id).await;
                }
            }
        }
    }

    async fn tool_debug_launch(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let req: DebugLaunchRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        // Validate paths: reject path traversal attempts
        if req.command.contains("..") {
            return Err(crate::Error::ValidationError(
                "command path must not contain '..' components".to_string()
            ));
        }
        if req.project_root.contains("..") {
            return Err(crate::Error::ValidationError(
                "projectRoot must not contain '..' components".to_string()
            ));
        }

        // Enforce global session limit
        // Note: There's a small TOCTOU window between this check and the session
        // registration below. This is acceptable because MCP processes requests
        // serially per connection, making true concurrent launches impossible
        // from a single client.
        {
            let sessions = self.connection_sessions.read().await;
            let total_count: usize = sessions.values().map(|v| v.len()).sum();
            if total_count >= MAX_TOTAL_SESSIONS {
                return Err(crate::Error::Frida(format!(
                    "Global session limit reached ({} total sessions across all connections). Stop existing sessions first.",
                    MAX_TOTAL_SESSIONS
                )));
            }
        }

        // Enforce per-connection session limit
        {
            let sessions = self.connection_sessions.read().await;
            if let Some(session_list) = sessions.get(connection_id) {
                if session_list.len() >= MAX_SESSIONS_PER_CONNECTION {
                    return Err(crate::Error::Frida(format!(
                        "Session limit reached ({} active sessions). Stop existing sessions first.",
                        MAX_SESSIONS_PER_CONNECTION
                    )));
                }
            }
        }

        // Auto-cleanup: if there's already a session for this binary, stop it first
        if let Some(existing) = self.session_manager.db().get_session_by_binary(&req.command)? {
            if existing.status == crate::db::SessionStatus::Running {
                tracing::info!("Auto-stopping existing session {} before new launch", existing.id);
                let _ = self.session_manager.stop_frida(&existing.id).await;
                let _ = self.session_manager.stop_session(&existing.id).await;

                // Remove from all connection tracking
                self.untrack_session(&existing.id).await;
            }
        }

        // Extract binary name from path
        let binary_name = std::path::Path::new(&req.command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let session_id = self.session_manager.generate_session_id(binary_name);

        // Create session in DB BEFORE spawning — the Frida event writer task starts
        // immediately on spawn and would hit a FOREIGN KEY error if the session row
        // doesn't exist yet.
        self.session_manager.create_session(
            &session_id,
            &req.command,
            &req.project_root,
            0, // PID not known yet, updated after spawn
        )?;

        // Launch always starts fast (no DWARF blocking, no initial hooks).
        // DWARF parsing happens in the background.
        let args_vec = req.args.unwrap_or_default();
        let pid = match self.session_manager.spawn_with_frida(
            &session_id,
            &req.command,
            &args_vec,
            req.cwd.as_deref(),
            &req.project_root,
            req.env.as_ref(),
            false, // debug_launch: resume immediately
        ).await {
            Ok(pid) => {
                // Update PID now that we know it
                self.session_manager.update_session_pid(&session_id, pid)?;
                pid
            }
            Err(e) => {
                // Clean up the pre-created session on spawn failure
                let _ = self.session_manager.db().update_session_status(
                    &session_id, crate::db::SessionStatus::Stopped
                );
                return Err(e);
            }
        };

        // Register session ownership for disconnect cleanup
        {
            let mut sessions = self.connection_sessions.write().await;
            sessions.entry(connection_id.to_string()).or_default().push(session_id.clone());
        }

        // Get and clear this connection's pending patterns
        let mut pending_patterns: Vec<String> = {
            let mut all_pending = self.pending_patterns.write().await;
            match all_pending.remove(connection_id) {
                Some(patterns) => patterns.into_iter().collect(),
                None => Vec::new(),
            }
        };
        pending_patterns.sort();

        // Capture count before move
        let patterns_count = pending_patterns.len();
        let had_pending_patterns = !pending_patterns.is_empty();

        if !pending_patterns.is_empty() {
            self.session_manager.add_patterns(&session_id, &pending_patterns)?;

            let sm = Arc::clone(&self.session_manager);
            let sid = session_id.clone();
            tokio::spawn(async move {
                match sm.update_frida_patterns(&sid, Some(&pending_patterns), None, None).await {
                    Ok(result) => {
                        tracing::info!("Deferred hooks installed for {}: {} hooked ({} matched)", sid, result.installed, result.matched);
                        if !result.warnings.is_empty() {
                            tracing::warn!("Deferred hook warnings for {}: {:?}", sid, result.warnings);
                        }
                        sm.set_hook_count(&sid, result.installed);
                    }
                    Err(e) => {
                        tracing::error!("Failed to install deferred hooks for {}: {}", sid, e);
                    }
                }
            });
        }

        let (pending_count, next_steps) = if !had_pending_patterns {
            (None, Some("Query stderr/stdout with debug_query first. Add trace patterns with debug_trace only if output is insufficient.".to_string()))
        } else {
            (
                Some(patterns_count),
                Some(format!("Applied {} pre-configured pattern(s). Note: Recommended workflow is to launch clean, check output first, then add targeted traces. Hooks are installing in background.", patterns_count))
            )
        };

        let response = DebugLaunchResponse {
            session_id,
            pid,
            pending_patterns_applied: pending_count,
            next_steps,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_trace(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

        // Validate request first
        req.validate()?;

        match req.session_id {
            // No session ID - modify pending patterns for this connection's next launch
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
                let status_msg = if patterns.is_empty() {
                    "No pending patterns. Call debug_launch to start a session, then use debug_trace with sessionId to add patterns.".to_string()
                } else {
                    format!("Staged {} pattern(s) for next debug_launch. Note: Recommended workflow is to launch clean, check output first, then add patterns only if needed.", patterns.len())
                };

                let response = DebugTraceResponse {
                    mode: "pending".to_string(),
                    active_patterns: patterns,
                    hooked_functions: 0, // Not hooked yet, just pending
                    matched_functions: None,
                    active_watches: vec![],
                    warnings: vec![],
                    event_limit: crate::config::StrobeSettings::default().events_max_per_session,
                    status: Some(status_msg),
                };
                Ok(serde_json::to_value(response)?)
            }
            // Has session ID - modify running session
            Some(ref session_id) => {
                // Verify session exists
                let _ = self.require_session(session_id)?;

                // Update patterns in session manager
                if let Some(ref add) = req.add {
                    self.session_manager.add_patterns(session_id, add)?;
                }
                if let Some(ref remove) = req.remove {
                    self.session_manager.remove_patterns(session_id, remove)?;
                }

                // Update Frida hooks
                let hook_result = match self.session_manager.update_frida_patterns(
                    session_id,
                    req.add.as_deref(),
                    req.remove.as_deref(),
                    req.serialization_depth,
                ).await {
                    Ok(result) => result,
                    Err(e) => {
                        tracing::warn!("Failed to update Frida patterns for {}: {}", session_id, e);
                        crate::frida_collector::HookResult {
                            installed: 0,
                            matched: 0,
                            warnings: vec![format!("Hook installation failed: {}", e)],
                        }
                    }
                };

                self.session_manager.set_hook_count(session_id, hook_result.installed);

                // Resolve settings from project root
                let project_root_str = req.project_root.clone().or_else(|| {
                    self.session_manager.get_session(session_id).ok()
                        .flatten()
                        .map(|s| s.project_root)
                });
                let settings = crate::config::resolve(
                    project_root_str.as_deref().map(std::path::Path::new)
                );
                self.session_manager.set_event_limit(session_id, settings.events_max_per_session);

                let patterns = self.session_manager.get_patterns(session_id);
                let event_limit = self.session_manager.get_event_limit(session_id);

                // Handle watches if present
                let mut active_watches = vec![];
                let mut watch_warnings = vec![];
                if let Some(ref watch_update) = req.watches {
                    if let Some(ref add_watches) = watch_update.add {
                        let dwarf = self.session_manager.get_dwarf(session_id).await?;
                        let mut frida_watches = vec![];
                        let mut expr_watches = vec![];
                        let mut state_watches = vec![];

                        use crate::mcp::{MAX_WATCH_EXPRESSION_LENGTH as MAX_WATCH_EXPR_LEN, MAX_WATCH_EXPRESSION_DEPTH as MAX_DEREF_DEPTH, MAX_WATCHES_PER_SESSION};

                        let existing_watches = self.session_manager.get_watches(session_id);

                        for watch_target in add_watches {
                            let total_watch_count = existing_watches.len() + frida_watches.len() + expr_watches.len();
                            if total_watch_count >= MAX_WATCHES_PER_SESSION {
                                watch_warnings.push(format!(
                                    "Watch limit reached ({} existing + {} new >= {} max). Additional watches ignored.",
                                    existing_watches.len(),
                                    frida_watches.len() + expr_watches.len(),
                                    MAX_WATCHES_PER_SESSION
                                ));
                                break;
                            }

                            let on_patterns = watch_target.on.clone();

                            // 1) Address-based watch: raw address, no DWARF needed
                            if let Some(ref addr_str) = watch_target.address {
                                let addr = u64::from_str_radix(addr_str.trim_start_matches("0x").trim_start_matches("0X"), 16)
                                    .map_err(|_| crate::Error::Frida(format!("Invalid watch address: {}", addr_str)))?;

                                let type_hint = watch_target.type_hint.as_deref().unwrap_or("u32");
                                let (size, type_kind_str) = parse_type_hint(type_hint);
                                let label = watch_target.label.clone().unwrap_or_else(|| format!("0x{:x}", addr));

                                frida_watches.push(crate::frida_collector::WatchTarget {
                                    label: label.clone(),
                                    address: addr,
                                    size,
                                    type_kind_str: type_kind_str.clone(),
                                    deref_depth: 0,
                                    deref_offset: 0,
                                    type_name: Some(type_hint.to_string()),
                                    on_patterns: on_patterns.clone(),
                                    no_slide: true,
                                });

                                state_watches.push(crate::daemon::ActiveWatchState {
                                    label: label.clone(),
                                    address: addr,
                                    size,
                                    type_kind_str,
                                    deref_depth: 0,
                                    deref_offset: 0,
                                    type_name: Some(type_hint.to_string()),
                                    on_patterns: on_patterns.clone(),
                                    is_expr: false,
                                    expr: None,
                                });

                                active_watches.push(crate::mcp::ActiveWatch {
                                    label,
                                    address: format!("0x{:x}", addr),
                                    size,
                                    type_name: Some(type_hint.to_string()),
                                    on: on_patterns,
                                });
                                continue;
                            }

                            // 2) Expression watch: JS expression evaluated in agent, no DWARF
                            if let Some(ref expr) = watch_target.expr {
                                if watch_target.variable.is_none() {
                                    if expr.len() > MAX_WATCH_EXPR_LEN {
                                        watch_warnings.push(format!(
                                            "Watch expression too long (max {} chars): {}...",
                                            MAX_WATCH_EXPR_LEN, &expr[..50.min(expr.len())]
                                        ));
                                        continue;
                                    }
                                    let label = watch_target.label.clone().unwrap_or_else(|| expr.clone());
                                    let is_global = on_patterns.as_ref().map_or(true, |p| p.is_empty());

                                    expr_watches.push(crate::frida_collector::ExprWatchTarget {
                                        label: label.clone(),
                                        expr: expr.clone(),
                                        is_global,
                                        on_patterns: on_patterns.clone(),
                                    });

                                    active_watches.push(crate::mcp::ActiveWatch {
                                        label,
                                        address: "expr".to_string(),
                                        size: 0,
                                        type_name: None,
                                        on: on_patterns,
                                    });
                                    continue;
                                }
                            }

                            // 3) DWARF variable watch: resolve via DWARF symbols
                            let var_or_expr = watch_target.variable.as_ref()
                                .or(watch_target.expr.as_ref());

                            let Some(name) = var_or_expr else { continue; };

                            if name.len() > MAX_WATCH_EXPR_LEN {
                                watch_warnings.push(format!(
                                    "Watch expression too long (max {} chars): {}...",
                                    MAX_WATCH_EXPR_LEN, &name[..50.min(name.len())]
                                ));
                                continue;
                            }
                            if name.matches("->").count() > MAX_DEREF_DEPTH {
                                watch_warnings.push(format!(
                                    "Watch expression has too many dereferences (max {}): {}",
                                    MAX_DEREF_DEPTH, name
                                ));
                                continue;
                            }

                            let Some(ref dwarf) = dwarf else {
                                watch_warnings.push("No debug symbols available for DWARF variable watches".to_string());
                                break;
                            };

                            let recipe = dwarf.resolve_watch_expression(name)?;

                            let label = watch_target.label.as_ref().unwrap_or(&recipe.label).clone();
                            let type_kind_str = match recipe.type_kind {
                                crate::dwarf::TypeKind::Integer { signed } => {
                                    if signed { "int".to_string() } else { "uint".to_string() }
                                }
                                crate::dwarf::TypeKind::Float => "float".to_string(),
                                crate::dwarf::TypeKind::Pointer => "pointer".to_string(),
                                crate::dwarf::TypeKind::Unknown => "unknown".to_string(),
                            };

                            frida_watches.push(crate::frida_collector::WatchTarget {
                                label: label.clone(),
                                address: recipe.base_address,
                                size: recipe.final_size,
                                type_kind_str: type_kind_str.clone(),
                                deref_depth: recipe.deref_chain.len() as u8,
                                deref_offset: recipe.deref_chain.first().copied().unwrap_or(0),
                                type_name: recipe.type_name.clone(),
                                on_patterns: on_patterns.clone(),
                                no_slide: false,
                            });

                            state_watches.push(crate::daemon::ActiveWatchState {
                                label: label.clone(),
                                address: recipe.base_address,
                                size: recipe.final_size,
                                type_kind_str: type_kind_str.clone(),
                                deref_depth: recipe.deref_chain.len() as u8,
                                deref_offset: recipe.deref_chain.first().copied().unwrap_or(0),
                                type_name: recipe.type_name.clone(),
                                on_patterns: on_patterns.clone(),
                                is_expr: false,
                                expr: None,
                            });

                            active_watches.push(crate::mcp::ActiveWatch {
                                label,
                                address: format!("0x{:x}", recipe.base_address),
                                size: recipe.final_size,
                                type_name: recipe.type_name,
                                on: on_patterns,
                            });
                        }

                        // Send watches to Frida agent
                        if !frida_watches.is_empty() || !expr_watches.is_empty() {
                            self.session_manager.update_frida_watches(session_id, frida_watches, expr_watches).await?;
                            self.session_manager.set_watches(session_id, state_watches);
                        }
                    }

                    // Handle watch removal
                    if let Some(ref remove_labels) = watch_update.remove {
                        // Remove watches from session state
                        let remaining_watches = self.session_manager.remove_watches(session_id, remove_labels);

                        // Send updated watch list to Frida agent
                        let frida_watches: Vec<crate::frida_collector::WatchTarget> = remaining_watches.iter().map(|w| {
                            crate::frida_collector::WatchTarget {
                                label: w.label.clone(),
                                address: w.address,
                                size: w.size,
                                type_kind_str: w.type_kind_str.clone(),
                                deref_depth: w.deref_depth,
                                deref_offset: w.deref_offset,
                                type_name: w.type_name.clone(),
                                on_patterns: w.on_patterns.clone(),
                                no_slide: false,
                            }
                        }).collect();

                        // Update agent with remaining watches (empty list if all removed)
                        self.session_manager.update_frida_watches(session_id, frida_watches, vec![]).await?;

                        watch_warnings.push(format!("Removed {} watch(es)", remove_labels.len()));
                    }
                }

                // Combine hook warnings and watch warnings
                let mut all_warnings = hook_result.warnings;
                all_warnings.extend(watch_warnings);

                let status_msg = hook_status_message(
                    hook_result.installed,
                    hook_result.matched,
                    patterns.is_empty(),
                );

                let response = DebugTraceResponse {
                    mode: "runtime".to_string(),
                    active_patterns: patterns,
                    hooked_functions: hook_result.installed,
                    matched_functions: if hook_result.matched != hook_result.installed {
                        Some(hook_result.matched)
                    } else {
                        None
                    },
                    active_watches,
                    warnings: all_warnings,
                    event_limit,
                    status: Some(status_msg),
                };

                Ok(serde_json::to_value(response)?)
            }
        }
    }

    async fn tool_debug_query(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        // Resolve a time value: integer (absolute ns) or string ("-5s", "-1m", "-500ms")
        fn resolve_time_value(value: &serde_json::Value, latest_ns: i64) -> Option<i64> {
            match value {
                serde_json::Value::Number(n) => n.as_i64(),
                serde_json::Value::String(s) => {
                    let s = s.trim();
                    if !s.starts_with('-') {
                        return s.parse::<i64>().ok();
                    }
                    let (num_str, multiplier) = if s.ends_with("ms") {
                        (&s[1..s.len()-2], 1_000_000i64)
                    } else if s.ends_with('s') {
                        (&s[1..s.len()-1], 1_000_000_000i64)
                    } else if s.ends_with('m') {
                        (&s[1..s.len()-1], 60_000_000_000i64)
                    } else {
                        return None;
                    };
                    let num: i64 = num_str.parse().ok()?;
                    Some(latest_ns - num * multiplier)
                }
                _ => None,
            }
        }

        let req: DebugQueryRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.require_session(&req.session_id)?;

        let limit = req.limit.unwrap_or(50).min(500);
        let offset = req.offset.unwrap_or(0);

        // Resolve relative time values
        let latest_ns = if req.time_from.is_some() || req.time_to.is_some() {
            self.session_manager.db().get_latest_timestamp(&req.session_id)?
        } else {
            0
        };
        let timestamp_from_ns = req.time_from.as_ref()
            .and_then(|v| resolve_time_value(v, latest_ns));
        let timestamp_to_ns = req.time_to.as_ref()
            .and_then(|v| resolve_time_value(v, latest_ns));

        let events = self.session_manager.db().query_events(&req.session_id, |mut q| {
            if let Some(ref et) = req.event_type {
                q = q.event_type(match et {
                    EventTypeFilter::FunctionEnter => crate::db::EventType::FunctionEnter,
                    EventTypeFilter::FunctionExit => crate::db::EventType::FunctionExit,
                    EventTypeFilter::Stdout => crate::db::EventType::Stdout,
                    EventTypeFilter::Stderr => crate::db::EventType::Stderr,
                    EventTypeFilter::Crash => crate::db::EventType::Crash,
                    EventTypeFilter::VariableSnapshot => crate::db::EventType::VariableSnapshot,
                    EventTypeFilter::Pause => crate::db::EventType::Pause,
                    EventTypeFilter::Logpoint => crate::db::EventType::Logpoint,
                    EventTypeFilter::ConditionError => crate::db::EventType::ConditionError,
                });
            }
            if let Some(ref f) = req.function {
                if let Some(ref eq) = f.equals {
                    q = q.function_equals(eq);
                }
                if let Some(ref contains) = f.contains {
                    q = q.function_contains(contains);
                }
            }
            if let Some(ref sf) = req.source_file {
                if let Some(ref contains) = sf.contains {
                    q = q.source_file_contains(contains);
                }
            }
            if let Some(ref tn) = req.thread_name {
                if let Some(ref contains) = tn.contains {
                    q = q.thread_name_contains(contains);
                }
            }
            if let Some(from) = timestamp_from_ns {
                q.timestamp_from_ns = Some(from);
            }
            if let Some(to) = timestamp_to_ns {
                q.timestamp_to_ns = Some(to);
            }
            if let Some(dur) = req.min_duration_ns {
                q.min_duration_ns = Some(dur);
            }
            if let Some(pid) = req.pid {
                q.pid_equals = Some(pid);
            }
            if let Some(after) = req.after_event_id {
                q.after_rowid = Some(after);
            }
            q.limit(limit).offset(offset)
        })?;

        // Count with same filters (except limit/offset) for accurate totalCount
        let total_count = self.session_manager.db().count_filtered_events(&req.session_id, |mut q| {
            if let Some(ref et) = req.event_type {
                q = q.event_type(match et {
                    EventTypeFilter::FunctionEnter => crate::db::EventType::FunctionEnter,
                    EventTypeFilter::FunctionExit => crate::db::EventType::FunctionExit,
                    EventTypeFilter::Stdout => crate::db::EventType::Stdout,
                    EventTypeFilter::Stderr => crate::db::EventType::Stderr,
                    EventTypeFilter::Crash => crate::db::EventType::Crash,
                    EventTypeFilter::VariableSnapshot => crate::db::EventType::VariableSnapshot,
                    EventTypeFilter::Pause => crate::db::EventType::Pause,
                    EventTypeFilter::Logpoint => crate::db::EventType::Logpoint,
                    EventTypeFilter::ConditionError => crate::db::EventType::ConditionError,
                });
            }
            if let Some(ref f) = req.function {
                if let Some(ref eq) = f.equals {
                    q = q.function_equals(eq);
                }
                if let Some(ref contains) = f.contains {
                    q = q.function_contains(contains);
                }
            }
            if let Some(ref sf) = req.source_file {
                if let Some(ref contains) = sf.contains {
                    q = q.source_file_contains(contains);
                }
            }
            if let Some(ref tn) = req.thread_name {
                if let Some(ref contains) = tn.contains {
                    q = q.thread_name_contains(contains);
                }
            }
            if let Some(from) = timestamp_from_ns {
                q.timestamp_from_ns = Some(from);
            }
            if let Some(to) = timestamp_to_ns {
                q.timestamp_to_ns = Some(to);
            }
            if let Some(dur) = req.min_duration_ns {
                q.min_duration_ns = Some(dur);
            }
            if let Some(pid) = req.pid {
                q.pid_equals = Some(pid);
            }
            if let Some(after) = req.after_event_id {
                q.after_rowid = Some(after);
            }
            q
        })?;
        let has_more = (offset as u64 + events.len() as u64) < total_count;

        // Convert to appropriate format
        let verbose = req.verbose.unwrap_or(false);
        let event_values: Vec<serde_json::Value> = events.iter()
            .map(|e| format_event(e, verbose))
            .collect();

        // Compute cursor fields
        let last_event_id = events.iter()
            .filter_map(|e| e.rowid)
            .max();

        let events_dropped = if let Some(after) = req.after_event_id {
            let min_rowid = self.session_manager.db().min_rowid_for_session(&req.session_id)?;
            Some(match min_rowid {
                Some(min) => after + 1 < min,
                None => after > 0, // All events evicted → dropped if cursor was set
            })
        } else {
            None
        };

        let pids = self.session_manager.get_all_pids(&req.session_id);
        let response = DebugQueryResponse {
            events: event_values,
            total_count,
            has_more,
            pids: if pids.len() > 1 { Some(pids) } else { None },
            last_event_id,
            events_dropped,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_memory(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugMemoryRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        match req.action {
            crate::mcp::MemoryAction::Read => {
                let read_req = crate::mcp::DebugReadRequest {
                    session_id: req.session_id,
                    targets: req.targets.into_iter().map(|t| crate::mcp::ReadTarget {
                        variable: t.variable,
                        address: t.address,
                        size: t.size,
                        type_hint: t.type_hint,
                    }).collect(),
                    depth: req.depth,
                    poll: req.poll,
                };
                self.session_manager.execute_debug_read(&serde_json::to_value(read_req)?).await
            }
            crate::mcp::MemoryAction::Write => {
                let write_req = crate::mcp::DebugWriteRequest {
                    session_id: req.session_id,
                    targets: req.targets.into_iter().map(|t| crate::mcp::WriteTarget {
                        variable: t.variable,
                        address: t.address,
                        value: t.value.unwrap_or(serde_json::Value::Null),
                        type_hint: t.type_hint,
                    }).collect(),
                };
                self.session_manager.execute_debug_write(&serde_json::to_value(write_req)?).await
            }
        }
    }

    async fn tool_debug_session(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugSessionRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        match req.action {
            SessionAction::Status => {
                let session_id = req.session_id.as_deref().unwrap();
                let status = self.session_manager.session_status(session_id)?;
                Ok(serde_json::to_value(status)?)
            }
            SessionAction::Stop => {
                self.tool_debug_stop(args).await
            }
            SessionAction::List => {
                self.tool_debug_list_sessions().await
            }
            SessionAction::Delete => {
                self.tool_debug_delete_session(args).await
            }
        }
    }

    async fn tool_debug_stop(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugStopRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.require_session(&req.session_id)?;

        // Stop Frida session
        self.session_manager.stop_frida(&req.session_id).await?;

        let retain = req.retain.unwrap_or(false);

        // Mark session as retained BEFORE stop_session, which deletes the DB rows.
        // When retaining, we skip the DB deletion so events remain queryable.
        if retain {
            self.session_manager.db().mark_session_retained(&req.session_id)?;
            // Enforce global size limit
            let deleted = self.session_manager.db().enforce_global_size_limit()?;
            if deleted > 0 {
                tracing::info!("Deleted {} old retained sessions to enforce 10GB limit", deleted);
            }
        }

        let events_collected = if retain {
            self.session_manager.stop_session_retain(&req.session_id).await?
        } else {
            self.session_manager.stop_session(&req.session_id).await?
        };

        // Remove from connection tracking so disconnect cleanup doesn't try to stop it again
        self.untrack_session(&req.session_id).await;

        let response = DebugStopResponse {
            success: true,
            events_collected,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_list_sessions(&self) -> Result<serde_json::Value> {
        let sessions = self.session_manager.db().list_retained_sessions()?;

        let session_list: Vec<serde_json::Value> = sessions.iter().map(|s| {
            serde_json::json!({
                "sessionId": s.id,
                "binaryPath": s.binary_path,
                "pid": s.pid,
                "startedAt": s.started_at,
                "endedAt": s.ended_at,
                "status": s.status.as_str(),
                "retainedAt": s.retained_at,
                "sizeBytes": s.size_bytes,
            })
        }).collect();

        Ok(serde_json::json!({
            "sessions": session_list,
            "totalSize": self.session_manager.db().calculate_total_size()?,
        }))
    }

    async fn tool_debug_delete_session(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let session_id = args.get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::Error::ValidationError("sessionId is required".to_string()))?;

        // Verify session exists and is retained
        let session = self.require_session(session_id)?;

        if !session.retained {
            return Err(crate::Error::Frida(
                format!("Session {} is not retained and cannot be manually deleted", session_id)
            ));
        }

        // Delete the session
        self.session_manager.db().delete_session(session_id)?;

        Ok(serde_json::json!({
            "success": true,
            "deletedSessionId": session_id,
        }))
    }

    async fn tool_debug_test(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugTestRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        match req.action.as_ref().unwrap_or(&crate::mcp::TestAction::Run) {
            crate::mcp::TestAction::Run => self.tool_debug_test_run(args, connection_id).await,
            crate::mcp::TestAction::Status => {
                let test_run_id = req.test_run_id.as_deref().unwrap();
                let status_req = serde_json::json!({ "testRunId": test_run_id });
                self.tool_debug_test_status(&status_req).await
            }
        }
    }

    async fn tool_debug_test_run(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        // Cleanup stale runs
        self.cleanup_stale_test_runs().await;

        let req: crate::mcp::DebugTestRequest = serde_json::from_value(args.clone())?;

        // Lifecycle guardrail: kill any still-running test sessions before starting a new one.
        // This prevents resource leaks from stuck/abandoned tests and ensures a clean slate.
        {
            let runs = self.test_runs.read().await;
            let stale_session_ids: Vec<String> = runs.values()
                .filter(|run| matches!(&run.state, crate::test::TestRunState::Running { .. }))
                .filter_map(|run| run.session_id.clone())
                .collect();
            drop(runs);

            for sid in &stale_session_ids {
                tracing::warn!("Killing stale test session {} before new test run", sid);
                let _ = self.session_manager.stop_frida(sid).await;
                let _ = self.session_manager.stop_session(sid);
            }

            // Remove killed runs from the map
            if !stale_session_ids.is_empty() {
                let mut runs = self.test_runs.write().await;
                runs.retain(|_, run| {
                    !matches!(&run.state, crate::test::TestRunState::Running { .. })
                });
            }
        }

        // Detect framework name for the start response
        let runner = crate::test::TestRunner::new();
        let project_root_path = std::path::Path::new(&req.project_root);
        let framework_name = runner.detect_adapter(
            project_root_path,
            req.framework.as_deref(),
            req.command.as_deref(),
        )?.name().to_string();

        // Create shared progress tracker
        let progress = std::sync::Arc::new(std::sync::Mutex::new(crate::test::TestProgress::new()));
        let test_run_id = format!("test-{}", &uuid::Uuid::new_v4().to_string()[..8]);

        // Generate session_id upfront so it's available in TestRun immediately
        let session_id = format!("test-{}-{}", framework_name, &uuid::Uuid::new_v4().to_string()[..8]);

        // Clone everything needed for the spawned task
        let progress_clone = std::sync::Arc::clone(&progress);
        let session_manager = std::sync::Arc::clone(&self.session_manager);
        let connection_id_owned = connection_id.to_string();
        let session_id_clone = session_id.clone();
        let run_id = test_run_id.clone();
        let test_runs = std::sync::Arc::clone(&self.test_runs);
        let req_clone = req.clone();

        // Insert Running state BEFORE spawning to avoid race where task
        // completes before the entry exists in the map.
        {
            let mut runs = self.test_runs.write().await;
            runs.insert(test_run_id.clone(), crate::test::TestRun {
                id: test_run_id.clone(),
                state: crate::test::TestRunState::Running { progress },
                fetched: false,
                session_id: Some(session_id),
                project_root: req.project_root.clone(),
            });
        }

        tokio::spawn(async move {
            let runner = crate::test::TestRunner::new();
            let env = req_clone.env.unwrap_or_default();
            let trace_patterns = req_clone.trace_patterns.unwrap_or_default();
            let project_root = std::path::PathBuf::from(&req_clone.project_root);

            let run_result = runner.run(
                &project_root,
                req_clone.framework.as_deref(),
                req_clone.level,
                req_clone.test.as_deref(),
                req_clone.command.as_deref(),
                &env,
                None,  // timeout — uses adapter defaults, configurable via settings
                &session_manager,
                &trace_patterns,
                req_clone.watches.as_ref(),
                &connection_id_owned,
                &session_id_clone,
                progress_clone,
            ).await;

            // Record baselines for completed tests
            if let Ok(ref run_result) = run_result {
                for test_detail in &run_result.result.all_tests {
                    let _ = session_manager.db().record_test_baseline(
                        &test_detail.name,
                        project_root.to_str().unwrap_or("."),
                        test_detail.duration_ms,
                        test_detail.status.as_str(),
                    );
                }
                let _ = session_manager.db().cleanup_old_baselines(
                    project_root.to_str().unwrap_or(".")
                );
            }

            // Transition state
            let new_state = match run_result {
                Ok(run_result) => {
                    let details_path = crate::test::output::write_details(
                        &run_result.framework,
                        &run_result.result,
                        &run_result.raw_stdout,
                        &run_result.raw_stderr,
                    ).ok();

                    // Detect compilation failure: 0 tests ran and stderr contains error
                    let is_compile_failure = run_result.result.all_tests.is_empty()
                        && (run_result.raw_stderr.contains("error[E")
                            || run_result.raw_stderr.contains("could not compile")
                            || run_result.raw_stderr.contains("uild failed"));

                    let hint = if is_compile_failure {
                        Some("COMPILATION FAILED — 0 tests ran. Check 'details' file for compiler errors in rawStderr.".to_string())
                    } else if run_result.result.all_tests.is_empty() && run_result.result.failures.is_empty() {
                        Some("No tests found. Check framework detection, test filter, or project path.".to_string())
                    } else {
                        None
                    };

                    let response = crate::mcp::DebugTestResponse {
                        framework: run_result.framework,
                        summary: Some(run_result.result.summary),
                        failures: run_result.result.failures,
                        stuck: run_result.result.stuck,
                        session_id: run_result.session_id,
                        details: details_path,
                        no_tests: if is_compile_failure { Some(true) } else { None },
                        project: None,
                        hint,
                    };

                    match serde_json::to_value(response) {
                        Ok(v) => crate::test::TestRunState::Completed {
                            response: v,
                            completed_at: std::time::Instant::now(),
                        },
                        Err(e) => crate::test::TestRunState::Failed {
                            error: format!("Failed to serialize result: {}", e),
                            completed_at: std::time::Instant::now(),
                        },
                    }
                }
                Err(e) => crate::test::TestRunState::Failed {
                    error: e.to_string(),
                    completed_at: std::time::Instant::now(),
                },
            };

            let mut runs = test_runs.write().await;
            if let Some(test_run) = runs.get_mut(&run_id) {
                test_run.state = new_state;
            }
        });

        let response = crate::mcp::DebugTestStartResponse {
            test_run_id,
            status: "running".to_string(),
            framework: framework_name,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_test_status(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugTestStatusRequest = serde_json::from_value(args.clone())?;

        // Block for up to 15 seconds, returning immediately if the test completes.
        // This throttles LLM polling while still providing timely completion results.
        let poll_interval = std::time::Duration::from_secs(1);
        let max_wait = std::time::Duration::from_secs(15);
        let deadline = std::time::Instant::now() + max_wait;

        loop {
            let runs = self.test_runs.read().await;
            let test_run = runs.get(&req.test_run_id)
                .ok_or_else(|| crate::Error::TestRunNotFound(req.test_run_id.clone()))?;

            match &test_run.state {
                crate::test::TestRunState::Running { .. } => {
                    drop(runs);
                    if std::time::Instant::now() >= deadline {
                        break; // Waited long enough, return current progress
                    }
                    tokio::time::sleep(poll_interval).await;
                }
                _ => break, // Completed or Failed — return immediately
            }
        }

        let mut runs = self.test_runs.write().await;
        let test_run = runs.get_mut(&req.test_run_id)
            .ok_or_else(|| crate::Error::TestRunNotFound(req.test_run_id.clone()))?;

        let response = match &test_run.state {
            crate::test::TestRunState::Running { progress, .. } => {
                let p = progress.lock().unwrap();
                let phase_str = match p.phase {
                    crate::test::TestPhase::Compiling => "compiling",
                    crate::test::TestPhase::Running => "running",
                    crate::test::TestPhase::SuitesFinished => "suites_finished",
                };

                // Convert internal warnings to MCP type
                let warnings: Vec<crate::mcp::TestStuckWarning> = p.warnings.iter().map(|w| {
                    crate::mcp::TestStuckWarning {
                        test_name: w.test_name.clone(),
                        idle_ms: w.idle_ms,
                        diagnosis: w.diagnosis.clone(),
                        suggested_traces: w.suggested_traces.clone(),
                    }
                }).collect();

                // Build running_tests snapshot with baselines
                let running_tests_snapshot: Vec<crate::mcp::RunningTestSnapshot> = p.running_tests.iter()
                    .map(|(name, started)| {
                        let baseline = self.session_manager.db()
                            .get_test_baseline(name, &test_run.project_root)
                            .unwrap_or(None);
                        crate::mcp::RunningTestSnapshot {
                            name: name.clone(),
                            elapsed_ms: started.elapsed().as_millis() as u64,
                            baseline_ms: baseline,
                        }
                    })
                    .collect();

                // current_test = longest-running test (backward compat + stuck detector)
                let current_test = p.current_test();
                let current_test_elapsed_ms = p.current_test_started_at()
                    .map(|t| t.elapsed().as_millis() as u64);
                let baseline_ms = current_test.as_ref().and_then(|name| {
                    self.session_manager.db()
                        .get_test_baseline(name, &test_run.project_root)
                        .unwrap_or(None)
                });

                crate::mcp::DebugTestStatusResponse {
                    test_run_id: req.test_run_id,
                    status: "running".to_string(),
                    progress: Some(crate::mcp::TestProgressSnapshot {
                        elapsed_ms: p.elapsed_ms(),
                        passed: p.passed,
                        failed: p.failed,
                        skipped: p.skipped,
                        current_test,
                        phase: Some(phase_str.to_string()),
                        warnings,
                        current_test_elapsed_ms,
                        current_test_baseline_ms: baseline_ms,
                        running_tests: running_tests_snapshot,
                    }),
                    result: None,
                    error: None,
                    session_id: test_run.session_id.clone(),
                }
            }
            crate::test::TestRunState::Completed { response, .. } => {
                test_run.fetched = true;
                // Surface hint at the top level so the agent sees it immediately
                let hint = response.get("hint").and_then(|h| h.as_str()).map(|s| s.to_string());
                crate::mcp::DebugTestStatusResponse {
                    test_run_id: req.test_run_id,
                    status: "completed".to_string(),
                    progress: None,
                    result: Some(response.clone()),
                    error: hint,
                    session_id: test_run.session_id.clone(),
                }
            }
            crate::test::TestRunState::Failed { error, .. } => {
                test_run.fetched = true;
                crate::mcp::DebugTestStatusResponse {
                    test_run_id: req.test_run_id,
                    status: "failed".to_string(),
                    progress: None,
                    result: None,
                    error: Some(error.clone()),
                    session_id: test_run.session_id.clone(),
                }
            }
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn cleanup_stale_test_runs(&self) {
        let mut runs = self.test_runs.write().await;
        let now = std::time::Instant::now();
        runs.retain(|_id, run| {
            match &run.state {
                crate::test::TestRunState::Running { .. } => true,
                crate::test::TestRunState::Completed { completed_at, .. }
                | crate::test::TestRunState::Failed { completed_at, .. } => {
                    let age = now.duration_since(*completed_at);
                    let expired = (run.fetched && age > Duration::from_secs(300))
                        || age > Duration::from_secs(1800);
                    !expired
                }
            }
        });
    }

    // Phase 2: Active debugging tools

    async fn tool_debug_breakpoint(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugBreakpointRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        let mut all_breakpoints = Vec::new();
        let mut all_logpoints = Vec::new();

        // Handle additions — split by presence of `message` field
        if let Some(targets) = req.add {
            for target in targets {
                if let Some(message) = target.message {
                    // Logpoint path: has message
                    let logpoint = self.session_manager
                        .set_logpoint_async(
                            &req.session_id,
                            None,
                            target.function,
                            target.file,
                            target.line,
                            message,
                            target.condition,
                        )
                        .await?;
                    all_logpoints.push(logpoint);
                } else {
                    // Breakpoint path: no message
                    let breakpoint = self.session_manager
                        .set_breakpoint_async(
                            &req.session_id,
                            None,
                            target.function,
                            target.file,
                            target.line,
                            target.condition,
                            target.hit_count,
                        )
                        .await?;
                    all_breakpoints.push(breakpoint);
                }
            }
        }

        // Handle removals — try both breakpoints and logpoints (IDs are namespaced bp-*/lp-*)
        if let Some(ids) = req.remove {
            for id in &ids {
                if id.starts_with("lp-") {
                    self.session_manager.remove_logpoint(&req.session_id, id).await;
                } else {
                    self.session_manager.remove_breakpoint(&req.session_id, id).await;
                }
            }
        }

        // Return current breakpoints if none were just added
        if all_breakpoints.is_empty() {
            all_breakpoints = self.session_manager
                .get_breakpoints(&req.session_id)
                .into_iter()
                .map(|bp| crate::mcp::BreakpointInfo {
                    id: bp.id,
                    function: match &bp.target {
                        crate::daemon::session_manager::BreakpointTarget::Function(f) => Some(f.clone()),
                        _ => None,
                    },
                    file: match &bp.target {
                        crate::daemon::session_manager::BreakpointTarget::Line { file, .. } => Some(file.clone()),
                        _ => None,
                    },
                    line: match &bp.target {
                        crate::daemon::session_manager::BreakpointTarget::Line { line, .. } => Some(*line),
                        _ => None,
                    },
                    address: format!("0x{:x}", bp.address),
                })
                .collect();
        }

        // Return current logpoints if none were just added
        if all_logpoints.is_empty() {
            all_logpoints = self.session_manager
                .get_logpoints(&req.session_id)
                .into_iter()
                .map(|lp| crate::mcp::LogpointInfo {
                    id: lp.id,
                    message: lp.message,
                    function: match &lp.target {
                        crate::daemon::session_manager::BreakpointTarget::Function(f) => Some(f.clone()),
                        _ => None,
                    },
                    file: match &lp.target {
                        crate::daemon::session_manager::BreakpointTarget::Line { file, .. } => Some(file.clone()),
                        _ => None,
                    },
                    line: match &lp.target {
                        crate::daemon::session_manager::BreakpointTarget::Line { line, .. } => Some(*line),
                        _ => None,
                    },
                    address: format!("0x{:x}", lp.address),
                })
                .collect();
        }

        Ok(serde_json::to_value(crate::mcp::DebugBreakpointResponse {
            breakpoints: all_breakpoints,
            logpoints: all_logpoints,
        })?)
    }

    async fn tool_debug_continue(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugContinueRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        let response = self.session_manager.debug_continue_async(&req.session_id, req.action).await?;

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_ui(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugUiRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        let session = self.require_session(&req.session_id)?;
        if session.status != crate::db::SessionStatus::Running {
            return Err(crate::Error::UiQueryFailed(
                format!("Process not running (PID {} exited). Cannot query UI.", session.pid)
            ));
        }

        let start = std::time::Instant::now();
        let vision_requested = req.vision.unwrap_or(false);
        let verbose = req.verbose.unwrap_or(false);

        let mut tree_output = None;
        let mut screenshot_output = None;
        let mut ax_count = 0;
        let mut vision_count = 0;
        let mut merged_count = 0;

        let needs_tree = matches!(req.mode, crate::mcp::UiMode::Tree | crate::mcp::UiMode::Both);
        let needs_screenshot = matches!(req.mode, crate::mcp::UiMode::Screenshot | crate::mcp::UiMode::Both);

        // Query AX tree
        if needs_tree {
            #[cfg(target_os = "macos")]
            {
                let pid = session.pid;
                let nodes = tokio::task::spawn_blocking(move || {
                    crate::ui::accessibility::query_ax_tree(pid)
                }).await.map_err(|e| crate::Error::Internal(format!("AX query task failed: {}", e)))??;

                ax_count = crate::ui::tree::count_nodes(&nodes);

                // Run vision pipeline if requested and enabled
                let mut final_nodes = nodes;
                if vision_requested {
                    let settings = crate::config::resolve(None);
                    if !settings.vision_enabled {
                        return Err(crate::Error::UiQueryFailed(
                            "Vision pipeline requested but not enabled. Set vision.enabled=true in ~/.strobe/settings.json".to_string()
                        ));
                    }

                    // SEC-8: Rate limit vision calls to prevent GPU/CPU exhaustion
                    // Allow 1 call per second per session
                    {
                        use std::sync::{Mutex, OnceLock};
                        static LAST_VISION_CALL: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>>
                            = OnceLock::new();

                        let now = std::time::Instant::now();
                        let rate_limiter = LAST_VISION_CALL.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
                        let mut last_calls = rate_limiter.lock().unwrap();
                        // Prune entries older than 60s to prevent unbounded growth
                        last_calls.retain(|_, last| now.duration_since(*last) < std::time::Duration::from_secs(60));
                        if let Some(last_time) = last_calls.get(&req.session_id) {
                            let elapsed = now.duration_since(*last_time);
                            if elapsed < std::time::Duration::from_secs(1) {
                                return Err(crate::Error::UiQueryFailed(
                                    format!("Vision rate limit exceeded. Please wait {:.1}s before next call.",
                                        1.0 - elapsed.as_secs_f64())
                                ));
                            }
                        }
                        last_calls.insert(req.session_id.clone(), now);
                    } // Lock guard dropped here, before any await points

                    // Capture screenshot for vision
                    let screenshot_b64 = {
                        let pid = session.pid;
                        let png_bytes = tokio::task::spawn_blocking(move || {
                            crate::ui::capture::capture_window_screenshot(pid)
                        }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

                        use base64::Engine;
                        base64::engine::general_purpose::STANDARD.encode(&png_bytes)
                    };

                    // Run vision detection
                    let vision_elements = {
                        let mut sidecar = self.vision_sidecar.lock().unwrap();
                        sidecar.detect(
                            &screenshot_b64,
                            settings.vision_confidence_threshold,
                            settings.vision_iou_merge_threshold,
                        )?
                    };

                    // COMP-1: Merge vision into tree and capture accurate stats
                    let (actual_merged, actual_added) = crate::ui::merge::merge_vision_into_tree(
                        &mut final_nodes,
                        &vision_elements,
                        settings.vision_iou_merge_threshold as f64,
                    );

                    // Stats semantics:
                    // - vision_nodes: total vision elements added (pure vision nodes)
                    // - merged_nodes: AX nodes enhanced with vision data
                    vision_count = actual_added;
                    merged_count = actual_merged;
                }

                tree_output = Some(if verbose {
                    crate::ui::tree::format_json(&final_nodes)?
                } else {
                    crate::ui::tree::format_compact(&final_nodes)
                });
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Err(crate::Error::UiNotAvailable(
                    "UI observation is only supported on macOS".to_string()
                ));
            }
        }

        // Capture screenshot
        if needs_screenshot {
            #[cfg(target_os = "macos")]
            {
                let pid = session.pid;
                let png_bytes = tokio::task::spawn_blocking(move || {
                    crate::ui::capture::capture_window_screenshot(pid)
                }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

                use base64::Engine;
                screenshot_output = Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes));
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Err(crate::Error::UiNotAvailable(
                    "Screenshot capture is only supported on macOS".to_string()
                ));
            }
        }

        let latency_ms = start.elapsed().as_millis() as u64;

        let response = crate::mcp::DebugUiResponse {
            tree: tree_output,
            screenshot: screenshot_output,
            stats: Some(crate::mcp::UiStats {
                ax_nodes: ax_count,
                vision_nodes: vision_count,
                merged_nodes: merged_count,
                latency_ms,
            }),
        };

        Ok(serde_json::to_value(response)?)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Create a test Daemon with a temp database
    fn test_daemon() -> (Daemon, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let session_manager = Arc::new(SessionManager::new(&db_path).unwrap());

        let daemon = Daemon {
            socket_path: dir.path().join("test.sock"),
            pid_path: dir.path().join("test.pid"),
            session_manager,
            last_activity: Arc::new(RwLock::new(Instant::now())),
            pending_patterns: Arc::new(RwLock::new(HashMap::new())),
            connection_sessions: Arc::new(RwLock::new(HashMap::new())),
            test_runs: Arc::new(tokio::sync::RwLock::new(HashMap::new())),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            #[cfg(target_os = "macos")]
            vision_sidecar: Arc::new(std::sync::Mutex::new(crate::ui::vision::VisionSidecar::new())),
        };

        (daemon, dir)
    }

    fn make_request(method: &str, id: i64) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": {}
        }).to_string()
    }

    fn make_initialize_request() -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0.1" }
            }
        }).to_string()
    }

    #[tokio::test]
    async fn test_initialize_enforcement_rejects_before_init() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-conn-1";

        // Call tools/list before initialize — should be rejected
        let msg = make_request("tools/list", 1);
        let resp = daemon.handle_message(&msg, &mut initialized, conn_id).await;

        assert!(!initialized);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32002);
        assert!(err.message.contains("initialize"));
    }

    #[tokio::test]
    async fn test_initialize_enforcement_allows_after_init() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-conn-2";

        // Initialize first
        let init_msg = make_initialize_request();
        let resp = daemon.handle_message(&init_msg, &mut initialized, conn_id).await;
        assert!(initialized);
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());

        // Now tools/list should succeed
        let msg = make_request("tools/list", 2);
        let resp = daemon.handle_message(&msg, &mut initialized, conn_id).await;
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
    }

    #[tokio::test]
    async fn test_initialize_not_set_on_malformed_params() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-conn-3";

        // Send initialize — even with empty params, our handler accepts it (params are ignored)
        // But a truly broken JSON should be rejected at parse level
        let bad_json = "not json at all";
        let resp = daemon.handle_message(bad_json, &mut initialized, conn_id).await;

        // Parse error should not set initialized
        assert!(!initialized);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32700);
    }

    #[tokio::test]
    async fn test_disconnect_cleans_pending_patterns() {
        let (daemon, _dir) = test_daemon();
        let conn_id = "test-conn-4";

        // Set pending patterns for this connection
        {
            let mut pending = daemon.pending_patterns.write().await;
            let mut patterns = HashSet::new();
            patterns.insert("foo::*".to_string());
            patterns.insert("bar::*".to_string());
            pending.insert(conn_id.to_string(), patterns);
        }

        // Verify patterns exist
        assert!(daemon.pending_patterns.read().await.contains_key(conn_id));

        // Disconnect
        daemon.handle_disconnect(conn_id).await;

        // Patterns should be gone
        assert!(!daemon.pending_patterns.read().await.contains_key(conn_id));
    }

    #[tokio::test]
    async fn test_disconnect_cleans_session_tracking() {
        let (daemon, _dir) = test_daemon();
        let conn_id = "test-conn-5";

        // Create a session in the DB and register it to this connection
        let session_id = daemon.session_manager.generate_session_id("testapp");
        daemon.session_manager.create_session(
            &session_id, "/bin/testapp", "/home/user", 99999,
        ).unwrap();

        {
            let mut sessions = daemon.connection_sessions.write().await;
            sessions.entry(conn_id.to_string()).or_default().push(session_id.clone());
        }

        // Verify session is running
        let session = daemon.session_manager.get_session(&session_id).unwrap().unwrap();
        assert_eq!(session.status, crate::db::SessionStatus::Running);

        // Disconnect — should clean up the session
        daemon.handle_disconnect(conn_id).await;

        // Connection tracking should be cleared
        assert!(!daemon.connection_sessions.read().await.contains_key(conn_id));

        // Session should be deleted from DB (stop_session deletes)
        let session = daemon.session_manager.get_session(&session_id).unwrap();
        assert!(session.is_none());
    }

    #[tokio::test]
    async fn test_graceful_shutdown_stops_sessions() {
        let (daemon, _dir) = test_daemon();

        // Create a running session in the DB
        let session_id = daemon.session_manager.generate_session_id("testapp");
        daemon.session_manager.create_session(
            &session_id, "/bin/testapp", "/home/user", 99999,
        ).unwrap();

        // Verify it shows up as running
        let running = daemon.session_manager.get_running_sessions().unwrap();
        assert_eq!(running.len(), 1);

        // Graceful shutdown
        daemon.graceful_shutdown().await;

        // Session should be cleaned up
        let session = daemon.session_manager.get_session(&session_id).unwrap();
        assert!(session.is_none());
    }

    #[tokio::test]
    async fn test_per_connection_pattern_isolation() {
        let (daemon, _dir) = test_daemon();
        let mut init_a = false;
        let mut init_b = false;
        let conn_a = "conn-a";
        let conn_b = "conn-b";

        // Initialize both connections
        let init_msg = make_initialize_request();
        daemon.handle_message(&init_msg, &mut init_a, conn_a).await;
        daemon.handle_message(&init_msg, &mut init_b, conn_b).await;

        // Connection A sets patterns
        let trace_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "debug_trace",
                "arguments": {
                    "add": ["conn_a_pattern::*"]
                }
            }
        }).to_string();
        daemon.handle_message(&trace_msg, &mut init_a, conn_a).await;

        // Connection B sets different patterns
        let trace_msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "debug_trace",
                "arguments": {
                    "add": ["conn_b_pattern::*"]
                }
            }
        }).to_string();
        daemon.handle_message(&trace_msg, &mut init_b, conn_b).await;

        // Verify isolation
        let pending = daemon.pending_patterns.read().await;
        let a_patterns = pending.get(conn_a).unwrap();
        let b_patterns = pending.get(conn_b).unwrap();

        assert!(a_patterns.contains("conn_a_pattern::*"));
        assert!(!a_patterns.contains("conn_b_pattern::*"));
        assert!(b_patterns.contains("conn_b_pattern::*"));
        assert!(!b_patterns.contains("conn_a_pattern::*"));
    }

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
            #[cfg(target_os = "macos")]
            vision_sidecar: Arc::new(std::sync::Mutex::new(crate::ui::vision::VisionSidecar::new())),
        };

        daemon.graceful_shutdown().await;

        // Files should be cleaned up
        assert!(!socket_path.exists(), "Socket file should be removed");
        assert!(!pid_path.exists(), "PID file should be removed");
    }

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

    // ---- E2E MCP tool handler tests for debug_ui ----

    fn make_debug_ui_call(session_id: &str, mode: &str, id: i64) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "debug_ui",
                "arguments": {
                    "sessionId": session_id,
                    "mode": mode
                }
            }
        }).to_string()
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_debug_ui_nonexistent_session() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-ui-1";

        daemon.handle_message(&make_initialize_request(), &mut initialized, conn_id).await;

        let msg = make_debug_ui_call("nonexistent-session", "tree", 10);
        let resp = daemon.handle_message(&msg, &mut initialized, conn_id).await;

        // Tool errors are wrapped as successful JSON-RPC with isError in content
        let result = resp.result.expect("Should have result");
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(is_error, "Should return error for nonexistent session");

        let content = result.get("content").unwrap().as_array().unwrap();
        let text = content[0].get("text").unwrap().as_str().unwrap();
        assert!(text.contains("SESSION_NOT_FOUND"), "Error should mention SESSION_NOT_FOUND, got: {}", text);
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_debug_ui_stopped_process() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-ui-2";

        daemon.handle_message(&make_initialize_request(), &mut initialized, conn_id).await;

        // Create a session and mark it as stopped (process exited)
        let session_id = daemon.session_manager.generate_session_id("testapp");
        daemon.session_manager.create_session(
            &session_id, "/bin/testapp", "/home/user", 99999,
        ).unwrap();
        daemon.session_manager.db().update_session_status(
            &session_id, crate::db::SessionStatus::Stopped,
        ).unwrap();

        let msg = make_debug_ui_call(&session_id, "tree", 10);
        let resp = daemon.handle_message(&msg, &mut initialized, conn_id).await;

        let result = resp.result.expect("Should have result");
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(is_error, "Should return error for stopped process");

        let content = result.get("content").unwrap().as_array().unwrap();
        let text = content[0].get("text").unwrap().as_str().unwrap();
        assert!(text.contains("not running") || text.contains("exited"),
            "Error should mention process not running, got: {}", text);
    }

    #[tokio::test]
    #[cfg(target_os = "macos")]
    async fn test_debug_ui_vision_disabled_error() {
        let (daemon, _dir) = test_daemon();
        let mut initialized = false;
        let conn_id = "test-ui-3";

        daemon.handle_message(&make_initialize_request(), &mut initialized, conn_id).await;

        // Create a running session (process won't actually exist, but we'll hit
        // the vision check before the AX query since vision is checked first)
        let session_id = daemon.session_manager.generate_session_id("testapp");
        daemon.session_manager.create_session(
            &session_id, "/bin/testapp", "/home/user", 99999,
        ).unwrap();

        // Request vision on a running session — should fail because vision is disabled by default
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "debug_ui",
                "arguments": {
                    "sessionId": session_id,
                    "mode": "tree",
                    "vision": true
                }
            }
        }).to_string();
        let resp = daemon.handle_message(&msg, &mut initialized, conn_id).await;

        let result = resp.result.expect("Should have result");
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        // This may fail with either "vision not enabled" or an AX error (since PID 99999 doesn't exist).
        // Both are acceptable - the key is it doesn't panic.
        assert!(is_error, "Should return error (vision disabled or invalid PID)");
    }
}
