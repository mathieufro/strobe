use rusqlite::params;
use serde::{Deserialize, Serialize};
use crate::Result;
use super::Database;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FunctionEnter => "function_enter",
            Self::FunctionExit => "function_exit",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Crash => "crash",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function_enter" => Some(Self::FunctionEnter),
            "function_exit" => Some(Self::FunctionExit),
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            "crash" => Some(Self::Crash),
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
    pub thread_name: Option<String>,
    pub parent_event_id: Option<String>,
    pub event_type: EventType,
    pub function_name: String,
    pub function_name_raw: Option<String>,
    pub source_file: Option<String>,
    pub line_number: Option<i32>,
    pub arguments: Option<serde_json::Value>,
    pub return_value: Option<serde_json::Value>,
    pub duration_ns: Option<i64>,
    pub text: Option<String>,
    pub sampled: Option<bool>,
    pub watch_values: Option<serde_json::Value>,
    pub pid: Option<u32>,
    pub signal: Option<String>,
    pub fault_address: Option<String>,
    pub registers: Option<serde_json::Value>,
    pub backtrace: Option<serde_json::Value>,
    pub locals: Option<serde_json::Value>,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            id: String::new(),
            session_id: String::new(),
            timestamp_ns: 0,
            thread_id: 0,
            thread_name: None,
            parent_event_id: None,
            event_type: EventType::FunctionEnter,
            function_name: String::new(),
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
        }
    }
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
    #[serde(rename = "watchValues", skip_serializing_if = "Option::is_none")]
    pub watch_values: Option<serde_json::Value>,
}

pub struct EventQuery {
    pub event_type: Option<EventType>,
    pub function_equals: Option<String>,
    pub function_contains: Option<String>,
    pub source_file_contains: Option<String>,
    pub return_value_is_null: Option<bool>,
    pub thread_id_equals: Option<i64>,
    pub thread_name_contains: Option<String>,
    pub pid_equals: Option<u32>,
    pub timestamp_from_ns: Option<i64>,
    pub timestamp_to_ns: Option<i64>,
    pub min_duration_ns: Option<i64>,
    pub limit: u32,
    pub offset: u32,
}

impl Default for EventQuery {
    fn default() -> Self {
        Self {
            event_type: None,
            function_equals: None,
            function_contains: None,
            source_file_contains: None,
            return_value_is_null: None,
            thread_id_equals: None,
            thread_name_contains: None,
            pid_equals: None,
            timestamp_from_ns: None,
            timestamp_to_ns: None,
            min_duration_ns: None,
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

    /// Internal limit without the 500-event MCP cap. Used for test output collection.
    pub(crate) fn limit_uncapped(mut self, n: u32) -> Self {
        self.limit = n;
        self
    }

    pub fn offset(mut self, n: u32) -> Self {
        self.offset = n;
        self
    }

    pub fn thread_name_contains(mut self, s: &str) -> Self {
        self.thread_name_contains = Some(s.to_string());
        self
    }
}

fn escape_like_pattern(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\0')
        .collect::<String>()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

const INSERT_EVENT_SQL: &str =
    "INSERT INTO events (id, session_id, timestamp_ns, thread_id, thread_name, parent_event_id,
     event_type, function_name, function_name_raw, source_file, line_number,
     arguments, return_value, duration_ns, text, sampled, watch_values, pid,
     signal, fault_address, registers, backtrace, locals)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

/// Insert a single event row using a connection or transaction.
fn insert_event_row(conn: &rusqlite::Connection, event: &Event) -> std::result::Result<(), rusqlite::Error> {
    conn.execute(
        INSERT_EVENT_SQL,
        params![
            &event.id,
            &event.session_id,
            event.timestamp_ns,
            event.thread_id,
            &event.thread_name,
            &event.parent_event_id,
            event.event_type.as_str(),
            &event.function_name,
            &event.function_name_raw,
            &event.source_file,
            event.line_number,
            event.arguments.as_ref().map(|v| v.to_string()),
            event.return_value.as_ref().map(|v| v.to_string()),
            event.duration_ns,
            &event.text,
            event.sampled,
            event.watch_values.as_ref().map(|v| v.to_string()),
            event.pid.map(|p| p as i64),
            &event.signal,
            &event.fault_address,
            event.registers.as_ref().map(|v| v.to_string()),
            event.backtrace.as_ref().map(|v| v.to_string()),
            event.locals.as_ref().map(|v| v.to_string()),
        ],
    )?;
    Ok(())
}

/// Read a JSON column that may be stored as Text, Integer, or Real.
fn read_json_flexible(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<serde_json::Value>> {
    match row.get_ref(idx)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Text(s) => {
            Ok(serde_json::from_str(std::str::from_utf8(s).unwrap_or("null")).ok())
        }
        rusqlite::types::ValueRef::Integer(i) => Ok(Some(serde_json::json!(i))),
        rusqlite::types::ValueRef::Real(f) => Ok(Some(serde_json::json!(f))),
        _ => Ok(None),
    }
}

/// Read a JSON column stored only as Text.
fn read_json_text(row: &rusqlite::Row, idx: usize) -> rusqlite::Result<Option<serde_json::Value>> {
    match row.get_ref(idx)? {
        rusqlite::types::ValueRef::Null => Ok(None),
        rusqlite::types::ValueRef::Text(s) => {
            Ok(serde_json::from_str(std::str::from_utf8(s).unwrap_or("null")).ok())
        }
        _ => Ok(None),
    }
}

/// Parse an Event from a row with the standard 23-column SELECT order.
fn event_from_row(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let event_type_str: String = row.get(6)?;
    Ok(Event {
        id: row.get(0)?,
        session_id: row.get(1)?,
        timestamp_ns: row.get(2)?,
        thread_id: row.get(3)?,
        thread_name: row.get(4)?,
        parent_event_id: row.get(5)?,
        event_type: EventType::from_str(&event_type_str).unwrap_or(EventType::FunctionEnter),
        function_name: row.get(7)?,
        function_name_raw: row.get(8)?,
        source_file: row.get(9)?,
        line_number: row.get(10)?,
        arguments: read_json_flexible(row, 11)?,
        return_value: read_json_flexible(row, 12)?,
        duration_ns: row.get(13)?,
        text: row.get(14)?,
        sampled: row.get(15)?,
        watch_values: read_json_text(row, 16)?,
        pid: row.get::<_, Option<i64>>(17)?.map(|p| p as u32),
        signal: row.get(18)?,
        fault_address: row.get(19)?,
        registers: read_json_text(row, 20)?,
        backtrace: read_json_text(row, 21)?,
        locals: read_json_text(row, 22)?,
    })
}

impl Database {
    pub fn insert_event(&self, event: &Event) -> Result<()> {
        let conn = self.connection();
        insert_event_row(&conn, event)?;
        Ok(())
    }

    pub fn insert_events_batch(&self, events: &[Event]) -> Result<()> {
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        for event in events {
            insert_event_row(&tx, event)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query_events<F>(&self, session_id: &str, build_query: F) -> Result<Vec<Event>>
    where
        F: FnOnce(EventQuery) -> EventQuery,
    {
        let query = build_query(EventQuery::default());
        let conn = self.connection();

        let mut sql = String::from(
            "SELECT id, session_id, timestamp_ns, thread_id, thread_name, parent_event_id,
             event_type, function_name, function_name_raw, source_file, line_number,
             arguments, return_value, duration_ns, text, sampled, watch_values, pid,
             signal, fault_address, registers, backtrace, locals
             FROM events WHERE session_id = ?"
        );

        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(session_id.to_string())];

        if let Some(ref et) = query.event_type {
            sql.push_str(" AND event_type = ?");
            params_vec.push(Box::new(et.as_str().to_string()));
        }

        if let Some(ref f) = query.function_equals {
            sql.push_str(" AND event_type IN ('function_enter', 'function_exit') AND function_name = ?");
            params_vec.push(Box::new(f.clone()));
        }

        if let Some(ref f) = query.function_contains {
            sql.push_str(" AND event_type IN ('function_enter', 'function_exit') AND function_name LIKE ? ESCAPE '\\'");
            params_vec.push(Box::new(format!("%{}%", escape_like_pattern(f))));
        }

        if let Some(ref f) = query.source_file_contains {
            sql.push_str(" AND source_file LIKE ? ESCAPE '\\'");
            params_vec.push(Box::new(format!("%{}%", escape_like_pattern(f))));
        }

        if let Some(is_null) = query.return_value_is_null {
            if is_null {
                sql.push_str(" AND return_value IS NULL");
            } else {
                sql.push_str(" AND return_value IS NOT NULL");
            }
        }

        if let Some(tid) = query.thread_id_equals {
            sql.push_str(" AND thread_id = ?");
            params_vec.push(Box::new(tid));
        }

        if let Some(ref name) = query.thread_name_contains {
            sql.push_str(" AND thread_name LIKE ? ESCAPE '\\'");
            params_vec.push(Box::new(format!("%{}%", escape_like_pattern(name))));
        }

        if let Some(pid) = query.pid_equals {
            sql.push_str(" AND pid = ?");
            params_vec.push(Box::new(pid as i64));
        }

        if let Some(from) = query.timestamp_from_ns {
            sql.push_str(" AND timestamp_ns >= ?");
            params_vec.push(Box::new(from));
        }
        if let Some(to) = query.timestamp_to_ns {
            sql.push_str(" AND timestamp_ns <= ?");
            params_vec.push(Box::new(to));
        }
        if let Some(min_dur) = query.min_duration_ns {
            sql.push_str(" AND duration_ns IS NOT NULL AND duration_ns >= ?");
            params_vec.push(Box::new(min_dur));
        }

        sql.push_str(" ORDER BY timestamp_ns ASC");
        sql.push_str(" LIMIT ? OFFSET ?");
        params_vec.push(Box::new(query.limit as i64));
        params_vec.push(Box::new(query.offset as i64));

        let params_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let events = stmt.query_map(params_refs.as_slice(), event_from_row)?;

        events.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_latest_timestamp(&self, session_id: &str) -> Result<i64> {
        let conn = self.connection();
        let ts: i64 = conn.query_row(
            "SELECT COALESCE(MAX(timestamp_ns), 0) FROM events WHERE session_id = ?",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(ts)
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

    /// Delete oldest events for a session, keeping only the most recent N.
    /// Returns the number of events deleted.
    pub fn cleanup_old_events(&self, session_id: &str, keep_count: usize) -> Result<u64> {
        let conn = self.connection();

        let deleted = conn.execute(
            "DELETE FROM events
             WHERE session_id = ?
             AND id NOT IN (
                 SELECT id FROM events
                 WHERE session_id = ?
                 ORDER BY timestamp_ns DESC
                 LIMIT ?
             )",
            params![session_id, session_id, keep_count as i64],
        )?;

        Ok(deleted as u64)
    }

    /// Insert events with automatic cleanup to enforce per-session limits.
    /// If inserting would exceed max_events_per_session, oldest events are deleted first.
    pub fn insert_events_with_limit(
        &self,
        events: &[Event],
        max_events_per_session: usize,
    ) -> Result<EventInsertStats> {
        if events.is_empty() {
            return Ok(EventInsertStats::default());
        }

        let mut conn = self.connection();
        let tx = conn.transaction()?;

        let mut stats = EventInsertStats::default();
        let mut session_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        // Count current events per session
        for event in events {
            if !session_counts.contains_key(&event.session_id) {
                let count: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM events WHERE session_id = ?",
                    params![&event.session_id],
                    |row| row.get(0),
                )?;
                session_counts.insert(event.session_id.clone(), count as usize);
            }
        }

        // Group events by session for efficient cleanup
        let mut events_by_session: std::collections::HashMap<String, Vec<&Event>> =
            std::collections::HashMap::new();
        for event in events {
            events_by_session
                .entry(event.session_id.clone())
                .or_default()
                .push(event);
        }

        // For each session, cleanup if needed, then insert
        for (session_id, session_events) in events_by_session {
            let current_count = session_counts.get(&session_id).copied().unwrap_or(0);
            let new_count = current_count + session_events.len();

            if new_count > max_events_per_session {
                let to_delete = new_count - max_events_per_session;
                let deleted = tx.execute(
                    "DELETE FROM events
                     WHERE session_id = ?
                     AND id IN (
                         SELECT id FROM events
                         WHERE session_id = ?
                         ORDER BY timestamp_ns ASC
                         LIMIT ?
                     )",
                    params![&session_id, &session_id, to_delete as i64],
                )?;

                stats.events_deleted += deleted as u64;
                if deleted > 0 {
                    stats.sessions_cleaned.push(session_id.clone());
                }
            }

            for event in session_events {
                insert_event_row(&tx, event)?;
                stats.events_inserted += 1;
            }
        }

        tx.commit()?;
        Ok(stats)
    }

    pub fn update_event_locals(&self, event_id: &str, locals: &serde_json::Value) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE events SET locals = ? WHERE id = ?",
            params![locals.to_string(), event_id],
        )?;
        Ok(())
    }
}

/// Statistics returned from insert_events_with_limit
#[derive(Debug, Default)]
pub struct EventInsertStats {
    pub events_inserted: u64,
    pub events_deleted: u64,
    pub sessions_cleaned: Vec<String>,
}
