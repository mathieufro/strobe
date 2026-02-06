use std::collections::HashSet;
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

pub struct Daemon {
    socket_path: PathBuf,
    pid_path: PathBuf,
    session_manager: Arc<SessionManager>,
    last_activity: Arc<RwLock<Instant>>,
    /// Pending trace patterns to apply on next launch
    pending_patterns: Arc<RwLock<HashSet<String>>>,
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
            pending_patterns: Arc::new(RwLock::new(HashSet::new())),
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
                self.cleanup();
                std::process::exit(0);
            }
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_path);
    }

    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break; // EOF
            }

            // Update activity timestamp
            *self.last_activity.write().await = Instant::now();

            let response = self.handle_message(&line).await;
            let response_json = serde_json::to_string(&response)?;
            writer.write_all(response_json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        Ok(())
    }

    async fn handle_message(&self, message: &str) -> JsonRpcResponse {
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

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request.params).await,
            "initialized" => Ok(serde_json::json!({})),
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(&request.params).await,
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
        };

        Ok(serde_json::to_value(response)?)
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

    async fn handle_tools_call(&self, params: &serde_json::Value) -> Result<serde_json::Value> {
        let call: McpToolCallRequest = serde_json::from_value(params.clone())?;

        let result = match call.name.as_str() {
            "debug_launch" => self.tool_debug_launch(&call.arguments).await,
            "debug_trace" => self.tool_debug_trace(&call.arguments).await,
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

    async fn tool_debug_launch(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugLaunchRequest = serde_json::from_value(args.clone())?;

        // Auto-cleanup: if there's already a session for this binary, stop it first
        if let Some(existing) = self.session_manager.db().get_session_by_binary(&req.command)? {
            if existing.status == crate::db::SessionStatus::Running {
                tracing::info!("Auto-stopping existing session {} before new launch", existing.id);
                let _ = self.session_manager.stop_frida(&existing.id).await;
                let _ = self.session_manager.stop_session(&existing.id);
            }
        }

        // Extract binary name from path
        let binary_name = std::path::Path::new(&req.command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let session_id = self.session_manager.generate_session_id(binary_name);

        // Get pending patterns to apply on launch (don't clear them)
        let initial_patterns: Vec<String> = {
            let pending = self.pending_patterns.read().await;
            pending.iter().cloned().collect()
        };

        // Spawn with Frida, passing initial patterns
        let args_vec = req.args.unwrap_or_default();
        let pid = self.session_manager.spawn_with_frida(
            &session_id,
            &req.command,
            &args_vec,
            req.cwd.as_deref(),
            &req.project_root,
            req.env.as_ref(),
            &initial_patterns,
        ).await?;

        self.session_manager.create_session(
            &session_id,
            &req.command,
            &req.project_root,
            pid,
        )?;

        // Store initial patterns in the session
        if !initial_patterns.is_empty() {
            self.session_manager.add_patterns(&session_id, &initial_patterns)?;
        }

        let response = DebugLaunchResponse {
            session_id,
            pid,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_trace(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

        match req.session_id {
            // No session ID - modify pending patterns for next launch
            None => {
                let mut pending = self.pending_patterns.write().await;

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
                let hook_count = self.session_manager.update_frida_patterns(
                    session_id,
                    req.add.as_deref(),
                    req.remove.as_deref(),
                ).await.unwrap_or(0);

                self.session_manager.set_hook_count(session_id, hook_count);

                let patterns = self.session_manager.get_patterns(session_id);

                let response = DebugTraceResponse {
                    active_patterns: patterns,
                    hooked_functions: hook_count,
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

        let response = DebugStopResponse {
            success: true,
            events_collected,
        };

        Ok(serde_json::to_value(response)?)
    }
}
