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
