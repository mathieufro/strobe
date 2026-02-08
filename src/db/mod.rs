mod baselines;
mod schema;
mod session;
mod event;

pub use schema::Database;
pub use session::{Session, SessionStatus};
pub use event::{Event, EventType, TraceEventSummary, TraceEventVerbose, EventInsertStats};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_database_creation() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");

        let db = Database::open(&db_path).unwrap();

        // Should create tables
        assert!(db.table_exists("sessions").unwrap());
        assert!(db.table_exists("events").unwrap());
    }

    #[test]
    fn test_session_lifecycle() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        // Create session
        let session = db.create_session(
            "myapp-2026-02-05-14h32",
            "/path/to/myapp",
            "/home/user/project",
            12345,
        ).unwrap();

        assert_eq!(session.id, "myapp-2026-02-05-14h32");
        assert_eq!(session.status, SessionStatus::Running);

        // Get session
        let retrieved = db.get_session("myapp-2026-02-05-14h32").unwrap();
        assert!(retrieved.is_some());

        // Update status
        db.update_session_status("myapp-2026-02-05-14h32", SessionStatus::Exited).unwrap();
        let updated = db.get_session("myapp-2026-02-05-14h32").unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Exited);

        // Delete session
        db.delete_session("myapp-2026-02-05-14h32").unwrap();
        let deleted = db.get_session("myapp-2026-02-05-14h32").unwrap();
        assert!(deleted.is_none());
    }

    #[test]
    fn test_event_insertion_and_query() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();

        db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

        // Insert events
        db.insert_event(Event {
            id: "evt-1".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 1000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::FunctionEnter,
            function_name: "main::process".to_string(),
            function_name_raw: Some("_ZN4main7processEv".to_string()),
            source_file: Some("/home/src/main.rs".to_string()),
            line_number: Some(42),
            arguments: Some(serde_json::json!([1, "hello"])),
            return_value: None,
            duration_ns: None,
            text: None,
            sampled: None,
            watch_values: None,
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        db.insert_event(Event {
            id: "evt-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 2000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: Some("evt-1".to_string()),
            event_type: EventType::FunctionExit,
            function_name: "main::process".to_string(),
            function_name_raw: Some("_ZN4main7processEv".to_string()),
            source_file: Some("/home/src/main.rs".to_string()),
            line_number: Some(42),
            arguments: None,
            return_value: Some(serde_json::json!(42)),
            duration_ns: Some(1000),
            text: None,
            sampled: None,
            watch_values: None,
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        // Query by function name
        let results = db.query_events("test-session", |q| {
            q.function_contains("process")
        }).unwrap();

        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_database_roundtrip() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("test-session", "/bin/test", "/home/user", 1234).unwrap();

        db.insert_event(Event {
            id: "evt-1".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 1000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::FunctionEnter,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        let events = db.query_events("test-session", |q| q).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].function_name, "main");
    }

    #[test]
    fn test_event_with_watch_values() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/home", 1).unwrap();

        db.insert_event(Event {
            id: "evt-w1".to_string(),
            session_id: "s1".to_string(),
            timestamp_ns: 5000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::FunctionEnter,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        assert_eq!(events.len(), 1);
        let wv = events[0].watch_values.as_ref().unwrap();
        assert_eq!(wv["gClock"], 48291);
    }

    #[test]
    fn test_output_event_insertion_and_query() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

        db.insert_event(Event {
            id: "evt-out-1".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 1500,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::Stdout,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        db.insert_event(Event {
            id: "evt-out-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 2500,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::Stderr,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        let all = db.query_events("test-session", |q| q).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_type, EventType::Stdout);
        assert_eq!(all[0].text.as_deref(), Some("Hello from stdout\n"));
        assert_eq!(all[1].event_type, EventType::Stderr);

        let stdout_only = db.query_events("test-session", |q| {
            q.event_type(EventType::Stdout)
        }).unwrap();
        assert_eq!(stdout_only.len(), 1);
    }

    #[test]
    fn test_mixed_event_types_in_unified_timeline() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

        db.insert_event(Event {
            id: "evt-1".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 1000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::FunctionEnter,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        db.insert_event(Event {
            id: "evt-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 1500,
            thread_id: 1,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::Stdout,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        db.insert_event(Event {
            id: "evt-3".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 2000,
            thread_id: 1,
            thread_name: None,
            parent_event_id: Some("evt-1".to_string()),
            event_type: EventType::FunctionExit,
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
            pid: None,
            signal: None,
            fault_address: None,
            registers: None,
            backtrace: None,
            locals: None,
        }).unwrap();

        let all = db.query_events("test-session", |q| q).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].event_type, EventType::FunctionEnter);
        assert_eq!(all[1].event_type, EventType::Stdout);
        assert_eq!(all[1].text.as_deref(), Some("Running...\n"));
        assert_eq!(all[2].event_type, EventType::FunctionExit);

        let func_events = db.query_events("test-session", |q| {
            q.function_contains("run")
        }).unwrap();
        assert_eq!(func_events.len(), 2);

        let stdout = db.query_events("test-session", |q| {
            q.event_type(EventType::Stdout)
        }).unwrap();
        assert_eq!(stdout.len(), 1);
    }

    #[test]
    fn test_batch_insert_with_output_events() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

        let events = vec![
            Event {
                id: "batch-1".to_string(),
                session_id: "test-session".to_string(),
                timestamp_ns: 100,
                thread_id: 1,
                thread_name: None,
                parent_event_id: None,
                event_type: EventType::FunctionEnter,
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
                pid: None,
                signal: None,
                fault_address: None,
                registers: None,
                backtrace: None,
                locals: None,
            },
            Event {
                id: "batch-2".to_string(),
                session_id: "test-session".to_string(),
                timestamp_ns: 200,
                thread_id: 1,
                thread_name: None,
                parent_event_id: None,
                event_type: EventType::Stdout,
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
                pid: None,
                signal: None,
                fault_address: None,
                registers: None,
                backtrace: None,
                locals: None,
            },
        ];

        db.insert_events_batch(&events).unwrap();

        let results = db.query_events("test-session", |q| q).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].function_name, "init");
        assert_eq!(results[1].text.as_deref(), Some("batch output line\n"));
    }

    #[test]
    fn test_session_cleanup_on_stop() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();

        db.create_session("session-1", "/bin/app1", "/home", 1000).unwrap();
        db.create_session("session-2", "/bin/app2", "/home", 2000).unwrap();

        let running = db.get_running_sessions().unwrap();
        assert_eq!(running.len(), 2);

        db.update_session_status("session-1", SessionStatus::Stopped).unwrap();

        let running = db.get_running_sessions().unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, "session-2");
    }

    #[test]
    fn test_session_status_serialization() {
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
    fn test_crash_event_db_roundtrip() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        let crash_event = Event {
            id: "crash-evt-1".to_string(),
            session_id: "s1".to_string(),
            timestamp_ns: 1000000,
            thread_id: 1,
            thread_name: Some("main".to_string()),
            parent_event_id: None,
            event_type: EventType::Crash,
            function_name: "crash_null_deref".to_string(),
            function_name_raw: None,
            source_file: Some("main.c".to_string()),
            line_number: Some(30),
            arguments: None,
            return_value: None,
            duration_ns: None,
            text: None,
            sampled: None,
            watch_values: None,
            pid: Some(1234),
            signal: Some("access-violation".to_string()),
            fault_address: Some("0x0".to_string()),
            registers: Some(serde_json::json!({"pc": "0x100003f20", "sp": "0x16f603e00", "fp": "0x16f604000"})),
            backtrace: Some(serde_json::json!([
                {"address": "0x100003f20", "name": "crash_null_deref"},
                {"address": "0x100004100", "name": "main"}
            ])),
            locals: Some(serde_json::json!([
                {"name": "local_counter", "value": "42", "type": "int"},
                {"name": "ptr", "value": "0x0", "type": "int *"}
            ])),
        };

        db.insert_event(crash_event).unwrap();

        let events = db.query_events("s1", |q| q.event_type(EventType::Crash)).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];

        assert_eq!(e.event_type, EventType::Crash);
        assert_eq!(e.signal.as_deref(), Some("access-violation"));
        assert_eq!(e.fault_address.as_deref(), Some("0x0"));
        assert_eq!(e.pid, Some(1234));

        let regs = e.registers.as_ref().unwrap();
        assert_eq!(regs["pc"], "0x100003f20");

        let frames = e.backtrace.as_ref().unwrap().as_array().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["name"], "crash_null_deref");
    }

    #[test]
    fn test_crash_event_in_unified_timeline() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        db.insert_event(Event {
            id: "evt-1".to_string(), session_id: "s1".to_string(),
            timestamp_ns: 1000, thread_id: 1, thread_name: None,
            parent_event_id: None, event_type: EventType::Stdout,
            function_name: String::new(), function_name_raw: None,
            source_file: None, line_number: None,
            arguments: None, return_value: None, duration_ns: None,
            text: Some("About to crash\n".to_string()),
            sampled: None, watch_values: None,
            pid: Some(1234), signal: None, fault_address: None,
            registers: None, backtrace: None, locals: None,
        }).unwrap();

        db.insert_event(Event {
            id: "evt-2".to_string(), session_id: "s1".to_string(),
            timestamp_ns: 2000, thread_id: 1, thread_name: None,
            parent_event_id: None, event_type: EventType::Crash,
            function_name: "crash_null_deref".to_string(), function_name_raw: None,
            source_file: Some("main.c".to_string()), line_number: Some(30),
            arguments: None, return_value: None, duration_ns: None,
            text: None, sampled: None, watch_values: None,
            pid: Some(1234),
            signal: Some("access-violation".to_string()),
            fault_address: Some("0x0".to_string()),
            registers: Some(serde_json::json!({"pc": "0x100003f20"})),
            backtrace: Some(serde_json::json!([{"address": "0x100003f20", "name": "crash_null_deref"}])),
            locals: None,
        }).unwrap();

        let all = db.query_events("s1", |q| q).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_type, EventType::Stdout);
        assert_eq!(all[1].event_type, EventType::Crash);

        let crashes = db.query_events("s1", |q| q.event_type(EventType::Crash)).unwrap();
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].signal.as_deref(), Some("access-violation"));
    }

    #[test]
    fn test_pid_filter_on_events() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        for (i, pid_val) in [1234u32, 1234, 5678, 5678, 9999].iter().enumerate() {
            db.insert_event(Event {
                id: format!("evt-{}", i), session_id: "s1".to_string(),
                timestamp_ns: i as i64 * 1000, thread_id: 1, thread_name: None,
                parent_event_id: None, event_type: EventType::FunctionEnter,
                function_name: format!("func_{}", pid_val), function_name_raw: None,
                source_file: None, line_number: None,
                arguments: None, return_value: None, duration_ns: None,
                text: None, sampled: None, watch_values: None,
                pid: Some(*pid_val), signal: None, fault_address: None,
                registers: None, backtrace: None, locals: None,
            }).unwrap();
        }

        let pid_1234 = db.query_events("s1", |q| { let mut q = q; q.pid_equals = Some(1234); q }).unwrap();
        assert_eq!(pid_1234.len(), 2);

        let pid_5678 = db.query_events("s1", |q| { let mut q = q; q.pid_equals = Some(5678); q }).unwrap();
        assert_eq!(pid_5678.len(), 2);

        let no_pid = db.query_events("s1", |q| { let mut q = q; q.pid_equals = Some(11111); q }).unwrap();
        assert_eq!(no_pid.len(), 0);
    }

    #[test]
    fn test_min_duration_filter() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        let durations: Vec<(&str, i64)> = vec![
            ("fast_func", 100_000),
            ("medium_func", 5_000_000),
            ("slow_func", 50_000_000),
            ("very_slow_func", 500_000_000),
        ];

        for (i, (name, duration)) in durations.iter().enumerate() {
            db.insert_event(Event {
                id: format!("evt-{}", i), session_id: "s1".to_string(),
                timestamp_ns: i as i64 * 1_000_000, thread_id: 1, thread_name: None,
                parent_event_id: None, event_type: EventType::FunctionExit,
                function_name: name.to_string(), function_name_raw: None,
                source_file: None, line_number: None,
                arguments: None, return_value: None, duration_ns: Some(*duration),
                text: None, sampled: None, watch_values: None,
                pid: None, signal: None, fault_address: None,
                registers: None, backtrace: None, locals: None,
            }).unwrap();
        }

        let ge_1ms = db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(1_000_000); q }).unwrap();
        assert_eq!(ge_1ms.len(), 3);

        let ge_10ms = db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(10_000_000); q }).unwrap();
        assert_eq!(ge_10ms.len(), 2);

        let ge_100ms = db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(100_000_000); q }).unwrap();
        assert_eq!(ge_100ms.len(), 1);
        assert_eq!(ge_100ms[0].function_name, "very_slow_func");
    }

    #[test]
    fn test_time_range_filter() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        for i in 0..10 {
            db.insert_event(Event {
                id: format!("evt-{}", i), session_id: "s1".to_string(),
                timestamp_ns: (i + 1) * 1_000_000_000, thread_id: 1, thread_name: None,
                parent_event_id: None, event_type: EventType::FunctionEnter,
                function_name: format!("func_{}", i), function_name_raw: None,
                source_file: None, line_number: None,
                arguments: None, return_value: None, duration_ns: None,
                text: None, sampled: None, watch_values: None,
                pid: None, signal: None, fault_address: None,
                registers: None, backtrace: None, locals: None,
            }).unwrap();
        }

        let range = db.query_events("s1", |q| {
            let mut q = q;
            q.timestamp_from_ns = Some(3_000_000_000);
            q.timestamp_to_ns = Some(7_000_000_000);
            q
        }).unwrap();
        assert_eq!(range.len(), 5);

        let from_8s = db.query_events("s1", |q| {
            let mut q = q;
            q.timestamp_from_ns = Some(8_000_000_000);
            q
        }).unwrap();
        assert_eq!(from_8s.len(), 3);
    }

    #[test]
    fn test_update_event_locals() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session("s1", "/bin/test", "/tmp", 1234).unwrap();

        db.insert_event(Event {
            id: "crash-1".to_string(), session_id: "s1".to_string(),
            timestamp_ns: 1000, thread_id: 1, thread_name: None,
            parent_event_id: None, event_type: EventType::Crash,
            function_name: "crash_func".to_string(), function_name_raw: None,
            source_file: None, line_number: None,
            arguments: None, return_value: None, duration_ns: None,
            text: None, sampled: None, watch_values: None,
            pid: Some(1234),
            signal: Some("access-violation".to_string()),
            fault_address: Some("0x0".to_string()),
            registers: None, backtrace: None, locals: None,
        }).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        assert!(events[0].locals.is_none());

        let locals = serde_json::json!([
            {"name": "counter", "value": "42", "type": "int"},
            {"name": "ratio", "value": "3.14", "type": "float"}
        ]);
        db.update_event_locals("crash-1", &locals).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        let stored_locals = events[0].locals.as_ref().unwrap();
        let arr = stored_locals.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "counter");
    }
}
