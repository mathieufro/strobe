use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use tokio::sync::{mpsc, oneshot};
use crate::db::{Event, EventType};
use crate::dwarf::{DwarfParser, DwarfHandle, FunctionInfo};
use crate::Result;
use super::{HookManager, HookMode};
use libc;

/// Check a GLib error pointer. Returns Ok(()) if null, or the error message if set.
/// Frees the GError after extracting the message.
unsafe fn check_gerror(error: *mut frida_sys::GError) -> std::result::Result<(), String> {
    if error.is_null() {
        return Ok(());
    }
    let msg = CStr::from_ptr((*error).message)
        .to_str()
        .unwrap_or("unknown error")
        .to_string();
    frida_sys::g_error_free(error);
    Err(msg)
}

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
    tracing::debug!("raw_on_message [{}]: envelope_type={}", handler.session_id, msg_type);

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
    if opt.is_null() {
        return Err("Failed to create script options".to_string());
    }
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

    check_gerror(error)?;

    if script_ptr.is_null() {
        return Err("script_ptr is null".to_string());
    }

    Ok(script_ptr)
}

/// C callback to free the AgentMessageHandler when the signal is disconnected.
unsafe extern "C" fn destroy_handler(data: *mut c_void, _closure: *mut frida_sys::_GClosure) {
    if !data.is_null() {
        let _ = Box::from_raw(data as *mut AgentMessageHandler);
    }
}

/// Register message handler on a raw script pointer.
/// The handler is freed by `destroy_handler` when the signal is disconnected.
unsafe fn register_handler_raw(
    script_ptr: *mut frida_sys::_FridaScript,
    handler: AgentMessageHandler,
) {
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
        Some(destroy_handler),
        0,
    );
}

/// Load a raw script.
unsafe fn load_script_raw(
    script_ptr: *mut frida_sys::_FridaScript,
) -> std::result::Result<(), String> {
    let mut error: *mut frida_sys::GError = std::ptr::null_mut();
    frida_sys::frida_script_load_sync(script_ptr, std::ptr::null_mut(), &mut error);
    check_gerror(error)
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
        event_type,
        text: Some(text),
        pid: Some(ctx.pid),
        ..Event::default()
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
type ReadResponseSignal = Arc<Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>>;

/// Signal the worker that hooks or watches are ready.
fn signal_ready(hooks_ready: &HooksReadySignal, label: &str, session_id: &str, payload: &serde_json::Value) {
    let count = payload.get("activeCount").and_then(|v| v.as_u64()).unwrap_or(0);
    tracing::info!("{} for session {}: {} active", label, session_id, count);
    if let Ok(mut guard) = hooks_ready.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(count);
        }
    }
}

/// Pause notification sent from agent message handler to daemon
#[derive(Debug, Clone)]
pub struct PauseNotification {
    pub session_id: String,
    pub thread_id: u64,
    pub breakpoint_id: String,
    pub hits: u32,
    pub func_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub return_address: Option<u64>,
    /// Runtime address where the thread paused (for one-shot step BPs)
    pub address: Option<u64>,
    pub backtrace: Vec<crate::mcp::BacktraceFrame>,
    pub arguments: Vec<crate::mcp::CapturedArg>,
}

/// Channel for pause notifications from agent to daemon
pub type PauseNotifyTx = mpsc::Sender<PauseNotification>;

/// Message handler passed as user_data to the raw GLib signal callback.
/// No longer implements `ScriptHandler` — messages are parsed directly in `raw_on_message`.
type WriteResponseSignal = Arc<Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>>;

struct AgentMessageHandler {
    event_tx: mpsc::Sender<Event>,
    session_id: String,
    hooks_ready: HooksReadySignal,
    read_response: ReadResponseSignal,
    write_response: WriteResponseSignal,
    crash_reported: Arc<AtomicBool>,
    pause_notify_tx: Option<PauseNotifyTx>,
    /// Wall-clock epoch nanos at process start, subtracted from event timestamps
    /// to produce process-relative timestamps consistent with trace events.
    start_ns: i64,
}

impl AgentMessageHandler {
    fn handle_payload(&self, msg_type: &str, payload: &serde_json::Value) {
        tracing::debug!("Agent message [{}]: type={}", self.session_id, msg_type);
        match msg_type {
            "events" => {
                if let Some(events) = payload.get("events").and_then(|v| v.as_array()) {
                    tracing::debug!("Received {} events from agent [{}]", events.len(), self.session_id);
                    for event_json in events {
                        if let Some(event) = parse_event(&self.session_id, event_json) {
                            if event.event_type == EventType::Crash {
                                self.crash_reported.store(true, Ordering::Release);
                                tracing::info!("Crash event received from agent [{}]", self.session_id);
                            }
                            let _ = self.event_tx.try_send(event);
                        }
                    }
                }
            }
            "initialized" => {
                tracing::info!("Agent initialized for session {}", self.session_id);
            }
            "hooks_updated" => {
                signal_ready(&self.hooks_ready, "Hooks updated", &self.session_id, payload);
            }
            "watches_updated" => {
                signal_ready(&self.hooks_ready, "Watches updated", &self.session_id, payload);
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
            "sampling_state_change" => {
                let func_name = payload.get("funcName").and_then(|v| v.as_str()).unwrap_or("unknown");
                let enabled = payload.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
                let sample_rate = payload.get("sampleRate").and_then(|v| v.as_f64()).unwrap_or(1.0);
                let rate_pct = (sample_rate * 100.0) as u32;

                if enabled {
                    tracing::warn!(
                        "[{}] Hot function detected: '{}' - auto-sampling at {}%",
                        self.session_id, func_name, rate_pct
                    );
                } else {
                    tracing::info!(
                        "[{}] Function cooled down: '{}' - full capture resumed",
                        self.session_id, func_name
                    );
                }
            }
            "sampling_stats" => {
                if let Some(stats) = payload.get("stats").and_then(|v| v.as_array()) {
                    let sampling_count = stats.iter().filter(|s| {
                        s.get("samplingEnabled").and_then(|v| v.as_bool()).unwrap_or(false)
                    }).count();
                    tracing::debug!("[{}] Sampling stats: {} functions being sampled", self.session_id, sampling_count);
                }
            }
            "read_response" => {
                if let Ok(mut guard) = self.read_response.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(payload.clone());
                    }
                }
            }
            "write_response" => {
                if let Ok(mut guard) = self.write_response.lock() {
                    if let Some(tx) = guard.take() {
                        let _ = tx.send(payload.clone());
                    }
                }
            }
            "poll_complete" => {
                tracing::info!("Poll complete for session {}", self.session_id);
            }
            "paused" => {
                // Phase 2: Breakpoint pause event
                let thread_id = payload.get("threadId").and_then(|v| v.as_u64()).unwrap_or(0);
                let breakpoint_id_str = payload.get("breakpointId").and_then(|v| v.as_str()).unwrap_or("unknown");
                let hits = payload.get("hits").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let func_name = payload.get("funcName").and_then(|v| v.as_str()).map(|s| s.to_string());
                let file = payload.get("file").and_then(|v| v.as_str()).map(|s| s.to_string());
                let line = payload.get("line").and_then(|v| v.as_u64()).map(|n| n as u32);
                let return_address = payload.get("returnAddress")
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());
                let address = payload.get("address")
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());

                let backtrace: Vec<crate::mcp::BacktraceFrame> = payload
                    .get("backtrace")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().filter_map(|frame| {
                            Some(crate::mcp::BacktraceFrame {
                                address: frame.get("address")?.as_str()?.to_string(),
                                module_name: frame.get("moduleName").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                function_name: frame.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                file: frame.get("fileName").and_then(|v| v.as_str()).map(|s| s.to_string()),
                                line: frame.get("lineNumber").and_then(|v| v.as_u64()).map(|n| n as u32),
                            })
                        }).collect()
                    })
                    .unwrap_or_default();

                let arguments: Vec<crate::mcp::CapturedArg> = payload
                    .get("arguments")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter().filter_map(|arg| {
                            Some(crate::mcp::CapturedArg {
                                index: arg.get("index")?.as_u64()? as u32,
                                value: arg.get("value")?.as_str()?.to_string(),
                            })
                        }).collect()
                    })
                    .unwrap_or_default();

                tracing::info!(
                    "[{}] Thread {} paused at breakpoint {} (addr=0x{:x?}, ret=0x{:x?})",
                    self.session_id, thread_id, breakpoint_id_str,
                    address.unwrap_or(0), return_address.unwrap_or(0)
                );

                // Create a Pause event for the database (process-relative timestamp)
                let timestamp_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as i64
                    - self.start_ns;

                let event = Event {
                    id: format!("{}-pause-{}-{}", self.session_id, thread_id, timestamp_ns),
                    session_id: self.session_id.clone(),
                    timestamp_ns,
                    thread_id: thread_id as i64,
                    event_type: EventType::Pause,
                    breakpoint_id: Some(breakpoint_id_str.to_string()),
                    function_name: func_name.clone().unwrap_or_default(),
                    source_file: file.clone(),
                    line_number: line.map(|n| n as i32),
                    ..Event::default()
                };

                let _ = self.event_tx.try_send(event);

                // Notify SessionManager of the pause (critical for stepping/continue)
                if let Some(ref tx) = self.pause_notify_tx {
                    let notification = PauseNotification {
                        session_id: self.session_id.clone(),
                        thread_id,
                        breakpoint_id: breakpoint_id_str.to_string(),
                        hits,
                        func_name,
                        file,
                        line,
                        return_address,
                        address,
                        backtrace,
                        arguments,
                    };
                    if let Err(e) = tx.try_send(notification) {
                        tracing::warn!(
                            "[{}] Failed to send pause notification for thread {}: {} (thread may remain blocked)",
                            self.session_id, thread_id, e
                        );
                    }
                }
            }
            "breakpointSet" | "logpointSet" => {
                let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                tracing::info!("[{}] {} confirmed: {}", self.session_id, msg_type, id);
                // Signal the hooks_ready channel so set_breakpoint_async can unblock
                signal_ready(&self.hooks_ready, msg_type, &self.session_id, payload);
            }
            "breakpointRemoved" | "logpointRemoved" => {
                let id = payload.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
                tracing::info!("[{}] {} confirmed: {}", self.session_id, msg_type, id);
                signal_ready(&self.hooks_ready, msg_type, &self.session_id, payload);
            }
            "conditionError" => {
                let bp_id = payload.get("breakpointId").and_then(|v| v.as_str()).unwrap_or("unknown");
                let error = payload.get("error").and_then(|v| v.as_str()).unwrap_or("unknown error");
                tracing::warn!("[{}] Condition error on breakpoint {}: {}", self.session_id, bp_id, error);

                // Store as event for queryability (process-relative timestamp)
                let timestamp_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as i64
                    - self.start_ns;
                let event = Event {
                    id: format!("{}-cond-err-{}", self.session_id, timestamp_ns),
                    session_id: self.session_id.clone(),
                    timestamp_ns,
                    event_type: EventType::ConditionError,
                    breakpoint_id: Some(bp_id.to_string()),
                    function_name: error.to_string(),
                    ..Event::default()
                };
                let _ = self.event_tx.try_send(event);
            }
            _ => {
                tracing::debug!("Unknown message type from agent: {}", msg_type);
            }
        }
    }
}

/// Result of a hook installation attempt
pub struct HookResult {
    pub installed: u32,
    pub matched: u32,
    pub warnings: Vec<String>,
}

/// Safety limits for hook installation.
/// Empirically determined on ARM64 with 79MB binary:
///   ~50 hooks: fast install (~5s), rock solid
///   ~100 hooks: install ~10s, stable
///   ~150+ hooks: crash risk with hot functions
const MAX_HOOKS_PER_CALL: usize = 100;
const CHUNK_SIZE: usize = 50;
const TIMEOUT_PER_CHUNK_SECS: u64 = 45;

/// Wrapper to move raw script pointer across threads.
/// Safety: each session's script is only accessed by its dedicated worker thread.
struct SendScriptPtr(*mut frida_sys::_FridaScript);
unsafe impl Send for SendScriptPtr {}

/// Result returned by coordinator after spawning a process.
struct SpawnResult {
    pid: u32,
    script_ptr: SendScriptPtr,
    hooks_ready: HooksReadySignal,
    read_response: ReadResponseSignal,
    write_response: WriteResponseSignal,
}

/// Commands for the coordinator thread (device-level operations).
enum CoordinatorCommand {
    Spawn {
        session_id: String,
        command: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        event_tx: mpsc::Sender<Event>,
        defer_resume: bool,
        pause_notify_tx: Option<PauseNotifyTx>,
        response: oneshot::Sender<Result<SpawnResult>>,
    },
    Resume {
        pid: u32,
        response: oneshot::Sender<Result<()>>,
    },
    StopSession {
        session_id: String,
        response: oneshot::Sender<Result<()>>,
    },
}

/// Commands for per-session worker threads (script-level operations).
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
        response: oneshot::Sender<Result<u32>>,
    },
    SetWatches {
        watches: Vec<WatchTarget>,
        expr_watches: Vec<ExprWatchTarget>,
        response: oneshot::Sender<Result<()>>,
    },
    ReadMemory {
        recipes_json: String,
        response: oneshot::Sender<Result<serde_json::Value>>,
    },
    WriteMemory {
        recipes_json: String,
        response: oneshot::Sender<Result<serde_json::Value>>,
    },
    SetBreakpoint {
        message: serde_json::Value,
        response: oneshot::Sender<Result<()>>,
    },
    RemoveBreakpoint {
        breakpoint_id: String,
        response: oneshot::Sender<Result<()>>,
    },
    RemoveLogpoint {
        logpoint_id: String,
        response: oneshot::Sender<Result<()>>,
    },
    ResumeThread {
        thread_id: u64,
        /// Each address: (addr, no_slide). no_slide=true for runtime addresses (e.g., return address).
        one_shot_addresses: Vec<(u64, bool)>,
        image_base: u64,
        /// Original return address to carry forward during stepping.
        /// Step hooks can't reliably capture return addresses (Frida trampoline on stack).
        return_address: Option<u64>,
        response: oneshot::Sender<Result<()>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct WatchTarget {
    pub label: String,
    pub address: u64,
    pub size: u8,
    pub type_kind_str: String,
    pub deref_depth: u8,
    pub deref_offset: u64,
    pub type_name: Option<String>,
    pub on_patterns: Option<Vec<String>>,
    /// If true, address is already absolute (user-provided) — don't apply ASLR slide.
    pub no_slide: bool,
}

#[derive(Clone)]
pub struct ExprWatchTarget {
    pub label: String,
    pub expr: String,
    pub is_global: bool,
    pub on_patterns: Option<Vec<String>>,
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

/// Raw C callback for Frida's Device "spawn-added" signal.
/// Notifies the worker loop about new child processes spawned via fork/exec.
unsafe extern "C" fn raw_on_spawn_added(
    _device: *mut frida_sys::_FridaDevice,
    spawn: *mut frida_sys::_FridaSpawn,
    user_data: *mut c_void,
) {
    let tx = &*(user_data as *const std::sync::mpsc::Sender<u32>);
    let child_pid = frida_sys::frida_spawn_get_pid(spawn);
    tracing::info!("Spawn signal: child PID {}", child_pid);
    let _ = tx.send(child_pid);
}

/// C callback to free the spawn_tx Sender when the signal is disconnected.
unsafe extern "C" fn destroy_spawn_tx(data: *mut c_void, _closure: *mut frida_sys::_GClosure) {
    if !data.is_null() {
        let _ = Box::from_raw(data as *mut std::sync::mpsc::Sender<u32>);
    }
}

/// Coordinator thread: handles device-level operations (spawn, kill, child processes).
/// Per-session script operations are delegated to dedicated session_worker threads.
fn coordinator_worker(cmd_rx: std::sync::mpsc::Receiver<CoordinatorCommand>) {
    use frida::{Frida, DeviceManager, DeviceType, SpawnOptions, SpawnStdio};

    let frida = unsafe { Frida::obtain() };
    let device_manager = DeviceManager::obtain(&frida);

    let devices = device_manager.enumerate_all_devices();
    let mut device = match devices.into_iter().find(|d| d.get_type() == DeviceType::Local) {
        Some(d) => d,
        None => {
            tracing::error!("No local Frida device found");
            return;
        }
    };

    // Set up Device "output" signal handler for stdout/stderr capture.
    let output_registry: OutputRegistry = Arc::new(Mutex::new(HashMap::new()));
    unsafe {
        let device_ptr = device_raw_ptr(&device);
        let signal_name = CString::new("output").unwrap();
        let callback = Some(std::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(),
        >(raw_on_output as *mut c_void));
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

    // Enable spawn gating to intercept child processes (fork/exec)
    let (spawn_tx, spawn_rx) = std::sync::mpsc::channel::<u32>();
    unsafe {
        let device_ptr = device_raw_ptr(&device);
        let mut error: *mut frida_sys::GError = std::ptr::null_mut();
        frida_sys::frida_device_enable_spawn_gating_sync(
            device_ptr,
            std::ptr::null_mut(),
            &mut error,
        );
        if let Err(msg) = check_gerror(error) {
            tracing::warn!("Failed to enable spawn gating: {}", msg);
        } else {
            tracing::info!("Spawn gating enabled — will intercept child processes");

            let signal_name = CString::new("spawn-added").unwrap();
            let tx_ptr = Box::into_raw(Box::new(spawn_tx.clone()));
            let callback = Some(std::mem::transmute::<
                *mut c_void,
                unsafe extern "C" fn(),
            >(raw_on_spawn_added as *mut c_void));
            frida_sys::g_signal_connect_data(
                device_ptr as *mut _,
                signal_name.as_ptr(),
                callback,
                tx_ptr as *mut c_void,
                Some(destroy_spawn_tx),
                0,
            );
        }
    }

    // Track raw Frida session pointers per PID for proper cleanup.
    // Sessions are created via device.attach() and mem::forget'd to extract raw ptrs.
    // We must explicitly detach + unref them during StopSession.
    let mut session_ptrs: HashMap<u32, *mut frida_sys::_FridaSession> = HashMap::new();

    loop {
        // Check for spawn notifications (non-blocking)
        while let Ok(child_pid) = spawn_rx.try_recv() {
            handle_child_spawn(&mut device, child_pid, &output_registry, &mut session_ptrs);
        }

        // Wait for commands with timeout so we periodically check for spawns
        let cmd = match cmd_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(cmd) => cmd,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };

        match cmd {
            CoordinatorCommand::Resume { pid, response } => {
                let result = device.resume(pid)
                    .map_err(|e| crate::Error::FridaAttachFailed(format!("Resume failed: {}", e)));
                let _ = response.send(result);
            }
            CoordinatorCommand::Spawn {
                session_id,
                command,
                args,
                cwd,
                env,
                event_tx,
                defer_resume,
                pause_notify_tx,
                response,
            } => {
                let result = (|| -> Result<SpawnResult> {
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
                        // Merge user-provided env vars with the parent environment.
                        // envp() replaces the entire environment, so we must include
                        // all existing vars plus user overrides.
                        let mut merged: std::collections::HashMap<String, String> = std::env::vars().collect();
                        for (k, v) in env_vars.iter() {
                            merged.insert(k.clone(), v.clone());
                        }
                        let env_tuples: Vec<(&str, &str)> = merged
                            .iter()
                            .map(|(k, v)| (k.as_str(), v.as_str()))
                            .collect();
                        spawn_opts = spawn_opts.envp(env_tuples);
                    }

                    let t = std::time::Instant::now();
                    let max_attempts = 5u32;
                    let pid = {
                        let mut last_err = String::new();
                        let mut spawned_pid = None;
                        for attempt in 0..max_attempts {
                            match device.spawn(&command, &spawn_opts) {
                                Ok(p) => { spawned_pid = Some(p); break; }
                                Err(e) => {
                                    last_err = format!("{}", e);
                                    if attempt + 1 < max_attempts {
                                        let delay = 100 * (1u64 << attempt); // 100, 200, 400, 800ms
                                        tracing::warn!("Spawn attempt {}/{} failed: {}. Retrying in {}ms...", attempt + 1, max_attempts, e, delay);
                                        thread::sleep(std::time::Duration::from_millis(delay));
                                    }
                                }
                            }
                        }
                        spawned_pid.ok_or_else(|| crate::Error::FridaAttachFailed(format!("Spawn failed after {} attempts: {}", max_attempts, last_err)))?
                    };
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

                    // Small delay after spawn to let macOS kernel fully register the process.
                    // Frida's device.attach() can transiently fail without this.
                    thread::sleep(std::time::Duration::from_millis(50));

                    let t = std::time::Instant::now();
                    let frida_session = {
                        let mut last_err = String::new();
                        let mut attached = None;
                        for attempt in 0..max_attempts {
                            match device.attach(pid) {
                                Ok(s) => { attached = Some(s); break; }
                                Err(e) => {
                                    last_err = format!("{}", e);
                                    if attempt + 1 < max_attempts {
                                        let delay = 100 * (1u64 << attempt); // 100, 200, 400, 800ms
                                        tracing::warn!("Attach attempt {}/{} to PID {} failed: {}. Retrying in {}ms...", attempt + 1, max_attempts, pid, e, delay);
                                        thread::sleep(std::time::Duration::from_millis(delay));
                                    }
                                }
                            }
                        }
                        attached.ok_or_else(|| {
                            tracing::error!("Attach to PID {} failed after {} attempts: {}", pid, max_attempts, last_err);
                            crate::Error::FridaAttachFailed(format!("Attach to PID {} failed after {} attempts: {}", pid, max_attempts, last_err))
                        })?
                    };
                    tracing::debug!("PERF: device.attach() took {:?}", t.elapsed());

                    let raw_session = unsafe { session_raw_ptr(&frida_session) };
                    std::mem::forget(frida_session);
                    session_ptrs.insert(pid, raw_session);

                    let t = std::time::Instant::now();
                    let script_ptr = unsafe {
                        create_script_raw(raw_session, AGENT_CODE)
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Script creation failed: {}", e)))?
                    };
                    tracing::debug!("PERF: create_script took {:?}", t.elapsed());

                    let t = std::time::Instant::now();

                    let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
                    let read_response: ReadResponseSignal = Arc::new(Mutex::new(None));
                    let write_response: WriteResponseSignal = Arc::new(Mutex::new(None));
                    let crash_reported = Arc::new(AtomicBool::new(false));

                    let handler = AgentMessageHandler {
                        event_tx: event_tx.clone(),
                        session_id: session_id.clone(),
                        hooks_ready: hooks_ready.clone(),
                        read_response: read_response.clone(),
                        write_response: write_response.clone(),
                        crash_reported: crash_reported.clone(),
                        pause_notify_tx,
                        start_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as i64,
                    };

                    unsafe { register_handler_raw(script_ptr, handler) };

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

                    // Resume process unless caller deferred it (e.g., to install hooks first)
                    if !defer_resume {
                        let t = std::time::Instant::now();
                        device.resume(pid)
                            .map_err(|e| crate::Error::FridaAttachFailed(format!("Resume failed: {}", e)))?;
                        tracing::debug!("PERF: device.resume() took {:?}", t.elapsed());
                    } else {
                        tracing::info!("Process {} spawned with deferred resume", pid);
                    }
                    tracing::debug!("PERF: Total coordinator spawn took {:?}", spawn_start.elapsed());

                    // Start process death monitor for crash detection fallback
                    let monitor_pid = pid;
                    let monitor_session_id = session_id.clone();
                    let monitor_event_tx = event_tx.clone();
                    let monitor_crash_reported = crash_reported;
                    let monitor_start_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos() as i64;

                    thread::spawn(move || {
                        process_death_monitor(
                            monitor_pid,
                            monitor_session_id,
                            monitor_event_tx,
                            monitor_crash_reported,
                            monitor_start_ns,
                        );
                    });

                    Ok(SpawnResult {
                        pid,
                        script_ptr: SendScriptPtr(script_ptr),
                        hooks_ready,
                        read_response,
                        write_response,
                    })
                })();

                let _ = response.send(result);
            }

            CoordinatorCommand::StopSession {
                session_id,
                response,
            } => {
                // Collect PIDs and release the lock BEFORE killing.
                // CRITICAL: device.kill() is a sync Frida call that waits for the GLib
                // main loop to complete the operation. raw_on_output also runs on the
                // GLib thread and needs the output_registry lock. Holding the lock while
                // calling device.kill() causes a deadlock:
                //   coordinator holds lock → device.kill() waits for GLib →
                //   GLib blocked on lock in raw_on_output → deadlock
                let pids_to_remove: Vec<u32> = if let Ok(mut reg) = output_registry.lock() {
                    let pids: Vec<u32> = reg.iter()
                        .filter(|(_, ctx)| ctx.session_id == session_id)
                        .map(|(&pid, _)| pid)
                        .collect();
                    for pid in &pids {
                        reg.remove(pid);
                    }
                    pids
                    // Lock released here
                } else {
                    vec![]
                };

                // Kill process trees FIRST with SIGKILL, THEN clean up Frida state.
                // Order matters: detaching while a process is paused at a breakpoint
                // can hang because Frida tries to restore the original code in the
                // stopped process. SIGKILL always works.
                for pid in &pids_to_remove {
                    tracing::info!("Killing process tree for PID {} (session {})", pid, session_id);
                    crate::test::stacks::kill_process_tree(*pid);
                }

                // Frida-level kill to update device's internal bookkeeping
                for pid in &pids_to_remove {
                    device.kill(*pid)
                        .unwrap_or_else(|e| tracing::debug!("Frida cleanup PID {}: {:?}", pid, e));
                }

                // Release Frida session objects. The process is already dead, so we
                // just need to unref the GObject to prevent resource leaks. This is
                // equivalent to what frida::Session's Drop impl does (frida_unref).
                // Without this, forgotten sessions accumulate and exhaust Frida state.
                for pid in pids_to_remove {
                    if let Some(session_ptr) = session_ptrs.remove(&pid) {
                        unsafe {
                            frida_sys::frida_unref(session_ptr as *mut c_void);
                        }
                    }
                }
                let _ = response.send(Ok(()));
            }
        }
    }
}

/// Per-session worker thread: handles script-level operations that may block.
/// Each session gets its own thread so blocking waits (hooks_ready) don't stall other sessions.
fn session_worker(
    session_id: String,
    script_ptr: SendScriptPtr,
    hooks_ready: HooksReadySignal,
    read_response: ReadResponseSignal,
    write_response: WriteResponseSignal,
    pid: u32,
    cmd_rx: std::sync::mpsc::Receiver<SessionCommand>,
) {
    let raw_ptr = script_ptr.0;

    loop {
        let cmd = match cmd_rx.recv() {
            Ok(cmd) => cmd,
            Err(_) => break, // Channel closed
        };

        match cmd {
            SessionCommand::AddPatterns {
                functions,
                image_base,
                mode,
                serialization_depth,
                response,
            } => {
                let result = handle_add_patterns(raw_ptr, &hooks_ready, &session_id, &functions, image_base, mode, serialization_depth);
                let _ = response.send(result);
            }

            SessionCommand::RemovePatterns { functions, response } => {
                let result = handle_remove_patterns(raw_ptr, &hooks_ready, &functions);
                let _ = response.send(result);
            }

            SessionCommand::SetWatches { watches, expr_watches, response } => {
                let result = handle_set_watches(raw_ptr, &hooks_ready, &session_id, pid, &watches, &expr_watches);
                let _ = response.send(result);
            }

            SessionCommand::ReadMemory { recipes_json, response } => {
                let result = handle_read_memory(raw_ptr, &read_response, &recipes_json, pid);
                let _ = response.send(result);
            }

            SessionCommand::WriteMemory { recipes_json, response } => {
                let result = handle_write_memory(raw_ptr, &write_response, &recipes_json, pid);
                let _ = response.send(result);
            }

            SessionCommand::SetBreakpoint { message, response } => {
                // Arm the hooks_ready signal to wait for agent confirmation
                let (signal_tx, signal_rx) = std::sync::mpsc::channel();
                {
                    let mut guard = hooks_ready.lock().unwrap();
                    *guard = Some(signal_tx);
                }

                let post_result = unsafe {
                    post_message_raw(raw_ptr, &serde_json::to_string(&message).unwrap())
                        .map_err(|e| crate::Error::Frida(format!("Failed to send breakpoint: {}", e)))
                };

                let result = match post_result {
                    Ok(()) => {
                        // Wait for breakpointSet/logpointSet confirmation from agent
                        match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                            Ok(_) => Ok(()),
                            Err(_) => {
                                tracing::warn!("Timed out waiting for breakpoint confirmation (5s)");
                                // Don't fail — the breakpoint may still work, just wasn't confirmed
                                Ok(())
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = response.send(result);
            }

            SessionCommand::RemoveBreakpoint { breakpoint_id, response } => {
                let (signal_tx, signal_rx) = std::sync::mpsc::channel();
                {
                    let mut guard = hooks_ready.lock().unwrap();
                    *guard = Some(signal_tx);
                }

                let msg = serde_json::json!({
                    "type": "removeBreakpoint",
                    "id": breakpoint_id,
                });
                let post_result = unsafe {
                    post_message_raw(raw_ptr, &serde_json::to_string(&msg).unwrap())
                        .map_err(|e| crate::Error::Frida(format!("Failed to send removeBreakpoint: {}", e)))
                };

                let result = match post_result {
                    Ok(()) => {
                        match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                            Ok(_) => Ok(()),
                            Err(_) => {
                                tracing::warn!("Timed out waiting for breakpoint removal confirmation (5s)");
                                Ok(())
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = response.send(result);
            }

            SessionCommand::RemoveLogpoint { logpoint_id, response } => {
                let (signal_tx, signal_rx) = std::sync::mpsc::channel();
                {
                    let mut guard = hooks_ready.lock().unwrap();
                    *guard = Some(signal_tx);
                }

                let msg = serde_json::json!({
                    "type": "removeLogpoint",
                    "id": logpoint_id,
                });
                let post_result = unsafe {
                    post_message_raw(raw_ptr, &serde_json::to_string(&msg).unwrap())
                        .map_err(|e| crate::Error::Frida(format!("Failed to send removeLogpoint: {}", e)))
                };

                let result = match post_result {
                    Ok(()) => {
                        match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                            Ok(_) => Ok(()),
                            Err(_) => {
                                tracing::warn!("Timed out waiting for logpoint removal confirmation (5s)");
                                Ok(())
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
                let _ = response.send(result);
            }

            SessionCommand::ResumeThread { thread_id, one_shot_addresses, image_base, return_address, response } => {
                // Two-message protocol for stepping:
                // 1. Send installStepHooks (if any) — handled at top-level agent context
                // 2. Send resume — unblocks the paused thread
                // This ordering is guaranteed: Frida processes messages sequentially,
                // so hooks are installed before the thread resumes.
                if !one_shot_addresses.is_empty() {
                    let install_msg = serde_json::json!({
                        "type": "installStepHooks",
                        "threadId": thread_id,
                        "oneShot": one_shot_addresses.iter().map(|(addr, no_slide)| {
                            serde_json::json!({
                                "address": format!("0x{:x}", addr),
                                "noSlide": no_slide,
                            })
                        }).collect::<Vec<_>>(),
                        "imageBase": format!("0x{:x}", image_base),
                        "returnAddress": return_address.map(|a| format!("0x{:x}", a)),
                    });
                    let install_result = unsafe {
                        post_message_raw(raw_ptr, &serde_json::to_string(&install_msg).unwrap())
                    };
                    if let Err(e) = install_result {
                        let _ = response.send(Err(crate::Error::Frida(
                            format!("Failed to send installStepHooks: {}", e)
                        )));
                        continue;
                    }
                }

                let resume_msg = serde_json::json!({
                    "type": format!("resume-{}", thread_id),
                });
                let result = unsafe {
                    post_message_raw(raw_ptr, &serde_json::to_string(&resume_msg).unwrap())
                        .map_err(|e| crate::Error::Frida(format!("Failed to send resume: {}", e)))
                };
                let _ = response.send(result);
            }

            SessionCommand::Shutdown => {
                tracing::info!("Session worker {} shutting down", session_id);
                // Unload and unref the script to prevent memory leaks
                unsafe {
                    let mut error: *mut frida_sys::GError = std::ptr::null_mut();
                    frida_sys::frida_script_unload_sync(
                        raw_ptr,
                        std::ptr::null_mut(),
                        &mut error,
                    );
                    let _ = check_gerror(error);
                    frida_sys::frida_unref(raw_ptr as *mut c_void);
                }
                break;
            }
        }
    }
}

/// Handle AddPatterns on a session worker thread.
fn handle_add_patterns(
    script_ptr: *mut frida_sys::_FridaScript,
    hooks_ready: &HooksReadySignal,
    session_id: &str,
    functions: &[FunctionTarget],
    image_base: u64,
    mode: HookMode,
    serialization_depth: Option<u32>,
) -> Result<u32> {
    tracing::info!("AddPatterns: {} functions ({:?} mode) for session {}", functions.len(), mode, session_id);

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

    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = hooks_ready.lock().unwrap();
        *guard = Some(signal_tx);
    }

    let mode_str = match mode {
        HookMode::Full => "full",
        HookMode::Light => "light",
    };

    let mut hooks_msg = serde_json::json!({
        "type": "hooks",
        "action": "add",
        "functions": func_list,
        "imageBase": format!("0x{:x}", image_base),
        "mode": mode_str,
    });
    if let Some(depth) = serialization_depth {
        hooks_msg["serializationDepth"] = serde_json::json!(depth);
    }

    unsafe {
        post_message_raw(script_ptr, &serde_json::to_string(&hooks_msg).unwrap())
            .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;
    }

    match signal_rx.recv_timeout(std::time::Duration::from_secs(TIMEOUT_PER_CHUNK_SECS)) {
        Ok(count) => {
            tracing::info!("Agent confirmed {} hooks active after add", count);
            Ok(count as u32)
        }
        Err(_) => {
            tracing::warn!("Timed out waiting for hooks confirmation ({}s)", TIMEOUT_PER_CHUNK_SECS);
            Err(crate::Error::Frida(
                format!("Agent did not respond within {}s — hooks may not be installed", TIMEOUT_PER_CHUNK_SECS)
            ))
        }
    }
}

/// Handle RemovePatterns on a session worker thread.
/// Returns the number of hooks still active after removal.
fn handle_remove_patterns(
    script_ptr: *mut frida_sys::_FridaScript,
    hooks_ready: &HooksReadySignal,
    functions: &[FunctionTarget],
) -> Result<u32> {
    let func_list: Vec<serde_json::Value> = functions.iter().map(|f| {
        serde_json::json!({
            "address": format!("0x{:x}", f.address),
        })
    }).collect();

    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = hooks_ready.lock().unwrap();
        *guard = Some(signal_tx);
    }

    let hooks_msg = serde_json::json!({
        "type": "hooks",
        "action": "remove",
        "functions": func_list,
    });

    unsafe {
        post_message_raw(script_ptr, &serde_json::to_string(&hooks_msg).unwrap())
            .map_err(|e| crate::Error::Frida(format!("Failed to send hooks: {}", e)))?;
    }

    match signal_rx.recv_timeout(std::time::Duration::from_secs(TIMEOUT_PER_CHUNK_SECS)) {
        Ok(count) => {
            tracing::info!("Agent confirmed {} hooks active after remove", count);
            Ok(count as u32)
        }
        Err(_) => {
            tracing::warn!("Timed out waiting for remove confirmation ({}s)", TIMEOUT_PER_CHUNK_SECS);
            Ok(0)
        }
    }
}

/// Handle SetWatches on a session worker thread.
fn handle_set_watches(
    script_ptr: *mut frida_sys::_FridaScript,
    hooks_ready: &HooksReadySignal,
    session_id: &str,
    pid: u32,
    watches: &[WatchTarget],
    expr_watches: &[ExprWatchTarget],
) -> Result<()> {
    let is_alive = unsafe { libc::kill(pid as i32, 0) == 0 };
    if !is_alive {
        return Err(crate::Error::WatchFailed(
            format!("Process {} is no longer running", pid)
        ));
    }

    tracing::info!(
        "SetWatches for session {}: {} native + {} expr watches, PID {} alive",
        session_id, watches.len(), expr_watches.len(), pid
    );

    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = hooks_ready.lock().unwrap();
        *guard = Some(signal_tx);
    }

    let watch_list: Vec<serde_json::Value> = watches.iter().map(|w| {
        let mut obj = serde_json::json!({
            "label": w.label,
            "address": format!("0x{:x}", w.address),
            "size": w.size,
            "typeKind": w.type_kind_str,
            "derefDepth": w.deref_depth,
            "derefOffset": w.deref_offset,
            "typeName": w.type_name,
            "onPatterns": w.on_patterns,
        });
        if w.no_slide {
            obj["noSlide"] = serde_json::json!(true);
        }
        obj
    }).collect();

    let expr_watch_list: Vec<serde_json::Value> = expr_watches.iter().map(|e| {
        serde_json::json!({
            "label": e.label,
            "expr": e.expr,
            "isGlobal": e.is_global,
            "onPatterns": e.on_patterns,
        })
    }).collect();

    let mut watches_msg = serde_json::json!({
        "type": "watches",
        "watches": watch_list,
    });
    if !expr_watch_list.is_empty() {
        watches_msg["exprWatches"] = serde_json::json!(expr_watch_list);
    }

    tracing::debug!("Posting watches message to agent");
    unsafe {
        post_message_raw(script_ptr, &serde_json::to_string(&watches_msg).unwrap())
            .map_err(|e| crate::Error::WatchFailed(format!("Failed to send watches: {}", e)))?;
    }

    match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(count) => {
            tracing::info!("Agent confirmed {} watches active", count);
            Ok(())
        }
        Err(_) => {
            let still_alive = unsafe { libc::kill(pid as i32, 0) == 0 };
            if !still_alive {
                tracing::warn!("Watch confirmation timeout — process {} is dead", pid);
                Err(crate::Error::WatchFailed(
                    format!("Process {} terminated before watches could be confirmed", pid)
                ))
            } else {
                tracing::warn!("Timeout waiting for watch confirmation (process {} still alive)", pid);
                Err(crate::Error::WatchFailed("Timeout waiting for watch confirmation".into()))
            }
        }
    }
}

/// Handle ReadMemory on a session worker thread.
fn handle_read_memory(
    script_ptr: *mut frida_sys::_FridaScript,
    read_response: &ReadResponseSignal,
    recipes_json: &str,
    pid: u32,
) -> Result<serde_json::Value> {
    let msg: serde_json::Value = serde_json::from_str(recipes_json)
        .map_err(|e| crate::Error::Frida(format!("Invalid recipes JSON: {}", e)))?;

    if msg.get("poll").is_some() {
        // Poll mode — send message, return immediately (events stream through normal pipeline)
        unsafe {
            post_message_raw(script_ptr, recipes_json)
                .map_err(|e| crate::Error::Frida(format!("Failed to send read_memory: {}", e)))?;
        }
        return Ok(serde_json::json!({ "polling": true }));
    }

    // One-shot: arm the response channel, send message, wait
    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = read_response.lock().unwrap();
        *guard = Some(signal_tx);
    }

    unsafe {
        post_message_raw(script_ptr, recipes_json)
            .map_err(|e| crate::Error::Frida(format!("Failed to send read_memory: {}", e)))?;
    }

    // Wait for response with periodic process-liveness checks.
    // If the process dies, the Frida agent dies too and no response will come.
    // Detect this early instead of waiting the full timeout.
    for _ in 0..10 {
        match signal_rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(response) => return Ok(response),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(crate::Error::ReadFailed("Response channel closed".to_string()));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
                if !alive {
                    return Err(crate::Error::ReadFailed(
                        "Process exited before memory read completed".to_string()
                    ));
                }
            }
        }
    }
    Err(crate::Error::Frida("Memory read timed out (5s)".to_string()))
}

fn handle_write_memory(
    script_ptr: *mut frida_sys::_FridaScript,
    write_response: &WriteResponseSignal,
    recipes_json: &str,
    pid: u32,
) -> Result<serde_json::Value> {
    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = write_response.lock().unwrap();
        *guard = Some(signal_tx);
    }

    unsafe {
        post_message_raw(script_ptr, recipes_json)
            .map_err(|e| crate::Error::WriteFailed(format!("Failed to send write_memory: {}", e)))?;
    }

    for _ in 0..10 {
        match signal_rx.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(response) => return Ok(response),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(crate::Error::WriteFailed("Response channel closed".to_string()));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
                if !alive {
                    return Err(crate::Error::WriteFailed(
                        "Process exited before memory write completed".to_string()
                    ));
                }
            }
        }
    }
    Err(crate::Error::WriteFailed("Memory write timed out (5s)".to_string()))
}

/// Handle a child process spawned via fork/exec.
/// Attaches Frida to the child, loads the agent, and registers it for output capture.
fn handle_child_spawn(
    device: &mut frida::Device,
    child_pid: u32,
    output_registry: &OutputRegistry,
    session_ptrs: &mut HashMap<u32, *mut frida_sys::_FridaSession>,
) {
    // Find which session this child belongs to by checking the output registry
    let parent_info = {
        let reg = match output_registry.lock() {
            Ok(g) => g,
            Err(_) => {
                tracing::warn!("Failed to lock output registry for child {}", child_pid);
                let _ = device.resume(child_pid);
                return;
            }
        };
        // Find any active session to associate the child with
        reg.values()
            .next()
            .map(|ctx| (ctx.session_id.clone(), ctx.event_tx.clone(), ctx.start_ns))
    };

    let (session_id, event_tx, start_ns) = match parent_info {
        Some(info) => info,
        None => {
            tracing::debug!("No active session for child PID {}, resuming without attaching", child_pid);
            let _ = device.resume(child_pid);
            return;
        }
    };

    tracing::info!("Attaching to child process {} (session: {})", child_pid, session_id);

    // Register output context for the child
    let output_ctx = Arc::new(OutputContext {
        pid: child_pid,
        session_id: session_id.clone(),
        event_tx: event_tx.clone(),
        event_counter: AtomicU64::new(0),
        start_ns,
    });
    if let Ok(mut reg) = output_registry.lock() {
        reg.insert(child_pid, output_ctx);
    }

    // Attach to child
    match device.attach(child_pid) {
        Ok(frida_session) => {
            let raw_session = unsafe { session_raw_ptr(&frida_session) };
            std::mem::forget(frida_session);
            session_ptrs.insert(child_pid, raw_session);

            // Create and load agent script in child
            match unsafe { create_script_raw(raw_session, AGENT_CODE) } {
                Ok(script_ptr) => {
                    let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
                    let read_response: ReadResponseSignal = Arc::new(Mutex::new(None));
                    let write_response: WriteResponseSignal = Arc::new(Mutex::new(None));
                    let handler = AgentMessageHandler {
                        event_tx: event_tx.clone(),
                        session_id: session_id.clone(),
                        hooks_ready: hooks_ready.clone(),
                        read_response: read_response.clone(),
                        write_response: write_response.clone(),
                        crash_reported: Arc::new(AtomicBool::new(false)),
                        pause_notify_tx: None,
                        start_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as i64,
                    };
                    unsafe {
                        register_handler_raw(script_ptr, handler);
                        if let Err(e) = load_script_raw(script_ptr) {
                            tracing::error!("Failed to load script in child {}: {}", child_pid, e);
                            let _ = device.resume(child_pid);
                            return;
                        }
                    }

                    // Initialize agent in child
                    let init_msg = serde_json::json!({
                        "type": "initialize",
                        "sessionId": session_id,
                    });
                    unsafe {
                        let _ = post_message_raw(script_ptr, &serde_json::to_string(&init_msg).unwrap());
                    }

                    tracing::info!("Agent loaded in child process {}", child_pid);
                }
                Err(e) => {
                    tracing::error!("Failed to create script in child {}: {}", child_pid, e);
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to attach to child {}: {}", child_pid, e);
        }
    }

    // Resume the child process
    let _ = device.resume(child_pid);
}

fn parse_event(session_id: &str, json: &serde_json::Value) -> Option<Event> {
    let event_type = match json.get("eventType")?.as_str()? {
        "function_enter" => EventType::FunctionEnter,
        "function_exit" => EventType::FunctionExit,
        "stdout" => EventType::Stdout,
        "stderr" => EventType::Stderr,
        "crash" => EventType::Crash,
        "variable_snapshot" => EventType::VariableSnapshot,
        "pause" => EventType::Pause,
        "logpoint" => EventType::Logpoint,
        "condition_error" => EventType::ConditionError,
        _ => return None,
    };

    let pid = json.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);

    if event_type == EventType::VariableSnapshot {
        return Some(Event {
            id: json.get("id").and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}-snap-{}", session_id, chrono::Utc::now().timestamp_millis())),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            event_type,
            arguments: json.get("data").cloned(),
            pid,
            ..Event::default()
        });
    }

    if event_type == EventType::Crash {
        return Some(Event {
            id: json.get("id").and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("{}-crash-{}", session_id, chrono::Utc::now().timestamp_millis())),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            event_type,
            // Store frameMemory/frameBase in text as JSON for later local variable resolution
            text: {
                let fm = json.get("frameMemory");
                let fb = json.get("frameBase");
                if fm.is_some() || fb.is_some() {
                    Some(serde_json::json!({
                        "frameMemory": fm,
                        "frameBase": fb,
                    }).to_string())
                } else {
                    None
                }
            },
            pid,
            signal: json.get("signal").and_then(|v| v.as_str()).map(|s| s.to_string()),
            fault_address: json.get("faultAddress").and_then(|v| v.as_str()).map(|s| s.to_string()),
            registers: json.get("registers").cloned(),
            backtrace: json.get("backtrace").cloned(),
            ..Event::default()
        });
    }

    if event_type == EventType::Stdout || event_type == EventType::Stderr {
        return Some(Event {
            id: json.get("id")?.as_str()?.to_string(),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            thread_name: json.get("threadName").and_then(|v| v.as_str()).map(|s| s.to_string()),
            event_type,
            text: json.get("text").and_then(|v| v.as_str()).map(|s| s.to_string()),
            pid,
            ..Event::default()
        });
    }

    // Phase 2: Breakpoint events (Pause, Logpoint, ConditionError)
    if event_type == EventType::Pause || event_type == EventType::Logpoint || event_type == EventType::ConditionError {
        return Some(Event {
            id: json.get("id")?.as_str()?.to_string(),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            thread_name: json.get("threadName").and_then(|v| v.as_str()).map(|s| s.to_string()),
            event_type,
            breakpoint_id: json.get("breakpointId").and_then(|v| v.as_str()).map(|s| s.to_string()),
            logpoint_message: json.get("message").and_then(|v| v.as_str()).map(|s| s.to_string()),
            function_name: json.get("functionName").and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_default(),
            source_file: json.get("file").and_then(|v| v.as_str()).map(|s| s.to_string()),
            line_number: json.get("line").and_then(|v| v.as_i64()).map(|n| n as i32),
            pid,
            ..Event::default()
        });
    }

    Some(Event {
        id: json.get("id")?.as_str()?.to_string(),
        session_id: session_id.to_string(),
        timestamp_ns: json.get("timestampNs")?.as_i64()?,
        thread_id: json.get("threadId")?.as_i64()?,
        thread_name: json.get("threadName").and_then(|v| v.as_str()).map(|s| s.to_string()),
        parent_event_id: json.get("parentEventId").and_then(|v| v.as_str()).map(|s| s.to_string()),
        event_type,
        function_name: json.get("functionName")?.as_str()?.to_string(),
        function_name_raw: json.get("functionNameRaw").and_then(|v| v.as_str()).map(|s| s.to_string()),
        source_file: json.get("sourceFile").and_then(|v| v.as_str()).map(|s| s.to_string()),
        line_number: json.get("lineNumber").and_then(|v| v.as_i64()).map(|n| n as i32),
        arguments: json.get("arguments").cloned(),
        return_value: json.get("returnValue").cloned(),
        duration_ns: json.get("durationNs").and_then(|v| v.as_i64()),
        sampled: json.get("sampled").and_then(|v| v.as_bool()),
        watch_values: json.get("watchValues").cloned(),
        pid,
        ..Event::default()
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

/// Monitor a spawned process for crash detection.
/// When the process dies, checks for a crash file written by the agent's
/// exception handler (synchronous native I/O). Falls back to waitpid
/// for basic signal info. This ensures crash events are captured even when
/// GLib can't flush the agent's async send() before the OS kills the process.
fn process_death_monitor(
    pid: u32,
    session_id: String,
    event_tx: mpsc::Sender<Event>,
    crash_reported: Arc<AtomicBool>,
    start_ns: i64,
) {
    // Poll until process is dead
    loop {
        let alive = unsafe { libc::kill(pid as i32, 0) == 0 };
        if !alive { break; }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    // Give the agent's async crash event time to arrive via GLib
    std::thread::sleep(std::time::Duration::from_millis(500));

    // If agent already reported the crash via the normal GLib path, just clean up
    if crash_reported.load(Ordering::Acquire) {
        let crash_file = format!("/tmp/.strobe-crash-{}.json", pid);
        let _ = std::fs::remove_file(&crash_file);
        tracing::debug!("Process {} died, agent crash event already received", pid);
        return;
    }

    // Try to read crash data from the file written by the agent's exception handler
    let crash_file = format!("/tmp/.strobe-crash-{}.json", pid);
    if let Ok(data) = std::fs::read_to_string(&crash_file) {
        let _ = std::fs::remove_file(&crash_file);

        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&data) {
            if let Some(events) = parsed.get("events").and_then(|v| v.as_array()) {
                tracing::info!("Recovered crash event from file for PID {} [{}]", pid, session_id);
                for event_json in events {
                    if let Some(event) = parse_event(&session_id, event_json) {
                        let _ = event_tx.try_send(event);
                    }
                }
                return;
            }
        }
        tracing::warn!("Crash file for PID {} exists but couldn't be parsed", pid);
    }

    // Last resort: check waitpid for signal-based termination
    let mut status: i32 = 0;
    let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };

    if result > 0 {
        let killed_by_signal = libc::WIFSIGNALED(status);
        if killed_by_signal {
            let sig = libc::WTERMSIG(status);
            // Don't report SIGKILL/SIGTERM as crashes (usually intentional)
            if sig == libc::SIGKILL || sig == libc::SIGTERM {
                tracing::debug!("Process {} killed by signal {} (intentional)", pid, sig);
                return;
            }
            let signal_name = match sig {
                libc::SIGSEGV => "access-violation",
                libc::SIGABRT => "abort",
                libc::SIGBUS => "bus-error",
                libc::SIGFPE => "arithmetic",
                libc::SIGILL => "illegal-instruction",
                _ => "unknown-signal",
            };

            let now_ns = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as i64) - start_ns;

            tracing::info!(
                "Host-side crash detection: PID {} killed by {} [{}]",
                pid, signal_name, session_id
            );

            let event = Event {
                id: format!("{}-crash-host-{}", session_id, chrono::Utc::now().timestamp_millis()),
                session_id: session_id.clone(),
                timestamp_ns: now_ns,
                event_type: EventType::Crash,
                signal: Some(signal_name.to_string()),
                pid: Some(pid),
                thread_id: 0,
                ..Event::default()
            };
            let _ = event_tx.try_send(event);
        }
    } else {
        tracing::debug!("Process {} already reaped (waitpid returned {})", pid, result);
    }
}

/// Session state on the main thread
pub struct FridaSession {
    pub project_root: String,
    hook_manager: HookManager,
    dwarf_handle: DwarfHandle,
    image_base: u64,
}

/// Spawner that communicates with the coordinator and per-session worker threads
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
        defer_resume: bool,
        pause_notify_tx: Option<PauseNotifyTx>,
    ) -> Result<u32> {
        let (response_tx, response_rx) = oneshot::channel();

        self.coordinator_tx.send(CoordinatorCommand::Spawn {
            session_id: session_id.to_string(),
            command: command.to_string(),
            args: args.to_vec(),
            cwd: cwd.map(|s| s.to_string()),
            env: env.cloned(),
            event_tx: event_sender,
            defer_resume,
            pause_notify_tx,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Coordinator thread died".to_string()))?;

        let spawn_result = response_rx.await
            .map_err(|_| crate::Error::Frida("Coordinator response lost".to_string()))??;

        let pid = spawn_result.pid;

        // Spawn dedicated worker thread for this session
        let (session_tx, session_rx) = std::sync::mpsc::channel();
        let sid = session_id.to_string();
        thread::spawn(move || {
            session_worker(sid, spawn_result.script_ptr, spawn_result.hooks_ready, spawn_result.read_response, spawn_result.write_response, pid, session_rx);
        });
        self.session_workers.insert(session_id.to_string(), session_tx);

        let session = FridaSession {
            project_root: project_root.to_string(),
            hook_manager: HookManager::new(),
            dwarf_handle,
            image_base,
        };

        self.sessions.insert(session_id.to_string(), session);

        Ok(pid)
    }

    /// Resume a previously suspended process (used with defer_resume=true).
    pub async fn resume(&self, pid: u32) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();
        self.coordinator_tx.send(CoordinatorCommand::Resume {
            pid,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Coordinator thread died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Coordinator response lost".to_string()))?
    }

    pub async fn add_patterns(&mut self, session_id: &str, patterns: &[String], serialization_depth: Option<u32>, resolver: Option<&dyn crate::symbols::SymbolResolver>) -> Result<HookResult> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        session.hook_manager.add_patterns(patterns);

        // Group functions by mode
        let mut full_funcs: Vec<FunctionTarget> = Vec::new();
        let mut light_funcs: Vec<FunctionTarget> = Vec::new();

        // Use SymbolResolver if available, otherwise fall back to DWARF
        if let Some(resolver) = resolver {
            // For interpreted languages (Python, JS, etc.)
            use std::path::Path;
            for pattern in patterns {
                let targets = resolver.resolve_pattern(pattern, Path::new(&session.project_root))?;
                let mode = HookManager::classify_with_count(pattern, targets.len());
                tracing::info!("Pattern '{}' -> {:?} mode ({} targets, resolver)", pattern, mode, targets.len());

                let target_list = if mode == HookMode::Full { &mut full_funcs } else { &mut light_funcs };
                for target in targets {
                    // For interpreted languages, send file:line targets instead of address targets
                    match target {
                        crate::symbols::ResolvedTarget::SourceLocation { file, line, name } => {
                            // Create a FunctionTarget with file and line
                            target_list.push(FunctionTarget {
                                address: 0, // No address for interpreted
                                name: name.clone(),
                                name_raw: Some(name.clone()),
                                source_file: Some(file),
                                line_number: Some(line),
                            });
                        }
                        crate::symbols::ResolvedTarget::Address { address, name, name_raw, file, line } => {
                            // Native function from resolver (DwarfResolver)
                            target_list.push(FunctionTarget {
                                address,
                                name: name.clone(),
                                name_raw: name_raw.clone(),
                                source_file: file.clone(),
                                line_number: line,
                            });
                        }
                    }
                }
            }
        } else {
            // For native binaries (C++/Rust) - use DWARF
            let dwarf = session.dwarf_handle.clone().get().await?;
            for pattern in patterns {
                let matches: Vec<&FunctionInfo> = resolve_pattern(&dwarf, pattern, &session.project_root);
                let mode = HookManager::classify_with_count(pattern, matches.len());
                tracing::info!("Pattern '{}' -> {:?} mode ({} functions, DWARF)", pattern, mode, matches.len());

                let target = if mode == HookMode::Full { &mut full_funcs } else { &mut light_funcs };
                for func in matches {
                    target.push(FunctionTarget::from(func));
                }
            }
        }

        let matched = (full_funcs.len() + light_funcs.len()) as u32;
        let mut warnings: Vec<String> = Vec::new();

        // Enforce hook cap — truncate light funcs first (cheaper to skip), then full
        let total = full_funcs.len() + light_funcs.len();
        if total > MAX_HOOKS_PER_CALL {
            let excess = total - MAX_HOOKS_PER_CALL;
            let light_trim = excess.min(light_funcs.len());
            light_funcs.truncate(light_funcs.len() - light_trim);
            let remaining_excess = excess - light_trim;
            if remaining_excess > 0 {
                full_funcs.truncate(full_funcs.len() - remaining_excess);
            }
            warnings.push(format!(
                "Pattern matched {} functions (limit: {}). Only {} were hooked. \
                 Use more specific patterns like @file:specific_module to stay under the limit.",
                matched, MAX_HOOKS_PER_CALL, full_funcs.len() + light_funcs.len()
            ));
            tracing::warn!("Hook cap: {} matched, {} capped to {}", matched, total, MAX_HOOKS_PER_CALL);
        }

        let image_base = session.image_base;
        let mut total_hooks = 0u32;

        // Send chunks for both modes (serialization_depth only on the first chunk overall)
        let mut depth_sent = false;
        let batches: [(Vec<FunctionTarget>, HookMode); 2] = [
            (full_funcs, HookMode::Full),
            (light_funcs, HookMode::Light),
        ];

        'outer: for (funcs, mode) in &batches {
            for chunk in funcs.chunks(CHUNK_SIZE) {
                let depth = if !depth_sent { depth_sent = true; serialization_depth } else { None };
                match self.send_add_chunk(session_id, chunk.to_vec(), image_base, *mode, depth).await {
                    // activeCount is the total hooks active (not delta), so use latest value
                    Ok(count) => total_hooks = count,
                    Err(e) => {
                        warnings.push(format!("Hook installation error: {}", e));
                        break 'outer;
                    }
                }
            }
        }

        Ok(HookResult { installed: total_hooks, matched, warnings })
    }

    async fn send_add_chunk(
        &self,
        session_id: &str,
        functions: Vec<FunctionTarget>,
        image_base: u64,
        mode: HookMode,
        serialization_depth: Option<u32>,
    ) -> Result<u32> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::AddPatterns {
            functions,
            image_base,
            mode,
            serialization_depth,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn remove_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<u32> {
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

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::RemovePatterns {
            functions,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn read_memory(&self, session_id: &str, recipes_json: String) -> Result<serde_json::Value> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::ReadMemory {
            recipes_json,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn write_memory(&self, session_id: &str, recipes_json: String) -> Result<serde_json::Value> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::WriteMemory {
            recipes_json,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn stop(&mut self, session_id: &str) -> Result<()> {
        self.sessions.remove(session_id);

        // Phase 1: Shut down session worker (stops script operations)
        if let Some(worker_tx) = self.session_workers.remove(session_id) {
            let _ = worker_tx.send(SessionCommand::Shutdown);
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

    pub async fn set_watches(
        &mut self,
        session_id: &str,
        watches: Vec<WatchTarget>,
        expr_watches: Vec<ExprWatchTarget>,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::SetWatches {
            watches,
            expr_watches,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        self.sessions
            .get(session_id)
            .map(|s| s.hook_manager.active_patterns())
            .unwrap_or_default()
    }

    // Phase 2: Breakpoint support
    /// Send a hook setup message (breakpoint or logpoint) to the agent.
    /// Both use the same SessionCommand since the message type field
    /// distinguishes them at the agent level.
    pub async fn send_hook_message(
        &mut self,
        session_id: &str,
        message: serde_json::Value,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::SetBreakpoint {
            message,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn set_breakpoint(
        &mut self,
        session_id: &str,
        message: serde_json::Value,
    ) -> Result<()> {
        self.send_hook_message(session_id, message).await
    }

    pub async fn set_logpoint(
        &mut self,
        session_id: &str,
        message: serde_json::Value,
    ) -> Result<()> {
        self.send_hook_message(session_id, message).await
    }

    pub async fn remove_breakpoint(
        &mut self,
        session_id: &str,
        breakpoint_id: &str,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::RemoveBreakpoint {
            breakpoint_id: breakpoint_id.to_string(),
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn remove_logpoint(
        &mut self,
        session_id: &str,
        logpoint_id: &str,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::RemoveLogpoint {
            logpoint_id: logpoint_id.to_string(),
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
    }

    pub async fn resume_thread(
        &mut self,
        session_id: &str,
        thread_id: u64,
    ) -> Result<()> {
        self.resume_thread_with_step(session_id, thread_id, Vec::new(), 0, None).await
    }

    /// Resume thread with optional one-shot breakpoints for stepping
    pub async fn resume_thread_with_step(
        &mut self,
        session_id: &str,
        thread_id: u64,
        one_shot_addresses: Vec<(u64, bool)>,
        image_base: u64,
        return_address: Option<u64>,
    ) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let worker_tx = self.session_workers.get(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        worker_tx.send(SessionCommand::ResumeThread {
            thread_id,
            one_shot_addresses,
            image_base,
            return_address,
            response: response_tx,
        }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

        response_rx.await
            .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
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
        let read_response: ReadResponseSignal = Arc::new(Mutex::new(None));
        let write_response: WriteResponseSignal = Arc::new(Mutex::new(None));
        let handler = AgentMessageHandler {
            event_tx,
            session_id: "test-session".to_string(),
            hooks_ready: hooks_ready.clone(),
            read_response,
            write_response,
            crash_reported: Arc::new(AtomicBool::new(false)),
            pause_notify_tx: None,
            start_ns: 1_000_000_000, // 1s offset for test determinism
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

    // --- Pause notification tests ---

    #[tokio::test]
    async fn test_handler_paused_creates_event_and_notification() {
        let (pause_tx, mut pause_rx) = mpsc::channel(10);
        let (event_tx, mut event_rx) = mpsc::channel(1000);
        let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
        let read_response: ReadResponseSignal = Arc::new(Mutex::new(None));
        let write_response: WriteResponseSignal = Arc::new(Mutex::new(None));
        let handler = AgentMessageHandler {
            event_tx,
            session_id: "pause-test".to_string(),
            hooks_ready,
            read_response,
            write_response,
            crash_reported: Arc::new(AtomicBool::new(false)),
            pause_notify_tx: Some(pause_tx),
            start_ns: 1_000_000_000,
        };

        // Simulate a "paused" message from agent
        let payload = json!({
            "type": "paused",
            "threadId": 42,
            "breakpointId": "bp-1",
            "funcName": "main",
            "file": "main.cpp",
            "line": 10,
            "returnAddress": "0x1234abcd"
        });

        handler.handle_payload("paused", &payload);

        // Should receive a Pause event on the event channel
        let event = event_rx.recv().await.unwrap();
        assert_eq!(event.event_type, EventType::Pause);
        assert_eq!(event.breakpoint_id, Some("bp-1".to_string()));
        assert_eq!(event.thread_id, 42);

        // Should also receive a PauseNotification
        let notification = pause_rx.recv().await.unwrap();
        assert_eq!(notification.session_id, "pause-test");
        assert_eq!(notification.thread_id, 42);
        assert_eq!(notification.breakpoint_id, "bp-1");
        assert_eq!(notification.func_name, Some("main".to_string()));
        assert_eq!(notification.file, Some("main.cpp".to_string()));
        assert_eq!(notification.line, Some(10));
        assert_eq!(notification.return_address, Some(0x1234abcd));
    }

    #[tokio::test]
    async fn test_handler_paused_without_notification_channel() {
        let (handler, mut event_rx, _hooks_ready) = make_handler();

        // Should still work without pause_notify_tx (None)
        let payload = json!({
            "type": "paused",
            "threadId": 1,
            "breakpointId": "bp-1"
        });

        handler.handle_payload("paused", &payload);

        // Event should still be created
        let event = event_rx.recv().await.unwrap();
        assert_eq!(event.event_type, EventType::Pause);
    }

    #[tokio::test]
    async fn test_handler_paused_with_backtrace_and_arguments() {
        let (pause_tx, mut pause_rx) = mpsc::channel(10);
        let (event_tx, mut event_rx) = mpsc::channel(1000);
        let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
        let read_response: ReadResponseSignal = Arc::new(Mutex::new(None));
        let write_response: WriteResponseSignal = Arc::new(Mutex::new(None));
        let handler = AgentMessageHandler {
            event_tx,
            session_id: "bt-test".to_string(),
            hooks_ready,
            read_response,
            write_response,
            crash_reported: Arc::new(AtomicBool::new(false)),
            pause_notify_tx: Some(pause_tx),
            start_ns: 1_000_000_000,
        };

        let payload = json!({
            "type": "paused",
            "threadId": 99,
            "breakpointId": "bp-42",
            "funcName": "audio::process",
            "file": "src/audio.rs",
            "line": 120,
            "returnAddress": "0xdeadbeef",
            "backtrace": [
                {
                    "address": "0x1000",
                    "moduleName": "libfoo.dylib",
                    "name": "foo::bar",
                    "fileName": "src/foo.rs",
                    "lineNumber": 42
                },
                {
                    "address": "0x2000",
                    "moduleName": "libsystem.dylib"
                }
            ],
            "arguments": [
                { "index": 0, "value": "0x7fff1234" },
                { "index": 1, "value": "42" }
            ]
        });

        handler.handle_payload("paused", &payload);

        // Verify event
        let event = event_rx.recv().await.unwrap();
        assert_eq!(event.event_type, EventType::Pause);
        assert_eq!(event.breakpoint_id, Some("bp-42".to_string()));

        // Verify notification with backtrace and arguments
        let notification = pause_rx.recv().await.unwrap();
        assert_eq!(notification.session_id, "bt-test");
        assert_eq!(notification.thread_id, 99);
        assert_eq!(notification.func_name, Some("audio::process".to_string()));

        // Backtrace
        assert_eq!(notification.backtrace.len(), 2);
        assert_eq!(notification.backtrace[0].address, "0x1000");
        assert_eq!(notification.backtrace[0].function_name, Some("foo::bar".to_string()));
        assert_eq!(notification.backtrace[0].file, Some("src/foo.rs".to_string()));
        assert_eq!(notification.backtrace[0].line, Some(42));
        assert_eq!(notification.backtrace[1].address, "0x2000");
        assert_eq!(notification.backtrace[1].module_name, Some("libsystem.dylib".to_string()));
        assert_eq!(notification.backtrace[1].function_name, None);

        // Arguments
        assert_eq!(notification.arguments.len(), 2);
        assert_eq!(notification.arguments[0].index, 0);
        assert_eq!(notification.arguments[0].value, "0x7fff1234");
        assert_eq!(notification.arguments[1].index, 1);
        assert_eq!(notification.arguments[1].value, "42");
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
