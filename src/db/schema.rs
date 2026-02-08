use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use crate::Result;

/// Add a column to a table, ignoring "duplicate column" errors (idempotent migration).
fn add_column_if_not_exists(conn: &Connection, table: &str, column: &str, col_type: &str) -> Result<()> {
    match conn.execute(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type), []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for concurrent access
        // Use query_row to handle PRAGMA that returns a value
        let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;")?;

        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };

        db.initialize_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.initialize_schema()?;
        Ok(db)
    }

    fn initialize_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        // Create main tables
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                binary_path TEXT NOT NULL,
                project_root TEXT NOT NULL,
                pid INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                status TEXT NOT NULL
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS events (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                timestamp_ns INTEGER NOT NULL,
                thread_id INTEGER NOT NULL,
                parent_event_id TEXT,
                event_type TEXT NOT NULL,
                function_name TEXT NOT NULL,
                function_name_raw TEXT,
                source_file TEXT,
                line_number INTEGER,
                arguments JSON,
                return_value JSON,
                duration_ns INTEGER,
                text TEXT,
                sampled INTEGER,
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            )",
            [],
        )?;

        // Idempotent column migrations
        add_column_if_not_exists(&conn, "events", "watch_values", "JSON")?;
        add_column_if_not_exists(&conn, "events", "thread_name", "TEXT")?;
        add_column_if_not_exists(&conn, "sessions", "retained_at", "INTEGER")?;
        add_column_if_not_exists(&conn, "sessions", "size_bytes", "INTEGER")?;
        add_column_if_not_exists(&conn, "events", "pid", "INTEGER")?;
        add_column_if_not_exists(&conn, "events", "signal", "TEXT")?;
        add_column_if_not_exists(&conn, "events", "fault_address", "TEXT")?;
        add_column_if_not_exists(&conn, "events", "registers", "JSON")?;
        add_column_if_not_exists(&conn, "events", "backtrace", "JSON")?;
        add_column_if_not_exists(&conn, "events", "locals", "JSON")?;

        // Test baselines table for historical per-test durations
        conn.execute(
            "CREATE TABLE IF NOT EXISTS test_baselines (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                test_name TEXT NOT NULL,
                project_root TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                status TEXT NOT NULL,
                recorded_at INTEGER NOT NULL
            )",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_baseline_lookup
             ON test_baselines(test_name, project_root, recorded_at DESC)",
            [],
        )?;

        // Create indexes
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_session_time ON events(session_id, timestamp_ns)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_function ON events(function_name)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_source ON events(source_file)",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_thread ON events(session_id, thread_id, timestamp_ns)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_pid ON events(session_id, pid)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_type ON events(session_id, event_type, timestamp_ns)",
            [],
        )?;

        // Note: FTS5 virtual table is omitted for now due to linker issues
        // with static SQLite builds. Full-text search can use LIKE queries
        // or be added later with proper FTS5 linking.

        Ok(())
    }

    pub fn table_exists(&self, table_name: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
            params![table_name],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub(crate) fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

impl Clone for Database {
    fn clone(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
        }
    }
}
