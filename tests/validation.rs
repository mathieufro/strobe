use strobe::mcp::{DebugTraceRequest, WatchTarget, WatchUpdate};

#[test]
fn test_event_limit_too_large() {
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        event_limit: Some(11_000_000), // Over 10M limit
    };

    // Validation should fail
    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("10000000"));
}

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
        event_limit: None,
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("32"));
}

#[test]
fn test_watch_expression_too_long() {
    let long_expr = "a".repeat(1025); // Over 1KB

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
        event_limit: None,
    };

    let result = req.validate();
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("1024"));
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
        event_limit: None,
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
        event_limit: Some(500_000), // Well under 10M
    };

    let result = req.validate();
    assert!(result.is_ok());
}
