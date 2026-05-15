//! Adaptive stuck detection driven by historical green-time medians.
//!
//! `TestStats` records the last N green durations for each (project_root,
//! test_id) and exposes a rolling median. `AdaptiveDetector` queries the
//! median and returns a stall threshold of `max(5×median, 30s)`, with a
//! cold-start fallback to `DEFAULT_TIMEOUT_MS`.

use crate::db::Database;
use crate::Result;

pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const FLOOR_MS: u64 = 30_000;
pub const WINDOW_SIZE: usize = 10;

pub struct TestStats {
    db: Database,
}

impl TestStats {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    /// Record a green (passing) duration. Maintains a rolling window of the
    /// last `WINDOW_SIZE` durations and recomputes the median.
    pub fn record_green(&self, project_root: &str, test_id: &str, ms: u64) -> Result<()> {
        let mut durations = self.durations(project_root, test_id)?;
        durations.push(ms);
        while durations.len() > WINDOW_SIZE {
            durations.remove(0);
        }
        let median = median_of(&durations);
        let json = serde_json::to_string(&durations).unwrap_or_else(|_| "[]".into());
        let now = now_ms() as i64;
        let conn = self.db.connection();
        conn.execute(
            "INSERT INTO test_stats (project_root, test_id, durations_ms, median_ms, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(project_root, test_id) DO UPDATE SET
                durations_ms = excluded.durations_ms,
                median_ms = excluded.median_ms,
                updated_at = excluded.updated_at",
            rusqlite::params![project_root, test_id, json, median as i64, now],
        )?;
        Ok(())
    }

    pub fn median(&self, project_root: &str, test_id: &str) -> Option<u64> {
        let conn = self.db.connection();
        let row: rusqlite::Result<i64> = conn.query_row(
            "SELECT median_ms FROM test_stats WHERE project_root = ? AND test_id = ?",
            rusqlite::params![project_root, test_id],
            |r| r.get(0),
        );
        row.ok().map(|v| v as u64)
    }

    pub fn window_size(&self, project_root: &str, test_id: &str) -> Option<usize> {
        Some(self.durations(project_root, test_id).ok()?.len())
    }

    fn durations(&self, project_root: &str, test_id: &str) -> Result<Vec<u64>> {
        let conn = self.db.connection();
        let json: rusqlite::Result<String> = conn.query_row(
            "SELECT durations_ms FROM test_stats WHERE project_root = ? AND test_id = ?",
            rusqlite::params![project_root, test_id],
            |r| r.get(0),
        );
        let json = match json {
            Ok(s) => s,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(serde_json::from_str(&json).unwrap_or_default())
    }
}

pub struct AdaptiveDetector {
    stats: TestStats,
}

impl AdaptiveDetector {
    pub fn new(stats: TestStats) -> Self {
        Self { stats }
    }

    /// Compute the stall threshold for a given test. Unknown test → `DEFAULT_TIMEOUT_MS`.
    /// Known test → `max(5 × median, FLOOR_MS)`.
    pub fn threshold_ms(&self, project_root: &str, test_id: &str) -> u64 {
        match self.stats.median(project_root, test_id) {
            Some(m) => (m.saturating_mul(5)).max(FLOOR_MS),
            None => DEFAULT_TIMEOUT_MS,
        }
    }

    pub fn is_stalled(&self, project_root: &str, test_id: &str, elapsed_ms: u64) -> bool {
        elapsed_ms > self.threshold_ms(project_root, test_id)
    }
}

fn median_of(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u64> = values.to_vec();
    sorted.sort();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        // Even-length: average the two middle values.
        (sorted[mid - 1] + sorted[mid]) / 2
    }
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
