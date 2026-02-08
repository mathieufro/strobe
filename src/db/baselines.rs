use rusqlite::params;
use std::collections::HashMap;

impl super::Database {
    pub fn record_test_baseline(
        &self,
        test_name: &str,
        project_root: &str,
        duration_ms: u64,
        status: &str,
    ) -> crate::Result<()> {
        let conn = self.connection();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        conn.execute(
            "INSERT INTO test_baselines (test_name, project_root, duration_ms, status, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![test_name, project_root, duration_ms as i64, status, now],
        )?;
        Ok(())
    }

    pub fn get_test_baseline(
        &self,
        test_name: &str,
        project_root: &str,
    ) -> crate::Result<Option<u64>> {
        let conn = self.connection();
        let result = conn.query_row(
            "SELECT AVG(duration_ms) FROM (
                SELECT duration_ms FROM test_baselines
                WHERE test_name = ?1 AND project_root = ?2 AND status = 'passed'
                ORDER BY recorded_at DESC
                LIMIT 10
            )",
            params![test_name, project_root],
            |row| row.get::<_, Option<f64>>(0),
        )?;
        Ok(result.map(|avg| avg.round() as u64))
    }

    pub fn get_project_baselines(
        &self,
        project_root: &str,
    ) -> crate::Result<HashMap<String, u64>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            "SELECT test_name, AVG(duration_ms) FROM (
                SELECT test_name, duration_ms,
                    ROW_NUMBER() OVER (PARTITION BY test_name ORDER BY recorded_at DESC) as rn
                FROM test_baselines
                WHERE project_root = ?1 AND status = 'passed'
            ) WHERE rn <= 10
            GROUP BY test_name"
        )?;
        let mut map = HashMap::new();
        let rows = stmt.query_map(params![project_root], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;
        for row in rows {
            let (name, avg) = row?;
            map.insert(name, avg.round() as u64);
        }
        Ok(map)
    }

    pub fn cleanup_old_baselines(&self, project_root: &str) -> crate::Result<()> {
        let conn = self.connection();
        conn.execute(
            "DELETE FROM test_baselines WHERE project_root = ?1 AND id NOT IN (
                SELECT id FROM (
                    SELECT id, ROW_NUMBER() OVER (
                        PARTITION BY test_name ORDER BY recorded_at DESC
                    ) as rn
                    FROM test_baselines WHERE project_root = ?1
                ) WHERE rn <= 20
            )",
            params![project_root],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::Database;

    #[test]
    fn test_record_and_query_baseline() {
        let db = Database::open_in_memory().unwrap();

        // No baseline yet
        let baseline = db.get_test_baseline("test_auth", "/project").unwrap();
        assert!(baseline.is_none());

        // Record some runs
        db.record_test_baseline("test_auth", "/project", 1000, "passed").unwrap();
        db.record_test_baseline("test_auth", "/project", 1200, "passed").unwrap();
        db.record_test_baseline("test_auth", "/project", 1100, "passed").unwrap();

        // Average of last 10 passed runs
        let baseline = db.get_test_baseline("test_auth", "/project").unwrap();
        assert_eq!(baseline, Some(1100)); // avg(1000, 1200, 1100) = 1100

        // Failed runs should not affect baseline
        db.record_test_baseline("test_auth", "/project", 9999, "failed").unwrap();
        let baseline = db.get_test_baseline("test_auth", "/project").unwrap();
        assert_eq!(baseline, Some(1100)); // unchanged
    }

    #[test]
    fn test_project_baselines_batch() {
        let db = Database::open_in_memory().unwrap();

        db.record_test_baseline("test_a", "/project", 500, "passed").unwrap();
        db.record_test_baseline("test_b", "/project", 1500, "passed").unwrap();
        db.record_test_baseline("test_a", "/project", 700, "passed").unwrap();

        let baselines = db.get_project_baselines("/project").unwrap();
        assert_eq!(baselines.get("test_a"), Some(&600)); // avg(500, 700)
        assert_eq!(baselines.get("test_b"), Some(&1500));
    }

    #[test]
    fn test_cleanup_old_baselines() {
        let db = Database::open_in_memory().unwrap();

        // Record 25 entries
        for i in 0..25 {
            db.record_test_baseline("test_x", "/project", 1000 + i, "passed").unwrap();
        }

        db.cleanup_old_baselines("/project").unwrap();

        // Should only keep last 20
        let conn = db.connection();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM test_baselines WHERE test_name = 'test_x' AND project_root = '/project'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 20);
    }
}
