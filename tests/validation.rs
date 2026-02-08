use strobe::mcp::{DebugTraceRequest, WatchTarget, WatchUpdate};

#[test]
fn test_too_many_watches() {
    let watches: Vec<WatchTarget> = (0..33)
        .map(|i| WatchTarget {
            variable: Some(format!("var{}", i)),
            address: None,
            type_hint: None,
            label: None,
            expr: None,
            on: None,
        })
        .collect();

    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(watches),
            remove: None,
        }),
        project_root: None,
        serialization_depth: None,
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("32"));
}

#[test]
fn test_watch_expression_too_long() {
    let long_expr = "a".repeat(257); // Over 256 byte limit

    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![WatchTarget {
                variable: None,
                address: None,
                type_hint: None,
                label: Some("test".to_string()),
                expr: Some(long_expr),
                on: None,
            }]),
            remove: None,
        }),
        project_root: None,
        serialization_depth: None,
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("256"));
}

#[test]
fn test_watch_expression_too_deep() {
    // 11 dereference operators (over 10 limit)
    let deep_expr = "a->b->c->d->e->f->g->h->i->j->k->l";

    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![WatchTarget {
                variable: None,
                address: None,
                type_hint: None,
                label: Some("test".to_string()),
                expr: Some(deep_expr.to_string()),
                on: None,
            }]),
            remove: None,
        }),
        project_root: None,
        serialization_depth: None,
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("depth"));
}

#[test]
fn test_valid_requests_pass() {
    // Within all limits
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: Some(vec!["foo::*".to_string()]),
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![WatchTarget {
                variable: Some("gCounter".to_string()),
                address: None,
                type_hint: None,
                label: Some("counter".to_string()),
                expr: None,
                on: Some(vec!["process::*".to_string()]),
            }]),
            remove: None,
        }),
        project_root: None,
        serialization_depth: None,
    };

    let result = req.validate();
    assert!(result.is_ok());
}

// ============ Serialization Depth Validation ============

#[test]
fn test_serialization_depth_zero_rejected() {
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(0),
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("serialization_depth must be between 1 and 10"));
}

#[test]
fn test_serialization_depth_exceeds_max() {
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(11),
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("serialization_depth must be between 1 and 10"));
}

#[test]
fn test_serialization_depth_valid_range() {
    // Test all valid values 1..=10
    for depth in 1..=10 {
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: Some(vec!["foo::*".to_string()]),
            remove: None,
            watches: None,
            project_root: None,
            serialization_depth: Some(depth),
        };

        let result = req.validate();
        assert!(result.is_ok(), "depth={} should be valid", depth);
    }
}

#[test]
fn test_serialization_depth_none_is_valid() {
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: None,
    };

    assert!(req.validate().is_ok());
}

#[test]
fn test_serialization_depth_boundary_values() {
    // Just below minimum
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(0),
    };
    assert!(req.validate().is_err());

    // Minimum valid
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(1),
    };
    assert!(req.validate().is_ok());

    // Maximum valid
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(10),
    };
    assert!(req.validate().is_ok());

    // Just above maximum
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(11),
    };
    assert!(req.validate().is_err());
}

#[test]
fn test_serialization_depth_with_other_params() {
    // Serialization depth combined with other valid params
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: Some(vec!["foo::*".to_string()]),
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![WatchTarget {
                variable: Some("gCounter".to_string()),
                address: None,
                type_hint: None,
                label: Some("counter".to_string()),
                expr: None,
                on: None,
            }]),
            remove: None,
        }),
        project_root: None,
        serialization_depth: Some(5),
    };

    assert!(req.validate().is_ok());
}

#[test]
fn test_serialization_depth_invalid_with_valid_params() {
    // Invalid depth should fail even with other valid params
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(0),
    };

    assert!(req.validate().is_err());
}

#[test]
fn test_serialization_depth_json_roundtrip() {
    // Test camelCase serialization
    let req = DebugTraceRequest {
        session_id: Some("test-123".to_string()),
        add: Some(vec!["audio::*".to_string()]),
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: Some(5),
    };

    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("serializationDepth"));
    assert!(json.contains("5"));

    let parsed: DebugTraceRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.serialization_depth, Some(5));
}

#[test]
fn test_serialization_depth_omitted_from_json_when_none() {
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        project_root: None,
        serialization_depth: None,
    };

    let json = serde_json::to_string(&req).unwrap();
    assert!(!json.contains("serializationDepth"), "None field should be omitted from JSON");
}

#[test]
fn test_serialization_depth_deserialization_from_mcp() {
    // Simulate what an MCP client would send
    let json = r#"{"sessionId":"test","add":["foo::*"],"serializationDepth":3}"#;
    let req: DebugTraceRequest = serde_json::from_str(json).unwrap();

    assert_eq!(req.serialization_depth, Some(3));
    assert!(req.validate().is_ok());
}

#[test]
fn test_serialization_depth_large_values_rejected() {
    for depth in [100, 255, 1000, u32::MAX] {
        let req = DebugTraceRequest {
            session_id: Some("test".to_string()),
            add: None,
            remove: None,
            watches: None,
            project_root: None,
            serialization_depth: Some(depth),
        };
        assert!(req.validate().is_err(), "depth={} should be rejected", depth);
    }
}
