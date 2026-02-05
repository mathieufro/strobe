mod schema;
mod session;
mod event;

pub use schema::Database;
pub use session::{Session, SessionStatus};
pub use event::{Event, EventType, TraceEventSummary, TraceEventVerbose};

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
        }).unwrap();

        db.insert_event(Event {
            id: "evt-2".to_string(),
            session_id: "test-session".to_string(),
            timestamp_ns: 2000,
            thread_id: 1,
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
        }).unwrap();

        // Query by function name
        let results = db.query_events("test-session", |q| {
            q.function_contains("process")
        }).unwrap();

        assert_eq!(results.len(), 2);
    }
}
