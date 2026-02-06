use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("NO_DEBUG_SYMBOLS: Binary has no DWARF debug info. Ask user permission to modify build configuration to compile with debug symbols.")]
    NoDebugSymbols,

    #[error("SIP_BLOCKED: macOS System Integrity Protection prevents Frida attachment.")]
    SipBlocked,

    #[error("SESSION_EXISTS: Session already active for this binary. Call debug_stop first.")]
    SessionExists,

    #[error("SESSION_NOT_FOUND: No session found with ID '{0}'.")]
    SessionNotFound(String),

    #[error("PROCESS_EXITED: Target process has exited (code: {0}). Session still queryable.")]
    ProcessExited(i32),

    #[error("FRIDA_ATTACH_FAILED: Failed to attach Frida: {0}")]
    FridaAttachFailed(String),

    #[error("INVALID_PATTERN: Invalid trace pattern '{pattern}': {reason}")]
    InvalidPattern { pattern: String, reason: String },

    #[error("WATCH_FAILED: {0}")]
    WatchFailed(String),

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Frida error: {0}")]
    Frida(String),
}

pub type Result<T> = std::result::Result<T, Error>;
