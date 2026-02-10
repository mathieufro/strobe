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
            after_event_id: None,
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

    #[test]
    fn test_watch_types_serialization() {
        let target = WatchTarget {
            variable: Some("gClock->counter".to_string()),
            address: None,
            type_hint: None,
            label: None,
            expr: None,
            on: Some(vec!["NoteOn".to_string()]),
        };
        let json = serde_json::to_string(&target).unwrap();
        assert!(json.contains("gClock->counter"));
        assert!(json.contains("NoteOn"));

        let update = WatchUpdate {
            add: Some(vec![target]),
            remove: Some(vec!["old_watch".to_string()]),
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("gClock->counter"));
        assert!(json.contains("old_watch"));
    }

    #[test]
    fn test_initialize_response_has_instructions() {
        let response = McpInitializeResponse {
            protocol_version: "2024-11-05".to_string(),
            capabilities: McpServerCapabilities {
                tools: McpToolsCapability { list_changed: false },
            },
            server_info: McpServerInfo {
                name: "strobe".to_string(),
                version: "0.1.0".to_string(),
            },
            instructions: Some("Test instructions".to_string()),
        };

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("instructions"));
        assert!(json.contains("Test instructions"));

        let response_no = McpInitializeResponse {
            protocol_version: "2024-11-05".to_string(),
            capabilities: McpServerCapabilities {
                tools: McpToolsCapability { list_changed: false },
            },
            server_info: McpServerInfo {
                name: "strobe".to_string(),
                version: "0.1.0".to_string(),
            },
            instructions: None,
        };

        let json = serde_json::to_string(&response_no).unwrap();
        assert!(!json.contains("instructions"));
    }

    #[test]
    fn test_watch_on_field_patterns() {
        let watch_with_on = WatchTarget {
            variable: Some("gCounter".to_string()),
            address: None,
            type_hint: Some("int".to_string()),
            label: Some("counter".to_string()),
            expr: None,
            on: Some(vec!["audio::process".to_string(), "midi::*".to_string()]),
        };

        assert_eq!(watch_with_on.on.as_ref().unwrap().len(), 2);
        assert_eq!(watch_with_on.on.as_ref().unwrap()[0], "audio::process");

        let global_watch = WatchTarget {
            variable: Some("gTempo".to_string()),
            address: None,
            type_hint: Some("float".to_string()),
            label: Some("tempo".to_string()),
            expr: None,
            on: None,
        };
        assert!(global_watch.on.is_none());

        let update = WatchUpdate {
            add: Some(vec![watch_with_on, global_watch]),
            remove: Some(vec!["old_watch".to_string()]),
        };
        assert_eq!(update.add.as_ref().unwrap().len(), 2);
        assert_eq!(update.remove.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_too_many_watches() {
        let watches: Vec<WatchTarget> = (0..33)
            .map(|i| WatchTarget {
                variable: Some(format!("var{}", i)),
                address: None, type_hint: None, label: None, expr: None, on: None,
            })
            .collect();

        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None,
            watches: Some(WatchUpdate { add: Some(watches), remove: None }),
            project_root: None, serialization_depth: None,
        };

        let result = req.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("32"));
    }

    #[test]
    fn test_watch_expression_too_long() {
        let long_expr = "a".repeat(257);

        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None,
            watches: Some(WatchUpdate {
                add: Some(vec![WatchTarget {
                    variable: None, address: None, type_hint: None,
                    label: Some("test".to_string()),
                    expr: Some(long_expr), on: None,
                }]),
                remove: None,
            }),
            project_root: None, serialization_depth: None,
        };

        let result = req.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("256"));
    }

    #[test]
    fn test_watch_expression_too_deep() {
        let deep_expr = "a->b->c->d->e->f->g->h->i->j->k->l";

        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None,
            watches: Some(WatchUpdate {
                add: Some(vec![WatchTarget {
                    variable: None, address: None, type_hint: None,
                    label: Some("test".to_string()),
                    expr: Some(deep_expr.to_string()), on: None,
                }]),
                remove: None,
            }),
            project_root: None, serialization_depth: None,
        };

        let result = req.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("depth"));
    }

    #[test]
    fn test_valid_trace_request() {
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: Some(vec!["foo::*".to_string()]),
            remove: None,
            watches: Some(WatchUpdate {
                add: Some(vec![WatchTarget {
                    variable: Some("gCounter".to_string()),
                    address: None, type_hint: None,
                    label: Some("counter".to_string()),
                    expr: None, on: Some(vec!["process::*".to_string()]),
                }]),
                remove: None,
            }),
            project_root: None, serialization_depth: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_serialization_depth_validation() {
        // Zero rejected
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None, watches: None,
            project_root: None, serialization_depth: Some(0),
        };
        assert!(req.validate().is_err());

        // 11 rejected
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None, watches: None,
            project_root: None, serialization_depth: Some(11),
        };
        assert!(req.validate().is_err());

        // 1..=10 all valid
        for depth in 1..=10 {
            let req = DebugTraceRequest {
                session_id: Some("test".to_string()),
                add: Some(vec!["foo::*".to_string()]),
                remove: None, watches: None,
                project_root: None, serialization_depth: Some(depth),
            };
            assert!(req.validate().is_ok(), "depth={} should be valid", depth);
        }

        // None is valid
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None, watches: None,
            project_root: None, serialization_depth: None,
        };
        assert!(req.validate().is_ok());

        // Large values rejected
        for depth in [100, 255, 1000, u32::MAX] {
            let req = DebugTraceRequest {
                session_id: Some("test".to_string()),
                add: None, remove: None, watches: None,
                project_root: None, serialization_depth: Some(depth),
            };
            assert!(req.validate().is_err(), "depth={} should be rejected", depth);
        }
    }

    #[test]
    fn test_serialization_depth_json_roundtrip() {
        let req = DebugTraceRequest {
            session_id: Some("test-123".to_string()),
            add: Some(vec!["audio::*".to_string()]),
            remove: None, watches: None,
            project_root: None, serialization_depth: Some(5),
        };

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("serializationDepth"));

        let parsed: DebugTraceRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.serialization_depth, Some(5));

        // None should be omitted
        let req_none = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None, remove: None, watches: None,
            project_root: None, serialization_depth: None,
        };
        let json = serde_json::to_string(&req_none).unwrap();
        assert!(!json.contains("serializationDepth"));

        // From MCP client format
        let json = r#"{"sessionId":"test","add":["foo::*"],"serializationDepth":3}"#;
        let req: DebugTraceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.serialization_depth, Some(3));
        assert!(req.validate().is_ok());
    }
}
