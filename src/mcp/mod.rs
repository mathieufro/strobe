mod types;
mod protocol;
mod proxy;

pub use types::*;
pub use protocol::*;
pub use proxy::stdio_proxy;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_launch_request_serialization() {
        let req = DebugLaunchRequest {
            command: "/path/to/app".to_string(),
            args: Some(vec!["--verbose".to_string()]),
            cwd: None,
            project_root: "/home/user/project".to_string(),
            env: None,
        };

        let json = serde_json::to_string(&req).unwrap();
        let parsed: DebugLaunchRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.command, "/path/to/app");
        assert_eq!(parsed.project_root, "/home/user/project");
    }

    #[test]
    fn test_query_request_filters() {
        let req = DebugQueryRequest {
            session_id: "test-session".to_string(),
            event_type: Some(EventTypeFilter::FunctionExit),
            function: Some(FunctionFilter {
                equals: None,
                contains: Some("validate".to_string()),
                matches: None,
            }),
            source_file: None,
            return_value: None,
            thread_name: None,
            time_from: None,
            time_to: None,
            min_duration_ns: None,
            pid: None,
            limit: Some(100),
            offset: None,
            verbose: Some(true),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("validate"));
        assert!(json.contains("function_exit"));
    }

    #[test]
    fn test_error_code_serialization() {
        let err = McpError {
            code: ErrorCode::SessionNotFound,
            message: "Session 'test' not found".to_string(),
        };

        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("SESSION_NOT_FOUND"));
    }
}
