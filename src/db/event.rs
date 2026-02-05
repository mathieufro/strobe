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
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FunctionEnter => "function_enter",
            Self::FunctionExit => "function_exit",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function_enter" => Some(Self::FunctionEnter),
            "function_exit" => Some(Self::FunctionExit),
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
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
    pub text: Option<String>,
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
             arguments, return_value, duration_ns, text)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                event.text,
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
                 arguments, return_value, duration_ns, text)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                    &event.text,
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
             arguments, return_value, duration_ns, text
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

            // Handle JSON columns that might be stored as different SQLite types
            let args = match row.get_ref(10)? {
                rusqlite::types::ValueRef::Null => None,
                rusqlite::types::ValueRef::Text(s) => {
                    serde_json::from_str(std::str::from_utf8(s).unwrap_or("null")).ok()
                }
                rusqlite::types::ValueRef::Integer(i) => Some(serde_json::json!(i)),
                rusqlite::types::ValueRef::Real(f) => Some(serde_json::json!(f)),
                _ => None,
            };

            let ret = match row.get_ref(11)? {
                rusqlite::types::ValueRef::Null => None,
                rusqlite::types::ValueRef::Text(s) => {
                    serde_json::from_str(std::str::from_utf8(s).unwrap_or("null")).ok()
                }
                rusqlite::types::ValueRef::Integer(i) => Some(serde_json::json!(i)),
                rusqlite::types::ValueRef::Real(f) => Some(serde_json::json!(f)),
                _ => None,
            };

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
                arguments: args,
                return_value: ret,
                duration_ns: row.get(12)?,
                text: row.get(13)?,
            })
        })?;

        events.collect::<std::result::Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn count_events(&self, session_id: &str) -> Result<u64> {
        self.count_session_events(session_id)
    }
}
