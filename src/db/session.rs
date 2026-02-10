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
    pub retained: bool,
    pub retained_at: Option<i64>,
    pub size_bytes: Option<i64>,
}

impl Session {
    /// Parse a Session from a row with the standard 9-column SELECT order.
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let retained_at: Option<i64> = row.get(7).ok().flatten();
        Ok(Self {
            id: row.get(0)?,
            binary_path: row.get(1)?,
            project_root: row.get(2)?,
            pid: row.get(3)?,
            started_at: row.get(4)?,
            ended_at: row.get(5)?,
            status: SessionStatus::from_str(&row.get::<_, String>(6)?).unwrap_or(SessionStatus::Stopped),
            retained: retained_at.is_some(),
            retained_at,
            size_bytes: row.get(8).ok().flatten(),
        })
    }
}

/// Convert QueryReturnedNoRows into Ok(None).
fn optional_query<T>(result: rusqlite::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

const SESSION_SELECT: &str =
    "SELECT id, binary_path, project_root, pid, started_at, ended_at, status, retained_at, size_bytes";

impl Database {
    /// Mark all sessions with status='running' as 'stopped'.
    /// Called on daemon startup to clean up stale sessions from previous runs.
    pub fn cleanup_stale_sessions(&self) -> Result<()> {
        let conn = self.connection();
        let ended_at = chrono::Utc::now().timestamp();
        let count = conn.execute(
            "UPDATE sessions SET status = 'stopped', ended_at = ? WHERE status = 'running'",
            params![ended_at],
        )?;
        if count > 0 {
            tracing::info!("Cleaned up {} stale running sessions from previous daemon", count);
        }
        Ok(())
    }

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
            retained: false,
            retained_at: None,
            size_bytes: None,
        })
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            &format!("{} FROM sessions WHERE id = ?", SESSION_SELECT)
        )?;
        optional_query(stmt.query_row(params![id], Session::from_row))
    }

    pub fn get_running_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            &format!("{} FROM sessions WHERE status = 'running'", SESSION_SELECT)
        )?;

        let sessions = stmt.query_map([], Session::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn get_session_by_binary(&self, binary_path: &str) -> Result<Option<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            &format!("{} FROM sessions WHERE binary_path = ? AND status = 'running'", SESSION_SELECT)
        )?;
        optional_query(stmt.query_row(params![binary_path], Session::from_row))
    }

    pub fn update_session_status(&self, id: &str, new_status: SessionStatus) -> Result<()> {
        let conn = self.connection();

        // Validate transition: Stopped is a terminal state
        let current: Option<String> = conn.query_row(
            "SELECT status FROM sessions WHERE id = ?",
            params![id],
            |row| row.get(0),
        ).ok();

        if let Some(ref current_str) = current {
            if current_str == "stopped" && new_status != SessionStatus::Stopped {
                tracing::warn!(
                    "Ignoring invalid transition from stopped to {} for session {}",
                    new_status.as_str(), id
                );
                return Ok(());
            }
        }

        let ended_at = if new_status != SessionStatus::Running {
            Some(chrono::Utc::now().timestamp())
        } else {
            None
        };

        conn.execute(
            "UPDATE sessions SET status = ?, ended_at = ? WHERE id = ?",
            params![new_status.as_str(), ended_at, id],
        )?;

        Ok(())
    }

    pub fn update_session_pid(&self, id: &str, pid: u32) -> Result<()> {
        let conn = self.connection();
        conn.execute(
            "UPDATE sessions SET pid = ? WHERE id = ?",
            params![pid, id],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let conn = self.connection();
        conn.execute("DELETE FROM events WHERE session_id = ?", params![id])?;
        conn.execute("DELETE FROM sessions WHERE id = ?", params![id])?;
        Ok(())
    }

    pub fn mark_session_stopped(&self, id: &str) -> Result<()> {
        self.update_session_status(id, SessionStatus::Stopped)
    }

    pub fn mark_session_retained(&self, id: &str) -> Result<()> {
        let conn = self.connection();
        let retained_at = chrono::Utc::now().timestamp();

        // Calculate session size (includes all text/JSON columns for accuracy)
        let size: i64 = conn.query_row(
            "SELECT COALESCE(SUM(
                LENGTH(id) + LENGTH(session_id) + LENGTH(COALESCE(function_name,''))
                + LENGTH(COALESCE(function_name_raw,'')) + LENGTH(COALESCE(source_file,''))
                + LENGTH(COALESCE(arguments,'')) + LENGTH(COALESCE(return_value,''))
                + LENGTH(COALESCE(text,'')) + LENGTH(COALESCE(thread_name,''))
                + LENGTH(COALESCE(watch_values,'')) + LENGTH(COALESCE(signal,''))
                + LENGTH(COALESCE(fault_address,'')) + LENGTH(COALESCE(registers,''))
                + LENGTH(COALESCE(backtrace,'')) + LENGTH(COALESCE(locals,''))
                + 100
            ), 0) FROM events WHERE session_id = ?",
            params![id],
            |row| row.get(0),
        )?;

        conn.execute(
            "UPDATE sessions SET retained_at = ?, size_bytes = ? WHERE id = ?",
            params![retained_at, size, id],
        )?;

        Ok(())
    }

    pub fn list_retained_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            &format!("{} FROM sessions WHERE retained_at IS NOT NULL ORDER BY retained_at DESC", SESSION_SELECT)
        )?;

        let sessions = stmt.query_map([], Session::from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    /// Enforce 10GB global size limit by deleting oldest retained sessions
    pub fn enforce_global_size_limit(&self) -> Result<u64> {
        const MAX_TOTAL_BYTES: i64 = 10 * 1024 * 1024 * 1024; // 10GB

        let total = self.calculate_total_size()?;
        if total <= MAX_TOTAL_BYTES {
            return Ok(0);
        }

        let conn = self.connection();
        let mut deleted = 0u64;

        let mut stmt = conn.prepare(
            "SELECT id, COALESCE(size_bytes, 0) FROM sessions WHERE retained_at IS NOT NULL ORDER BY retained_at ASC"
        )?;

        let sessions: Vec<(String, i64)> = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?.collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);

        let mut remaining = total;
        for (session_id, size) in sessions {
            if remaining <= MAX_TOTAL_BYTES {
                break;
            }
            self.delete_session(&session_id)?;
            remaining -= size;
            deleted += 1;
        }

        Ok(deleted)
    }

    pub fn calculate_total_size(&self) -> Result<i64> {
        let conn = self.connection();
        let size: i64 = conn.query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM sessions WHERE retained_at IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        Ok(size)
    }
}
