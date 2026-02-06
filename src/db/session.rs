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

    pub fn get_running_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            "SELECT id, binary_path, project_root, pid, started_at, ended_at, status
             FROM sessions WHERE status = 'running'"
        )?;

        let sessions = stmt.query_map([], |row| {
            Ok(Session {
                id: row.get(0)?,
                binary_path: row.get(1)?,
                project_root: row.get(2)?,
                pid: row.get(3)?,
                started_at: row.get(4)?,
                ended_at: row.get(5)?,
                status: SessionStatus::from_str(&row.get::<_, String>(6)?).unwrap(),
            })
        })?.collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sessions)
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
