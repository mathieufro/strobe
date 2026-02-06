use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use tokio::sync::{mpsc, oneshot};
use crate::db::{Event, EventType};
use crate::dwarf::{DwarfParser, DwarfHandle, FunctionInfo};
use crate::Result;
use super::{HookManager, HookMode};

// ---------------------------------------------------------------------------
// Raw frida-sys wrappers
//
// frida-rs 0.17 has a type confusion bug in `Script::handle_message`:
// it passes `&CallbackHandler` as `user_data` to g_signal_connect_data, but
// `call_on_message` casts user_data to `*mut I` (the user's handler type).
// This causes a SIGSEGV when Frida delivers messages.
//
// Additionally, `Script`'s fields don't have `#[repr(C)]`, so we can't
// reliably extract the private `script_ptr` from a `frida::Script`.
//
// Solution: bypass `frida::Script` entirely. Use frida-sys for script
// creation, loading, posting, and message handling. Only use frida-rs for
// Device (spawn/attach/resume/kill) and Session (single non-ZST field, safe
// to extract raw pointer).
// ---------------------------------------------------------------------------

/// Raw C callback for Frida's "message" signal.
/// user_data points to a heap-allocated `AgentMessageHandler`.
unsafe extern "C" fn raw_on_message(
    _script: *mut frida_sys::_FridaScript,
    message: *const i8,
    _data: *const frida_sys::_GBytes,
    user_data: *mut c_void,
) {
    let handler = &mut *(user_data as *mut AgentMessageHandler);

    let c_msg = CStr::from_ptr(message as *const c_char)
        .to_str()
        .unwrap_or_default();

    // Parse the outer Frida message envelope
    let parsed: serde_json::Value = match serde_json::from_str(c_msg) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("Failed to parse Frida message: {}", e);
            return;
        }
    };

    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

    match msg_type {
        "send" => {
            // Extract the payload from `{ "type": "send", "payload": { ... } }`
            if let Some(payload) = parsed.get("payload") {
                if let Some(inner_type) = payload.get("type").and_then(|v| v.as_str()) {
                    handler.handle_payload(inner_type, payload);
                }
            }
        }
        "log" => {
            let level = parsed.get("level").and_then(|v| v.as_str()).unwrap_or("info");
            let payload = parsed.get("payload").and_then(|v| v.as_str()).unwrap_or("");
            tracing::info!("Agent log [{}] [{}]: {}", handler.session_id, level, payload);
        }
        "error" => {
            let desc = parsed.get("description").and_then(|v| v.as_str()).unwrap_or("?");
            let stack = parsed.get("stack").and_then(|v| v.as_str()).unwrap_or("");
            tracing::error!("Agent error [{}]: {}\n{}", handler.session_id, desc, stack);
        }
        _ => {
            tracing::debug!("Unknown Frida message type '{}': {}", msg_type, c_msg);
        }
    }
}

/// Extract raw `_FridaSession` pointer from `frida::Session`.
/// Safe because `Session` has only one non-ZST field (`session_ptr`),
/// so it's guaranteed to be at offset 0 regardless of field ordering.
unsafe fn session_raw_ptr(session: &frida::Session) -> *mut frida_sys::_FridaSession {
    *(session as *const frida::Session as *const *mut frida_sys::_FridaSession)
}

/// Create a Frida script using frida-sys directly (bypasses frida::Script).
/// Returns the raw script pointer.
unsafe fn create_script_raw(
    session_ptr: *mut frida_sys::_FridaSession,
    source: &str,
) -> std::result::Result<*mut frida_sys::_FridaScript, String> {
    let source_cstr = CString::new(source).map_err(|e| format!("CString error: {}", e))?;
    let opt = frida_sys::frida_script_options_new();
    let mut error: *mut frida_sys::GError = std::ptr::null_mut();

    let script_ptr = frida_sys::frida_session_create_script_sync(
        session_ptr,
        source_cstr.as_ptr(),
        opt,
        std::ptr::null_mut(),
        &mut error,
    );

    // Clean up options
    frida_sys::frida_unref(opt as *mut c_void);

    if !error.is_null() {
        let err_msg = CStr::from_ptr((*error).message)
            .to_str()
            .unwrap_or("unknown error")
            .to_string();
        frida_sys::g_error_free(error);
        return Err(err_msg);
    }

    if script_ptr.is_null() {
        return Err("script_ptr is null".to_string());
    }

    Ok(script_ptr)
}

/// Register message handler on a raw script pointer.
unsafe fn register_handler_raw(
    script_ptr: *mut frida_sys::_FridaScript,
    handler: AgentMessageHandler,
) -> *mut AgentMessageHandler {
    let handler_ptr = Box::into_raw(Box::new(handler));
    let signal_name = CString::new("message").unwrap();

    let callback = Some(std::mem::transmute::<
        *mut c_void,
        unsafe extern "C" fn(),
    >(raw_on_message as *mut c_void));

    frida_sys::g_signal_connect_data(
        script_ptr as *mut _,
        signal_name.as_ptr(),
        callback,
        handler_ptr as *mut c_void,
        None,
        0,
    );

    handler_ptr
}

/// Load a raw script.
unsafe fn load_script_raw(
    script_ptr: *mut frida_sys::_FridaScript,
) -> std::result::Result<(), String> {
    let mut error: *mut frida_sys::GError = std::ptr::null_mut();
    frida_sys::frida_script_load_sync(script_ptr, std::ptr::null_mut(), &mut error);

    if !error.is_null() {
        let err_msg = CStr::from_ptr((*error).message)
            .to_str()
            .unwrap_or("unknown error")
            .to_string();
        frida_sys::g_error_free(error);
        return Err(err_msg);
    }
    Ok(())
}

/// Extract raw `_FridaDevice` pointer from `frida::Device`.
/// Safe because `Device` has only one non-ZST field (`device_ptr`),
/// so it's guaranteed to be at offset 0 regardless of field ordering.
unsafe fn device_raw_ptr(device: &frida::Device) -> *mut frida_sys::_FridaDevice {
    *(device as *const frida::Device as *const *mut frida_sys::_FridaDevice)
}

/// Context for mapping PIDs to session info in the output callback.
struct OutputContext {
    pid: u32,
    session_id: String,
    event_tx: mpsc::Sender<Event>,
    event_counter: AtomicU64,
    start_ns: i64,
}

/// Shared registry of active output contexts, keyed by PID.
type OutputRegistry = Arc<Mutex<HashMap<u32, Arc<OutputContext>>>>;

/// Raw C callback for Frida's Device "output" signal.
/// Signature: void output(FridaDevice*, guint pid, gint fd, GBytes* data, gpointer user_data)
unsafe extern "C" fn raw_on_output(
    _device: *mut frida_sys::_FridaDevice,
    pid: u32,
    fd: i32,
    data: *mut frida_sys::GBytes,
    user_data: *mut c_void,
) {
    let registry = &*(user_data as *const Mutex<HashMap<u32, Arc<OutputContext>>>);

    let ctx = {
        let guard = match registry.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match guard.get(&pid) {
            Some(ctx) => ctx.clone(),
            None => return, // Unknown PID, ignore
        }
    };

    if data.is_null() {
        return;
    }

    let mut size: frida_sys::gsize = 0;
    let bytes_ptr = frida_sys::g_bytes_get_data(data, &mut size);
    if bytes_ptr.is_null() || size == 0 {
        return;
    }

    let slice = std::slice::from_raw_parts(bytes_ptr as *const u8, size as usize);
    let text = String::from_utf8_lossy(slice).to_string();
    if text.is_empty() {
        return;
    }

    let event_type = if fd == 1 { EventType::Stdout } else { EventType::Stderr };
    let counter = ctx.event_counter.fetch_add(1, Ordering::Relaxed);
    let now_ns = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64)
        - ctx.start_ns;

    let event = Event {
        id: format!("{}-output-{}", ctx.session_id, counter),
        session_id: ctx.session_id.clone(),
        timestamp_ns: now_ns,
        thread_id: 0,
        event_type,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        duration_ns: None,
        parent_event_id: None,
        arguments: None,
        return_value: None,
        text: Some(text),
    };

    let _ = ctx.event_tx.try_send(event);
}

/// Post a JSON message to a raw script.
unsafe fn post_message_raw(
    script_ptr: *mut frida_sys::_FridaScript,
    json: &str,
) -> std::result::Result<(), String> {
    let msg_cstr = CString::new(json).map_err(|e| format!("CString error: {}", e))?;
    frida_sys::frida_script_post(script_ptr, msg_cstr.as_ptr(), std::ptr::null_mut());
    Ok(())
}

// Embedded agent code
const AGENT_CODE: &str = include_str!("../../agent/dist/agent.js");

/// Channel for the worker to wait on agent responses (e.g., hooks_updated).
/// The message handler sends on this when it receives a hooks_updated message.
type HooksReadySignal = Arc<Mutex<Option<std::sync::mpsc::Sender<u64>>>>;

/// Message handler passed as user_data to the raw GLib signal callback.
/// No longer implements `ScriptHandler` — messages are parsed directly in `raw_on_message`.
struct AgentMessageHandler {
    event_tx: mpsc::Sender<Event>,
    session_id: String,
    hooks_ready: HooksReadySignal,
}

impl AgentMessageHandler {
    fn handle_payload(&self, msg_type: &str, payload: &serde_json::Value) {
        tracing::debug!("Agent message [{}]: type={}", self.session_id, msg_type);
        match msg_type {
            "events" => {
                if let Some(events) = payload.get("events").and_then(|v| v.as_array()) {
                    tracing::info!("Received {} events from agent [{}]", events.len(), self.session_id);
                    for event_json in events {
                        if let Some(event) = parse_event(&self.session_id, event_json) {
                            let _ = self.event_tx.try_send(event);
                        }
                    }
                }
            }
            "initialized" => {
                tracing::info!("Agent initialized for session {}", self.session_id);
            }
            "hooks_updated" => {
                let count = payload.get("activeCount").and_then(|v| v.as_u64()).unwrap_or(0);
                tracing::info!("Hooks updated for session {}: {} active", self.session_id, count);

                // Signal the worker that hooks are installed
                if let Ok(mut guard) = self.hooks_ready.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(count);
                    }
                }
            }
            "log" => {
                if let Some(msg) = payload.get("message").and_then(|v| v.as_str()) {
                    tracing::info!("Agent [{}]: {}", self.session_id, msg);
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
        image_base: u64,
        event_tx: mpsc::Sender<Event>,
        response: oneshot::Sender<Result<u32>>,
    },
    AddPatterns {
        session_id: String,
        functions: Vec<FunctionTarget>,
        image_base: u64,
        mode: HookMode,
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
struct WorkerSession {
    script_ptr: *mut frida_sys::_FridaScript,
    hooks_ready: HooksReadySignal,
    pid: u32,
}

/// Frida worker that runs on a dedicated thread
fn frida_worker(cmd_rx: std::sync::mpsc::Receiver<FridaCommand>) {
    use frida::{Frida, DeviceManager, DeviceType, SpawnOptions, SpawnStdio};

    let frida = unsafe { Frida::obtain() };
    let device_manager = DeviceManager::obtain(&frida);

    let mut sessions: HashMap<String, WorkerSession> = HashMap::new();

    let devices = device_manager.enumerate_all_devices();
    let mut device = match devices.into_iter().find(|d| d.get_type() == DeviceType::Local) {
        Some(d) => d,
        None => {
            tracing::error!("No local Frida device found");
            return;
        }
    };

    // Set up Device "output" signal handler for stdout/stderr capture.
    // This is the only reliable way to capture output from ASAN-instrumented binaries,
    // since the agent's write(2) hook doesn't work when ASAN wraps write().
    let output_registry: OutputRegistry = Arc::new(Mutex::new(HashMap::new()));
    unsafe {
        let device_ptr = device_raw_ptr(&device);
        let signal_name = CString::new("output").unwrap();
        let callback = Some(std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(),
        >(raw_on_output as *mut c_void));
        // Pass raw pointer to the Arc's inner Mutex — we keep the Arc alive for the worker's lifetime
        let registry_ptr = Arc::as_ptr(&output_registry) as *mut c_void;
        frida_sys::g_signal_connect_data(
            device_ptr as *mut _,
            signal_name.as_ptr(),
            callback,
            registry_ptr,
            None,
            0,
        );
    }

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
                image_base,
                event_tx,
                response,
            } => {
                let result = (|| -> Result<u32> {
                    let spawn_start = std::time::Instant::now();

                    let mut argv: Vec<&str> = vec![&command];
                    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
                    argv.extend(arg_refs);

                    let mut spawn_opts = SpawnOptions::new()
                        .argv(&argv)
                        .stdio(SpawnStdio::Pipe);

                    let cwd_cstr: Option<CString>;
                    if let Some(ref dir) = cwd {
                        if let Ok(c) = CString::new(dir.as_str()) {
                            cwd_cstr = Some(c);
                            spawn_opts = spawn_opts.cwd(cwd_cstr.as_ref().unwrap());
                        }
                    }

                    if let Some(ref env_vars) = env {
                        let env_tuples: Vec<(&str, &str)> = env_vars
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        spawn_opts = spawn_opts.envp(env_tuples);
                    }

                    let t = std::time::Instant::now();
                    let pid = device.spawn(&command, &spawn_opts)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Spawn failed: {}", e)))?;
                    tracing::info!("Spawned process {} with PID {}", command, pid);
                    tracing::debug!("PERF: device.spawn() took {:?}", t.elapsed());

                    // Register output context so raw_on_output can map this PID
                    let output_ctx = Arc::new(OutputContext {
                        pid,
                        session_id: session_id.clone(),
                        event_tx: event_tx.clone(),
                        event_counter: AtomicU64::new(0),
                        start_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as i64,
                    });
                    if let Ok(mut reg) = output_registry.lock() {
                        reg.insert(pid, output_ctx);
                    }

                    let t = std::time::Instant::now();
                    let frida_session = device.attach(pid)
                        .map_err(|e| {
                            tracing::error!("Attach to PID {} failed: {:?}", pid, e);
                            crate::Error::FridaAttachFailed(format!("Attach to PID {} failed: {}", pid, e))
                        })?;
                    tracing::debug!("PERF: device.attach() took {:?}", t.elapsed());

                    // Extract raw session pointer (Session has only one non-ZST field)
                    let raw_session = unsafe { session_raw_ptr(&frida_session) };
                    // Leak the session so it doesn't drop (which would call frida_unref)
                    std::mem::forget(frida_session);

                    // Create script via frida-sys (bypasses frida::Script entirely)
                    let t = std::time::Instant::now();
                    let script_ptr = unsafe {
                        create_script_raw(raw_session, AGENT_CODE)
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Script creation failed: {}", e)))?
                    };
                    tracing::debug!("PERF: create_script took {:?}", t.elapsed());

                    let t = std::time::Instant::now();

                    // Shared signal for hooks completion
                    let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));

                    let handler = AgentMessageHandler {
                        event_tx: event_tx.clone(),
                        session_id: session_id.clone(),
                        hooks_ready: hooks_ready.clone(),
                    };

                    // Register handler directly via GLib signal (correct user_data type)
                    let _handler_ptr = unsafe { register_handler_raw(script_ptr, handler) };

                    // Load the script
                    unsafe {
                        load_script_raw(script_ptr)
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Script load failed: {}", e)))?;
                    }
                    tracing::debug!("PERF: script load + handler setup took {:?}", t.elapsed());

                    // Initialize agent
                    let init_msg = serde_json::json!({ "type": "initialize", "sessionId": session_id });
                    unsafe {
                        post_message_raw(script_ptr, &serde_json::to_string(&init_msg).unwrap())
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Init message failed: {}", e)))?;
                    }

                    // Install initial hooks BEFORE resuming process
                    if !initial_functions.is_empty() {
                        tracing::info!("Installing {} initial hooks before resume", initial_functions.len());

                        // Set up the signal channel BEFORE posting the hooks message
                        let (signal_tx, signal_rx) = std::sync::mpsc::channel();
                        {
                            let mut guard = hooks_ready.lock().unwrap();
                            *guard = Some(signal_tx);
                        }

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
                            "imageBase": format!("0x{:x}", image_base),
                        });

                        unsafe {
                            post_message_raw(script_ptr, &serde_json::to_string(&hooks_msg).unwrap())
                                .map_err(|e| crate::Error::FridaAttachFailed(format!("Initial hooks failed: {}", e)))?;
                        }

                        // Wait for agent to finish installing hooks (deterministic, no sleep)
                        match signal_rx.recv_timeout(std::time::Duration::from_secs(60)) {
                            Ok(count) => {
                                tracing::info!("Agent confirmed {} hooks installed, resuming process", count);
                            }
                            Err(_) => {
                                tracing::warn!("Timed out waiting for hooks installation (60s), resuming anyway");
                            }
                        }
                    }

                    // Resume process (hooks are now installed)
                    let t = std::time::Instant::now();
                    device.resume(pid)
                        .map_err(|e| crate::Error::FridaAttachFailed(format!("Resume failed: {}", e)))?;
                    tracing::debug!("PERF: device.resume() took {:?}", t.elapsed());
                    tracing::debug!("PERF: Total Frida worker spawn took {:?}", spawn_start.elapsed());

                    sessions.insert(session_id.clone(), WorkerSession {
                        script_ptr,
                        hooks_ready,
                        pid,
                    });

                    Ok(pid)
                })();

                let _ = response.send(result);
            }

            FridaCommand::AddPatterns {
                session_id,
                functions,
                image_base,
                mode,
                response,
            } => {
                tracing::info!("AddPatterns: {} functions ({:?} mode) for session {}", functions.len(), mode, session_id);
                let result = (|| -> Result<u32> {
                    let session = sessions.get_mut(&session_id)
                        .ok_or_else(|| crate::Error::SessionNotFound(session_id.clone()))?;

                    let func_list: Vec<serde_json::Value> = functions.iter().map(|f| {
                        serde_json::json!({
                            "address": format!("0x{:x}", f.address),
                            "name": f.name,
                            "nameRaw": f.name_raw,
                            "sourceFile": f.source_file,
                            "lineNumber": f.line_number,
                        })
                    }).collect();

                    tracing::debug!("Sending hooks message with {} functions ({:?} mode)", func_list.len(), mode);

                    // Set up signal to wait for confirmation
                    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
                    {
                        let mut guard = session.hooks_ready.lock().unwrap();
                        *guard = Some(signal_tx);
                    }

                    let mode_str = match mode {
                        HookMode::Full => "full",
                        HookMode::Light => "light",
                    };

                    let hooks_msg = serde_json::json!({
                        "type": "hooks",
                        "action": "add",
                        "functions": func_list,
                        "imageBase": format!("0x{:x}", image_base),
                        "mode": mode_str,
                    });

                    unsafe {
                        post_message_raw(session.script_ptr, &serde_json::to_string(&hooks_msg).unwrap())
                            .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;
                    }

                    // Wait for agent confirmation
                    match signal_rx.recv_timeout(std::time::Duration::from_secs(60)) {
                        Ok(count) => {
                            tracing::info!("Agent confirmed {} hooks active after add", count);
                            Ok(count as u32)
                        }
                        Err(_) => {
                            tracing::warn!("Timed out waiting for hooks confirmation (60s)");
                            Ok(functions.len() as u32)
                        }
                    }
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

                    unsafe {
                        post_message_raw(session.script_ptr, &serde_json::to_string(&hooks_msg).unwrap())
                            .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;
                    }

                    Ok(())
                })();

                let _ = response.send(result);
            }

            FridaCommand::Stop {
                session_id,
                response,
            } => {
                if let Some(session) = sessions.remove(&session_id) {
                    // Remove from output registry
                    if let Ok(mut reg) = output_registry.lock() {
                        reg.remove(&session.pid);
                    }
                    // Kill the traced process
                    tracing::info!("Killing process {} for session {}", session.pid, session_id);
                    device.kill(session.pid)
                        .unwrap_or_else(|e| tracing::warn!("Failed to kill PID {}: {:?}", session.pid, e));
                }
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

/// Resolve a single pattern to matching functions from DWARF info.
fn resolve_pattern<'a>(
    dwarf: &'a DwarfParser,
    pattern: &str,
    project_root: &str,
) -> Vec<&'a FunctionInfo> {
    if pattern == "@usercode" {
        dwarf.user_code_functions(project_root)
    } else if let Some(file_pat) = pattern.strip_prefix("@file:") {
        dwarf.find_by_source_file(file_pat)
    } else {
        dwarf.find_by_pattern(pattern)
    }
}

/// Session state on the main thread
pub struct FridaSession {
    pub pid: u32,
    pub binary_path: String,
    pub project_root: String,
    hook_manager: HookManager,
    dwarf_handle: DwarfHandle,
    image_base: u64,
}

/// Spawner that communicates with the Frida worker thread
pub struct FridaSpawner {
    sessions: HashMap<String, FridaSession>,
    cmd_tx: std::sync::mpsc::Sender<FridaCommand>,
}

impl FridaSpawner {
    pub fn new() -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            frida_worker(cmd_rx);
        });

        Self {
            sessions: HashMap::new(),
            cmd_tx,
        }
    }

    pub async fn spawn(
        &mut self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&HashMap<String, String>>,
        dwarf_handle: DwarfHandle,
        image_base: u64,
        event_sender: mpsc::Sender<Event>,
    ) -> Result<u32> {
        // No DWARF parsing here — it happens in the background via DwarfHandle.
        // Launch always starts with zero hooks; patterns are installed later.

        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::Spawn {
            session_id: session_id.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            cwd: cwd.map(|s| s.to_string()),
            project_root: project_root.to_string(),
            env: env.cloned(),
            initial_functions: Vec::new(),
            image_base,
            event_tx: event_sender,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        let pid = response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))??;

        let session = FridaSession {
            pid,
            binary_path: command.to_string(),
            project_root: project_root.to_string(),
            hook_manager: HookManager::new(),
            dwarf_handle,
            image_base,
        };

        self.sessions.insert(session_id.to_string(), session);

        Ok(pid)
    }

    pub async fn add_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<u32> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        session.hook_manager.add_patterns(patterns);

        // Await DWARF parse completion (may block if still parsing in background)
        let dwarf = session.dwarf_handle.clone().get().await?;

        // Group functions by mode
        let mut full_funcs: Vec<FunctionTarget> = Vec::new();
        let mut light_funcs: Vec<FunctionTarget> = Vec::new();

        for pattern in patterns {
            let matches: Vec<&FunctionInfo> = resolve_pattern(&dwarf, pattern, &session.project_root);
            let mode = HookManager::classify_with_count(pattern, matches.len());
            tracing::info!("Pattern '{}' -> {:?} mode ({} functions)", pattern, mode, matches.len());

            let target = if mode == HookMode::Full { &mut full_funcs } else { &mut light_funcs };
            for func in matches {
                target.push(FunctionTarget::from(func));
            }
        }

        let image_base = session.image_base;
        let mut total_hooks = 0u32;

        // Send full-mode batch
        if !full_funcs.is_empty() {
            total_hooks += self.send_add_patterns(session_id, full_funcs, image_base, HookMode::Full).await?;
        }

        // Send light-mode batch
        if !light_funcs.is_empty() {
            total_hooks += self.send_add_patterns(session_id, light_funcs, image_base, HookMode::Light).await?;
        }

        Ok(total_hooks)
    }

    async fn send_add_patterns(
        &self,
        session_id: &str,
        functions: Vec<FunctionTarget>,
        image_base: u64,
        mode: HookMode,
    ) -> Result<u32> {
        let (response_tx, response_rx) = oneshot::channel();

        self.cmd_tx.send(FridaCommand::AddPatterns {
            session_id: session_id.to_string(),
            functions,
            image_base,
            mode,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Worker thread died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Worker response lost".to_string()))?
    }

    pub async fn remove_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<()> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        // Await DWARF parse completion
        let dwarf = session.dwarf_handle.clone().get().await?;

        let mut functions: Vec<FunctionTarget> = Vec::new();
        for pattern in patterns {
            for func in resolve_pattern(&dwarf, pattern, &session.project_root) {
                functions.push(FunctionTarget::from(func));
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
        assert!(parse_event("s", &json!({
            "timestampNs": 1000, "threadId": 1, "eventType": "stdout"
        })).is_none());

        assert!(parse_event("s", &json!({
            "id": "x", "threadId": 1, "eventType": "stdout"
        })).is_none());

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

    // --- HooksReadySignal synchronization tests ---

    #[test]
    fn test_hooks_ready_signal_basic() {
        // Verify that setting a sender and sending on it delivers to the receiver
        let signal: HooksReadySignal = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        // Arm the signal
        {
            let mut guard = signal.lock().unwrap();
            *guard = Some(tx);
        }

        // Simulate agent handler firing hooks_updated
        {
            let mut guard = signal.lock().unwrap();
            if let Some(tx) = guard.take() {
                tx.send(42).unwrap();
            }
        }

        // Worker side receives the count
        let count = rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(count, 42);
    }

    #[test]
    fn test_hooks_ready_signal_cross_thread() {
        // Verify the signal works across threads (emulating handler on Frida thread,
        // worker waiting on its own thread)
        let signal: HooksReadySignal = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        {
            let mut guard = signal.lock().unwrap();
            *guard = Some(tx);
        }

        let signal_clone = signal.clone();
        let handle = std::thread::spawn(move || {
            // Simulate agent handler on a different thread
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut guard = signal_clone.lock().unwrap();
            if let Some(tx) = guard.take() {
                tx.send(9059).unwrap();
            }
        });

        // Worker blocks until signal arrives
        let count = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(count, 9059);
        handle.join().unwrap();
    }

    #[test]
    fn test_hooks_ready_signal_not_armed() {
        // When no sender is set, take() returns None — nothing panics
        let signal: HooksReadySignal = Arc::new(Mutex::new(None));

        let mut guard = signal.lock().unwrap();
        assert!(guard.take().is_none());
    }

    #[test]
    fn test_hooks_ready_signal_only_fires_once() {
        // Verify the signal is one-shot: take() consumes the sender
        let signal: HooksReadySignal = Arc::new(Mutex::new(None));
        let (tx, rx) = std::sync::mpsc::channel();

        {
            let mut guard = signal.lock().unwrap();
            *guard = Some(tx);
        }

        // First take succeeds
        {
            let mut guard = signal.lock().unwrap();
            let sender = guard.take();
            assert!(sender.is_some());
            sender.unwrap().send(100).unwrap();
        }

        // Second take returns None (one-shot consumed)
        {
            let mut guard = signal.lock().unwrap();
            assert!(guard.take().is_none());
        }

        assert_eq!(rx.recv().unwrap(), 100);
    }

    // --- AgentMessageHandler tests ---

    fn make_handler() -> (AgentMessageHandler, mpsc::Receiver<Event>, HooksReadySignal) {
        let (event_tx, event_rx) = mpsc::channel(1000);
        let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
        let handler = AgentMessageHandler {
            event_tx,
            session_id: "test-session".to_string(),
            hooks_ready: hooks_ready.clone(),
        };
        (handler, event_rx, hooks_ready)
    }

    #[test]
    fn test_handler_hooks_updated_signals_worker() {
        let (handler, _event_rx, hooks_ready) = make_handler();

        // Arm the signal (as the worker would before posting hooks)
        let (signal_tx, signal_rx) = std::sync::mpsc::channel();
        {
            let mut guard = hooks_ready.lock().unwrap();
            *guard = Some(signal_tx);
        }

        // Simulate receiving hooks_updated from agent
        let payload = json!({ "type": "hooks_updated", "activeCount": 512 });
        handler.handle_payload("hooks_updated", &payload);

        // Worker should receive the count
        let count = signal_rx.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        assert_eq!(count, 512);
    }

    #[test]
    fn test_handler_hooks_updated_no_signal_armed() {
        let (handler, _event_rx, _hooks_ready) = make_handler();

        // No signal armed — should not panic
        let payload = json!({ "type": "hooks_updated", "activeCount": 100 });
        handler.handle_payload("hooks_updated", &payload);
    }

    #[tokio::test]
    async fn test_handler_events_forwarded() {
        let (handler, mut event_rx, _hooks_ready) = make_handler();

        // Simulate receiving events batch from agent
        let payload = json!({
            "type": "events",
            "events": [
                {
                    "id": "e1",
                    "timestampNs": 1000,
                    "threadId": 1,
                    "eventType": "function_enter",
                    "functionName": "foo::bar",
                    "parentEventId": null,
                },
                {
                    "id": "e2",
                    "timestampNs": 2000,
                    "threadId": 1,
                    "eventType": "stdout",
                    "text": "hello\n",
                }
            ]
        });

        handler.handle_payload("events", &payload);

        // Both events should arrive on the channel
        let ev1 = event_rx.recv().await.unwrap();
        assert_eq!(ev1.id, "e1");
        assert_eq!(ev1.event_type, EventType::FunctionEnter);
        assert_eq!(ev1.function_name, "foo::bar");

        let ev2 = event_rx.recv().await.unwrap();
        assert_eq!(ev2.id, "e2");
        assert_eq!(ev2.event_type, EventType::Stdout);
        assert_eq!(ev2.text.as_deref(), Some("hello\n"));
    }

    // --- @usercode pattern resolution ---

    #[test]
    fn test_usercode_resolves_by_source_file() {
        use crate::dwarf::FunctionInfo;

        // Simulate what the spawner does: for @usercode, use user_code_functions
        // which filters by source_file.starts_with(project_root)
        let project_root = "/home/user/myproject";

        let functions = vec![
            FunctionInfo {
                name: "myproject::main".to_string(),
                name_raw: None,
                low_pc: 0x1000,
                high_pc: 0x1100,
                source_file: Some("/home/user/myproject/src/main.cpp".to_string()),
                line_number: Some(10),
            },
            FunctionInfo {
                name: "std::vector::push_back".to_string(),
                name_raw: None,
                low_pc: 0x2000,
                high_pc: 0x2100,
                source_file: Some("/usr/include/c++/v1/vector".to_string()),
                line_number: Some(500),
            },
            FunctionInfo {
                name: "myproject::util::helper".to_string(),
                name_raw: None,
                low_pc: 0x3000,
                high_pc: 0x3100,
                source_file: Some("/home/user/myproject/src/util.cpp".to_string()),
                line_number: Some(42),
            },
            FunctionInfo {
                name: "no_source_func".to_string(),
                name_raw: None,
                low_pc: 0x4000,
                high_pc: 0x4100,
                source_file: None,
                line_number: None,
            },
        ];

        // Filter like user_code_functions does
        let user_code: Vec<&FunctionInfo> = functions
            .iter()
            .filter(|f| f.is_user_code(project_root))
            .collect();

        assert_eq!(user_code.len(), 2);
        assert_eq!(user_code[0].name, "myproject::main");
        assert_eq!(user_code[1].name, "myproject::util::helper");
    }

    #[test]
    fn test_function_target_from_function_info() {
        use crate::dwarf::FunctionInfo;

        let info = FunctionInfo {
            name: "foo::bar".to_string(),
            name_raw: Some("_ZN3foo3barEv".to_string()),
            low_pc: 0x10000b04c,
            high_pc: 0x10000b100,
            source_file: Some("/src/foo.cpp".to_string()),
            line_number: Some(42),
        };

        let target = FunctionTarget::from(&info);
        assert_eq!(target.address, 0x10000b04c);
        assert_eq!(target.name, "foo::bar");
        assert_eq!(target.name_raw.as_deref(), Some("_ZN3foo3barEv"));
        assert_eq!(target.source_file.as_deref(), Some("/src/foo.cpp"));
        assert_eq!(target.line_number, Some(42));
    }
}
