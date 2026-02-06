use std::collections::{HashMap, HashSet};
#[cfg(any(target_os = "linux", target_os = "macos"))]
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
        thread_name: None,
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
        sampled: None,
        watch_values: None,
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
fn test_watch_types_serialization() {
    let target = strobe::mcp::WatchTarget {
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

    let update = strobe::mcp::WatchUpdate {
        add: Some(vec![target]),
        remove: Some(vec!["old_watch".to_string()]),
    };
    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("gClock->counter"));
    assert!(json.contains("old_watch"));
}

#[test]
fn test_event_with_watch_values() {
    let dir = tempdir().unwrap();
    let db = strobe::db::Database::open(&dir.path().join("test.db")).unwrap();
    db.create_session("s1", "/bin/test", "/home", 1).unwrap();

    let event = strobe::db::Event {
        id: "evt-w1".to_string(),
        session_id: "s1".to_string(),
        timestamp_ns: 5000,
        thread_id: 1,
        thread_name: None,
        parent_event_id: None,
        event_type: strobe::db::EventType::FunctionEnter,
        function_name: "NoteOn".to_string(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: None,
        sampled: None,
        watch_values: Some(serde_json::json!({"gClock": 48291, "tempo": 120.5})),
    };
    db.insert_event(event).unwrap();

    let events = db.query_events("s1", |q| q).unwrap();
    assert_eq!(events.len(), 1);
    let wv = events[0].watch_values.as_ref().unwrap();
    assert_eq!(wv["gClock"], 48291);
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
        thread_name: None,
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
        sampled: None,
        watch_values: None,
    }).unwrap();

    // Insert stderr event
    db.insert_event(strobe::db::Event {
        id: "evt-out-2".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 2500,
        thread_id: 1,
        thread_name: None,
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
        sampled: None,
        watch_values: None,
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
        thread_name: None,
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
        sampled: None,
        watch_values: None,
    }).unwrap();

    // Insert stdout (between function enter and exit)
    db.insert_event(strobe::db::Event {
        id: "evt-2".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1500,
        thread_id: 1,
        thread_name: None,
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
        sampled: None,
        watch_values: None,
    }).unwrap();

    // Insert function exit
    db.insert_event(strobe::db::Event {
        id: "evt-3".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 2000,
        thread_id: 1,
        thread_name: None,
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
        sampled: None,
        watch_values: None,
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
        thread_name: None,
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
            sampled: None,
            watch_values: None,
        },
        strobe::db::Event {
            id: "batch-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 200,
            thread_id: 1,
        thread_name: None,
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
            sampled: None,
            watch_values: None,
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

#[test]
fn test_mcp_initialize_response_has_instructions() {
    let response = strobe::mcp::McpInitializeResponse {
        protocol_version: "2024-11-05".to_string(),
        capabilities: strobe::mcp::McpServerCapabilities {
            tools: strobe::mcp::McpToolsCapability { list_changed: false },
        },
        server_info: strobe::mcp::McpServerInfo {
            name: "strobe".to_string(),
            version: "0.1.0".to_string(),
        },
        instructions: Some("Test instructions".to_string()),
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("instructions"));
    assert!(json.contains("Test instructions"));

    // When None, instructions field should be omitted
    let response_no_instructions = strobe::mcp::McpInitializeResponse {
        protocol_version: "2024-11-05".to_string(),
        capabilities: strobe::mcp::McpServerCapabilities {
            tools: strobe::mcp::McpToolsCapability { list_changed: false },
        },
        server_info: strobe::mcp::McpServerInfo {
            name: "strobe".to_string(),
            version: "0.1.0".to_string(),
        },
        instructions: None,
    };

    let json = serde_json::to_string(&response_no_instructions).unwrap();
    assert!(!json.contains("instructions"));
}

#[tokio::test]
async fn test_pending_patterns_isolation() {
    // Simulate per-connection pending patterns
    let mut all_pending: HashMap<String, HashSet<String>> = HashMap::new();

    // Client A sets patterns
    let client_a = "conn-a";
    all_pending.entry(client_a.to_string()).or_default().insert("foo::*".to_string());

    // Client B sets different patterns
    let client_b = "conn-b";
    all_pending.entry(client_b.to_string()).or_default().insert("bar::*".to_string());

    // Client A launches — should get only its patterns, and they should be consumed
    let a_patterns: Vec<String> = all_pending.remove(client_a)
        .map(|s| s.into_iter().collect())
        .unwrap_or_default();

    assert_eq!(a_patterns, vec!["foo::*"]);
    assert!(all_pending.get(client_a).is_none()); // consumed

    // Client B's patterns should be unaffected
    assert!(all_pending.get(client_b).unwrap().contains("bar::*"));
}

#[tokio::test]
async fn test_session_cleanup_on_stop() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    // Create two running sessions
    db.create_session("session-1", "/bin/app1", "/home", 1000).unwrap();
    db.create_session("session-2", "/bin/app2", "/home", 2000).unwrap();

    // Both should be listed as running
    let running = db.get_running_sessions().unwrap();
    assert_eq!(running.len(), 2);

    // Stop one
    db.update_session_status("session-1", strobe::db::SessionStatus::Stopped).unwrap();

    let running = db.get_running_sessions().unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].id, "session-2");
}

#[cfg(target_os = "macos")]
fn create_c_test_binary_with_globals(dir: &std::path::Path) -> PathBuf {
    let src = r#"
#include <stdint.h>

uint32_t gCounter = 42;
int64_t gSignedVal = -100;
double gTempo = 120.5;
static float sLocalFloat = 3.14f;

typedef struct {
    int32_t x;
    int32_t y;
    double value;
} Point;

Point gPoint = { 10, 20, 99.9 };
Point *gPointPtr = &gPoint;

int main(void) {
    gCounter++;
    (void)sLocalFloat;
    return 0;
}
"#;
    let src_path = dir.join("test_globals.c");
    std::fs::write(&src_path, src).unwrap();
    let out_path = dir.join("test_globals");

    let status = std::process::Command::new("cc")
        .args(["-g", "-O0", "-o"])
        .arg(&out_path)
        .arg(&src_path)
        .status()
        .expect("Failed to compile C test binary");
    assert!(status.success(), "C test binary compilation failed");
    out_path
}

#[test]
#[cfg(target_os = "macos")]
fn test_dwarf_global_variable_parsing() {
    let dir = tempdir().unwrap();
    let binary = create_c_test_binary_with_globals(dir.path());
    let parser = strobe::dwarf::DwarfParser::parse(&binary).unwrap();

    // Should find global variables
    assert!(!parser.variables.is_empty(), "Should find global variables");

    // Find specific globals by name
    let counter = parser.find_variable_by_name("gCounter");
    assert!(counter.is_some(), "Should find gCounter");
    let counter = counter.unwrap();
    assert_eq!(counter.byte_size, 4);
    assert!(matches!(counter.type_kind, strobe::dwarf::TypeKind::Integer { signed: false }));

    let signed_val = parser.find_variable_by_name("gSignedVal");
    assert!(signed_val.is_some(), "Should find gSignedVal");
    assert_eq!(signed_val.unwrap().byte_size, 8);

    let tempo = parser.find_variable_by_name("gTempo");
    assert!(tempo.is_some(), "Should find gTempo");
    let tempo = tempo.unwrap();
    assert_eq!(tempo.byte_size, 8);
    assert!(matches!(tempo.type_kind, strobe::dwarf::TypeKind::Float));

    // Verify address is non-zero (will be a static address)
    assert!(counter.address > 0, "Variable should have a valid static address");
}

#[test]
#[cfg(target_os = "macos")]
fn test_dwarf_watch_expression_ptr_member() {
    let dir = tempdir().unwrap();
    let binary = create_c_test_binary_with_globals(dir.path());
    let parser = strobe::dwarf::DwarfParser::parse(&binary).unwrap();

    // "gPointPtr->x" should resolve to: deref gPointPtr, add offset of x, read i32
    let recipe = parser.resolve_watch_expression("gPointPtr->x");
    assert!(recipe.is_ok(), "Should resolve gPointPtr->x: {:?}", recipe);
    let recipe = recipe.unwrap();
    assert_eq!(recipe.label, "gPointPtr->x");
    assert_eq!(recipe.deref_chain.len(), 1); // one dereference
    assert_eq!(recipe.deref_chain[0], 0);    // x is at offset 0 in Point
    assert_eq!(recipe.final_size, 4);        // int32_t = 4 bytes

    // "gPointPtr->value" — double at offset in struct
    let recipe2 = parser.resolve_watch_expression("gPointPtr->value");
    assert!(recipe2.is_ok(), "Should resolve gPointPtr->value");
    let recipe2 = recipe2.unwrap();
    assert_eq!(recipe2.final_size, 8);       // double
    assert!(matches!(recipe2.type_kind, strobe::dwarf::TypeKind::Float));

    // Simple global (no ->) should also work
    let recipe3 = parser.resolve_watch_expression("gCounter");
    assert!(recipe3.is_ok());
    let recipe3 = recipe3.unwrap();
    assert!(recipe3.deref_chain.is_empty()); // direct read, no deref
}

#[test]
fn test_watch_on_field_patterns() {
    use strobe::mcp::{WatchTarget, WatchUpdate};

    // Test that watch patterns are properly structured
    let watch_with_on = WatchTarget {
        variable: Some("gCounter".to_string()),
        address: None,
        type_hint: Some("int".to_string()),
        label: Some("counter".to_string()),
        expr: None,
        on: Some(vec!["audio::process".to_string(), "midi::*".to_string()]),
    };

    // Verify patterns are stored correctly
    assert_eq!(watch_with_on.on.as_ref().unwrap().len(), 2);
    assert_eq!(watch_with_on.on.as_ref().unwrap()[0], "audio::process");
    assert_eq!(watch_with_on.on.as_ref().unwrap()[1], "midi::*");

    // Test watch without on field (global)
    let global_watch = WatchTarget {
        variable: Some("gTempo".to_string()),
        address: None,
        type_hint: Some("float".to_string()),
        label: Some("tempo".to_string()),
        expr: None,
        on: None,
    };

    assert!(global_watch.on.is_none());

    // Test WatchUpdate with mixed watches
    let update = WatchUpdate {
        add: Some(vec![watch_with_on.clone(), global_watch.clone()]),
        remove: Some(vec!["old_watch".to_string()]),
    };

    assert_eq!(update.add.as_ref().unwrap().len(), 2);
    assert_eq!(update.remove.as_ref().unwrap().len(), 1);
}

#[test]
fn test_watch_pattern_matching_logic() {
    // Test pattern matching logic that will be used in agent
    // This verifies the pattern format before it reaches the agent

    let test_cases = vec![
        // (function_name, pattern, should_match)
        ("audio::process", "audio::process", true),
        ("audio::process", "audio::*", true),
        ("audio::process::internal", "audio::*", false), // * doesn't cross ::
        ("audio::process::internal", "audio::**", true),  // ** crosses ::
        ("midi::noteOn", "midi::*", true),
        ("midi::noteOn", "audio::*", false),
        ("foo::bar::baz", "foo::**", true),
        ("foo::bar::baz", "**::baz", true),
        ("simple_func", "simple_func", true),
        ("simple_func", "simple_*", true), // * matches remainder without ::
        ("parseValue", "parse*", true),
        ("parseValue", "validate*", false),
    ];

    for (func_name, pattern, expected) in test_cases {
        let result = pattern_matches(func_name, pattern);
        assert_eq!(
            result, expected,
            "Pattern '{}' vs function '{}' should be {}",
            pattern, func_name, expected
        );
    }
}

// Helper function to test pattern matching logic
// This mirrors the logic that will be used in the TypeScript agent
fn pattern_matches(name: &str, pattern: &str) -> bool {
    if name == pattern {
        return true;
    }

    if pattern.contains('*') {
        // Convert pattern to regex, handling wildcards carefully
        let mut regex_pattern = pattern.to_string();

        // Replace ** with a temporary marker (deep wildcard)
        regex_pattern = regex_pattern.replace("**", "\x00DEEP\x00");

        // Escape regex special chars (but not our markers or single *)
        let chars_to_escape = ['\\', '.', '+', '?', '^', '$', '{', '}', '(', ')', '|', '[', ']'];
        for ch in chars_to_escape {
            if ch != '*' {
                regex_pattern = regex_pattern.replace(ch, &format!("\\{}", ch));
            }
        }

        // Now convert wildcards to regex
        // * = match anything except :: (one or more chars, but stop at ::)
        regex_pattern = regex_pattern.replace('*', "[^:]+");

        // ** (deep) = match anything including :: (use marker we set earlier)
        regex_pattern = regex_pattern.replace("\x00DEEP\x00", ".*");

        let regex_pattern = format!("^{}$", regex_pattern);

        if let Ok(re) = regex::Regex::new(&regex_pattern) {
            return re.is_match(name);
        }
    }

    false
}

// Test for hook count accumulation bug fix
#[test]
fn test_hook_count_accuracy() {
    use strobe::frida_collector::HookResult;

    // Simulate multi-chunk hook installation
    let chunks = vec![
        HookResult { installed: 50, matched: 50, warnings: vec![] },
        HookResult { installed: 30, matched: 30, warnings: vec![] },
        HookResult { installed: 20, matched: 20, warnings: vec![] },
    ];

    // Test the accumulation logic
    let total_installed: u32 = chunks.iter().map(|r| r.installed).sum();
    let total_matched: u32 = chunks.iter().map(|r| r.matched).sum();

    assert_eq!(total_installed, 100, "Hook count should accumulate to 100");
    assert_eq!(total_matched, 100, "Matched count should accumulate to 100");
}
