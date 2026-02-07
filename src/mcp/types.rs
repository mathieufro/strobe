use serde::{Deserialize, Serialize};

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
    /// Maximum events to keep for this session (default: 200,000)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_limit: Option<usize>,
    /// Maximum depth for recursive argument serialization (default: 3, max: 10)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serialization_depth: Option<u32>,
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
pub const MAX_EVENT_LIMIT: usize = 10_000_000;
pub const MAX_WATCHES_PER_SESSION: usize = 32;
pub const MAX_WATCH_EXPRESSION_LENGTH: usize = 256;
pub const MAX_WATCH_EXPRESSION_DEPTH: usize = 4;

impl DebugTraceRequest {
    /// Validate request parameters against limits
    pub fn validate(&self) -> crate::Result<()> {
        // Validate event_limit
        if let Some(limit) = self.event_limit {
            if limit > MAX_EVENT_LIMIT {
                return Err(crate::Error::ValidationError(
                    format!("event_limit ({}) exceeds maximum of {}", limit, MAX_EVENT_LIMIT)
                ));
            }
        }

        // Validate serialization depth
        if let Some(depth) = self.serialization_depth {
            if depth < 1 || depth > 10 {
                return Err(crate::Error::ValidationError(
                    "serialization_depth must be between 1 and 10".to_string()
                ));
            }
        }

        // Validate watches
        if let Some(ref watch_update) = self.watches {
            if let Some(ref add_watches) = watch_update.add {
                // Check watch count
                if add_watches.len() > MAX_WATCHES_PER_SESSION {
                    return Err(crate::Error::ValidationError(
                        format!("Cannot add {} watches (max {})", add_watches.len(), MAX_WATCHES_PER_SESSION)
                    ));
                }

                // Validate each watch
                for watch in add_watches {
                    // Check expression length and depth
                    if let Some(ref expr) = watch.expr {
                        if expr.len() > MAX_WATCH_EXPRESSION_LENGTH {
                            return Err(crate::Error::ValidationError(
                                format!("Watch expression length ({} bytes) exceeds maximum of {} bytes",
                                    expr.len(), MAX_WATCH_EXPRESSION_LENGTH)
                            ));
                        }

                        // Check expression depth (count -> and . operators)
                        let depth = expr.matches("->").count() + expr.matches('.').count();
                        if depth > MAX_WATCH_EXPRESSION_DEPTH {
                            return Err(crate::Error::ValidationError(
                                format!("Watch expression depth ({}) exceeds maximum of {}",
                                    depth, MAX_WATCH_EXPRESSION_DEPTH)
                            ));
                        }
                    }

                    // Check variable length and depth
                    if let Some(ref var) = watch.variable {
                        if var.len() > MAX_WATCH_EXPRESSION_LENGTH {
                            return Err(crate::Error::ValidationError(
                                format!("Watch variable length ({} bytes) exceeds maximum of {} bytes",
                                    var.len(), MAX_WATCH_EXPRESSION_LENGTH)
                            ));
                        }

                        let depth = var.matches("->").count() + var.matches('.').count();
                        if depth > MAX_WATCH_EXPRESSION_DEPTH {
                            return Err(crate::Error::ValidationError(
                                format!("Watch variable depth ({}) exceeds maximum of {}",
                                    depth, MAX_WATCH_EXPRESSION_DEPTH)
                            ));
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

// ============ debug_query ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
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
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugQueryResponse {
    pub events: Vec<serde_json::Value>,
    pub total_count: u64,
    pub has_more: bool,
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
    ValidationError,
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
            _ => ErrorCode::FridaAttachFailed, // Generic fallback
        };

        Self {
            code,
            message: err.to_string(),
        }
    }
}
