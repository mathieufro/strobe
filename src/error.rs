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

    #[error("VALIDATION_ERROR: {0}")]
    ValidationError(String),

    #[error("READ_FAILED: {0}")]
    ReadFailed(String),

    #[error("WRITE_FAILED: {0}")]
    WriteFailed(String),

    #[error("UI_QUERY_FAILED: {0}")]
    UiQueryFailed(String),

    #[error("UI_NOT_AVAILABLE: {0}")]
    UiNotAvailable(String),

    #[error("TEST_RUN_NOT_FOUND: No test run found with ID '{0}'.")]
    TestRunNotFound(String),

    #[error("TEST_ALREADY_RUNNING: A test is already running (ID: '{0}'). Wait for it to complete or poll its status before starting another. Only one test run at a time.")]
    TestAlreadyRunning(String),

    #[error("NO_CODE_AT_LINE: No executable code at {file}:{line}. Valid lines: {nearest_lines}")]
    NoCodeAtLine { file: String, line: u32, nearest_lines: String },

    #[error("OPTIMIZED_OUT: Variable '{variable}' is optimized out at this PC. Recompile with -O0.")]
    OptimizedOut { variable: String },

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Frida error: {0}")]
    Frida(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_types() {
        let err = Error::NoDebugSymbols;
        assert!(err.to_string().contains("NO_DEBUG_SYMBOLS"));

        let err = Error::SessionNotFound("test".to_string());
        assert!(err.to_string().contains("test"));

        let err = Error::InvalidPattern {
            pattern: "**".to_string(),
            reason: "bad pattern".to_string(),
        };
        assert!(err.to_string().contains("**"));
    }

    #[test]
    fn test_breakpoint_error_types() {
        let err = Error::NoCodeAtLine {
            file: "test.cpp".to_string(),
            line: 100,
            nearest_lines: "98, 102, 105".to_string(),
        };
        assert!(err.to_string().contains("NO_CODE_AT_LINE"));
        assert!(err.to_string().contains("test.cpp:100"));
        assert!(err.to_string().contains("98, 102, 105"));

        let err = Error::OptimizedOut {
            variable: "x".to_string(),
        };
        assert!(err.to_string().contains("OPTIMIZED_OUT"));
        assert!(err.to_string().contains("Variable 'x'"));
        assert!(err.to_string().contains("-O0"));
    }
}
