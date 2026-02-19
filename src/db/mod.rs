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

    /// Helper: create a DB with a session pre-created.
    fn test_db_with_session(session_id: &str) -> (tempfile::TempDir, Database) {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        db.create_session(session_id, "/bin/test", "/home", 1234).unwrap();
        (dir, db)
    }

    #[test]
    fn test_database_creation() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        assert!(db.table_exists("sessions").unwrap());
        assert!(db.table_exists("events").unwrap());
    }

    #[test]
    fn test_session_lifecycle() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();

        let session = db.create_session("s1", "/path/to/myapp", "/home/user/project", 12345).unwrap();
        assert_eq!(session.id, "s1");
        assert_eq!(session.status, SessionStatus::Running);

        assert!(db.get_session("s1").unwrap().is_some());

        db.update_session_status("s1", SessionStatus::Exited).unwrap();
        assert_eq!(db.get_session("s1").unwrap().unwrap().status, SessionStatus::Exited);

        db.delete_session("s1").unwrap();
        assert!(db.get_session("s1").unwrap().is_none());
    }

    #[test]
    fn test_event_insertion_and_query() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-1".into(), session_id: "s1".into(), timestamp_ns: 1000, thread_id: 1,
            event_type: EventType::FunctionEnter, function_name: "main::process".into(),
            function_name_raw: Some("_ZN4main7processEv".into()),
            source_file: Some("/home/src/main.rs".into()), line_number: Some(42),
            arguments: Some(serde_json::json!([1, "hello"])),
            ..Default::default()
        }).unwrap();

        db.insert_event(&Event {
            id: "evt-2".into(), session_id: "s1".into(), timestamp_ns: 2000, thread_id: 1,
            parent_event_id: Some("evt-1".into()),
            event_type: EventType::FunctionExit, function_name: "main::process".into(),
            function_name_raw: Some("_ZN4main7processEv".into()),
            source_file: Some("/home/src/main.rs".into()), line_number: Some(42),
            return_value: Some(serde_json::json!(42)), duration_ns: Some(1000),
            ..Default::default()
        }).unwrap();

        let results = db.query_events("s1", |q| q.function_contains("process")).unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_database_roundtrip() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-1".into(), session_id: "s1".into(), timestamp_ns: 1000, thread_id: 1,
            function_name: "main".into(),
            source_file: Some("/home/user/main.rs".into()), line_number: Some(1),
            ..Default::default()
        }).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].function_name, "main");
    }

    #[test]
    fn test_event_with_watch_values() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-w1".into(), session_id: "s1".into(), timestamp_ns: 5000, thread_id: 1,
            function_name: "NoteOn".into(),
            watch_values: Some(serde_json::json!({"gClock": 48291, "tempo": 120.5})),
            ..Default::default()
        }).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].watch_values.as_ref().unwrap()["gClock"], 48291);
    }

    #[test]
    fn test_output_event_insertion_and_query() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-out-1".into(), session_id: "s1".into(), timestamp_ns: 1500, thread_id: 1,
            event_type: EventType::Stdout, text: Some("Hello from stdout\n".into()),
            ..Default::default()
        }).unwrap();

        db.insert_event(&Event {
            id: "evt-out-2".into(), session_id: "s1".into(), timestamp_ns: 2500, thread_id: 1,
            event_type: EventType::Stderr, text: Some("Error: something went wrong\n".into()),
            ..Default::default()
        }).unwrap();

        let all = db.query_events("s1", |q| q).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_type, EventType::Stderr);
        assert_eq!(all[0].text.as_deref(), Some("Error: something went wrong\n"));
        assert_eq!(all[1].event_type, EventType::Stdout);

        let stdout_only = db.query_events("s1", |q| q.event_type(EventType::Stdout)).unwrap();
        assert_eq!(stdout_only.len(), 1);
    }

    #[test]
    fn test_mixed_event_types_in_unified_timeline() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-1".into(), session_id: "s1".into(), timestamp_ns: 1000, thread_id: 1,
            function_name: "main::run".into(),
            source_file: Some("/src/main.rs".into()), line_number: Some(10),
            ..Default::default()
        }).unwrap();

        db.insert_event(&Event {
            id: "evt-2".into(), session_id: "s1".into(), timestamp_ns: 1500, thread_id: 1,
            event_type: EventType::Stdout, text: Some("Running...\n".into()),
            ..Default::default()
        }).unwrap();

        db.insert_event(&Event {
            id: "evt-3".into(), session_id: "s1".into(), timestamp_ns: 2000, thread_id: 1,
            parent_event_id: Some("evt-1".into()),
            event_type: EventType::FunctionExit, function_name: "main::run".into(),
            source_file: Some("/src/main.rs".into()), line_number: Some(10),
            return_value: Some(serde_json::json!(0)), duration_ns: Some(1000),
            ..Default::default()
        }).unwrap();

        let all = db.query_events("s1", |q| q).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].event_type, EventType::FunctionExit);
        assert_eq!(all[1].event_type, EventType::Stdout);
        assert_eq!(all[1].text.as_deref(), Some("Running...\n"));
        assert_eq!(all[2].event_type, EventType::FunctionEnter);

        assert_eq!(db.query_events("s1", |q| q.function_contains("run")).unwrap().len(), 2);
        assert_eq!(db.query_events("s1", |q| q.event_type(EventType::Stdout)).unwrap().len(), 1);
    }

    #[test]
    fn test_batch_insert_with_output_events() {
        let (_dir, db) = test_db_with_session("s1");

        let events = vec![
            Event {
                id: "batch-1".into(), session_id: "s1".into(), timestamp_ns: 100, thread_id: 1,
                function_name: "init".into(),
                ..Default::default()
            },
            Event {
                id: "batch-2".into(), session_id: "s1".into(), timestamp_ns: 200, thread_id: 1,
                event_type: EventType::Stdout, text: Some("batch output line\n".into()),
                ..Default::default()
            },
        ];

        db.insert_events_batch(&events).unwrap();

        let results = db.query_events("s1", |q| q).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].text.as_deref(), Some("batch output line\n"));
        assert_eq!(results[1].function_name, "init");
    }

    #[test]
    fn test_session_cleanup_on_stop() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();

        db.create_session("session-1", "/bin/app1", "/home", 1000).unwrap();
        db.create_session("session-2", "/bin/app2", "/home", 2000).unwrap();
        assert_eq!(db.get_running_sessions().unwrap().len(), 2);

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
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "crash-evt-1".into(), session_id: "s1".into(),
            timestamp_ns: 1000000, thread_id: 1,
            thread_name: Some("main".into()),
            event_type: EventType::Crash, function_name: "crash_null_deref".into(),
            source_file: Some("main.c".into()), line_number: Some(30),
            pid: Some(1234),
            signal: Some("access-violation".into()),
            fault_address: Some("0x0".into()),
            registers: Some(serde_json::json!({"pc": "0x100003f20", "sp": "0x16f603e00", "fp": "0x16f604000"})),
            backtrace: Some(serde_json::json!([
                {"address": "0x100003f20", "name": "crash_null_deref"},
                {"address": "0x100004100", "name": "main"}
            ])),
            locals: Some(serde_json::json!([
                {"name": "local_counter", "value": "42", "type": "int"},
                {"name": "ptr", "value": "0x0", "type": "int *"}
            ])),
            ..Default::default()
        }).unwrap();

        let events = db.query_events("s1", |q| q.event_type(EventType::Crash)).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];

        assert_eq!(e.event_type, EventType::Crash);
        assert_eq!(e.signal.as_deref(), Some("access-violation"));
        assert_eq!(e.fault_address.as_deref(), Some("0x0"));
        assert_eq!(e.pid, Some(1234));
        assert_eq!(e.registers.as_ref().unwrap()["pc"], "0x100003f20");

        let frames = e.backtrace.as_ref().unwrap().as_array().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["name"], "crash_null_deref");
    }

    #[test]
    fn test_crash_event_in_unified_timeline() {
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "evt-1".into(), session_id: "s1".into(), timestamp_ns: 1000, thread_id: 1,
            event_type: EventType::Stdout, text: Some("About to crash\n".into()),
            pid: Some(1234),
            ..Default::default()
        }).unwrap();

        db.insert_event(&Event {
            id: "evt-2".into(), session_id: "s1".into(), timestamp_ns: 2000, thread_id: 1,
            event_type: EventType::Crash, function_name: "crash_null_deref".into(),
            source_file: Some("main.c".into()), line_number: Some(30),
            pid: Some(1234),
            signal: Some("access-violation".into()),
            fault_address: Some("0x0".into()),
            registers: Some(serde_json::json!({"pc": "0x100003f20"})),
            backtrace: Some(serde_json::json!([{"address": "0x100003f20", "name": "crash_null_deref"}])),
            ..Default::default()
        }).unwrap();

        let all = db.query_events("s1", |q| q).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].event_type, EventType::Crash);
        assert_eq!(all[1].event_type, EventType::Stdout);

        let crashes = db.query_events("s1", |q| q.event_type(EventType::Crash)).unwrap();
        assert_eq!(crashes.len(), 1);
        assert_eq!(crashes[0].signal.as_deref(), Some("access-violation"));
    }

    #[test]
    fn test_pid_filter_on_events() {
        let (_dir, db) = test_db_with_session("s1");

        for (i, pid_val) in [1234u32, 1234, 5678, 5678, 9999].iter().enumerate() {
            db.insert_event(&Event {
                id: format!("evt-{}", i), session_id: "s1".into(),
                timestamp_ns: i as i64 * 1000, thread_id: 1,
                function_name: format!("func_{}", pid_val),
                pid: Some(*pid_val),
                ..Default::default()
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
        let (_dir, db) = test_db_with_session("s1");

        for (i, (name, duration)) in [
            ("fast_func", 100_000i64),
            ("medium_func", 5_000_000),
            ("slow_func", 50_000_000),
            ("very_slow_func", 500_000_000),
        ].iter().enumerate() {
            db.insert_event(&Event {
                id: format!("evt-{}", i), session_id: "s1".into(),
                timestamp_ns: i as i64 * 1_000_000, thread_id: 1,
                event_type: EventType::FunctionExit,
                function_name: name.to_string(), duration_ns: Some(*duration),
                ..Default::default()
            }).unwrap();
        }

        assert_eq!(db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(1_000_000); q }).unwrap().len(), 3);
        assert_eq!(db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(10_000_000); q }).unwrap().len(), 2);

        let ge_100ms = db.query_events("s1", |q| { let mut q = q; q.min_duration_ns = Some(100_000_000); q }).unwrap();
        assert_eq!(ge_100ms.len(), 1);
        assert_eq!(ge_100ms[0].function_name, "very_slow_func");
    }

    #[test]
    fn test_time_range_filter() {
        let (_dir, db) = test_db_with_session("s1");

        for i in 0..10 {
            db.insert_event(&Event {
                id: format!("evt-{}", i), session_id: "s1".into(),
                timestamp_ns: (i + 1) * 1_000_000_000, thread_id: 1,
                function_name: format!("func_{}", i),
                ..Default::default()
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
        let (_dir, db) = test_db_with_session("s1");

        db.insert_event(&Event {
            id: "crash-1".into(), session_id: "s1".into(), timestamp_ns: 1000, thread_id: 1,
            event_type: EventType::Crash, function_name: "crash_func".into(),
            pid: Some(1234),
            signal: Some("access-violation".into()),
            fault_address: Some("0x0".into()),
            ..Default::default()
        }).unwrap();

        assert!(db.query_events("s1", |q| q).unwrap()[0].locals.is_none());

        let locals = serde_json::json!([
            {"name": "counter", "value": "42", "type": "int"},
            {"name": "ratio", "value": "3.14", "type": "float"}
        ]);
        db.update_event_locals("crash-1", &locals).unwrap();

        let events = db.query_events("s1", |q| q).unwrap();
        let arr = events[0].locals.as_ref().unwrap().as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["name"], "counter");
    }

    #[test]
    fn test_breakpoint_event_columns() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        let conn = db.conn.lock().unwrap();

        // Verify breakpoint_id column exists
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT breakpoint_id FROM events WHERE 1=0",
            [],
            |_| Ok(String::new()),
        );
        // Should error with "no rows" not "no such column"
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(!err_msg.contains("no such column"), "Column breakpoint_id should exist");

        // Verify logpoint_message column exists
        let result: rusqlite::Result<String> = conn.query_row(
            "SELECT logpoint_message FROM events WHERE 1=0",
            [],
            |_| Ok(String::new()),
        );
        assert!(result.is_err());
        let err_msg = result.err().unwrap().to_string();
        assert!(!err_msg.contains("no such column"), "Column logpoint_message should exist");
    }

    #[test]
    fn test_fifo_eviction_preserves_stdout_stderr() {
        let (_dir, db) = test_db_with_session("s1");

        // Insert 5 trace events + 3 stdout events + 2 stderr events = 10 total
        let mut events = Vec::new();
        for i in 0..5 {
            events.push(Event {
                id: format!("trace-{}", i), session_id: "s1".into(),
                timestamp_ns: i as i64 * 100, thread_id: 1,
                event_type: EventType::FunctionEnter, function_name: format!("func_{}", i),
                ..Default::default()
            });
        }
        for i in 0..3 {
            events.push(Event {
                id: format!("stdout-{}", i), session_id: "s1".into(),
                timestamp_ns: i as i64 * 100 + 50, thread_id: 1,
                event_type: EventType::Stdout, text: Some(format!("line {}\n", i)),
                ..Default::default()
            });
        }
        for i in 0..2 {
            events.push(Event {
                id: format!("stderr-{}", i), session_id: "s1".into(),
                timestamp_ns: i as i64 * 100 + 75, thread_id: 1,
                event_type: EventType::Stderr, text: Some(format!("err {}\n", i)),
                ..Default::default()
            });
        }

        let stats = db.insert_events_with_limit(&events, 10).unwrap();
        assert_eq!(stats.events_inserted, 10);
        assert_eq!(stats.events_deleted, 0);

        // Now insert 5 more trace events with limit=10 — should evict 5 oldest trace events
        let more: Vec<Event> = (0..5).map(|i| Event {
            id: format!("trace-new-{}", i), session_id: "s1".into(),
            timestamp_ns: 1000 + i as i64 * 100, thread_id: 1,
            event_type: EventType::FunctionEnter, function_name: format!("new_func_{}", i),
            ..Default::default()
        }).collect();

        let stats = db.insert_events_with_limit(&more, 10).unwrap();
        assert_eq!(stats.events_inserted, 5);
        assert_eq!(stats.events_deleted, 5); // 5 old trace events evicted

        // All stdout and stderr events must survive
        let stdout = db.query_events("s1", |q| q.event_type(EventType::Stdout)).unwrap();
        assert_eq!(stdout.len(), 3, "stdout events must not be evicted");

        let stderr = db.query_events("s1", |q| q.event_type(EventType::Stderr)).unwrap();
        assert_eq!(stderr.len(), 2, "stderr events must not be evicted");

        // Only new trace events should remain (old ones evicted)
        let traces = db.query_events("s1", |q| q.event_type(EventType::FunctionEnter)).unwrap();
        assert_eq!(traces.len(), 5, "only new trace events should remain");
        assert!(traces.iter().all(|e| e.function_name.starts_with("new_func_")));
    }

    #[test]
    fn test_fifo_eviction_with_only_output_events() {
        let (_dir, db) = test_db_with_session("s1");

        // Fill buffer with 5 stdout events
        let events: Vec<Event> = (0..5).map(|i| Event {
            id: format!("stdout-{}", i), session_id: "s1".into(),
            timestamp_ns: i as i64 * 100, thread_id: 1,
            event_type: EventType::Stdout, text: Some(format!("line {}\n", i)),
            ..Default::default()
        }).collect();

        db.insert_events_with_limit(&events, 5).unwrap();

        // Insert 3 more stdout events with limit=5 — no trace events to evict,
        // so output events should NOT be deleted (buffer grows past limit)
        let more: Vec<Event> = (0..3).map(|i| Event {
            id: format!("stdout-new-{}", i), session_id: "s1".into(),
            timestamp_ns: 1000 + i as i64 * 100, thread_id: 1,
            event_type: EventType::Stdout, text: Some(format!("new line {}\n", i)),
            ..Default::default()
        }).collect();

        let stats = db.insert_events_with_limit(&more, 5).unwrap();
        assert_eq!(stats.events_inserted, 3);
        assert_eq!(stats.events_deleted, 0, "should not evict output events");

        // All 8 stdout events should exist (buffer exceeded but output is protected)
        let all = db.query_events("s1", |q| q.event_type(EventType::Stdout)).unwrap();
        assert_eq!(all.len(), 8);
    }
}
