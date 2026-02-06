use std::collections::{HashMap, HashSet};
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

impl Daemon {
    pub async fn run() -> Result<()> {
        let strobe_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".strobe");

        std::fs::create_dir_all(&strobe_dir)?;

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
        });

        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!("Daemon listening on {:?}", socket_path);

        // Spawn idle timeout checker
        let daemon_clone = Arc::clone(&daemon);
        tokio::spawn(async move {
            daemon_clone.idle_timeout_loop().await;
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = Arc::clone(&daemon);
                    tokio::spawn(async move {
                        if let Err(e) = daemon.handle_connection(stream).await {
                            tracing::error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

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

        // Phase 2: Let DB writer tasks flush remaining buffered events
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Phase 3: Delete sessions from DB (safe now that writers have flushed)
        for id in &session_ids {
            let _ = self.session_manager.stop_session(id);
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
        r#"Strobe is a dynamic instrumentation tool. You launch programs, observe their runtime behavior (stdout/stderr, function calls, arguments, return values), and stop them — all without modifying source code or recompiling. Observation is non-intrusive and adjustable at runtime.

## Mindset

You have full control over the program lifecycle. You launch it, observe its behavior, and stop it. The behavior you want to observe may happen on startup, or it may require an external trigger — a user action, a network event, a specific input. Your role is to set up observation, let the behavior occur, then analyze what happened. Work backwards from the symptom to the root cause. Read the code, form a hypothesis, then use Strobe to confirm or refute it.

## The Observation Loop

1. Understand the goal — Read the code around the area of interest. Form a hypothesis about what should happen at runtime.
2. Launch with no tracing — Call debug_launch with no prior debug_trace. stdout/stderr are always captured automatically.
3. Let the behavior occur — If the behavior requires a user action or external event, tell the user what to trigger. Be specific.
4. Read output first — debug_query({ eventType: "stderr" }) then stdout. Crash reports, ASAN output, assertion failures, error logs are often the complete answer.
5. Instrument only if needed — If output doesn't explain the issue, add targeted trace patterns on the running session with debug_trace({ sessionId, add: [...] }). Start small and specific. Do not restart.
6. Iterate — Narrow or widen patterns based on what you learn. Remove noisy patterns, add specific ones. The session stays alive.

## Hook Limits

Each Interceptor.attach() rewrites a function's prologue in memory. This is fast for small numbers but destabilizes the process at scale. Practical limits:

- Under 50 hooks: fast install (~5s), rock solid — aim for this
- 50-100 hooks: install ~10s, stable
- 100+ hooks: crash risk increases, especially with hot functions
- Hard cap: 100 hooks per debug_trace call. Patterns exceeding this are truncated with a warning.

ALWAYS start with the narrowest pattern that covers your hypothesis. You can widen later — you cannot un-crash a process.

## Pattern Syntax

- `foo::bar` — exact function match (1 hook)
- `foo::*` — all direct functions in foo, not nested
- `foo::**` — all functions under foo, any depth
- `*::validate` — any function named "validate", one level deep
- `@file:parser.cpp` — all functions in files matching "parser.cpp"

`*` does not cross `::` boundaries. Use `**` for deep matching.

Best strategy: start with 1-3 exact function names or a very specific `@file:` pattern. Add more patterns incrementally as you learn from the first round of events.

## Query Tips

- eventType "stderr" / "stdout" — program output (always captured)
- eventType "function_enter" / "function_exit" — trace events (only when patterns are set)
- function: { contains: "parse" } — search by function name substring
- sourceFile: { contains: "auth" } — search by source file
- verbose: true — includes arguments, return values, raw symbol names
- Default limit is 50 events. Use offset to paginate. Check hasMore.

## Common Mistakes

- Do NOT set trace patterns before launch unless you already know exactly what to trace. Launch clean, read output first.
- Do NOT use @usercode. It hooks all project functions and will overwhelm the target.
- Do NOT use broad `@file:` patterns that match many source files. Be specific: `@file:parser.cpp` not `@file:src`.
- Do NOT restart the session to add traces. Use debug_trace with sessionId on the running session.
- Always check stderr before instrumenting — the answer is often already there.
- If debug_trace returns warnings about hook limits, narrow your patterns. Do NOT retry the same broad pattern."#
    }

    async fn handle_tools_list(&self) -> Result<serde_json::Value> {
        let tools = vec![
            McpTool {
                name: "debug_launch".to_string(),
                description: "Launch a binary with Frida attached. Applies any pending trace patterns set via debug_trace (without sessionId). If no patterns were set, no functions will be traced — call debug_trace first. Process stdout/stderr are captured and queryable as events.".to_string(),
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
                name: "debug_trace".to_string(),
                description: "Configure trace patterns. Call BEFORE debug_launch (without sessionId) to set which functions to trace — patterns are applied when the process spawns. Can also be called WITH sessionId to add/remove patterns on a running session.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string", "description": "Session ID. Omit to set pending patterns for the next debug_launch. Provide to modify a running session." },
                        "add": { "type": "array", "items": { "type": "string" }, "description": "Patterns to start tracing (e.g. \"mymodule::*\", \"*::init\", \"@usercode\")" },
                        "remove": { "type": "array", "items": { "type": "string" }, "description": "Patterns to stop tracing" }
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
                        "eventType": { "type": "string", "enum": ["function_enter", "function_exit", "stdout", "stderr"] },
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
                        "limit": { "type": "integer", "default": 50, "maximum": 500 },
                        "offset": { "type": "integer" },
                        "verbose": { "type": "boolean", "default": false }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_stop".to_string(),
                description: "Stop a debug session and clean up resources".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" }
                    },
                    "required": ["sessionId"]
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
            "debug_stop" => self.tool_debug_stop(&call.arguments).await,
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
                    let _ = self.session_manager.stop_session(&session_id);
                }
            }
        }
    }

    async fn tool_debug_launch(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let req: DebugLaunchRequest = serde_json::from_value(args.clone())?;

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
                let _ = self.session_manager.stop_session(&existing.id);

                // Remove from all connection tracking
                let mut sessions = self.connection_sessions.write().await;
                for session_list in sessions.values_mut() {
                    session_list.retain(|s| s != &existing.id);
                }
            }
        }

        // Extract binary name from path
        let binary_name = std::path::Path::new(&req.command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let session_id = self.session_manager.generate_session_id(binary_name);

        // Launch always starts fast (no DWARF blocking, no initial hooks).
        // DWARF parsing happens in the background.
        let args_vec = req.args.unwrap_or_default();
        let pid = self.session_manager.spawn_with_frida(
            &session_id,
            &req.command,
            &args_vec,
            req.cwd.as_deref(),
            &req.project_root,
            req.env.as_ref(),
        ).await?;

        self.session_manager.create_session(
            &session_id,
            &req.command,
            &req.project_root,
            pid,
        )?;

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

        if !pending_patterns.is_empty() {
            self.session_manager.add_patterns(&session_id, &pending_patterns)?;

            let sm = Arc::clone(&self.session_manager);
            let sid = session_id.clone();
            tokio::spawn(async move {
                match sm.update_frida_patterns(&sid, Some(&pending_patterns), None).await {
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

        let response = DebugLaunchResponse {
            session_id,
            pid,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_trace(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
        let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

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
                let response = DebugTraceResponse {
                    active_patterns: patterns,
                    hooked_functions: 0, // Not hooked yet, just pending
                    matched_functions: None,
                    warnings: vec![],
                };
                Ok(serde_json::to_value(response)?)
            }
            // Has session ID - modify running session
            Some(ref session_id) => {
                // Verify session exists
                let _ = self.session_manager.get_session(session_id)?
                    .ok_or_else(|| crate::Error::SessionNotFound(session_id.clone()))?;

                // Update patterns in session manager
                if let Some(ref add) = req.add {
                    self.session_manager.add_patterns(session_id, add)?;
                }
                if let Some(ref remove) = req.remove {
                    self.session_manager.remove_patterns(session_id, remove)?;
                }

                // Update Frida hooks
                let default_result = crate::frida_collector::HookResult {
                    installed: 0, matched: 0, warnings: vec![],
                };
                let hook_result = self.session_manager.update_frida_patterns(
                    session_id,
                    req.add.as_deref(),
                    req.remove.as_deref(),
                ).await.unwrap_or(default_result);

                self.session_manager.set_hook_count(session_id, hook_result.installed);

                let patterns = self.session_manager.get_patterns(session_id);

                let response = DebugTraceResponse {
                    active_patterns: patterns,
                    hooked_functions: hook_result.installed,
                    matched_functions: if hook_result.matched != hook_result.installed {
                        Some(hook_result.matched)
                    } else {
                        None
                    },
                    warnings: hook_result.warnings,
                };

                Ok(serde_json::to_value(response)?)
            }
        }
    }

    async fn tool_debug_query(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugQueryRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.session_manager.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

        let limit = req.limit.unwrap_or(50).min(500);
        let offset = req.offset.unwrap_or(0);

        let events = self.session_manager.db().query_events(&req.session_id, |mut q| {
            if let Some(ref et) = req.event_type {
                q = q.event_type(match et {
                    EventTypeFilter::FunctionEnter => crate::db::EventType::FunctionEnter,
                    EventTypeFilter::FunctionExit => crate::db::EventType::FunctionExit,
                    EventTypeFilter::Stdout => crate::db::EventType::Stdout,
                    EventTypeFilter::Stderr => crate::db::EventType::Stderr,
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
            q.limit(limit).offset(offset)
        })?;

        let total_count = self.session_manager.db().count_events(&req.session_id)?;
        let has_more = (offset + events.len() as u32) < total_count as u32;

        // Convert to appropriate format
        let verbose = req.verbose.unwrap_or(false);
        let event_values: Vec<serde_json::Value> = events.iter().map(|e| {
            // Output events have a different shape
            if e.event_type == crate::db::EventType::Stdout || e.event_type == crate::db::EventType::Stderr {
                return serde_json::json!({
                    "id": e.id,
                    "timestamp_ns": e.timestamp_ns,
                    "eventType": e.event_type.as_str(),
                    "threadId": e.thread_id,
                    "text": e.text,
                });
            }

            // Function trace events
            if verbose {
                serde_json::json!({
                    "id": e.id,
                    "timestamp_ns": e.timestamp_ns,
                    "eventType": e.event_type.as_str(),
                    "function": e.function_name,
                    "functionRaw": e.function_name_raw,
                    "sourceFile": e.source_file,
                    "line": e.line_number,
                    "duration_ns": e.duration_ns,
                    "threadId": e.thread_id,
                    "parentEventId": e.parent_event_id,
                    "arguments": e.arguments,
                    "returnValue": e.return_value,
                })
            } else {
                serde_json::json!({
                    "id": e.id,
                    "timestamp_ns": e.timestamp_ns,
                    "eventType": e.event_type.as_str(),
                    "function": e.function_name,
                    "sourceFile": e.source_file,
                    "line": e.line_number,
                    "duration_ns": e.duration_ns,
                    "returnType": e.return_value.as_ref()
                        .map(|v| match v {
                            serde_json::Value::Null => "null",
                            serde_json::Value::Bool(_) => "bool",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Array(_) => "array",
                            serde_json::Value::Object(_) => "object",
                        })
                        .unwrap_or("void"),
                })
            }
        }).collect();

        let response = DebugQueryResponse {
            events: event_values,
            total_count,
            has_more,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_stop(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugStopRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.session_manager.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

        // Stop Frida session
        self.session_manager.stop_frida(&req.session_id).await?;

        let events_collected = self.session_manager.stop_session(&req.session_id)?;

        // Remove from connection tracking so disconnect cleanup doesn't try to stop it again
        {
            let mut sessions = self.connection_sessions.write().await;
            for session_list in sessions.values_mut() {
                session_list.retain(|s| s != &req.session_id);
            }
        }

        let response = DebugStopResponse {
            success: true,
            events_collected,
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
}
