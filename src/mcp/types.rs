use serde::{Deserialize, Serialize};

fn default_empty_string() -> String { String::new() }

// ============ debug_launch ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLaunchRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
}

impl DebugLaunchRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.command.is_empty() {
            return Err(crate::Error::ValidationError(
                "command must not be empty".to_string()
            ));
        }
        if self.project_root.is_empty() {
            return Err(crate::Error::ValidationError(
                "projectRoot must not be empty".to_string()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLaunchResponse {
    pub session_id: String,
    pub pid: u32,
    /// Number of pending patterns that were applied (0 if none)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_patterns_applied: Option<usize>,
    /// Guidance on recommended next steps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_steps: Option<String>,
}

// ============ debug_trace ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTraceRequest {
    /// Session ID - if omitted, modifies pending patterns for next launch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watches: Option<WatchUpdate>,
    /// Maximum depth for recursive argument serialization (default: 3, max: 10)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serialization_depth: Option<u32>,
    /// Project root for settings resolution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<WatchTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    /// Optional function patterns to restrict when this watch is captured.
    /// Supports wildcards: `*` (shallow, doesn't cross ::), `**` (deep, crosses ::).
    /// Examples: ["NoteOn"], ["audio::*"], ["juce::**"]
    /// If omitted, watch is global (captured on all traced functions).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWatch {
    pub label: String,
    pub address: String,
    pub size: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTraceResponse {
    /// Mode: "pending" (pre-launch) or "runtime" (on running session)
    pub mode: String,
    /// Active trace patterns
    pub active_patterns: Vec<String>,
    /// Number of functions actually hooked (0 if pending or no matches)
    pub hooked_functions: u32,
    /// If different from hooked_functions, shows total matched before hook limit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_functions: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub active_watches: Vec<ActiveWatch>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<String>,
    pub event_limit: usize,
    /// Contextual status message explaining current state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

// Validation limits
pub const MAX_WATCHES_PER_SESSION: usize = 32;
pub const MAX_WATCH_EXPRESSION_LENGTH: usize = 256;
pub const MAX_WATCH_EXPRESSION_DEPTH: usize = 4;
pub const MAX_BREAKPOINTS_PER_SESSION: usize = 50;
pub const MAX_LOGPOINTS_PER_SESSION: usize = 100;
pub const MAX_LINE_NUMBER: u32 = 1_000_000;
pub const MAX_CONDITION_LENGTH: usize = 1024;
pub const MAX_LOGPOINT_MESSAGE_LENGTH: usize = 2048;

/// Validate a watch field (expression or variable name) against length and depth limits.
fn validate_watch_field(value: &str, field_name: &str) -> crate::Result<()> {
    if value.len() > MAX_WATCH_EXPRESSION_LENGTH {
        return Err(crate::Error::ValidationError(
            format!("Watch {} length ({} bytes) exceeds maximum of {} bytes",
                field_name, value.len(), MAX_WATCH_EXPRESSION_LENGTH)
        ));
    }
    let depth = value.matches("->").count() + value.matches('.').count();
    if depth > MAX_WATCH_EXPRESSION_DEPTH {
        return Err(crate::Error::ValidationError(
            format!("Watch {} depth ({}) exceeds maximum of {}",
                field_name, depth, MAX_WATCH_EXPRESSION_DEPTH)
        ));
    }
    Ok(())
}

impl DebugTraceRequest {
    /// Validate request parameters against limits
    pub fn validate(&self) -> crate::Result<()> {
        if let Some(depth) = self.serialization_depth {
            if depth < 1 || depth > 10 {
                return Err(crate::Error::ValidationError(
                    "serialization_depth must be between 1 and 10".to_string()
                ));
            }
        }

        if let Some(ref watch_update) = self.watches {
            if let Some(ref add_watches) = watch_update.add {
                if add_watches.len() > MAX_WATCHES_PER_SESSION {
                    return Err(crate::Error::ValidationError(
                        format!("Cannot add {} watches (max {})", add_watches.len(), MAX_WATCHES_PER_SESSION)
                    ));
                }

                for watch in add_watches {
                    if let Some(ref expr) = watch.expr {
                        validate_watch_field(expr, "expression")?;
                    }
                    if let Some(ref var) = watch.variable {
                        validate_watch_field(var, "variable")?;
                    }
                }
            }
        }

        Ok(())
    }
}

// ============ debug_query ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
    VariableSnapshot,
    Pause,
    Logpoint,
    ConditionError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFileFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnValueFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_null: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNameFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugQueryRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<EventTypeFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<SourceFileFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<ReturnValueFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<ThreadNameFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_from: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_duration_ns: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
    /// Cursor: return only events with rowid > after_event_id (for incremental polling)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_event_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugQueryResponse {
    pub events: Vec<serde_json::Value>,
    pub total_count: u64,
    pub has_more: bool,
    /// All process IDs in this session (parent + children), only present when multiple
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pids: Option<Vec<u32>>,
    /// Highest event rowid in this response (use as next cursor)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<i64>,
    /// True if FIFO eviction happened since the cursor position
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_dropped: Option<bool>,
    /// Crash event, if the process crashed. Always included regardless of eventType filter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash: Option<serde_json::Value>,
}

// ============ debug_stop ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStopRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retain: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStopResponse {
    pub success: bool,
    pub events_collected: u64,
}

// ============ debug_read ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTarget {
    /// DWARF variable name or pointer chain (e.g. "gClock->counter")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    /// Raw hex address (e.g. "0x7ff800")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Size in bytes (required for raw address reads)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// Type hint for raw address reads: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer/bytes
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollConfig {
    pub interval_ms: u32,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadRequest {
    pub session_id: String,
    pub targets: Vec<ReadTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<PollConfig>,
}

// Validation limits for debug_read
pub const MAX_READ_TARGETS: usize = 16;
pub const MAX_READ_DEPTH: u32 = 5;
pub const MIN_POLL_INTERVAL_MS: u32 = 50;
pub const MAX_POLL_INTERVAL_MS: u32 = 5000;
pub const MIN_POLL_DURATION_MS: u32 = 100;
pub const MAX_POLL_DURATION_MS: u32 = 30000;
pub const MAX_RAW_READ_SIZE: u32 = 65536;
const VALID_TYPE_HINTS: &[&str] = &[
    "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64",
    "f32", "f64", "pointer", "bytes",
];

impl DebugReadRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.targets.is_empty() {
            return Err(crate::Error::ValidationError(
                "targets must not be empty".to_string()
            ));
        }
        if self.targets.len() > MAX_READ_TARGETS {
            return Err(crate::Error::ValidationError(
                format!("Too many targets ({}, max {})", self.targets.len(), MAX_READ_TARGETS)
            ));
        }
        if let Some(depth) = self.depth {
            if depth < 1 || depth > MAX_READ_DEPTH {
                return Err(crate::Error::ValidationError(
                    format!("depth must be between 1 and {}", MAX_READ_DEPTH)
                ));
            }
        }
        if let Some(ref poll) = self.poll {
            if poll.interval_ms < MIN_POLL_INTERVAL_MS || poll.interval_ms > MAX_POLL_INTERVAL_MS {
                return Err(crate::Error::ValidationError(
                    format!("poll.intervalMs must be between {} and {}", MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS)
                ));
            }
            if poll.duration_ms < MIN_POLL_DURATION_MS || poll.duration_ms > MAX_POLL_DURATION_MS {
                return Err(crate::Error::ValidationError(
                    format!("poll.durationMs must be between {} and {}", MIN_POLL_DURATION_MS, MAX_POLL_DURATION_MS)
                ));
            }
        }
        for target in &self.targets {
            if target.variable.is_none() && target.address.is_none() {
                return Err(crate::Error::ValidationError(
                    "Each target must have either 'variable' or 'address'".to_string()
                ));
            }
            if target.address.is_some() {
                if target.size.is_none() || target.type_hint.is_none() {
                    return Err(crate::Error::ValidationError(
                        "Raw address targets require 'size' and 'type'".to_string()
                    ));
                }
                if let Some(size) = target.size {
                    if size == 0 || size > MAX_RAW_READ_SIZE {
                        return Err(crate::Error::ValidationError(
                            format!("size must be between 1 and {}", MAX_RAW_READ_SIZE)
                        ));
                    }
                }
                if let Some(ref type_hint) = target.type_hint {
                    if !VALID_TYPE_HINTS.contains(&type_hint.as_str()) {
                        return Err(crate::Error::ValidationError(
                            format!("Invalid type '{}'. Valid: {}", type_hint, VALID_TYPE_HINTS.join(", "))
                        ));
                    }
                }
            }
            if let Some(ref var) = target.variable {
                validate_watch_field(var, "variable")?;
            }
        }
        Ok(())
    }
}

/// A single read result in the debug_read response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// File path for bytes-type reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Hex preview for bytes-type reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

impl Default for ReadResult {
    fn default() -> Self {
        Self {
            target: String::new(),
            address: None,
            type_name: None,
            value: None,
            size: None,
            fields: None,
            error: None,
            file: None,
            preview: None,
        }
    }
}

/// Response for one-shot debug_read
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadResponse {
    pub results: Vec<ReadResult>,
}

/// Response for poll-mode debug_read
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadPollResponse {
    pub polling: bool,
    pub variable_count: usize,
    pub interval_ms: u32,
    pub duration_ms: u32,
    pub expected_samples: u32,
    pub event_type: String,
    pub hint: String,
}

// ============ debug_test ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestAction {
    Run,
    Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestRequest {
    /// Action: "run" (default) starts a test, "status" polls for results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<TestAction>,
    /// Required for action: "status" — the test run ID to poll
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    #[serde(default = "default_empty_string")]
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<crate::test::adapter::TestLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_patterns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watches: Option<WatchUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
}

impl DebugTestRequest {
    pub fn validate(&self) -> crate::Result<()> {
        match self.action.as_ref().unwrap_or(&TestAction::Run) {
            TestAction::Status => {
                if self.test_run_id.as_ref().map_or(true, |s| s.is_empty()) {
                    return Err(crate::Error::ValidationError(
                        "testRunId is required for action: 'status'".to_string(),
                    ));
                }
            }
            TestAction::Run => {
                if self.project_root.is_empty() {
                    return Err(crate::Error::ValidationError(
                        "projectRoot is required for action: 'run'".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestResponse {
    pub framework: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<crate::test::adapter::TestSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<crate::test::adapter::TestFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stuck: Vec<crate::test::adapter::StuckTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tests: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::test::adapter::ProjectInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_info: Option<CrashSummary>,
}

// ============ debug_test (async start response) ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestStartResponse {
    pub test_run_id: String,
    pub status: String,
    pub framework: String,
}

// ============ debug_test_status ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestStatusRequest {
    pub test_run_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestStatusResponse {
    pub test_run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<TestProgressSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Frida session ID — use with debug_trace/debug_stop to instrument or kill the test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestStuckWarning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_name: Option<String>,
    pub idle_ms: u64,
    pub diagnosis: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProgressSnapshot {
    pub elapsed_ms: u64,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_test: Option<String>,
    /// Current phase: "compiling", "running", or "suites_finished"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Advisory warnings from stuck detector (deadlock, infinite loop, hard timeout).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<TestStuckWarning>,
    /// How long the current test has been running (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_test_elapsed_ms: Option<u64>,
    /// Historical baseline duration for the current test (average of last 10 passed runs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_test_baseline_ms: Option<u64>,
    /// All currently running tests (cargo runs tests in parallel within a binary).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub running_tests: Vec<RunningTestSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningTestSnapshot {
    pub name: String,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ms: Option<u64>,
}

// ============ Errors ============

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NoDebugSymbols,
    SipBlocked,
    SessionExists,
    SessionNotFound,
    ProcessExited,
    FridaAttachFailed,
    InvalidPattern,
    WatchFailed,
    TestRunNotFound,
    ValidationError,
    ReadFailed,
    WriteFailed,
    UiQueryFailed,
    UiNotAvailable,
    InternalError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: ErrorCode,
    pub message: String,
}

impl From<crate::Error> for McpError {
    fn from(err: crate::Error) -> Self {
        let code = match &err {
            crate::Error::NoDebugSymbols => ErrorCode::NoDebugSymbols,
            crate::Error::SipBlocked => ErrorCode::SipBlocked,
            crate::Error::SessionExists => ErrorCode::SessionExists,
            crate::Error::SessionNotFound(_) => ErrorCode::SessionNotFound,
            crate::Error::ProcessExited(_) => ErrorCode::ProcessExited,
            crate::Error::FridaAttachFailed(_) => ErrorCode::FridaAttachFailed,
            crate::Error::InvalidPattern { .. } => ErrorCode::InvalidPattern,
            crate::Error::WatchFailed(_) => ErrorCode::WatchFailed,
            crate::Error::ValidationError(_) => ErrorCode::ValidationError,
            crate::Error::TestRunNotFound(_) => ErrorCode::TestRunNotFound,
            crate::Error::ReadFailed(_) => ErrorCode::ReadFailed,
            crate::Error::WriteFailed(_) => ErrorCode::WriteFailed,
            crate::Error::UiQueryFailed(_) => ErrorCode::UiQueryFailed,
            crate::Error::UiNotAvailable(_) => ErrorCode::UiNotAvailable,
            _ => ErrorCode::InternalError,
        };

        Self {
            code,
            message: err.to_string(),
        }
    }
}

// ============ debug_write ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTarget {
    /// DWARF variable name (e.g. "g_counter", "g_tempo")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    /// Raw hex address (e.g. "0x7ff800")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Value to write
    pub value: serde_json::Value,
    /// Type hint: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugWriteRequest {
    pub session_id: String,
    pub targets: Vec<WriteTarget>,
}

const VALID_WRITE_TYPE_HINTS: &[&str] = &[
    "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64",
    "f32", "f64", "pointer",
];

impl DebugWriteRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }
        if self.targets.is_empty() {
            return Err(crate::Error::ValidationError(
                "targets must not be empty".to_string()
            ));
        }
        if self.targets.len() > MAX_READ_TARGETS {
            return Err(crate::Error::ValidationError(
                format!("Too many targets ({}, max {})", self.targets.len(), MAX_READ_TARGETS)
            ));
        }
        for target in &self.targets {
            if target.variable.is_none() && target.address.is_none() {
                return Err(crate::Error::ValidationError(
                    "Each target must have either 'variable' or 'address'".to_string()
                ));
            }
            if target.address.is_some() && target.type_hint.is_none() {
                return Err(crate::Error::ValidationError(
                    "Raw address targets require 'type'".to_string()
                ));
            }
            if let Some(ref type_hint) = target.type_hint {
                if !VALID_WRITE_TYPE_HINTS.contains(&type_hint.as_str()) {
                    return Err(crate::Error::ValidationError(
                        format!("Invalid type '{}'. Valid: {}", type_hint, VALID_WRITE_TYPE_HINTS.join(", "))
                    ));
                }
            }
            if let Some(ref var) = target.variable {
                validate_watch_field(var, "variable")?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_value: Option<serde_json::Value>,
    pub new_value: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugWriteResponse {
    pub results: Vec<WriteResult>,
}

// ============ debug_breakpoint ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugBreakpointRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<BreakpointTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>, // Breakpoint IDs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakpointTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_count: Option<u32>,
    /// If present, this entry is a logpoint (non-blocking log on hit).
    /// Use {args[0]}, {args[1]} for arguments, {threadId} for thread ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DebugBreakpointRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }

        if let Some(targets) = &self.add {
            if targets.len() > MAX_BREAKPOINTS_PER_SESSION {
                return Err(crate::Error::ValidationError(
                    format!("Too many breakpoints: {} (max {})", targets.len(), MAX_BREAKPOINTS_PER_SESSION)
                ));
            }

            for target in targets {
                // Must specify either function OR file:line
                let has_function = target.function.is_some();
                let has_file_line = target.file.is_some() && target.line.is_some();

                if !has_function && !has_file_line {
                    return Err(crate::Error::ValidationError(
                        "Breakpoint target must specify either 'function' or 'file'+'line'".to_string()
                    ));
                }

                if has_function && has_file_line {
                    return Err(crate::Error::ValidationError(
                        "Breakpoint target cannot specify both 'function' and 'file'+'line'".to_string()
                    ));
                }

                if target.file.is_some() && target.line.is_none() {
                    return Err(crate::Error::ValidationError(
                        "Breakpoint with 'file' must also specify 'line'".to_string()
                    ));
                }

                if let Some(line) = target.line {
                    if line > MAX_LINE_NUMBER {
                        return Err(crate::Error::ValidationError(
                            format!("Line number {} exceeds maximum ({})", line, MAX_LINE_NUMBER)
                        ));
                    }
                }

                if let Some(ref condition) = target.condition {
                    if condition.len() > MAX_CONDITION_LENGTH {
                        return Err(crate::Error::ValidationError(
                            format!("Condition length ({} bytes) exceeds maximum of {} bytes",
                                condition.len(), MAX_CONDITION_LENGTH)
                        ));
                    }
                }

                // Logpoint-specific validation (message present)
                if let Some(ref message) = target.message {
                    if message.is_empty() {
                        return Err(crate::Error::ValidationError(
                            "Logpoint message must not be empty".to_string()
                        ));
                    }
                    if message.len() > MAX_LOGPOINT_MESSAGE_LENGTH {
                        return Err(crate::Error::ValidationError(
                            format!("Logpoint message length ({} bytes) exceeds maximum of {} bytes",
                                message.len(), MAX_LOGPOINT_MESSAGE_LENGTH)
                        ));
                    }
                    if target.hit_count.is_some() {
                        return Err(crate::Error::ValidationError(
                            "hit_count is not valid for logpoints (entries with 'message')".to_string()
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugBreakpointResponse {
    pub breakpoints: Vec<BreakpointInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub logpoints: Vec<LogpointInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakpointInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub address: String, // Hex
}

// ============ debug_continue ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugContinueRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>, // "continue", "step-over", "step-into", "step-out"
}

impl DebugContinueRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }

        if let Some(action) = &self.action {
            match action.as_str() {
                "continue" | "step-over" | "step-into" | "step-out" => {}
                _ => {
                    return Err(crate::Error::ValidationError(
                        format!("Invalid action '{}'. Must be: continue, step-over, step-into, step-out", action)
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugContinueResponse {
    pub status: String, // "paused", "running", "exited"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub breakpoint_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
}

// ============ debug_logpoint ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLogpointRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<LogpointTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>, // Logpoint IDs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogpointTarget {
    /// Log message template. Use `{args[0]}`, `{args[1]}` etc for arguments.
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl DebugLogpointRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }

        if let Some(targets) = &self.add {
            if targets.len() > MAX_LOGPOINTS_PER_SESSION {
                return Err(crate::Error::ValidationError(
                    format!("Too many logpoints: {} (max {})", targets.len(), MAX_LOGPOINTS_PER_SESSION)
                ));
            }

            for target in targets {
                let has_function = target.function.is_some();
                let has_file_line = target.file.is_some() && target.line.is_some();

                if !has_function && !has_file_line {
                    return Err(crate::Error::ValidationError(
                        "Logpoint target must specify either 'function' or 'file'+'line'".to_string()
                    ));
                }

                if target.message.is_empty() {
                    return Err(crate::Error::ValidationError(
                        "Logpoint message must not be empty".to_string()
                    ));
                }

                if target.message.len() > MAX_LOGPOINT_MESSAGE_LENGTH {
                    return Err(crate::Error::ValidationError(
                        format!("Logpoint message length ({} bytes) exceeds maximum of {} bytes",
                            target.message.len(), MAX_LOGPOINT_MESSAGE_LENGTH)
                    ));
                }

                if let Some(ref condition) = target.condition {
                    if condition.len() > MAX_CONDITION_LENGTH {
                        return Err(crate::Error::ValidationError(
                            format!("Condition length ({} bytes) exceeds maximum of {} bytes",
                                condition.len(), MAX_CONDITION_LENGTH)
                        ));
                    }
                }

                if let Some(line) = target.line {
                    if line > MAX_LINE_NUMBER {
                        return Err(crate::Error::ValidationError(
                            format!("Line number {} exceeds maximum ({})", line, MAX_LINE_NUMBER)
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLogpointResponse {
    pub logpoints: Vec<LogpointInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogpointInfo {
    pub id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub address: String,
}

// ============ debug_memory (consolidated read + write) ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    Read,
    Write,
}

impl Default for MemoryAction {
    fn default() -> Self { Self::Read }
}

/// Unified target for debug_memory — works for both read and write.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
    /// Value to write (required for action: "write", ignored for "read")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugMemoryRequest {
    pub session_id: String,
    #[serde(default)]
    pub action: MemoryAction,
    pub targets: Vec<MemoryTarget>,
    /// Max struct traversal depth for reads (1-5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Poll config for reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<PollConfig>,
}

impl DebugMemoryRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }
        if self.targets.is_empty() {
            return Err(crate::Error::ValidationError(
                "targets must not be empty".to_string()
            ));
        }
        match self.action {
            MemoryAction::Read => {
                // Delegate validation to DebugReadRequest
                let read_req = DebugReadRequest {
                    session_id: self.session_id.clone(),
                    targets: self.targets.iter().map(|t| ReadTarget {
                        variable: t.variable.clone(),
                        address: t.address.clone(),
                        size: t.size,
                        type_hint: t.type_hint.clone(),
                    }).collect(),
                    depth: self.depth,
                    poll: self.poll.clone(),
                };
                read_req.validate()
            }
            MemoryAction::Write => {
                // Reject write targets missing a value
                for target in &self.targets {
                    if target.value.is_none() {
                        return Err(crate::Error::ValidationError(
                            "Write targets must include 'value'".to_string(),
                        ));
                    }
                }
                // Delegate validation to DebugWriteRequest
                let write_req = DebugWriteRequest {
                    session_id: self.session_id.clone(),
                    targets: self.targets.iter().map(|t| WriteTarget {
                        variable: t.variable.clone(),
                        address: t.address.clone(),
                        value: t.value.clone().unwrap_or(serde_json::Value::Null),
                        type_hint: t.type_hint.clone(),
                    }).collect(),
                };
                write_req.validate()
            }
        }
    }
}

// ============ debug_session (consolidated stop + list + delete + status) ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    Status,
    Stop,
    List,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSessionRequest {
    pub action: SessionAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retain: Option<bool>,
}

impl DebugSessionRequest {
    pub fn validate(&self) -> crate::Result<()> {
        match self.action {
            SessionAction::Status | SessionAction::Stop | SessionAction::Delete => {
                if self.session_id.as_ref().map_or(true, |s| s.is_empty()) {
                    return Err(crate::Error::ValidationError(
                        format!("sessionId is required for action: {:?}", self.action)
                    ));
                }
            }
            SessionAction::List => {} // no sessionId needed
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacktraceFrame {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedArg {
    pub index: u32,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedThreadInfo {
    pub thread_id: u64,
    pub breakpoint_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub backtrace: Vec<BacktraceFrame>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub arguments: Vec<CapturedArg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusResponse {
    pub status: String,             // "running" | "paused" | "exited" | "crashed"
    pub pid: u32,
    pub event_count: u64,
    pub hooked_functions: u32,
    pub trace_patterns: Vec<String>,
    pub breakpoints: Vec<BreakpointInfo>,
    pub logpoints: Vec<LogpointInfo>,
    pub watches: Vec<ActiveWatch>,
    pub paused_threads: Vec<PausedThreadInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_info: Option<CrashSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrashSummary {
    pub signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exception_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_frame: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub throw_top_frame: Option<String>,
}

// ============ debug_ui ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMode {
    Tree,
    Screenshot,
    Both,
}

impl Default for UiMode {
    fn default() -> Self { Self::Tree }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiRequest {
    pub session_id: String,
    #[serde(default)]
    pub mode: UiMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
}

impl DebugUiRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStats {
    pub ax_nodes: usize,
    pub vision_nodes: usize,
    pub merged_nodes: usize,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    /// Absolute path to the saved PNG screenshot file (in `<projectRoot>/screenshots/`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<UiStats>,
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn test_debug_write_request_validation_valid_variable() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![WriteTarget {
                variable: Some("g_counter".to_string()),
                address: None,
                value: serde_json::json!(42),
                type_hint: None,
            }],
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_write_request_validation_valid_address() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![WriteTarget {
                variable: None,
                address: Some("0x7ff800".to_string()),
                value: serde_json::json!(100),
                type_hint: Some("u32".to_string()),
            }],
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_write_request_validation_empty_targets() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![],
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_write_request_validation_no_variable_or_address() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![WriteTarget {
                variable: None,
                address: None,
                value: serde_json::json!(42),
                type_hint: None,
            }],
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_write_request_validation_address_requires_type() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![WriteTarget {
                variable: None,
                address: Some("0x1000".to_string()),
                value: serde_json::json!(42),
                type_hint: None, // missing
            }],
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_write_request_validation_invalid_type() {
        let req = DebugWriteRequest {
            session_id: "s1".to_string(),
            targets: vec![WriteTarget {
                variable: None,
                address: Some("0x1000".to_string()),
                value: serde_json::json!(42),
                type_hint: Some("bytes".to_string()), // not valid for writes
            }],
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_ui_request_serde() {
        let req: DebugUiRequest = serde_json::from_str(r#"{"sessionId": "s1", "mode": "tree"}"#).unwrap();
        assert_eq!(req.session_id, "s1");
        assert_eq!(req.mode, UiMode::Tree);
        assert!(req.vision.is_none());
    }

    #[test]
    fn test_debug_ui_request_validation() {
        let req = DebugUiRequest {
            session_id: "".to_string(),
            mode: UiMode::Tree,
            vision: None,
            verbose: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_ui_response_serde() {
        let resp = DebugUiResponse {
            tree: Some("[window \"Test\" id=w1]".to_string()),
            screenshot: None,
            stats: Some(UiStats { ax_nodes: 5, vision_nodes: 0, merged_nodes: 0, latency_ms: 12 }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("tree").is_some());
        assert!(json.get("screenshot").is_none()); // skip_serializing_if
        assert_eq!(json["stats"]["axNodes"], 5);
    }
}

#[cfg(test)]
mod breakpoint_tests {
    use super::*;

    #[test]
    fn test_debug_breakpoint_request_validation() {
        // Valid: function target
        let req = DebugBreakpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![BreakpointTarget {
                function: Some("foo".to_string()),
                file: None,
                line: None,
                condition: None,
                hit_count: None,
                message: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_ok());

        // Valid: file:line target
        let req = DebugBreakpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![BreakpointTarget {
                function: None,
                file: Some("main.cpp".to_string()),
                line: Some(42),
                condition: None,
                hit_count: None,
                message: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_ok());

        // Invalid: neither function nor file:line
        let req = DebugBreakpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![BreakpointTarget {
                function: None,
                file: None,
                line: None,
                condition: None,
                hit_count: None,
                message: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_err());

        // Invalid: file without line
        let req = DebugBreakpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![BreakpointTarget {
                function: None,
                file: Some("main.cpp".to_string()),
                line: None,
                condition: None,
                hit_count: None,
                message: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_continue_request_validation() {
        // Valid: no action (defaults to continue)
        let req = DebugContinueRequest {
            session_id: "test".to_string(),
            action: None,
        };
        assert!(req.validate().is_ok());

        // Valid: continue action
        let req = DebugContinueRequest {
            session_id: "test".to_string(),
            action: Some("continue".to_string()),
        };
        assert!(req.validate().is_ok());

        // Valid: step-over action (for Phase 2b)
        let req = DebugContinueRequest {
            session_id: "test".to_string(),
            action: Some("step-over".to_string()),
        };
        assert!(req.validate().is_ok());

        // Invalid: empty session_id
        let req = DebugContinueRequest {
            session_id: "".to_string(),
            action: None,
        };
        assert!(req.validate().is_err());

        // Invalid: unknown action
        let req = DebugContinueRequest {
            session_id: "test".to_string(),
            action: Some("invalid-action".to_string()),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_logpoint_request_validation() {
        // Valid: function logpoint
        let req = DebugLogpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![LogpointTarget {
                message: "hit: {args[0]}".to_string(),
                function: Some("foo".to_string()),
                file: None,
                line: None,
                condition: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_ok());

        // Valid: file:line logpoint
        let req = DebugLogpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![LogpointTarget {
                message: "reached line 42".to_string(),
                function: None,
                file: Some("main.cpp".to_string()),
                line: Some(42),
                condition: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_ok());

        // Invalid: empty message
        let req = DebugLogpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![LogpointTarget {
                message: "".to_string(),
                function: Some("foo".to_string()),
                file: None,
                line: None,
                condition: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_err());

        // Invalid: no function or file:line
        let req = DebugLogpointRequest {
            session_id: "test".to_string(),
            add: Some(vec![LogpointTarget {
                message: "hello".to_string(),
                function: None,
                file: None,
                line: None,
                condition: None,
            }]),
            remove: None,
        };
        assert!(req.validate().is_err());

        // Invalid: empty session_id
        let req = DebugLogpointRequest {
            session_id: "".to_string(),
            add: None,
            remove: None,
        };
        assert!(req.validate().is_err());
    }
}

#[cfg(test)]
mod read_tests {
    use super::*;

    #[test]
    fn test_debug_read_request_validation_empty_targets() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_too_many_targets() {
        let targets: Vec<ReadTarget> = (0..17).map(|i| ReadTarget {
            variable: Some(format!("var{}", i)),
            address: None,
            size: None,
            type_hint: None,
        }).collect();
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets,
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_valid() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_read_request_validation_poll_limits() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: None,
            poll: Some(PollConfig {
                interval_ms: 10,  // below min 50
                duration_ms: 2000,
            }),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_depth_limits() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: Some(10),  // above max 5
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_raw_address_requires_size_and_type() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x7ff800".to_string()),
                size: None,  // missing
                type_hint: None,  // missing
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_depth_zero() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None, size: None, type_hint: None,
            }],
            depth: Some(0),
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_poll_interval_too_high() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None, size: None, type_hint: None,
            }],
            depth: None,
            poll: Some(PollConfig {
                interval_ms: 6000,  // above max 5000
                duration_ms: 10000,
            }),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_poll_duration_too_low() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None, size: None, type_hint: None,
            }],
            depth: None,
            poll: Some(PollConfig {
                interval_ms: 100,
                duration_ms: 50,  // below min 100
            }),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_poll_duration_too_high() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None, size: None, type_hint: None,
            }],
            depth: None,
            poll: Some(PollConfig {
                interval_ms: 100,
                duration_ms: 40000,  // above max 30000
            }),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_invalid_type_hint() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x1000".to_string()),
                size: Some(4),
                type_hint: Some("int64".to_string()),  // invalid — should be "i64"
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_size_zero() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x1000".to_string()),
                size: Some(0),  // invalid
                type_hint: Some("u32".to_string()),
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_size_too_large() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x1000".to_string()),
                size: Some(100000),  // above max 65536
                type_hint: Some("bytes".to_string()),
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_no_variable_or_address() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_valid_raw_address() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x7ff800".to_string()),
                size: Some(64),
                type_hint: Some("bytes".to_string()),
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_read_request_validation_valid_poll() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None, size: None, type_hint: None,
            }],
            depth: Some(1),
            poll: Some(PollConfig {
                interval_ms: 100,
                duration_ms: 2000,
            }),
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_read_request_validation_all_valid_type_hints() {
        let valid_types = ["i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64",
                           "f32", "f64", "pointer", "bytes"];
        for type_hint in valid_types {
            let req = DebugReadRequest {
                session_id: "s1".to_string(),
                targets: vec![ReadTarget {
                    variable: None,
                    address: Some("0x1000".to_string()),
                    size: Some(8),
                    type_hint: Some(type_hint.to_string()),
                }],
                depth: None,
                poll: None,
            };
            assert!(req.validate().is_ok(), "type '{}' should be valid", type_hint);
        }
    }
}

#[cfg(test)]
mod event_type_filter_tests {
    use super::*;

    #[test]
    fn test_event_type_filter_pause() {
        let json = serde_json::json!("pause");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::Pause));
    }

    #[test]
    fn test_event_type_filter_logpoint() {
        let json = serde_json::json!("logpoint");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::Logpoint));
    }

    #[test]
    fn test_event_type_filter_condition_error() {
        let json = serde_json::json!("condition_error");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::ConditionError));
    }
}

#[cfg(test)]
mod query_pagination_tests {
    use super::*;

    #[test]
    fn test_query_request_with_after_event_id() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "afterEventId": 42
        });
        let req: DebugQueryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.after_event_id, Some(42));
    }

    #[test]
    fn test_query_response_has_cursor_fields() {
        let resp = DebugQueryResponse {
            events: vec![],
            total_count: 0,
            has_more: false,
            pids: None,
            last_event_id: Some(99),
            events_dropped: Some(false),
            crash: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["lastEventId"], 99);
        assert_eq!(json["eventsDropped"], false);
    }
}

#[cfg(test)]
mod unified_breakpoint_tests {
    use super::*;

    #[test]
    fn test_breakpoint_target_with_message_is_logpoint() {
        let json = serde_json::json!({
            "function": "foo",
            "message": "hit: {args[0]}"
        });
        let target: BreakpointTarget = serde_json::from_value(json).unwrap();
        assert_eq!(target.message.as_deref(), Some("hit: {args[0]}"));
    }

    #[test]
    fn test_breakpoint_target_without_message_is_breakpoint() {
        let json = serde_json::json!({
            "function": "foo",
            "condition": "args[0] > 100"
        });
        let target: BreakpointTarget = serde_json::from_value(json).unwrap();
        assert!(target.message.is_none());
    }

    #[test]
    fn test_breakpoint_response_includes_logpoints() {
        let resp = DebugBreakpointResponse {
            breakpoints: vec![BreakpointInfo {
                id: "bp-1".to_string(),
                function: Some("foo".to_string()),
                file: None,
                line: None,
                address: "0x1000".to_string(),
            }],
            logpoints: vec![LogpointInfo {
                id: "lp-1".to_string(),
                message: "hit".to_string(),
                function: Some("bar".to_string()),
                file: None,
                line: None,
                address: "0x2000".to_string(),
            }],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["breakpoints"].as_array().unwrap().len(), 1);
        assert_eq!(json["logpoints"].as_array().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod memory_consolidation_tests {
    use super::*;

    #[test]
    fn test_memory_read_request() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "action": "read",
            "targets": [{ "variable": "gTempo" }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Read);
        assert_eq!(req.targets.len(), 1);
    }

    #[test]
    fn test_memory_write_request() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "action": "write",
            "targets": [{ "variable": "g_counter", "value": 42 }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Write);
    }

    #[test]
    fn test_memory_action_default_read() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "targets": [{ "variable": "gTempo" }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Read);
    }
}

#[cfg(test)]
mod test_consolidation_tests {
    use super::*;

    #[test]
    fn test_debug_test_with_action_run() {
        let json = serde_json::json!({
            "action": "run",
            "projectRoot": "/tmp/proj"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, Some(TestAction::Run));
    }

    #[test]
    fn test_debug_test_with_action_status() {
        let json = serde_json::json!({
            "action": "status",
            "testRunId": "tr-123"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, Some(TestAction::Status));
        assert_eq!(req.test_run_id.as_deref(), Some("tr-123"));
    }

    #[test]
    fn test_debug_test_default_action_is_run() {
        let json = serde_json::json!({
            "projectRoot": "/tmp/proj"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert!(req.action.is_none()); // None treated as "run"
    }
}

#[cfg(test)]
mod session_consolidation_tests {
    use super::*;

    #[test]
    fn test_session_action_serde() {
        let json = serde_json::json!({ "action": "status", "sessionId": "s1" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::Status);
        assert_eq!(req.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn test_session_action_list_no_session_id() {
        let json = serde_json::json!({ "action": "list" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::List);
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_session_status_requires_session_id() {
        let json = serde_json::json!({ "action": "status" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_session_stop_requires_session_id() {
        let json = serde_json::json!({ "action": "stop" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_session_delete_requires_session_id() {
        let json = serde_json::json!({ "action": "delete" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_session_stop_with_retain() {
        let json = serde_json::json!({ "action": "stop", "sessionId": "s1", "retain": true });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::Stop);
        assert_eq!(req.retain, Some(true));
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_session_status_response_serde() {
        let resp = SessionStatusResponse {
            status: "running".to_string(),
            pid: 1234,
            event_count: 100,
            hooked_functions: 5,
            trace_patterns: vec!["foo::*".to_string()],
            breakpoints: vec![],
            logpoints: vec![],
            watches: vec![],
            paused_threads: vec![],
            crash_info: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "running");
        assert_eq!(json["pid"], 1234);
        assert_eq!(json["eventCount"], 100);
    }

    #[test]
    fn test_paused_thread_info_serde() {
        let info = PausedThreadInfo {
            thread_id: 42,
            breakpoint_id: "bp-1".to_string(),
            function: Some("main".to_string()),
            file: None,
            line: None,
            backtrace: Vec::new(),
            arguments: Vec::new(),
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["threadId"], 42);
        assert_eq!(json["breakpointId"], "bp-1");
        assert_eq!(json["function"], "main");
        // file/line should be omitted (skip_serializing_if)
        assert!(json.get("file").is_none());
    }
}
