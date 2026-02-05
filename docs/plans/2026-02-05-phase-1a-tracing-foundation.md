# Phase 1a: Tracing Foundation Implementation Plan

**Spec:** `docs/specs/2026-02-05-phase-1a-tracing-foundation.md`
**Goal:** Enable LLM to launch a binary with Frida, add targeted traces, observe execution, and query captured events.
**Architecture:** Rust daemon with Unix socket IPC, SQLite storage, MCP protocol over stdio proxy, Frida agent for dynamic instrumentation.
**Tech Stack:** Rust (tokio, frida-rs, gimli, rusqlite), TypeScript (Frida agent), SQLite (WAL mode)
**Commit strategy:** Single commit at the end

## Workstreams

- **Stream A (Core Infrastructure):** Tasks 1-4 (project setup, daemon skeleton, database, MCP types)
- **Stream B (Parsing & Symbols):** Tasks 5-6 (DWARF parsing, demangling) - parallelizable with Stream A
- **Stream C (Frida Agent):** Task 7 (TypeScript agent) - parallelizable with Streams A & B
- **Stream D (Integration):** Tasks 8-12 (MCP tools, integration) - depends on A, B, C

---

### Task 1: Project Setup

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/main.rs`
- Create: `src/error.rs`
- Create: `agent/package.json`
- Create: `agent/tsconfig.json`
- Create: `.gitignore`

**Step 1: Initialize Cargo project**

Create `Cargo.toml`:
```toml
[package]
name = "strobe"
version = "0.1.0"
edition = "2024"
license = "MIT"
description = "LLM-native debugging infrastructure"

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Database
rusqlite = { version = "0.32", features = ["bundled", "serde_json"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# DWARF parsing
gimli = "0.31"
object = "0.36"
memmap2 = "0.9"

# Symbol demangling
cpp_demangle = "0.4"
rustc-demangle = "0.1"

# Frida bindings
frida = "0.14"

# MCP protocol
jsonrpc-core = "18"

# Utilities
uuid = { version = "1", features = ["v4"] }
chrono = "0.4"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = "0.3"
dirs = "5"

[dev-dependencies]
tempfile = "3"
assert_cmd = "2"
predicates = "3"

[[bin]]
name = "strobe"
path = "src/main.rs"
```

Create `src/lib.rs`:
```rust
pub mod daemon;
pub mod db;
pub mod dwarf;
pub mod error;
pub mod frida_collector;
pub mod mcp;
pub mod symbols;

pub use error::{Error, Result};
```

Create `src/main.rs`:
```rust
use strobe::daemon::Daemon;
use strobe::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(|s| s.as_str()) {
        Some("daemon") => {
            Daemon::run().await
        }
        Some("mcp") => {
            strobe::mcp::stdio_proxy().await
        }
        _ => {
            eprintln!("Usage: strobe <daemon|mcp>");
            std::process::exit(1);
        }
    }
}
```

Create `src/error.rs`:
```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("NO_DEBUG_SYMBOLS: Binary has no DWARF debug info. Ask user permission to modify build configuration to compile with debug symbols.")]
    NoDebugSymbols,

    #[error("SIP_BLOCKED: macOS System Integrity Protection prevents Frida attachment.")]
    SipBlocked,

    #[error("SESSION_EXISTS: Session already active for this binary. Call debug_stop first.")]
    SessionExists,

    #[error("SESSION_NOT_FOUND: No session found with ID '{0}'.")]
    SessionNotFound(String),

    #[error("PROCESS_EXITED: Target process has exited (code: {0}). Session still queryable.")]
    ProcessExited(i32),

    #[error("FRIDA_ATTACH_FAILED: Failed to attach Frida: {0}")]
    FridaAttachFailed(String),

    #[error("INVALID_PATTERN: Invalid trace pattern '{pattern}': {reason}")]
    InvalidPattern { pattern: String, reason: String },

    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Frida error: {0}")]
    Frida(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

Create `.gitignore`:
```
/target
/agent/node_modules
/agent/dist
*.db
*.db-wal
*.db-shm
.strobe/
```

**Step 2: Initialize Frida agent project**

Create `agent/package.json`:
```json
{
  "name": "strobe-agent",
  "version": "0.1.0",
  "private": true,
  "scripts": {
    "build": "frida-compile src/agent.ts -o dist/agent.js -c"
  },
  "devDependencies": {
    "@anthropic/frida-compile": "^10.0.0",
    "@anthropic/frida-types": "^6.0.0",
    "typescript": "^5.0.0"
  }
}
```

Create `agent/tsconfig.json`:
```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "CommonJS",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "outDir": "./dist",
    "rootDir": "./src",
    "types": ["@anthropic/frida-types"]
  },
  "include": ["src/**/*"]
}
```

**Step 3: Verify project compiles**

Run: `cargo check`
Expected: Compiles with warnings about unused modules (OK at this stage)

**Checkpoint:** Project structure established, dependencies declared.

---

### Task 2: Database Module

**Files:**
- Create: `src/db/mod.rs`
- Create: `src/db/schema.rs`
- Create: `src/db/session.rs`
- Create: `src/db/event.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test for database initialization**

Create `src/db/mod.rs`:
```rust
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
        }).unwrap();

        // Query by function name
        let results = db.query_events("test-session", |q| {
            q.function_contains("process")
        }).unwrap();

        assert_eq!(results.len(), 2);
    }
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test db::tests`
Expected: FAIL - module `schema` not found

**Step 3: Implement database schema**

Create `src/db/schema.rs`:
```rust
use rusqlite::{Connection, params};
use std::path::Path;
use std::sync::{Arc, Mutex};
use crate::Result;

pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for concurrent access
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;

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

        conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                binary_path TEXT NOT NULL,
                project_root TEXT NOT NULL,
                pid INTEGER NOT NULL,
                started_at INTEGER NOT NULL,
                ended_at INTEGER,
                status TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
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
                FOREIGN KEY (session_id) REFERENCES sessions(id)
            );

            CREATE INDEX IF NOT EXISTS idx_session_time ON events(session_id, timestamp_ns);
            CREATE INDEX IF NOT EXISTS idx_function ON events(function_name);
            CREATE INDEX IF NOT EXISTS idx_source ON events(source_file);

            CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
                function_name,
                source_file,
                content=events,
                content_rowid=rowid
            );

            CREATE TRIGGER IF NOT EXISTS events_ai AFTER INSERT ON events BEGIN
                INSERT INTO events_fts(rowid, function_name, source_file)
                VALUES (new.rowid, new.function_name, new.source_file);
            END;

            CREATE TRIGGER IF NOT EXISTS events_ad AFTER DELETE ON events BEGIN
                INSERT INTO events_fts(events_fts, rowid, function_name, source_file)
                VALUES ('delete', old.rowid, old.function_name, old.source_file);
            END;
        "#)?;

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

    pub(crate) fn connection(&self) -> std::sync::MutexGuard<Connection> {
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
```

Create `src/db/session.rs`:
```rust
use rusqlite::params;
use serde::{Deserialize, Serialize};
use crate::Result;
use super::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Running,
    Exited,
    Stopped,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Stopped => "stopped",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "stopped" => Some(Self::Stopped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub binary_path: String,
    pub project_root: String,
    pub pid: u32,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub status: SessionStatus,
}

impl Database {
    pub fn create_session(
        &self,
        id: &str,
        binary_path: &str,
        project_root: &str,
        pid: u32,
    ) -> Result<Session> {
        let conn = self.connection();
        let started_at = chrono::Utc::now().timestamp();

        conn.execute(
            "INSERT INTO sessions (id, binary_path, project_root, pid, started_at, status)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![id, binary_path, project_root, pid, started_at, "running"],
        )?;

        Ok(Session {
            id: id.to_string(),
            binary_path: binary_path.to_string(),
            project_root: project_root.to_string(),
            pid,
            started_at,
            ended_at: None,
            status: SessionStatus::Running,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            "SELECT id, binary_path, project_root, pid, started_at, ended_at, status
             FROM sessions WHERE id = ?"
        )?;

        let session = stmt.query_row(params![id], |row| {
            Ok(Session {
                id: row.get(0)?,
                binary_path: row.get(1)?,
                project_root: row.get(2)?,
                pid: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                status: SessionStatus::from_str(&row.get::<_, String>(6)?).unwrap(),
            })
        });

        match session {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_session_by_binary(&self, binary_path: &str) -> Result<Option<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            "SELECT id, binary_path, project_root, pid, started_at, ended_at, status
             FROM sessions WHERE binary_path = ? AND status = 'running'"
        )?;

        let session = stmt.query_row(params![binary_path], |row| {
            Ok(Session {
                id: row.get(0)?,
                binary_path: row.get(1)?,
                project_root: row.get(2)?,
                pid: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                status: SessionStatus::from_str(&row.get::<_, String>(6)?).unwrap(),
            })
        });

        match session {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn update_session_status(&self, id: &str, status: SessionStatus) -> Result<()> {
        let conn = self.connection();
        let ended_at = if status != SessionStatus::Running {
            Some(chrono::Utc::now().timestamp())
        } else {
            None
        };

        conn.execute(
            "UPDATE sessions SET status = ?, ended_at = ? WHERE id = ?",
            params![status.as_str(), ended_at, id],
        )?;

        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.connection();

        // Delete events first (foreign key)
        conn.execute("DELETE FROM events WHERE session_id = ?", params![id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?", params![id])?;

        Ok(())
    }

    pub fn count_session_events(&self, session_id: &str) -> Result<u64> {
        let conn = self.connection();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE session_id = ?",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count as u64)
    }
}
```

Create `src/db/event.rs`:
```rust
use rusqlite::params;
use serde::{Deserialize, Serialize};
use crate::Result;
use super::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FunctionEnter,
    FunctionExit,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FunctionEnter => "function_enter",
            Self::FunctionExit => "function_exit",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function_enter" => Some(Self::FunctionEnter),
            "function_exit" => Some(Self::FunctionExit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub session_id: String,
    pub timestamp_ns: i64,
    pub thread_id: i64,
    pub parent_event_id: Option<String>,
    pub event_type: EventType,
    pub function_name: String,
    pub function_name_raw: Option<String>,
    pub source_file: Option<String>,
    pub line_number: Option<i32>,
    pub arguments: Option<serde_json::Value>,
    pub return_value: Option<serde_json::Value>,
    pub duration_ns: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEventSummary {
    pub id: String,
    pub timestamp_ns: i64,
    pub function: String,
    #[serde(rename = "sourceFile")]
    pub source_file: String,
    pub line: i32,
    pub duration_ns: i64,
    #[serde(rename = "returnType")]
    pub return_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEventVerbose {
    pub id: String,
    pub timestamp_ns: i64,
    pub function: String,
    #[serde(rename = "functionRaw")]
    pub function_raw: String,
    #[serde(rename = "sourceFile")]
    pub source_file: String,
    pub line: i32,
    pub duration_ns: i64,
    #[serde(rename = "threadId")]
    pub thread_id: i64,
    #[serde(rename = "parentEventId")]
    pub parent_event_id: Option<String>,
    pub arguments: Vec<serde_json::Value>,
    #[serde(rename = "returnValue")]
    pub return_value: serde_json::Value,
}

pub struct EventQuery {
    pub event_type: Option<EventType>,
    pub function_equals: Option<String>,
    pub function_contains: Option<String>,
    pub function_matches: Option<String>,
    pub source_file_equals: Option<String>,
    pub source_file_contains: Option<String>,
    pub return_value_equals: Option<serde_json::Value>,
    pub return_value_is_null: Option<bool>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for EventQuery {
    fn default() -> Self {
        Self {
            event_type: None,
            function_equals: None,
            function_contains: None,
            function_matches: None,
            source_file_equals: None,
            source_file_contains: None,
            return_value_equals: None,
            return_value_is_null: None,
            limit: 50,
            offset: 0,
        }
    }
}

impl EventQuery {
    pub fn function_contains(mut self, s: &str) -> Self {
        self.function_contains = Some(s.to_string());
        self
    }

    pub fn function_equals(mut self, s: &str) -> Self {
        self.function_equals = Some(s.to_string());
        self
    }

    pub fn source_file_contains(mut self, s: &str) -> Self {
        self.source_file_contains = Some(s.to_string());
        self
    }

    pub fn event_type(mut self, t: EventType) -> Self {
        self.event_type = Some(t);
        self
    }

    pub fn limit(mut self, n: u32) -> Self {
        self.limit = n.min(500);
        self
    }

    pub fn offset(mut self, n: u32) -> Self {
        self.offset = n;
        self
    }
}

impl Database {
    pub fn insert_event(&self, event: Event) -> Result<()> {
        let conn = self.connection();

        conn.execute(
            "INSERT INTO events (id, session_id, timestamp_ns, thread_id, parent_event_id,
             event_type, function_name, function_name_raw, source_file, line_number,
             arguments, return_value, duration_ns)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                event.id,
                event.session_id,
                event.timestamp_ns,
                event.thread_id,
                event.parent_event_id,
                event.event_type.as_str(),
                event.function_name,
                event.function_name_raw,
                event.source_file,
                event.line_number,
                event.arguments.map(|v| v.to_string()),
                event.return_value.map(|v| v.to_string()),
                event.duration_ns,
            ],
        )?;

        Ok(())
    }

    pub fn insert_events_batch(&self, events: &[Event]) -> Result<()> {
        let conn = self.connection();

        for event in events {
            conn.execute(
                "INSERT INTO events (id, session_id, timestamp_ns, thread_id, parent_event_id,
                 event_type, function_name, function_name_raw, source_file, line_number,
                 arguments, return_value, duration_ns)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    event.id,
                    event.session_id,
                    event.timestamp_ns,
                    event.thread_id,
                    event.parent_event_id,
                    event.event_type.as_str(),
                    event.function_name,
                    event.function_name_raw,
                    event.source_file,
                    event.line_number,
                    event.arguments.as_ref().map(|v| v.to_string()),
                    event.return_value.as_ref().map(|v| v.to_string()),
                    event.duration_ns,
                ],
            )?;
        }

        Ok(())
    }

    pub fn query_events<F>(&self, session_id: &str, build_query: F) -> Result<Vec<Event>>
    where
        F: FnOnce(EventQuery) -> EventQuery,
    {
        let query = build_query(EventQuery::default());
        let conn = self.connection();

        let mut sql = String::from(
            "SELECT id, session_id, timestamp_ns, thread_id, parent_event_id,
             event_type, function_name, function_name_raw, source_file, line_number,
             arguments, return_value, duration_ns
             FROM events WHERE session_id = ?"
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(session_id.to_string())];

        if let Some(ref et) = query.event_type {
            sql.push_str(" AND event_type = ?");
            params_vec.push(Box::new(et.as_str().to_string()));
        }

        if let Some(ref f) = query.function_equals {
            sql.push_str(" AND function_name = ?");
            params_vec.push(Box::new(f.clone()));
        }

        if let Some(ref f) = query.function_contains {
            sql.push_str(" AND function_name LIKE ?");
            params_vec.push(Box::new(format!("%{}%", f)));
        }

        if let Some(ref f) = query.source_file_contains {
            sql.push_str(" AND source_file LIKE ?");
            params_vec.push(Box::new(format!("%{}%", f)));
        }

        if let Some(is_null) = query.return_value_is_null {
            if is_null {
                sql.push_str(" AND return_value IS NULL");
            } else {
                sql.push_str(" AND return_value IS NOT NULL");
            }
        }

        sql.push_str(" ORDER BY timestamp_ns ASC");
        sql.push_str(&format!(" LIMIT {} OFFSET {}", query.limit, query.offset));

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let events = stmt.query_map(params_refs.as_slice(), |row| {
            let event_type_str: String = row.get(5)?;
            let args_str: Option<String> = row.get(10)?;
            let ret_str: Option<String> = row.get(11)?;

            Ok(Event {
                id: row.get(0)?,
                session_id: row.get(1)?,
                timestamp_ns: row.get(2)?,
                thread_id: row.get(3)?,
                parent_event_id: row.get(4)?,
                event_type: EventType::from_str(&event_type_str).unwrap(),
                function_name: row.get(6)?,
                function_name_raw: row.get(7)?,
                source_file: row.get(8)?,
                line_number: row.get(9)?,
                arguments: args_str.and_then(|s| serde_json::from_str(&s).ok()),
                return_value: ret_str.and_then(|s| serde_json::from_str(&s).ok()),
                duration_ns: row.get(12)?,
            })
        })?;

        events.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn count_events(&self, session_id: &str) -> Result<u64> {
        self.count_session_events(session_id)
    }
}
```

Update `src/lib.rs`:
```rust
pub mod db;
pub mod error;

pub use error::{Error, Result};
```

**Step 4: Run tests - verify they pass**

Run: `cargo test db::tests`
Expected: PASS - all 3 tests pass

**Checkpoint:** Database module complete with session and event storage.

---

### Task 3: Symbol Demangling Module

**Files:**
- Create: `src/symbols/mod.rs`
- Create: `src/symbols/demangle.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test**

Create `src/symbols/mod.rs`:
```rust
mod demangle;

pub use demangle::demangle_symbol;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demangle_rust_symbol() {
        let mangled = "_ZN4test7example17h1234567890abcdefE";
        let demangled = demangle_symbol(mangled);
        assert!(demangled.contains("test::example"));
    }

    #[test]
    fn test_demangle_cpp_symbol() {
        let mangled = "_ZN4test7exampleEv";
        let demangled = demangle_symbol(mangled);
        assert!(demangled.contains("test::example"));
    }

    #[test]
    fn test_demangle_c_symbol() {
        // C symbols have no mangling
        let symbol = "main";
        let demangled = demangle_symbol(symbol);
        assert_eq!(demangled, "main");
    }

    #[test]
    fn test_demangle_unknown() {
        // Unknown format returns as-is
        let symbol = "some_random_symbol";
        let demangled = demangle_symbol(symbol);
        assert_eq!(demangled, "some_random_symbol");
    }
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test symbols::tests`
Expected: FAIL - module `demangle` not found

**Step 3: Implement demangling**

Create `src/symbols/demangle.rs`:
```rust
use rustc_demangle::demangle as rust_demangle;
use cpp_demangle::Symbol as CppSymbol;

/// Demangle a symbol name from any supported format (Rust, C++, or plain C).
/// Returns the demangled name, or the original if demangling fails.
pub fn demangle_symbol(mangled: &str) -> String {
    // Try Rust demangling first
    let rust_demangled = rust_demangle(mangled).to_string();
    if rust_demangled != mangled {
        return rust_demangled;
    }

    // Try C++ (Itanium ABI) demangling
    if let Ok(symbol) = CppSymbol::new(mangled) {
        if let Ok(demangled) = symbol.demangle(&cpp_demangle::DemangleOptions::default()) {
            return demangled;
        }
    }

    // Return original if no demangling worked (plain C or unknown)
    mangled.to_string()
}
```

Update `src/lib.rs`:
```rust
pub mod db;
pub mod error;
pub mod symbols;

pub use error::{Error, Result};
```

**Step 4: Run test - verify it passes**

Run: `cargo test symbols::tests`
Expected: PASS

**Checkpoint:** Symbol demangling works for Rust, C++, and C.

---

### Task 4: DWARF Parsing Module

**Files:**
- Create: `src/dwarf/mod.rs`
- Create: `src/dwarf/parser.rs`
- Create: `src/dwarf/function.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test**

Create `src/dwarf/mod.rs`:
```rust
mod parser;
mod function;

pub use parser::DwarfParser;
pub use function::FunctionInfo;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parser_no_debug_info() {
        // A binary without debug info should return an error
        let result = DwarfParser::parse(Path::new("/bin/ls"));
        // Note: /bin/ls typically has no debug info
        // This may need adjustment based on system
        assert!(result.is_err() || result.unwrap().functions.is_empty());
    }

    #[test]
    fn test_function_info() {
        let func = FunctionInfo {
            name: "main::process".to_string(),
            name_raw: Some("_ZN4main7processEv".to_string()),
            low_pc: 0x1000,
            high_pc: 0x1100,
            source_file: Some("/home/user/src/main.rs".to_string()),
            line_number: Some(42),
        };

        assert!(func.contains_address(0x1050));
        assert!(!func.contains_address(0x2000));
    }

    #[test]
    fn test_user_code_detection() {
        let func = FunctionInfo {
            name: "myapp::handler".to_string(),
            name_raw: None,
            low_pc: 0x1000,
            high_pc: 0x1100,
            source_file: Some("/home/user/myproject/src/handler.rs".to_string()),
            line_number: Some(10),
        };

        assert!(func.is_user_code("/home/user/myproject"));
        assert!(!func.is_user_code("/home/user/otherproject"));
    }
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test dwarf::tests`
Expected: FAIL - module not found

**Step 3: Implement DWARF parsing**

Create `src/dwarf/function.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub name: String,
    pub name_raw: Option<String>,
    pub low_pc: u64,
    pub high_pc: u64,
    pub source_file: Option<String>,
    pub line_number: Option<u32>,
}

impl FunctionInfo {
    pub fn contains_address(&self, addr: u64) -> bool {
        addr >= self.low_pc && addr < self.high_pc
    }

    pub fn is_user_code(&self, project_root: &str) -> bool {
        self.source_file
            .as_ref()
            .map(|f| f.starts_with(project_root))
            .unwrap_or(false)
    }
}
```

Create `src/dwarf/parser.rs`:
```rust
use gimli::{self, RunTimeEndian, EndianSlice, SectionId};
use object::{Object, ObjectSection};
use memmap2::Mmap;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use crate::{Error, Result};
use crate::symbols::demangle_symbol;
use super::FunctionInfo;

pub struct DwarfParser {
    pub functions: Vec<FunctionInfo>,
    functions_by_name: HashMap<String, Vec<usize>>,
}

impl DwarfParser {
    pub fn parse(binary_path: &Path) -> Result<Self> {
        let file = File::open(binary_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = object::File::parse(&*mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        // Check if debug info exists
        if object.section_by_name(".debug_info").is_none() {
            return Err(Error::NoDebugSymbols);
        }

        let endian = if object.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        let load_section = |id: SectionId| -> std::result::Result<Cow<[u8]>, gimli::Error> {
            let data = object
                .section_by_name(id.name())
                .and_then(|section| section.data().ok())
                .unwrap_or(&[]);
            Ok(Cow::Borrowed(data))
        };

        let dwarf_cow = gimli::Dwarf::load(&load_section)
            .map_err(|e| Error::Frida(format!("Failed to load DWARF: {}", e)))?;

        let dwarf = dwarf_cow.borrow(|section| {
            EndianSlice::new(section.as_ref(), endian)
        });

        let mut functions = Vec::new();

        // Iterate through compilation units
        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let unit = dwarf.unit(header)
                .map_err(|e| Error::Frida(format!("Failed to parse unit: {}", e)))?;

            let mut entries = unit.entries();
            while let Ok(Some((_, entry))) = entries.next_dfs() {
                if entry.tag() == gimli::DW_TAG_subprogram {
                    if let Some(func) = Self::parse_function(&dwarf, &unit, entry)? {
                        functions.push(func);
                    }
                }
            }
        }

        // Build index
        let mut functions_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, func) in functions.iter().enumerate() {
            functions_by_name
                .entry(func.name.clone())
                .or_default()
                .push(idx);
        }

        Ok(Self {
            functions,
            functions_by_name,
        })
    }

    fn parse_function<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Result<Option<FunctionInfo>> {
        // Get function name
        let name = match entry.attr_value(gimli::DW_AT_name)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::DebugStrRef(offset)) => {
                dwarf.debug_str.get_str(offset)
                    .map_err(|e| Error::Frida(format!("DWARF string error: {}", e)))?
                    .to_string_lossy()
                    .map_err(|e| Error::Frida(format!("UTF-8 error: {}", e)))?
                    .to_string()
            }
            Some(gimli::AttributeValue::String(s)) => {
                s.to_string_lossy()
                    .map_err(|e| Error::Frida(format!("UTF-8 error: {}", e)))?
                    .to_string()
            }
            _ => return Ok(None),
        };

        // Get low_pc
        let low_pc = match entry.attr_value(gimli::DW_AT_low_pc)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::Addr(addr)) => addr,
            _ => return Ok(None),
        };

        // Get high_pc (can be absolute address or offset from low_pc)
        let high_pc = match entry.attr_value(gimli::DW_AT_high_pc)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::Addr(addr)) => addr,
            Some(gimli::AttributeValue::Udata(offset)) => low_pc + offset,
            _ => low_pc + 1, // Minimal range if not specified
        };

        // Get source file
        let source_file = match entry.attr_value(gimli::DW_AT_decl_file)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::FileIndex(index)) => {
                if let Some(line_program) = &unit.line_program {
                    let header = line_program.header();
                    if let Some(file) = header.file(index) {
                        let mut path = String::new();
                        if let Some(dir) = file.directory(header) {
                            if let Ok(s) = dwarf.attr_string(unit, dir) {
                                path.push_str(&s.to_string_lossy().unwrap_or_default());
                                path.push('/');
                            }
                        }
                        if let Ok(s) = dwarf.attr_string(unit, file.path_name()) {
                            path.push_str(&s.to_string_lossy().unwrap_or_default());
                        }
                        if !path.is_empty() {
                            Some(path)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        // Get line number
        let line_number = match entry.attr_value(gimli::DW_AT_decl_line)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::Udata(n)) => Some(n as u32),
            _ => None,
        };

        // Demangle the name
        let demangled = demangle_symbol(&name);

        Ok(Some(FunctionInfo {
            name: demangled,
            name_raw: if name != demangled { Some(name) } else { None },
            low_pc,
            high_pc,
            source_file,
            line_number,
        }))
    }

    pub fn find_by_name(&self, name: &str) -> Vec<&FunctionInfo> {
        self.functions_by_name
            .get(name)
            .map(|indices| indices.iter().map(|&i| &self.functions[i]).collect())
            .unwrap_or_default()
    }

    pub fn find_by_pattern(&self, pattern: &str) -> Vec<&FunctionInfo> {
        let matcher = PatternMatcher::new(pattern);
        self.functions
            .iter()
            .filter(|f| matcher.matches(&f.name))
            .collect()
    }

    pub fn user_code_functions(&self, project_root: &str) -> Vec<&FunctionInfo> {
        self.functions
            .iter()
            .filter(|f| f.is_user_code(project_root))
            .collect()
    }
}

/// Glob-style pattern matcher for function names
pub struct PatternMatcher {
    pattern: String,
}

impl PatternMatcher {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        self.glob_match(&self.pattern, name)
    }

    fn glob_match(&self, pattern: &str, text: &str) -> bool {
        let mut p_chars = pattern.chars().peekable();
        let mut t_chars = text.chars().peekable();

        while let Some(p) = p_chars.next() {
            match p {
                '*' => {
                    if p_chars.peek() == Some(&'*') {
                        // ** matches anything including ::
                        p_chars.next();
                        let remaining: String = p_chars.collect();
                        if remaining.is_empty() {
                            return true;
                        }
                        // Try matching remaining pattern at every position
                        let t_remaining: String = t_chars.collect();
                        for i in 0..=t_remaining.len() {
                            if self.glob_match(&remaining, &t_remaining[i..]) {
                                return true;
                            }
                        }
                        return false;
                    } else {
                        // * matches anything except ::
                        let remaining: String = p_chars.collect();
                        if remaining.is_empty() {
                            // Consume rest of text, but stop at ::
                            return !text.contains("::");
                        }
                        // Try matching at every position that isn't after ::
                        let t_remaining: String = t_chars.collect();
                        for (i, c) in t_remaining.char_indices() {
                            if c == ':' && t_remaining.get(i+1..i+2) == Some(":") {
                                break;
                            }
                            if self.glob_match(&remaining, &t_remaining[i..]) {
                                return true;
                            }
                        }
                        // Also try matching at the very end
                        return self.glob_match(&remaining, "");
                    }
                }
                c => {
                    if t_chars.next() != Some(c) {
                        return false;
                    }
                }
            }
        }

        t_chars.next().is_none()
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let m = PatternMatcher::new("foo::bar");
        assert!(m.matches("foo::bar"));
        assert!(!m.matches("foo::baz"));
    }

    #[test]
    fn test_single_star() {
        let m = PatternMatcher::new("foo::*");
        assert!(m.matches("foo::bar"));
        assert!(m.matches("foo::baz"));
        assert!(!m.matches("foo::bar::qux")); // * doesn't match ::
    }

    #[test]
    fn test_double_star() {
        let m = PatternMatcher::new("foo::**");
        assert!(m.matches("foo::bar"));
        assert!(m.matches("foo::bar::baz"));
        assert!(m.matches("foo::bar::baz::qux"));
    }

    #[test]
    fn test_star_middle() {
        let m = PatternMatcher::new("*::process");
        assert!(m.matches("main::process"));
        assert!(m.matches("foo::process"));
        assert!(!m.matches("main::sub::process")); // * doesn't cross ::
    }

    #[test]
    fn test_double_star_middle() {
        let m = PatternMatcher::new("auth::**::validate");
        assert!(m.matches("auth::validate"));
        assert!(m.matches("auth::user::validate"));
        assert!(m.matches("auth::user::session::validate"));
    }
}
```

Update `src/lib.rs`:
```rust
pub mod db;
pub mod dwarf;
pub mod error;
pub mod symbols;

pub use error::{Error, Result};
```

**Step 4: Run tests - verify they pass**

Run: `cargo test dwarf::tests`
Run: `cargo test dwarf::parser::pattern_tests`
Expected: PASS

**Checkpoint:** DWARF parsing and pattern matching work.

---

### Task 5: MCP Protocol Types

**Files:**
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/types.rs`
- Create: `src/mcp/protocol.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test**

Create `src/mcp/mod.rs`:
```rust
mod types;
mod protocol;

pub use types::*;
pub use protocol::*;

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
            limit: Some(100),
            offset: None,
            verbose: Some(true),
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
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test mcp::tests`
Expected: FAIL - module not found

**Step 3: Implement MCP types**

Create `src/mcp/types.rs`:
```rust
use serde::{Deserialize, Serialize};

// ============ debug_launch ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLaunchRequest {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLaunchResponse {
    pub session_id: String,
    pub pid: u32,
}

// ============ debug_trace ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTraceRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTraceResponse {
    pub active_patterns: Vec<String>,
    pub hooked_functions: u32,
}

// ============ debug_query ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFileFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReturnValueFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equals: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_null: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugQueryRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<EventTypeFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<FunctionFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<SourceFileFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_value: Option<ReturnValueFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugQueryResponse {
    pub events: Vec<serde_json::Value>,
    pub total_count: u64,
    pub has_more: bool,
}

// ============ debug_stop ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStopRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugStopResponse {
    pub success: bool,
    pub events_collected: u64,
}

// ============ Errors ============

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NoDebugSymbols,
    SipBlocked,
    SessionExists,
    SessionNotFound,
    ProcessExited,
    FridaAttachFailed,
    InvalidPattern,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpError {
    pub code: ErrorCode,
    pub message: String,
}

impl From<crate::Error> for McpError {
    fn from(err: crate::Error) -> Self {
        let code = match &err {
            crate::Error::NoDebugSymbols => ErrorCode::NoDebugSymbols,
            crate::Error::SipBlocked => ErrorCode::SipBlocked,
            crate::Error::SessionExists => ErrorCode::SessionExists,
            crate::Error::SessionNotFound(_) => ErrorCode::SessionNotFound,
            crate::Error::ProcessExited(_) => ErrorCode::ProcessExited,
            crate::Error::FridaAttachFailed(_) => ErrorCode::FridaAttachFailed,
            crate::Error::InvalidPattern { .. } => ErrorCode::InvalidPattern,
            _ => ErrorCode::FridaAttachFailed, // Generic fallback
        };

        Self {
            code,
            message: err.to_string(),
        }
    }
}
```

Create `src/mcp/protocol.rs`:
```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: String, data: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message, data }),
        }
    }
}

// MCP-specific protocol messages

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeRequest {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: McpClientCapabilities,
    #[serde(rename = "clientInfo")]
    pub client_info: McpClientInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientCapabilities {
    #[serde(default)]
    pub roots: Option<Value>,
    #[serde(default)]
    pub sampling: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpClientInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpInitializeResponse {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    pub capabilities: McpServerCapabilities,
    #[serde(rename = "serverInfo")]
    pub server_info: McpServerInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerCapabilities {
    pub tools: McpToolsCapability,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolsCapability {
    #[serde(rename = "listChanged")]
    pub list_changed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolsListResponse {
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallRequest {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolCallResponse {
    pub content: Vec<McpContent>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum McpContent {
    #[serde(rename = "text")]
    Text { text: String },
}
```

Update `src/lib.rs`:
```rust
pub mod db;
pub mod dwarf;
pub mod error;
pub mod mcp;
pub mod symbols;

pub use error::{Error, Result};
```

**Step 4: Run tests - verify they pass**

Run: `cargo test mcp::tests`
Expected: PASS

**Checkpoint:** MCP protocol types defined and serializable.

---

### Task 6: Daemon Skeleton

**Files:**
- Create: `src/daemon/mod.rs`
- Create: `src/daemon/server.rs`
- Create: `src/daemon/session_manager.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test**

Create `src/daemon/mod.rs`:
```rust
mod server;
mod session_manager;

pub use server::Daemon;
pub use session_manager::SessionManager;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_session_manager_create() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let manager = SessionManager::new(&db_path).unwrap();

        let session_id = manager.generate_session_id("myapp");
        assert!(session_id.starts_with("myapp-"));
    }

    #[tokio::test]
    async fn test_session_id_collision_handling() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let manager = SessionManager::new(&db_path).unwrap();

        // Create first session
        let id1 = manager.generate_session_id("myapp");
        manager.create_session(&id1, "/bin/myapp", "/home/user", 1234).unwrap();

        // Second session should get -2 suffix if same timestamp
        // (In practice timestamps differ, but the logic should handle it)
        let id2 = manager.generate_session_id("myapp");

        // IDs should be different
        assert_ne!(id1, id2);
    }
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test daemon::tests`
Expected: FAIL - module not found

**Step 3: Implement daemon skeleton**

Create `src/daemon/session_manager.rs`:
```rust
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use chrono::Utc;
use crate::db::{Database, Session, SessionStatus};
use crate::dwarf::DwarfParser;
use crate::Result;

pub struct SessionManager {
    db: Database,
    /// Active trace patterns per session
    patterns: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Cached DWARF parsers per binary
    dwarf_cache: Arc<RwLock<HashMap<String, Arc<DwarfParser>>>>,
    /// Hooked function count per session
    hook_counts: Arc<RwLock<HashMap<String, u32>>>,
}

impl SessionManager {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db = Database::open(db_path)?;

        Ok(Self {
            db,
            patterns: Arc::new(RwLock::new(HashMap::new())),
            dwarf_cache: Arc::new(RwLock::new(HashMap::new())),
            hook_counts: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    pub fn generate_session_id(&self, binary_name: &str) -> String {
        let now = Utc::now();
        let base_id = format!(
            "{}-{}-{:02}h{:02}",
            binary_name,
            now.format("%Y-%m-%d"),
            now.hour(),
            now.minute()
        );

        // Check for collision
        let mut id = base_id.clone();
        let mut suffix = 2;

        while self.db.get_session(&id).ok().flatten().is_some() {
            id = format!("{}-{}", base_id, suffix);
            suffix += 1;
        }

        id
    }

    pub fn create_session(
        &self,
        id: &str,
        binary_path: &str,
        project_root: &str,
        pid: u32,
    ) -> Result<Session> {
        // Check for existing active session on this binary
        if let Some(existing) = self.db.get_session_by_binary(binary_path)? {
            if existing.status == SessionStatus::Running {
                return Err(crate::Error::SessionExists);
            }
        }

        let session = self.db.create_session(id, binary_path, project_root, pid)?;

        // Initialize pattern storage
        self.patterns.write().unwrap().insert(id.to_string(), Vec::new());
        self.hook_counts.write().unwrap().insert(id.to_string(), 0);

        Ok(session)
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        self.db.get_session(id)
    }

    pub fn stop_session(&self, id: &str) -> Result<u64> {
        let count = self.db.count_session_events(id)?;
        self.db.delete_session(id)?;

        // Clean up in-memory state
        self.patterns.write().unwrap().remove(id);
        self.hook_counts.write().unwrap().remove(id);

        Ok(count)
    }

    pub fn add_patterns(&self, session_id: &str, patterns: &[String]) -> Result<()> {
        let mut all_patterns = self.patterns.write().unwrap();
        let session_patterns = all_patterns
            .entry(session_id.to_string())
            .or_default();

        for pattern in patterns {
            if !session_patterns.contains(pattern) {
                session_patterns.push(pattern.clone());
            }
        }

        Ok(())
    }

    pub fn remove_patterns(&self, session_id: &str, patterns: &[String]) -> Result<()> {
        let mut all_patterns = self.patterns.write().unwrap();
        if let Some(session_patterns) = all_patterns.get_mut(session_id) {
            session_patterns.retain(|p| !patterns.contains(p));
        }
        Ok(())
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        self.patterns
            .read()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_hook_count(&self, session_id: &str, count: u32) {
        self.hook_counts
            .write()
            .unwrap()
            .insert(session_id.to_string(), count);
    }

    pub fn get_hook_count(&self, session_id: &str) -> u32 {
        self.hook_counts
            .read()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn get_or_parse_dwarf(&self, binary_path: &str) -> Result<Arc<DwarfParser>> {
        // Check cache first
        {
            let cache = self.dwarf_cache.read().unwrap();
            if let Some(parser) = cache.get(binary_path) {
                return Ok(Arc::clone(parser));
            }
        }

        // Parse and cache
        let parser = Arc::new(DwarfParser::parse(Path::new(binary_path))?);
        self.dwarf_cache
            .write()
            .unwrap()
            .insert(binary_path.to_string(), Arc::clone(&parser));

        Ok(parser)
    }

    pub fn db(&self) -> &Database {
        &self.db
    }
}

use chrono::Timelike;
```

Create `src/daemon/server.rs`:
```rust
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;
use tokio::time::Instant;
use crate::mcp::*;
use crate::Result;
use super::SessionManager;

const IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60); // 30 minutes

pub struct Daemon {
    socket_path: PathBuf,
    pid_path: PathBuf,
    session_manager: Arc<SessionManager>,
    last_activity: Arc<RwLock<Instant>>,
}

impl Daemon {
    pub async fn run() -> Result<()> {
        let strobe_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".strobe");

        std::fs::create_dir_all(&strobe_dir)?;

        let socket_path = strobe_dir.join("strobe.sock");
        let pid_path = strobe_dir.join("strobe.pid");
        let db_path = strobe_dir.join("strobe.db");

        // Remove stale socket
        let _ = std::fs::remove_file(&socket_path);

        // Write PID file
        std::fs::write(&pid_path, std::process::id().to_string())?;

        let session_manager = Arc::new(SessionManager::new(&db_path)?);

        let daemon = Arc::new(Self {
            socket_path: socket_path.clone(),
            pid_path,
            session_manager,
            last_activity: Arc::new(RwLock::new(Instant::now())),
        });

        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!("Daemon listening on {:?}", socket_path);

        // Spawn idle timeout checker
        let daemon_clone = Arc::clone(&daemon);
        tokio::spawn(async move {
            daemon_clone.idle_timeout_loop().await;
        });

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = Arc::clone(&daemon);
                    tokio::spawn(async move {
                        if let Err(e) = daemon.handle_connection(stream).await {
                            tracing::error!("Connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    async fn idle_timeout_loop(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            let last = *self.last_activity.read().await;
            if last.elapsed() > IDLE_TIMEOUT {
                tracing::info!("Idle timeout reached, shutting down");
                self.cleanup();
                std::process::exit(0);
            }
        }
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.socket_path);
        let _ = std::fs::remove_file(&self.pid_path);
    }

    async fn handle_connection(&self, stream: UnixStream) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break; // EOF
            }

            // Update activity timestamp
            *self.last_activity.write().await = Instant::now();

            let response = self.handle_message(&line).await;
            let response_json = serde_json::to_string(&response)?;
            writer.write_all(response_json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        Ok(())
    }

    async fn handle_message(&self, message: &str) -> JsonRpcResponse {
        let request: JsonRpcRequest = match serde_json::from_str(message) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::error(
                    serde_json::Value::Null,
                    -32700,
                    format!("Parse error: {}", e),
                    None,
                );
            }
        };

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize(&request.params).await,
            "initialized" => Ok(serde_json::json!({})),
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tools_call(&request.params).await,
            _ => Err(crate::Error::Frida(format!(
                "Unknown method: {}",
                request.method
            ))),
        };

        match result {
            Ok(value) => JsonRpcResponse::success(request.id, value),
            Err(e) => {
                let mcp_error: McpError = e.into();
                JsonRpcResponse::error(
                    request.id,
                    -32000,
                    mcp_error.message,
                    Some(serde_json::to_value(mcp_error.code).unwrap()),
                )
            }
        }
    }

    async fn handle_initialize(&self, _params: &serde_json::Value) -> Result<serde_json::Value> {
        let response = McpInitializeResponse {
            protocol_version: "2024-11-05".to_string(),
            capabilities: McpServerCapabilities {
                tools: McpToolsCapability { list_changed: false },
            },
            server_info: McpServerInfo {
                name: "strobe".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn handle_tools_list(&self) -> Result<serde_json::Value> {
        let tools = vec![
            McpTool {
                name: "debug_launch".to_string(),
                description: "Start a new debug session by launching a binary with Frida".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Path to executable" },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "Command line arguments" },
                        "cwd": { "type": "string", "description": "Working directory" },
                        "projectRoot": { "type": "string", "description": "Root directory for user code detection" },
                        "env": { "type": "object", "description": "Additional environment variables" }
                    },
                    "required": ["command", "projectRoot"]
                }),
            },
            McpTool {
                name: "debug_trace".to_string(),
                description: "Add or remove trace patterns for a debug session".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string", "description": "Session ID" },
                        "add": { "type": "array", "items": { "type": "string" }, "description": "Patterns to start tracing" },
                        "remove": { "type": "array", "items": { "type": "string" }, "description": "Patterns to stop tracing" }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_query".to_string(),
                description: "Query execution history from a debug session".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" },
                        "eventType": { "type": "string", "enum": ["function_enter", "function_exit"] },
                        "function": {
                            "type": "object",
                            "properties": {
                                "equals": { "type": "string" },
                                "contains": { "type": "string" },
                                "matches": { "type": "string" }
                            }
                        },
                        "sourceFile": {
                            "type": "object",
                            "properties": {
                                "equals": { "type": "string" },
                                "contains": { "type": "string" }
                            }
                        },
                        "returnValue": {
                            "type": "object",
                            "properties": {
                                "equals": {},
                                "isNull": { "type": "boolean" }
                            }
                        },
                        "limit": { "type": "integer", "default": 50, "maximum": 500 },
                        "offset": { "type": "integer" },
                        "verbose": { "type": "boolean", "default": false }
                    },
                    "required": ["sessionId"]
                }),
            },
            McpTool {
                name: "debug_stop".to_string(),
                description: "Stop a debug session and clean up resources".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string" }
                    },
                    "required": ["sessionId"]
                }),
            },
        ];

        let response = McpToolsListResponse { tools };
        Ok(serde_json::to_value(response)?)
    }

    async fn handle_tools_call(&self, params: &serde_json::Value) -> Result<serde_json::Value> {
        let call: McpToolCallRequest = serde_json::from_value(params.clone())?;

        let result = match call.name.as_str() {
            "debug_launch" => self.tool_debug_launch(&call.arguments).await,
            "debug_trace" => self.tool_debug_trace(&call.arguments).await,
            "debug_query" => self.tool_debug_query(&call.arguments).await,
            "debug_stop" => self.tool_debug_stop(&call.arguments).await,
            _ => Err(crate::Error::Frida(format!("Unknown tool: {}", call.name))),
        };

        match result {
            Ok(value) => {
                let response = McpToolCallResponse {
                    content: vec![McpContent::Text {
                        text: serde_json::to_string_pretty(&value)?,
                    }],
                    is_error: None,
                };
                Ok(serde_json::to_value(response)?)
            }
            Err(e) => {
                let mcp_error: McpError = e.into();
                let response = McpToolCallResponse {
                    content: vec![McpContent::Text {
                        text: format!("{}: {}", serde_json::to_string(&mcp_error.code)?, mcp_error.message),
                    }],
                    is_error: Some(true),
                };
                Ok(serde_json::to_value(response)?)
            }
        }
    }

    async fn tool_debug_launch(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugLaunchRequest = serde_json::from_value(args.clone())?;

        // Extract binary name from path
        let binary_name = std::path::Path::new(&req.command)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let session_id = self.session_manager.generate_session_id(binary_name);

        // TODO: Actually spawn process with Frida (Task 8)
        // For now, return a placeholder
        let pid = 0u32; // Will be set by Frida

        self.session_manager.create_session(
            &session_id,
            &req.command,
            &req.project_root,
            pid,
        )?;

        let response = DebugLaunchResponse {
            session_id,
            pid,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_trace(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let session = self.session_manager.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

        // Add/remove patterns
        if let Some(add) = req.add {
            self.session_manager.add_patterns(&req.session_id, &add)?;
        }
        if let Some(remove) = req.remove {
            self.session_manager.remove_patterns(&req.session_id, &remove)?;
        }

        // TODO: Actually update Frida hooks (Task 8)
        // For now, count matching functions from DWARF
        let patterns = self.session_manager.get_patterns(&req.session_id);
        let hook_count = if !patterns.is_empty() {
            if let Ok(dwarf) = self.session_manager.get_or_parse_dwarf(&session.binary_path) {
                let mut count = 0u32;
                for pattern in &patterns {
                    count += dwarf.find_by_pattern(pattern).len() as u32;
                }
                self.session_manager.set_hook_count(&req.session_id, count);
                count
            } else {
                0
            }
        } else {
            0
        };

        let response = DebugTraceResponse {
            active_patterns: patterns,
            hooked_functions: hook_count,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_query(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugQueryRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.session_manager.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

        let limit = req.limit.unwrap_or(50).min(500);
        let offset = req.offset.unwrap_or(0);

        let events = self.session_manager.db().query_events(&req.session_id, |mut q| {
            if let Some(ref et) = req.event_type {
                q = q.event_type(match et {
                    EventTypeFilter::FunctionEnter => crate::db::EventType::FunctionEnter,
                    EventTypeFilter::FunctionExit => crate::db::EventType::FunctionExit,
                });
            }
            if let Some(ref f) = req.function {
                if let Some(ref eq) = f.equals {
                    q = q.function_equals(eq);
                }
                if let Some(ref contains) = f.contains {
                    q = q.function_contains(contains);
                }
            }
            if let Some(ref sf) = req.source_file {
                if let Some(ref contains) = sf.contains {
                    q = q.source_file_contains(contains);
                }
            }
            q.limit(limit).offset(offset)
        })?;

        let total_count = self.session_manager.db().count_events(&req.session_id)?;
        let has_more = (offset + events.len() as u32) < total_count as u32;

        // Convert to appropriate format
        let verbose = req.verbose.unwrap_or(false);
        let event_values: Vec<serde_json::Value> = if verbose {
            events.iter().map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "timestamp_ns": e.timestamp_ns,
                    "function": e.function_name,
                    "functionRaw": e.function_name_raw,
                    "sourceFile": e.source_file,
                    "line": e.line_number,
                    "duration_ns": e.duration_ns,
                    "threadId": e.thread_id,
                    "parentEventId": e.parent_event_id,
                    "arguments": e.arguments,
                    "returnValue": e.return_value,
                })
            }).collect()
        } else {
            events.iter().map(|e| {
                serde_json::json!({
                    "id": e.id,
                    "timestamp_ns": e.timestamp_ns,
                    "function": e.function_name,
                    "sourceFile": e.source_file,
                    "line": e.line_number,
                    "duration_ns": e.duration_ns,
                    "returnType": e.return_value.as_ref()
                        .map(|v| match v {
                            serde_json::Value::Null => "null",
                            serde_json::Value::Bool(_) => "bool",
                            serde_json::Value::Number(_) => "number",
                            serde_json::Value::String(_) => "string",
                            serde_json::Value::Array(_) => "array",
                            serde_json::Value::Object(_) => "object",
                        })
                        .unwrap_or("void"),
                })
            }).collect()
        };

        let response = DebugQueryResponse {
            events: event_values,
            total_count,
            has_more,
        };

        Ok(serde_json::to_value(response)?)
    }

    async fn tool_debug_stop(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: DebugStopRequest = serde_json::from_value(args.clone())?;

        // Verify session exists
        let _ = self.session_manager.get_session(&req.session_id)?
            .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

        // TODO: Detach Frida cleanly (Task 8)

        let events_collected = self.session_manager.stop_session(&req.session_id)?;

        let response = DebugStopResponse {
            success: true,
            events_collected,
        };

        Ok(serde_json::to_value(response)?)
    }
}
```

Update `src/lib.rs`:
```rust
pub mod daemon;
pub mod db;
pub mod dwarf;
pub mod error;
pub mod mcp;
pub mod symbols;

pub use error::{Error, Result};
```

**Step 4: Run tests - verify they pass**

Run: `cargo test daemon::tests`
Expected: PASS

**Checkpoint:** Daemon skeleton with session management working.

---

### Task 7: MCP Stdio Proxy

**Files:**
- Create: `src/mcp/proxy.rs`
- Modify: `src/mcp/mod.rs`

**Step 1: Write failing test**

Add to `src/mcp/mod.rs`:
```rust
mod proxy;
pub use proxy::stdio_proxy;

// ... existing code ...

#[cfg(test)]
mod proxy_tests {
    // Proxy is harder to unit test since it connects to daemon
    // Integration tests will cover this
}
```

**Step 2: Implement stdio proxy**

Create `src/mcp/proxy.rs`:
```rust
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use crate::Result;

/// Stdio proxy that connects MCP clients to the daemon.
/// Launches daemon if not running.
pub async fn stdio_proxy() -> Result<()> {
    let strobe_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".strobe");

    let socket_path = strobe_dir.join("strobe.sock");
    let pid_path = strobe_dir.join("strobe.pid");

    // Check if daemon is running, start if not
    if !is_daemon_running(&pid_path, &socket_path).await {
        start_daemon().await?;
        // Wait for socket to be available
        for _ in 0..50 {
            if socket_path.exists() {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    // Connect to daemon
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut daemon_reader = BufReader::new(reader);

    // Read from stdin, write to daemon
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stdin_reader = BufReader::new(stdin);

    let mut stdin_line = String::new();
    let mut daemon_line = String::new();

    loop {
        tokio::select! {
            // Read from stdin -> send to daemon
            result = stdin_reader.read_line(&mut stdin_line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        writer.write_all(stdin_line.as_bytes()).await?;
                        writer.flush().await?;
                        stdin_line.clear();
                    }
                    Err(e) => {
                        eprintln!("stdin error: {}", e);
                        break;
                    }
                }
            }
            // Read from daemon -> send to stdout
            result = daemon_reader.read_line(&mut daemon_line) => {
                match result {
                    Ok(0) => break, // Daemon disconnected
                    Ok(_) => {
                        stdout.write_all(daemon_line.as_bytes()).await?;
                        stdout.flush().await?;
                        daemon_line.clear();
                    }
                    Err(e) => {
                        eprintln!("daemon error: {}", e);
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn is_daemon_running(pid_path: &PathBuf, socket_path: &PathBuf) -> bool {
    if !pid_path.exists() || !socket_path.exists() {
        return false;
    }

    // Read PID and check if process exists
    if let Ok(pid_str) = std::fs::read_to_string(pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Check if process exists (Unix-specific)
            unsafe {
                return libc::kill(pid, 0) == 0;
            }
        }
    }

    false
}

async fn start_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;

    std::process::Command::new(exe)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}
```

Add `libc` to `Cargo.toml`:
```toml
libc = "0.2"
```

**Step 3: Verify compilation**

Run: `cargo check`
Expected: Compiles successfully

**Checkpoint:** MCP stdio proxy can connect to daemon.

---

### Task 8: Frida Collector Module

**Files:**
- Create: `src/frida_collector/mod.rs`
- Create: `src/frida_collector/spawner.rs`
- Create: `src/frida_collector/hooks.rs`
- Modify: `src/lib.rs`

**Step 1: Write failing test**

Create `src/frida_collector/mod.rs`:
```rust
mod spawner;
mod hooks;

pub use spawner::FridaSpawner;
pub use hooks::HookManager;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_manager_pattern_expansion() {
        let manager = HookManager::new();

        // Test that @usercode expands correctly
        let patterns = manager.expand_patterns(
            &["@usercode".to_string()],
            "/home/user/project",
        );

        // Should contain the expanded pattern
        assert!(!patterns.is_empty());
    }
}
```

**Step 2: Run test - verify it fails**

Run: `cargo test frida_collector::tests`
Expected: FAIL - module not found

**Step 3: Implement Frida collector**

Create `src/frida_collector/hooks.rs`:
```rust
use std::collections::HashSet;

pub struct HookManager {
    active_patterns: HashSet<String>,
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            active_patterns: HashSet::new(),
        }
    }

    pub fn expand_patterns(&self, patterns: &[String], project_root: &str) -> Vec<String> {
        patterns
            .iter()
            .map(|p| {
                if p == "@usercode" {
                    // Expand to match all functions in project root
                    format!("{}/**", project_root)
                } else {
                    p.clone()
                }
            })
            .collect()
    }

    pub fn add_patterns(&mut self, patterns: &[String]) {
        for p in patterns {
            self.active_patterns.insert(p.clone());
        }
    }

    pub fn remove_patterns(&mut self, patterns: &[String]) {
        for p in patterns {
            self.active_patterns.remove(p);
        }
    }

    pub fn active_patterns(&self) -> Vec<String> {
        self.active_patterns.iter().cloned().collect()
    }
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}
```

Create `src/frida_collector/spawner.rs`:
```rust
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use crate::db::Event;
use crate::dwarf::DwarfParser;
use crate::Result;
use super::HookManager;

/// Represents a spawned process with Frida attached
pub struct FridaSession {
    pub pid: u32,
    pub binary_path: String,
    pub project_root: String,
    hook_manager: HookManager,
    dwarf: Option<Arc<DwarfParser>>,
    event_sender: mpsc::Sender<Event>,
}

pub struct FridaSpawner {
    sessions: HashMap<String, FridaSession>,
}

impl FridaSpawner {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Spawn a process with Frida attached
    pub async fn spawn(
        &mut self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&HashMap<String, String>>,
        event_sender: mpsc::Sender<Event>,
    ) -> Result<u32> {
        // Parse DWARF first to ensure we have debug symbols
        let dwarf = DwarfParser::parse(Path::new(command))?;

        // TODO: Actual Frida spawn implementation
        // For now, we'll spawn the process normally and attach
        // This is a placeholder for the actual Frida integration

        let mut cmd = std::process::Command::new(command);
        cmd.args(args);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }

        if let Some(env_vars) = env {
            for (k, v) in env_vars {
                cmd.env(k, v);
            }
        }

        // In real implementation, we'd use frida-rs here:
        // let device = frida::DeviceManager::obtain(&frida::Frida::obtain())
        //     .enumerate_devices()?
        //     .into_iter()
        //     .find(|d| d.get_type() == frida::DeviceType::Local)
        //     .ok_or(Error::FridaAttachFailed("No local device".to_string()))?;
        //
        // let pid = device.spawn(command, &frida::SpawnOptions::new())?;
        // let session = device.attach(pid)?;

        // Placeholder: spawn without Frida for now
        let child = cmd.spawn()?;
        let pid = child.id();

        let session = FridaSession {
            pid,
            binary_path: command.to_string(),
            project_root: project_root.to_string(),
            hook_manager: HookManager::new(),
            dwarf: Some(Arc::new(dwarf)),
            event_sender,
        };

        self.sessions.insert(session_id.to_string(), session);

        Ok(pid)
    }

    /// Add trace patterns to a session
    pub async fn add_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<u32> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        let expanded = session.hook_manager.expand_patterns(patterns, &session.project_root);
        session.hook_manager.add_patterns(&expanded);

        // Count matching functions
        let mut count = 0u32;
        if let Some(ref dwarf) = session.dwarf {
            for pattern in &expanded {
                count += dwarf.find_by_pattern(pattern).len() as u32;
            }
        }

        // TODO: Actually install Frida hooks
        // session.frida_session.create_script(agent_code)?;

        Ok(count)
    }

    /// Remove trace patterns from a session
    pub async fn remove_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<()> {
        let session = self.sessions.get_mut(session_id)
            .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

        session.hook_manager.remove_patterns(patterns);

        // TODO: Actually remove Frida hooks

        Ok(())
    }

    /// Stop a session and detach Frida
    pub async fn stop(&mut self, session_id: &str) -> Result<()> {
        if let Some(_session) = self.sessions.remove(session_id) {
            // TODO: Detach Frida cleanly
            // session.frida_session.detach()?;
        }

        Ok(())
    }

    pub fn get_patterns(&self, session_id: &str) -> Vec<String> {
        self.sessions
            .get(session_id)
            .map(|s| s.hook_manager.active_patterns())
            .unwrap_or_default()
    }
}

impl Default for FridaSpawner {
    fn default() -> Self {
        Self::new()
    }
}
```

Update `src/lib.rs`:
```rust
pub mod daemon;
pub mod db;
pub mod dwarf;
pub mod error;
pub mod frida_collector;
pub mod mcp;
pub mod symbols;

pub use error::{Error, Result};
```

**Step 4: Run tests - verify they pass**

Run: `cargo test frida_collector::tests`
Expected: PASS

**Checkpoint:** Frida collector module structure in place (actual Frida integration is stubbed).

---

### Task 9: Frida Agent (TypeScript)

**Files:**
- Create: `agent/src/agent.ts`
- Create: `agent/src/serializer.ts`
- Create: `agent/src/hooks.ts`

**Step 1: Create agent entry point**

Create `agent/src/agent.ts`:
```typescript
import { Serializer } from './serializer';
import { HookInstaller } from './hooks';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
}

interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

interface TraceEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  parentEventId: string | null;
  eventType: 'function_enter' | 'function_exit';
  functionName: string;
  functionNameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
  arguments?: any[];
  returnValue?: any;
  durationNs?: number;
}

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private serializer: Serializer;
  private hookInstaller: HookInstaller;
  private eventBuffer: TraceEvent[] = [];
  private eventIdCounter: number = 0;
  private flushInterval: number = 10; // ms
  private maxBufferSize: number = 1000;

  // Track call stack per thread for parent tracking
  private callStacks: Map<number, string[]> = new Map();

  constructor() {
    this.serializer = new Serializer();
    this.hookInstaller = new HookInstaller(this.onEnter.bind(this), this.onLeave.bind(this));
    this.sessionStartNs = Date.now() * 1000000;

    // Periodic flush
    setInterval(() => this.flush(), this.flushInterval);
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.sessionStartNs = Date.now() * 1000000;
    send({ type: 'initialized', sessionId });
  }

  handleMessage(message: HookInstruction): void {
    if (message.action === 'add') {
      for (const func of message.functions) {
        this.hookInstaller.installHook(func);
      }
    } else if (message.action === 'remove') {
      for (const func of message.functions) {
        this.hookInstaller.removeHook(func.address);
      }
    }

    send({
      type: 'hooks_updated',
      activeCount: this.hookInstaller.activeHookCount()
    });
  }

  private onEnter(
    threadId: number,
    func: FunctionTarget,
    args: NativePointer[]
  ): string {
    const eventId = this.generateEventId();
    const stack = this.callStacks.get(threadId) || [];
    const parentId = stack.length > 0 ? stack[stack.length - 1] : null;

    // Push this event onto call stack
    stack.push(eventId);
    this.callStacks.set(threadId, stack);

    const event: TraceEvent = {
      id: eventId,
      sessionId: this.sessionId,
      timestampNs: this.getTimestampNs(),
      threadId,
      parentEventId: parentId,
      eventType: 'function_enter',
      functionName: func.name,
      functionNameRaw: func.nameRaw,
      sourceFile: func.sourceFile,
      lineNumber: func.lineNumber,
      arguments: args.map(arg => this.serializer.serialize(arg)),
    };

    this.bufferEvent(event);
    return eventId;
  }

  private onLeave(
    threadId: number,
    func: FunctionTarget,
    retval: NativePointer,
    enterEventId: string,
    enterTimestampNs: number
  ): void {
    const now = this.getTimestampNs();

    // Pop from call stack
    const stack = this.callStacks.get(threadId) || [];
    stack.pop();
    this.callStacks.set(threadId, stack);

    const event: TraceEvent = {
      id: this.generateEventId(),
      sessionId: this.sessionId,
      timestampNs: now,
      threadId,
      parentEventId: enterEventId,
      eventType: 'function_exit',
      functionName: func.name,
      functionNameRaw: func.nameRaw,
      sourceFile: func.sourceFile,
      lineNumber: func.lineNumber,
      returnValue: this.serializer.serialize(retval),
      durationNs: now - enterTimestampNs,
    };

    this.bufferEvent(event);
  }

  private bufferEvent(event: TraceEvent): void {
    this.eventBuffer.push(event);

    if (this.eventBuffer.length >= this.maxBufferSize) {
      this.flush();
    }
  }

  private flush(): void {
    if (this.eventBuffer.length === 0) return;

    const events = this.eventBuffer;
    this.eventBuffer = [];

    send({ type: 'events', events });
  }

  private generateEventId(): string {
    return `${this.sessionId}-${++this.eventIdCounter}`;
  }

  private getTimestampNs(): number {
    return Date.now() * 1000000 - this.sessionStartNs;
  }
}

// Global agent instance
const agent = new StrobeAgent();

// Message handler
recv('initialize', (message: { sessionId: string }) => {
  agent.initialize(message.sessionId);
});

recv('hooks', (message: HookInstruction) => {
  agent.handleMessage(message);
});

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
```

Create `agent/src/serializer.ts`:
```typescript
const MAX_STRING_LENGTH = 1024;
const MAX_ARRAY_LENGTH = 100;
const MAX_DEPTH = 1;

export class Serializer {
  serialize(value: NativePointer | any, depth: number = 0): any {
    if (value === null || value === undefined) {
      return null;
    }

    // Handle NativePointer
    if (value instanceof NativePointer) {
      if (value.isNull()) {
        return null;
      }
      return `0x${value.toString(16)}`;
    }

    // Primitives
    if (typeof value === 'number' || typeof value === 'boolean') {
      return value;
    }

    if (typeof value === 'string') {
      return this.truncateString(value);
    }

    // Stop at max depth
    if (depth >= MAX_DEPTH) {
      return this.formatTypeRef(value);
    }

    // Arrays
    if (Array.isArray(value)) {
      return value
        .slice(0, MAX_ARRAY_LENGTH)
        .map(item => this.serialize(item, depth + 1));
    }

    // Objects (structs)
    if (typeof value === 'object') {
      const result: Record<string, any> = {};
      let count = 0;

      for (const key of Object.keys(value)) {
        if (count >= MAX_ARRAY_LENGTH) break;
        result[key] = this.serialize(value[key], depth + 1);
        count++;
      }

      return result;
    }

    return String(value);
  }

  private truncateString(s: string): string {
    if (s.length <= MAX_STRING_LENGTH) {
      return s;
    }
    return s.slice(0, MAX_STRING_LENGTH) + '...';
  }

  private formatTypeRef(value: any): string {
    const typeName = value?.constructor?.name || typeof value;
    if (value instanceof NativePointer) {
      return `<${typeName} at ${value}>`;
    }
    return `<${typeName}>`;
  }
}
```

Create `agent/src/hooks.ts`:
```typescript
interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

type EnterCallback = (
  threadId: number,
  func: FunctionTarget,
  args: NativePointer[]
) => string;

type LeaveCallback = (
  threadId: number,
  func: FunctionTarget,
  retval: NativePointer,
  enterEventId: string,
  enterTimestampNs: number
) => void;

export class HookInstaller {
  private hooks: Map<string, InvocationListener> = new Map();
  private onEnter: EnterCallback;
  private onLeave: LeaveCallback;

  constructor(onEnter: EnterCallback, onLeave: LeaveCallback) {
    this.onEnter = onEnter;
    this.onLeave = onLeave;
  }

  installHook(func: FunctionTarget): void {
    if (this.hooks.has(func.address)) {
      return; // Already hooked
    }

    const addr = ptr(func.address);
    const self = this;

    const listener = Interceptor.attach(addr, {
      onEnter(args) {
        const threadId = Process.getCurrentThreadId();
        const argsArray: NativePointer[] = [];

        // Capture first 10 arguments (reasonable limit)
        for (let i = 0; i < 10; i++) {
          try {
            argsArray.push(args[i]);
          } catch {
            break;
          }
        }

        const eventId = self.onEnter(threadId, func, argsArray);

        // Store context for onLeave
        (this as any).eventId = eventId;
        (this as any).enterTimestampNs = Date.now() * 1000000;
      },

      onLeave(retval) {
        const threadId = Process.getCurrentThreadId();
        const eventId = (this as any).eventId;
        const enterTimestampNs = (this as any).enterTimestampNs;

        self.onLeave(threadId, func, retval, eventId, enterTimestampNs);
      }
    });

    this.hooks.set(func.address, listener);
  }

  removeHook(address: string): void {
    const listener = this.hooks.get(address);
    if (listener) {
      listener.detach();
      this.hooks.delete(address);
    }
  }

  activeHookCount(): number {
    return this.hooks.size;
  }

  removeAll(): void {
    for (const listener of this.hooks.values()) {
      listener.detach();
    }
    this.hooks.clear();
  }
}
```

**Step 2: Verify agent compiles (requires npm install first)**

Run: `cd agent && npm install && npm run build`
Expected: Compiles to `agent/dist/agent.js`

**Checkpoint:** Frida agent implemented with event capture and serialization.

---

### Task 10: Integration - Wire Frida to Daemon

**Files:**
- Modify: `src/daemon/server.rs`
- Modify: `src/daemon/session_manager.rs`

**Step 1: Update session manager with Frida integration**

Update `src/daemon/session_manager.rs` to include event channel:
```rust
// Add to imports
use tokio::sync::mpsc;
use crate::db::Event;
use crate::frida_collector::FridaSpawner;

// Add to SessionManager struct
pub struct SessionManager {
    db: Database,
    patterns: Arc<RwLock<HashMap<String, Vec<String>>>>,
    dwarf_cache: Arc<RwLock<HashMap<String, Arc<DwarfParser>>>>,
    hook_counts: Arc<RwLock<HashMap<String, u32>>>,
    frida_spawner: Arc<RwLock<FridaSpawner>>,
}

// Update new() to initialize frida_spawner
impl SessionManager {
    pub fn new(db_path: &Path) -> Result<Self> {
        let db = Database::open(db_path)?;

        Ok(Self {
            db,
            patterns: Arc::new(RwLock::new(HashMap::new())),
            dwarf_cache: Arc::new(RwLock::new(HashMap::new())),
            hook_counts: Arc::new(RwLock::new(HashMap::new())),
            frida_spawner: Arc::new(RwLock::new(FridaSpawner::new())),
        })
    }

    // Add method to spawn process with Frida
    pub async fn spawn_with_frida(
        &self,
        session_id: &str,
        command: &str,
        args: &[String],
        cwd: Option<&str>,
        project_root: &str,
        env: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<u32> {
        // Create event channel
        let (tx, mut rx) = mpsc::channel::<Event>(10000);

        // Spawn database writer task
        let db = self.db.clone();
        let session_id_clone = session_id.to_string();
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(100);

            loop {
                tokio::select! {
                    Some(event) = rx.recv() => {
                        batch.push(event);

                        if batch.len() >= 100 {
                            if let Err(e) = db.insert_events_batch(&batch) {
                                tracing::error!("Failed to insert events: {}", e);
                            }
                            batch.clear();
                        }
                    }
                    _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {
                        if !batch.is_empty() {
                            if let Err(e) = db.insert_events_batch(&batch) {
                                tracing::error!("Failed to insert events: {}", e);
                            }
                            batch.clear();
                        }
                    }
                }
            }
        });

        // Spawn process
        let mut spawner = self.frida_spawner.write().await;
        spawner.spawn(
            session_id,
            command,
            args,
            cwd,
            project_root,
            env,
            tx,
        ).await
    }

    // Add method to update trace patterns via Frida
    pub async fn update_frida_patterns(
        &self,
        session_id: &str,
        add: Option<&[String]>,
        remove: Option<&[String]>,
    ) -> Result<u32> {
        let mut spawner = self.frida_spawner.write().await;

        if let Some(patterns) = add {
            return spawner.add_patterns(session_id, patterns).await;
        }

        if let Some(patterns) = remove {
            spawner.remove_patterns(session_id, patterns).await?;
        }

        Ok(0)
    }

    // Add method to stop Frida session
    pub async fn stop_frida(&self, session_id: &str) -> Result<()> {
        let mut spawner = self.frida_spawner.write().await;
        spawner.stop(session_id).await
    }
}
```

**Step 2: Update daemon server to use Frida integration**

Update the tool handlers in `src/daemon/server.rs`:

```rust
async fn tool_debug_launch(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: DebugLaunchRequest = serde_json::from_value(args.clone())?;

    // Extract binary name from path
    let binary_name = std::path::Path::new(&req.command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let session_id = self.session_manager.generate_session_id(binary_name);

    // Spawn with Frida
    let args = req.args.unwrap_or_default();
    let pid = self.session_manager.spawn_with_frida(
        &session_id,
        &req.command,
        &args,
        req.cwd.as_deref(),
        &req.project_root,
        req.env.as_ref(),
    ).await?;

    self.session_manager.create_session(
        &session_id,
        &req.command,
        &req.project_root,
        pid,
    )?;

    let response = DebugLaunchResponse {
        session_id,
        pid,
    };

    Ok(serde_json::to_value(response)?)
}

async fn tool_debug_trace(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: DebugTraceRequest = serde_json::from_value(args.clone())?;

    // Verify session exists
    let _ = self.session_manager.get_session(&req.session_id)?
        .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

    // Update patterns in session manager
    if let Some(ref add) = req.add {
        self.session_manager.add_patterns(&req.session_id, add)?;
    }
    if let Some(ref remove) = req.remove {
        self.session_manager.remove_patterns(&req.session_id, remove)?;
    }

    // Update Frida hooks
    let hook_count = self.session_manager.update_frida_patterns(
        &req.session_id,
        req.add.as_deref(),
        req.remove.as_deref(),
    ).await.unwrap_or(0);

    self.session_manager.set_hook_count(&req.session_id, hook_count);

    let patterns = self.session_manager.get_patterns(&req.session_id);

    let response = DebugTraceResponse {
        active_patterns: patterns,
        hooked_functions: hook_count,
    };

    Ok(serde_json::to_value(response)?)
}

async fn tool_debug_stop(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: DebugStopRequest = serde_json::from_value(args.clone())?;

    // Verify session exists
    let _ = self.session_manager.get_session(&req.session_id)?
        .ok_or_else(|| crate::Error::SessionNotFound(req.session_id.clone()))?;

    // Stop Frida session
    self.session_manager.stop_frida(&req.session_id).await?;

    let events_collected = self.session_manager.stop_session(&req.session_id)?;

    let response = DebugStopResponse {
        success: true,
        events_collected,
    };

    Ok(serde_json::to_value(response)?)
}
```

**Step 3: Verify compilation**

Run: `cargo check`
Expected: Compiles with warnings about async usage

**Checkpoint:** Daemon integrated with Frida collector.

---

### Task 11: Integration Tests

**Files:**
- Create: `tests/integration.rs`

**Step 1: Write integration tests**

Create `tests/integration.rs`:
```rust
use std::path::PathBuf;
use tempfile::tempdir;

// Test helper to create a simple test binary
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
    use strobe::dwarf::parser::PatternMatcher;

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
```

**Step 2: Run integration tests**

Run: `cargo test --test integration`
Expected: PASS (some tests may be skipped on non-Linux platforms)

**Checkpoint:** Integration tests verify core functionality.

---

### Task 12: Final Verification

**Files:** None (verification only)

**Step 1: Run all tests**

Run: `cargo test`
Expected: All tests pass

**Step 2: Build release binary**

Run: `cargo build --release`
Expected: Binary at `target/release/strobe`

**Step 3: Build Frida agent**

Run: `cd agent && npm run build`
Expected: Agent at `agent/dist/agent.js`

**Step 4: Manual smoke test**

```bash
# Start daemon
./target/release/strobe daemon &

# In another terminal, create a test program
echo 'fn main() { println!("hello"); }' > /tmp/test.rs
rustc -g -o /tmp/test_binary /tmp/test.rs

# Test MCP interface (if you have an MCP client)
# Or verify socket exists:
ls -la ~/.strobe/strobe.sock

# Stop daemon
kill $(cat ~/.strobe/strobe.pid)
```

**Checkpoint:** Phase 1a implementation complete and verified.

---

## Summary

This plan implements Phase 1a of Strobe in 12 tasks:

1. **Project Setup** - Cargo.toml, module structure
2. **Database Module** - SQLite with WAL, sessions, events
3. **Symbol Demangling** - Rust/C++ symbol demangling
4. **DWARF Parsing** - Debug info extraction with gimli
5. **MCP Protocol Types** - Request/response serialization
6. **Daemon Skeleton** - Unix socket server, session management
7. **MCP Stdio Proxy** - Connects MCP clients to daemon
8. **Frida Collector** - Process spawning, hook management (stubbed)
9. **Frida Agent** - TypeScript agent for in-process tracing
10. **Integration** - Wire Frida to daemon
11. **Integration Tests** - End-to-end verification
12. **Final Verification** - Build and smoke test

The Frida integration (Tasks 8, 10) is partially stubbed because full Frida integration requires careful platform-specific work. The stub implementation allows the architecture to be validated while actual Frida calls can be filled in incrementally.
