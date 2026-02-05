use std::path::PathBuf;
use tempfile::tempdir;

// Test helper to create a simple test binary
#[cfg(target_os = "linux")]
fn create_test_binary(dir: &std::path::Path) -> PathBuf {
    let src = r#"
        fn process(x: i32) -> i32 {
            x * 2
        }

        fn main() {
            let result = process(21);
            println!("Result: {}", result);
        }
    "#;

    let src_path = dir.join("test.rs");
    std::fs::write(&src_path, src).unwrap();

    let out_path = dir.join("test_binary");

    // Compile with debug info
    let status = std::process::Command::new("rustc")
        .args(["-g", "-o"])
        .arg(&out_path)
        .arg(&src_path)
        .status()
        .expect("Failed to compile test binary");

    assert!(status.success(), "Test binary compilation failed");
    out_path
}

#[test]
fn test_database_roundtrip() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    let db = strobe::db::Database::open(&db_path).unwrap();

    // Create session
    let session = db.create_session(
        "test-session",
        "/bin/test",
        "/home/user",
        1234,
    ).unwrap();

    assert_eq!(session.id, "test-session");

    // Insert event
    db.insert_event(strobe::db::Event {
        id: "evt-1".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1000,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::FunctionEnter,
        function_name: "main".to_string(),
        function_name_raw: None,
        source_file: Some("/home/user/main.rs".to_string()),
        line_number: Some(1),
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: None,
    }).unwrap();

    // Query
    let events = db.query_events("test-session", |q| q).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].function_name, "main");
}

#[test]
fn test_mcp_types_serialization() {
    let req = strobe::mcp::DebugLaunchRequest {
        command: "/path/to/app".to_string(),
        args: Some(vec!["--verbose".to_string()]),
        cwd: None,
        project_root: "/home/user/project".to_string(),
        env: None,
    };

    let json = serde_json::to_string(&req).unwrap();
    let parsed: strobe::mcp::DebugLaunchRequest = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.command, "/path/to/app");
}

#[test]
fn test_pattern_matching() {
    use strobe::dwarf::PatternMatcher;

    let m = PatternMatcher::new("foo::*");
    assert!(m.matches("foo::bar"));
    assert!(!m.matches("foo::bar::baz"));

    let m2 = PatternMatcher::new("foo::**");
    assert!(m2.matches("foo::bar::baz"));
}

#[test]
fn test_symbol_demangling() {
    let rust_mangled = "_ZN4test7example17h1234567890abcdefE";
    let demangled = strobe::symbols::demangle_symbol(rust_mangled);
    assert!(demangled.contains("test::example"));
}

#[cfg(target_os = "linux")]
#[test]
fn test_dwarf_parsing() {
    let dir = tempdir().unwrap();
    let binary = create_test_binary(dir.path());

    let parser = strobe::dwarf::DwarfParser::parse(&binary).unwrap();

    // Should find our test functions
    let main_funcs = parser.find_by_name("main");
    assert!(!main_funcs.is_empty(), "Should find main function");
}

#[test]
fn test_session_status_serialization() {
    use strobe::db::SessionStatus;

    assert_eq!(SessionStatus::Running.as_str(), "running");
    assert_eq!(SessionStatus::Exited.as_str(), "exited");
    assert_eq!(SessionStatus::Stopped.as_str(), "stopped");

    assert_eq!(SessionStatus::from_str("running"), Some(SessionStatus::Running));
    assert_eq!(SessionStatus::from_str("exited"), Some(SessionStatus::Exited));
    assert_eq!(SessionStatus::from_str("stopped"), Some(SessionStatus::Stopped));
    assert_eq!(SessionStatus::from_str("invalid"), None);
}

#[test]
fn test_event_type_serialization() {
    use strobe::db::EventType;

    assert_eq!(EventType::FunctionEnter.as_str(), "function_enter");
    assert_eq!(EventType::FunctionExit.as_str(), "function_exit");
    assert_eq!(EventType::Stdout.as_str(), "stdout");
    assert_eq!(EventType::Stderr.as_str(), "stderr");

    assert_eq!(EventType::from_str("function_enter"), Some(EventType::FunctionEnter));
    assert_eq!(EventType::from_str("function_exit"), Some(EventType::FunctionExit));
    assert_eq!(EventType::from_str("stdout"), Some(EventType::Stdout));
    assert_eq!(EventType::from_str("stderr"), Some(EventType::Stderr));
    assert_eq!(EventType::from_str("invalid"), None);
}

#[test]
fn test_error_types() {
    use strobe::Error;

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
fn test_hook_manager() {
    use strobe::frida_collector::HookManager;

    let mut manager = HookManager::new();

    // Test pattern expansion
    let patterns = manager.expand_patterns(
        &["@usercode".to_string()],
        "/home/user/project",
    );
    assert!(patterns[0].starts_with("/home/user/project"));

    // Test adding patterns
    manager.add_patterns(&["foo::*".to_string(), "bar::*".to_string()]);
    let active = manager.active_patterns();
    assert_eq!(active.len(), 2);

    // Test removing patterns
    manager.remove_patterns(&["foo::*".to_string()]);
    let active = manager.active_patterns();
    assert_eq!(active.len(), 1);
}

#[test]
fn test_output_event_insertion_and_query() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

    // Insert stdout event
    db.insert_event(strobe::db::Event {
        id: "evt-out-1".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1500,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::Stdout,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: Some("Hello from stdout\n".to_string()),
    }).unwrap();

    // Insert stderr event
    db.insert_event(strobe::db::Event {
        id: "evt-out-2".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 2500,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::Stderr,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: Some("Error: something went wrong\n".to_string()),
    }).unwrap();

    // Query all - should return both in timestamp order
    let all = db.query_events("test-session", |q| q).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].event_type, strobe::db::EventType::Stdout);
    assert_eq!(all[0].text.as_deref(), Some("Hello from stdout\n"));
    assert_eq!(all[1].event_type, strobe::db::EventType::Stderr);

    // Query filtered by event type
    let stdout_only = db.query_events("test-session", |q| {
        q.event_type(strobe::db::EventType::Stdout)
    }).unwrap();
    assert_eq!(stdout_only.len(), 1);
}

#[test]
fn test_mixed_event_types_in_unified_timeline() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

    // Insert function enter
    db.insert_event(strobe::db::Event {
        id: "evt-1".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1000,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::FunctionEnter,
        function_name: "main::run".to_string(),
        function_name_raw: None,
        source_file: Some("/src/main.rs".to_string()),
        line_number: Some(10),
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: None,
    }).unwrap();

    // Insert stdout (between function enter and exit)
    db.insert_event(strobe::db::Event {
        id: "evt-2".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1500,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::Stdout,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: Some("Running...\n".to_string()),
    }).unwrap();

    // Insert function exit
    db.insert_event(strobe::db::Event {
        id: "evt-3".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 2000,
        thread_id: 1,
        parent_event_id: Some("evt-1".to_string()),
        event_type: strobe::db::EventType::FunctionExit,
        function_name: "main::run".to_string(),
        function_name_raw: None,
        source_file: Some("/src/main.rs".to_string()),
        line_number: Some(10),
        arguments: None,
        return_value: Some(serde_json::json!(0)),
        duration_ns: Some(1000),
        text: None,
    }).unwrap();

    // Query all — should return 3 events in chronological order
    let all = db.query_events("test-session", |q| q).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].event_type, strobe::db::EventType::FunctionEnter);
    assert_eq!(all[1].event_type, strobe::db::EventType::Stdout);
    assert_eq!(all[1].text.as_deref(), Some("Running...\n"));
    assert_eq!(all[2].event_type, strobe::db::EventType::FunctionExit);

    // Function filter should only return function events, NOT output events
    let func_events = db.query_events("test-session", |q| {
        q.function_contains("run")
    }).unwrap();
    assert_eq!(func_events.len(), 2);
    assert!(func_events.iter().all(|e|
        e.event_type == strobe::db::EventType::FunctionEnter ||
        e.event_type == strobe::db::EventType::FunctionExit
    ));

    // Event type filter still works
    let stdout = db.query_events("test-session", |q| {
        q.event_type(strobe::db::EventType::Stdout)
    }).unwrap();
    assert_eq!(stdout.len(), 1);
}

#[test]
fn test_batch_insert_with_output_events() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

    let events = vec![
        strobe::db::Event {
            id: "batch-1".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 100,
            thread_id: 1,
            parent_event_id: None,
            event_type: strobe::db::EventType::FunctionEnter,
            function_name: "init".to_string(),
            function_name_raw: None,
            source_file: None,
            line_number: None,
            arguments: None,
            return_value: None,
            duration_ns: None,
            text: None,
        },
        strobe::db::Event {
            id: "batch-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 200,
            thread_id: 1,
            parent_event_id: None,
            event_type: strobe::db::EventType::Stdout,
            function_name: String::new(),
            function_name_raw: None,
            source_file: None,
            line_number: None,
            arguments: None,
            return_value: None,
            duration_ns: None,
            text: Some("batch output line\n".to_string()),
        },
    ];

    db.insert_events_batch(&events).unwrap();

    let results = db.query_events("test-session", |q| q).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].event_type, strobe::db::EventType::FunctionEnter);
    assert_eq!(results[0].function_name, "init");
    assert!(results[0].text.is_none());
    assert_eq!(results[1].event_type, strobe::db::EventType::Stdout);
    assert_eq!(results[1].text.as_deref(), Some("batch output line\n"));
}
