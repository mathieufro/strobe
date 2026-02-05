use std::collections::HashMap;
use std::ffi::CString;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use frida::{Message, ScriptHandler};
use tokio::sync::{mpsc, oneshot};
use crate::db::{Event, EventType};
use crate::dwarf::{DwarfParser, FunctionInfo};
use crate::Result;
use super::HookManager;

// Embedded agent code
const AGENT_CODE: &str = include_str!("../../agent/dist/agent.js");

/// Message handler that implements ScriptHandler trait
struct AgentMessageHandler {
    event_tx: mpsc::Sender<Event>,
    session_id: String,
}

impl ScriptHandler for AgentMessageHandler {
    fn on_message(&mut self, message: Message, _data: Option<Vec<u8>>) {
        tracing::info!("ON_MESSAGE CALLED: {:?}", message);
        match &message {
            Message::Send(msg) => {
                tracing::info!("Message::Send received, payload: {:?}", msg.payload);
                // Our agent sends custom payloads - extract from returns field
                if let Some(payload) = msg.payload.returns.as_object() {
                    if let Some(msg_type) = payload.get("type").and_then(|v| v.as_str()) {
                        handle_agent_payload(&self.event_tx, &self.session_id, msg_type, &msg.payload.returns);
                    }
                }
            }
            Message::Other(value) => {
                tracing::info!("Message::Other received: {:?}", value);
                // Messages arrive with payload in a nested "data" field as a JSON string
                // Format: {"data": "{\"type\":\"send\",\"payload\":{...}}", "error": "..."}
                if let Some(data_str) = value.get("data").and_then(|v| v.as_str()) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data_str) {
                        if let Some(payload) = parsed.get("payload") {
                            if let Some(msg_type) = payload.get("type").and_then(|v| v.as_str()) {
                                handle_agent_payload(&self.event_tx, &self.session_id, msg_type, payload);
                            }
                        }
                    }
                } else if let Some(payload) = value.get("payload") {
                    // Fallback: check for direct payload (in case format changes)
                    if let Some(msg_type) = payload.get("type").and_then(|v| v.as_str()) {
                        handle_agent_payload(&self.event_tx, &self.session_id, msg_type, payload);
                    }
                }
            }
            Message::Log(log) => {
                tracing::info!("Agent log [{}]: {}", self.session_id, log.payload);
            }
            Message::Error(err) => {
                tracing::error!("Agent error [{}]: {} at {}:{}:{}",
                    self.session_id, err.description, err.file_name, err.line_number, err.column_number);
            }
        }
    }
}

fn handle_agent_payload(tx: &mpsc::Sender<Event>, session_id: &str, msg_type: &str, payload: &serde_json::Value) {
    tracing::debug!("Agent message [{}]: type={}", session_id, msg_type);
    match msg_type {
        "events" => {
            if let Some(events) = payload.get("events").and_then(|v| v.as_array()) {
                tracing::info!("Received {} events from agent [{}]", events.len(), session_id);
                for event_json in events {
                    if let Some(event) = parse_event(session_id, event_json) {
                        let _ = tx.try_send(event);
                    }
                }
            }
        }
        "initialized" => {
            tracing::info!("Agent initialized for session {}", session_id);
        }
        "hooks_updated" => {
            if let Some(count) = payload.get("activeCount").and_then(|v| v.as_u64()) {
                tracing::info!("Hooks updated for session {}: {} active", session_id, count);
            }
        }
        "log" => {
            if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                tracing::info!("Agent [{}]: {}", session_id, msg);
            }
        }
        "agent_loaded" => {
            if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                tracing::info!("Agent loaded: {}", msg);
            }
        }
        _ => {
            tracing::debug!("Unknown message type from agent: {}", msg_type);
        }
    }
}

/// Commands sent to the Frida worker thread
enum FridaCommand {
    Spawn {
        session_id: String,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        project_root: String,
        env: Option<HashMap<String, String>>,
        initial_functions: Vec<FunctionTarget>,
        event_tx: mpsc::Sender<Event>,
        response: oneshot::Sender<Result<u32>>,
    },
    AddPatterns {
        session_id: String,
        functions: Vec<FunctionTarget>,
        response: oneshot::Sender<Result<u32>>,
    },
    RemovePatterns {
        session_id: String,
        functions: Vec<FunctionTarget>,
        response: oneshot::Sender<Result<()>>,
    },
    Stop {
        session_id: String,
        response: oneshot::Sender<Result<()>>,
    },
}

#[derive(Clone)]
struct FunctionTarget {
    address: u64,
    name: String,
    name_raw: Option<String>,
    source_file: Option<String>,
    line_number: Option<u32>,
}

impl From<&FunctionInfo> for FunctionTarget {
    fn from(f: &FunctionInfo) -> Self {
        Self {
            address: f.low_pc,
            name: f.name.clone(),
            name_raw: f.name_raw.clone(),
            source_file: f.source_file.clone(),
            line_number: f.line_number,
        }
    }
}

/// Session state managed in the worker thread
/// We store Session and Script as leaked boxes with 'static lifetime to avoid lifetime complexity.
/// This leaks memory but is acceptable for a long-running daemon with limited sessions.
struct WorkerSession {
    session_id: String,
    event_tx: mpsc::Sender<Event>,
    script: &'static mut frida::Script<'static>,
}

/// Frida worker that runs on a dedicated thread
fn frida_worker(cmd_rx: std::sync::mpsc::Receiver<FridaCommand>) {
    use frida::{Frida, DeviceManager, DeviceType, SpawnOptions, ScriptOption};

    // Initialize Frida on this thread (unsafe because it initializes global state)
    let frida = unsafe { Frida::obtain() };
    let device_manager = DeviceManager::obtain(&frida);

    // Track active sessions
    let mut sessions: HashMap<String, WorkerSession> = HashMap::new();

    // Get local device once
    let devices = device_manager.enumerate_all_devices();
    let mut device = match devices.into_iter().find(|d| d.get_type() == DeviceType::Local) {
        Some(d) => d,
        None => {
            tracing::error!("No local Frida device found");
            return;
        }
    };

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            FridaCommand::Spawn {
                session_id,
                command,
                args,
                cwd,
                project_root: _,
                env,
                initial_functions,
                event_tx,
                response,
            } => {
                let result = (|| -> Result<u32> {
                    // Build spawn options using builder pattern (each method consumes self)
                    let mut argv: Vec<&str> = vec![&command];
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    argv.extend(arg_refs);

                    let mut spawn_opts = SpawnOptions::new().argv(&argv);

                    // Add cwd if specified
                    let cwd_cstr: Option<CString>;
                    if let Some(ref dir) = cwd {
                        if let Ok(c) = CString::new(dir.as_str()) {
                            cwd_cstr = Some(c);
                            spawn_opts = spawn_opts.cwd(cwd_cstr.as_ref().unwrap());
                        }
                    }

                    // Build env as Vec of (key, value) tuples for frida API
                    if let Some(ref env_vars) = env {
                        let env_tuples: Vec<(&str, &str)> = env_vars
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        spawn_opts = spawn_opts.envp(env_tuples);
                    }

                    // Spawn process (suspended)
                    let pid = device.spawn(&command, &spawn_opts)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Spawn failed: {}", e)))?;

                    tracing::info!("Spawned process {} with PID {}", command, pid);

                    // Attach to process
                    let frida_session = device.attach(pid)
                        .map_err(|e| {
                            tracing::error!("Attach to PID {} failed: {:?}", pid, e);
                            crate::Error::FridaAttachFailed(format!("Attach to PID {} failed: {}", pid, e))
                        })?;

                    // Leak session to get 'static lifetime (acceptable for daemon with limited sessions)
                    let leaked_session: &'static mut frida::Session<'static> =
                        Box::leak(Box::new(unsafe { std::mem::transmute(frida_session) }));

                    // Create and load agent script
                    let mut script = leaked_session.create_script(AGENT_CODE, &mut ScriptOption::new())
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Script creation failed: {}", e)))?;

                    // Set up message handler
                    let handler = AgentMessageHandler {
                        event_tx: event_tx.clone(),
                        session_id: session_id.clone(),
                    };
                    script.handle_message(handler)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Message handler setup failed: {}", e)))?;

                    // Load script
                    script.load()
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Script load failed: {}", e)))?;

                    // Leak script to get 'static lifetime
                    let leaked_script: &'static mut frida::Script<'static> =
                        Box::leak(Box::new(unsafe { std::mem::transmute(script) }));

                    // Initialize agent
                    let init_msg = serde_json::json!({ "type": "initialize", "sessionId": session_id });
                    leaked_script.post(&serde_json::to_string(&init_msg).unwrap(), None)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Init message failed: {}", e)))?;

                    // Install initial hooks BEFORE resuming process
                    if !initial_functions.is_empty() {
                        tracing::info!("Installing {} initial hooks before resume", initial_functions.len());
                        let func_list: Vec<serde_json::Value> = initial_functions.iter().map(|f| {
                            serde_json::json!({
                                "address": format!("0x{:x}", f.address),
                                "name": f.name,
                                "nameRaw": f.name_raw,
                                "sourceFile": f.source_file,
                                "lineNumber": f.line_number,
                            })
                        }).collect();

                        let hooks_msg = serde_json::json!({
                            "type": "hooks",
                            "action": "add",
                            "functions": func_list,
                        });

                        leaked_script.post(&serde_json::to_string(&hooks_msg).unwrap(), None)
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Initial hooks failed: {}", e)))?;
                    }

                    // Resume process (now with hooks installed)
                    device.resume(pid)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Resume failed: {}", e)))?;

                    sessions.insert(session_id.clone(), WorkerSession {
                        session_id,
                        event_tx,
                        script: leaked_script,
                    });

                    Ok(pid)
                })();

                let _ = response.send(result);
            }

            FridaCommand::AddPatterns {
                session_id,
                functions,
                response,
            } => {
                tracing::info!("AddPatterns: {} functions for session {}", functions.len(), session_id);
                let result = (|| -> Result<u32> {
                    let session = sessions.get_mut(&session_id)
                        .ok_or_else(|| crate::Error::SessionNotFound(session_id.clone()))?;

                    // Build the hooks message for the agent
                    let func_list: Vec<serde_json::Value> = functions.iter().map(|f| {
                        serde_json::json!({
                            "address": format!("0x{:x}", f.address),
                            "name": f.name,
                            "nameRaw": f.name_raw,
                            "sourceFile": f.source_file,
                            "lineNumber": f.line_number,
                        })
                    }).collect();

                    tracing::debug!("Sending hooks message with {} functions", func_list.len());

                    let hooks_msg = serde_json::json!({
                        "type": "hooks",
                        "action": "add",
                        "functions": func_list,
                    });

                    session.script.post(&serde_json::to_string(&hooks_msg).unwrap(), None)
                        .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;

                    tracing::info!("Sent hooks message for {} functions", functions.len());
                    Ok(functions.len() as u32)
                })();

                let _ = response.send(result);
            }

            FridaCommand::RemovePatterns {
                session_id,
                functions,
                response,
            } => {
                let result = (|| -> Result<()> {
                    let session = sessions.get_mut(&session_id)
                        .ok_or_else(|| crate::Error::SessionNotFound(session_id.clone()))?;

                    // Build the hooks message for the agent
                    let func_list: Vec<serde_json::Value> = functions.iter().map(|f| {
                        serde_json::json!({
                            "address": format!("0x{:x}", f.address),
                        })
                    }).collect();

                    let hooks_msg = serde_json::json!({
                        "type": "hooks",
                        "action": "remove",
                        "functions": func_list,
                    });

                    session.script.post(&serde_json::to_string(&hooks_msg).unwrap(), None)
                        .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;

                    Ok(())
                })();

                let _ = response.send(result);
            }

            FridaCommand::Stop {
                session_id,
                response,
            } => {
                sessions.remove(&session_id);
                let _ = response.send(Ok(()));
            }
        }
    }
}

fn parse_event(session_id: &str, json: &serde_json::Value) -> Option<Event> {
    let event_type = match json.get("eventType")?.as_str()? {
        "function_enter" => EventType::FunctionEnter,
        "function_exit" => EventType::FunctionExit,
        "stdout" => EventType::Stdout,
        "stderr" => EventType::Stderr,
        _ => return None,
    };

    // Output events have a simpler structure
    if event_type == EventType::Stdout || event_type == EventType::Stderr {
        return Some(Event {
            id: json.get("id")?.as_str()?.to_string(),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            parent_event_id: None,
            event_type,
            function_name: String::new(),
            function_name_raw: None,
            source_file: None,
            line_number: None,
            arguments: None,
            return_value: None,
            duration_ns: None,
            text: json.get("text").and_then(|v| v.as_str()).map(|s| s.to_string()),
        });
    }

    Some(Event {
        id: json.get("id")?.as_str()?.to_string(),
        session_id: session_id.to_string(),
        timestamp_ns: json.get("timestampNs")?.as_i64()?,
        thread_id: json.get("threadId")?.as_i64()?,
        parent_event_id: json.get("parentEventId").and_then(|v| v.as_str()).map(|s| s.to_string()),
        event_type,
        function_name: json.get("functionName")?.as_str()?.to_string(),
        function_name_raw: json.get("functionNameRaw").and_then(|v| v.as_str()).map(|s| s.to_string()),
        source_file: json.get("sourceFile").and_then(|v| v.as_str()).map(|s| s.to_string()),
        line_number: json.get("lineNumber").and_then(|v| v.as_i64()).map(|n| n as i32),
        arguments: json.get("arguments").cloned(),
        return_value: json.get("returnValue").cloned(),
        duration_ns: json.get("durationNs").and_then(|v| v.as_i64()),
        text: None,
    })
}

/// Session state on the main thread
pub struct FridaSession {
    pub pid: u32,
    pub binary_path: String,
    pub project_root: String,
    hook_manager: HookManager,
    dwarf: Option<Arc<DwarfParser>>,
}

/// Spawner that communicates with the Frida worker thread
pub struct FridaSpawner {
    sessions: HashMap<String, FridaSession>,
    cmd_tx: std::sync::mpsc::Sender<FridaCommand>,
}

impl FridaSpawner {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

        // Spawn the Frida worker thread
        thread::spawn(move || {
            frida_worker(cmd_rx);
        });

        Self {
            sessions: HashMap::new(),
            cmd_tx,
        }
    }

    /// Spawn a process with Frida attached
    pub async fn spawn(
        &mut self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&HashMap<String, String>>,
        initial_patterns: &[String],
        event_sender: mpsc::Sender<Event>,
    ) -> Result<u32> {
        // Parse DWARF first to ensure we have debug symbols
        let dwarf = DwarfParser::parse(Path::new(command))?;
        let dwarf = Arc::new(dwarf);

        // Compute initial functions from patterns BEFORE spawn so hooks are installed before resume
        let mut initial_functions: Vec<FunctionTarget> = Vec::new();
        if !initial_patterns.is_empty() {
            let hook_manager = HookManager::new();
            let expanded = hook_manager.expand_patterns(initial_patterns, project_root);
            for pattern in &expanded {
                for func in dwarf.find_by_pattern(pattern) {
                    initial_functions.push(FunctionTarget::from(func));
                }
            }
            tracing::info!("Found {} functions matching {} initial patterns", initial_functions.len(), initial_patterns.len());
        }

        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::Spawn {
            session_id: session_id.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            cwd: cwd.map(|s| s.to_string()),
            project_root: project_root.to_string(),
            env: env.cloned(),
            initial_functions,
            event_tx: event_sender,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        let pid = response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))??;

        let mut session = FridaSession {
            pid,
            binary_path: command.to_string(),
            project_root: project_root.to_string(),
            hook_manager: HookManager::new(),
            dwarf: Some(dwarf),
        };

        // Record the patterns in the hook_manager for later queries
        if !initial_patterns.is_empty() {
            let expanded = session.hook_manager.expand_patterns(initial_patterns, project_root);
            session.hook_manager.add_patterns(&expanded);
        }

        self.sessions.insert(session_id.to_string(), session);

        Ok(pid)
    }

    /// Add trace patterns to a session
    pub async fn add_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<u32> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        let expanded = session.hook_manager.expand_patterns(patterns, &session.project_root);
        session.hook_manager.add_patterns(&expanded);

        // Find matching functions from DWARF
        let mut functions: Vec<FunctionTarget> = Vec::new();
        if let Some(ref dwarf) = session.dwarf {
            for pattern in &expanded {
                for func in dwarf.find_by_pattern(pattern) {
                    functions.push(FunctionTarget::from(func));
                }
            }
        }

        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::AddPatterns {
            session_id: session_id.to_string(),
            functions,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))?
    }

    /// Remove trace patterns from a session
    pub async fn remove_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<()> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        // Find functions to unhook
        let mut functions: Vec<FunctionTarget> = Vec::new();
        if let Some(ref dwarf) = session.dwarf {
            for pattern in patterns {
                for func in dwarf.find_by_pattern(pattern) {
                    functions.push(FunctionTarget::from(func));
                }
            }
        }

        session.hook_manager.remove_patterns(patterns);

        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::RemovePatterns {
            session_id: session_id.to_string(),
            functions,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))?
    }

    /// Stop a session and detach Frida
    pub async fn stop(&mut self, session_id: &str) -> Result<()> {
        self.sessions.remove(session_id);

        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::Stop {
            session_id: session_id.to_string(),
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))?
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        self.sessions
            .get(session_id)
            .map(|s| s.hook_manager.active_patterns())
            .unwrap_or_default()
    }
}

impl Default for FridaSpawner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_event_stdout() {
        let event = parse_event("session-1", &json!({
            "id": "evt-1",
            "timestampNs": 1000,
            "threadId": 42,
            "eventType": "stdout",
            "text": "hello world\n"
        }));

        let e = event.expect("should parse stdout event");
        assert_eq!(e.event_type, EventType::Stdout);
        assert_eq!(e.text.as_deref(), Some("hello world\n"));
        assert_eq!(e.function_name, "");
        assert_eq!(e.session_id, "session-1");
        assert_eq!(e.thread_id, 42);
        assert!(e.parent_event_id.is_none());
    }

    #[test]
    fn test_parse_event_stderr() {
        let event = parse_event("session-1", &json!({
            "id": "evt-2",
            "timestampNs": 2000,
            "threadId": 1,
            "eventType": "stderr",
            "text": "Error: crash\n"
        }));

        let e = event.expect("should parse stderr event");
        assert_eq!(e.event_type, EventType::Stderr);
        assert_eq!(e.text.as_deref(), Some("Error: crash\n"));
    }

    #[test]
    fn test_parse_event_stdout_missing_text() {
        let event = parse_event("session-1", &json!({
            "id": "evt-3",
            "timestampNs": 3000,
            "threadId": 1,
            "eventType": "stdout"
        }));

        let e = event.expect("should parse stdout even without text");
        assert_eq!(e.event_type, EventType::Stdout);
        assert!(e.text.is_none());
    }

    #[test]
    fn test_parse_event_stdout_missing_required_fields() {
        // Missing id
        assert!(parse_event("s", &json!({
            "timestampNs": 1000, "threadId": 1, "eventType": "stdout"
        })).is_none());

        // Missing timestampNs
        assert!(parse_event("s", &json!({
            "id": "x", "threadId": 1, "eventType": "stdout"
        })).is_none());

        // Missing threadId
        assert!(parse_event("s", &json!({
            "id": "x", "timestampNs": 1000, "eventType": "stdout"
        })).is_none());
    }

    #[test]
    fn test_parse_event_function_enter() {
        let event = parse_event("session-1", &json!({
            "id": "evt-4",
            "timestampNs": 4000,
            "threadId": 1,
            "eventType": "function_enter",
            "functionName": "main::run",
            "functionNameRaw": "_ZN4main3runEv",
            "sourceFile": "/src/main.rs",
            "lineNumber": 10,
            "parentEventId": null,
            "arguments": [1, 2]
        }));

        let e = event.expect("should parse function_enter event");
        assert_eq!(e.event_type, EventType::FunctionEnter);
        assert_eq!(e.function_name, "main::run");
        assert_eq!(e.source_file.as_deref(), Some("/src/main.rs"));
        assert!(e.text.is_none());
    }

    #[test]
    fn test_parse_event_unknown_type() {
        assert!(parse_event("s", &json!({
            "id": "x", "timestampNs": 1000, "threadId": 1,
            "eventType": "unknown_type"
        })).is_none());
    }
}
