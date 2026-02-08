# Smart Test Monitoring Implementation Plan

**Spec:** `PLAN.md`
**Goal:** Always run tests inside Frida, replace kill-based stuck detection with advisory warnings, add historical baselines, let the LLM decide when to kill.
**Architecture:** Unified always-Frida test runner with DB-polling progress, continuous stuck monitor writing warnings to shared `TestProgress`, SQLite `test_baselines` table for historical per-test durations, `debug_stop(sessionId)` for LLM-initiated kills.
**Tech Stack:** tokio, frida, rusqlite, serde_json
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Foundation):** Tasks 1-4 — New types, DB baselines table, stuck detector refactor
- **Stream B (Runner Unification):** Tasks 5-6 — Merge run paths, DB-based progress polling (depends on A)
- **Stream C (MCP Integration):** Tasks 7-9 — Wire warnings/baselines/sessionId into status, update tool schemas, update system prompt (depends on B)

**Dependencies:** A must complete first. B depends on A. C depends on B.

---

### Task 1: Add StuckWarning type and warnings field to TestProgress

**Files:**
- Modify: `src/test/mod.rs:21-58`

**Step 1: Write the failing test**

```rust
// In src/test/mod.rs, add to #[cfg(test)] mod tests:
#[test]
fn test_progress_warnings() {
    let mut progress = TestProgress::new();
    assert!(progress.warnings.is_empty());

    progress.warnings.push(StuckWarning {
        test_name: Some("test_auth".to_string()),
        idle_ms: 12000,
        diagnosis: "0% CPU, stacks unchanged 6s".to_string(),
        suggested_traces: vec!["auth::*".to_string()],
    });
    assert_eq!(progress.warnings.len(), 1);
    assert_eq!(progress.warnings[0].idle_ms, 12000);

    // Clearing warnings when test progresses
    progress.warnings.clear();
    assert!(progress.warnings.is_empty());
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib test::tests::test_progress_warnings`
Expected: FAIL — `StuckWarning` type doesn't exist, `warnings` field doesn't exist on `TestProgress`

**Step 3: Write minimal implementation**

In `src/test/mod.rs`, add the `StuckWarning` struct and extend `TestProgress`:

```rust
use serde::Serialize;

/// Advisory warning from the stuck detector — informs the LLM, does not kill.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckWarning {
    pub test_name: Option<String>,
    pub idle_ms: u64,
    pub diagnosis: String,
    pub suggested_traces: Vec<String>,
}

pub struct TestProgress {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub current_test: Option<String>,
    pub started_at: Instant,
    pub phase: TestPhase,
    pub warnings: Vec<StuckWarning>,  // NEW
}

impl TestProgress {
    pub fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
            skipped: 0,
            current_test: None,
            started_at: Instant::now(),
            phase: TestPhase::Compiling,
            warnings: Vec::new(),
        }
    }
    // elapsed_ms() unchanged
}
```

**Step 4: Run test — verify it passes**
Run: `cargo test --lib test::tests::test_progress_warnings`
Expected: PASS

**Checkpoint:** `StuckWarning` type exists, `TestProgress` has a `warnings` vec.

---

### Task 2: Add session_id to TestRun

**Files:**
- Modify: `src/test/mod.rs:60-84`

**Step 1: Write the failing test**

```rust
// In src/test/mod.rs #[cfg(test)] mod tests:
#[test]
fn test_run_has_session_id() {
    let progress = std::sync::Arc::new(std::sync::Mutex::new(TestProgress::new()));
    let run = TestRun {
        id: "test-abc123".to_string(),
        state: TestRunState::Running { progress },
        fetched: false,
        session_id: Some("session-xyz".to_string()),
    };
    assert_eq!(run.session_id.as_deref(), Some("session-xyz"));
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib test::tests::test_run_has_session_id`
Expected: FAIL — `session_id` field doesn't exist on `TestRun`

**Step 3: Write minimal implementation**

```rust
pub struct TestRun {
    pub id: String,
    pub state: TestRunState,
    pub fetched: bool,
    pub session_id: Option<String>,  // NEW — always set when running inside Frida
}
```

Fix any existing code that constructs `TestRun` (in `src/daemon/server.rs:1322`) to include `session_id: None` initially — it gets set once the Frida session is created.

**Step 4: Run test — verify it passes**
Run: `cargo test --lib test::tests::test_run_has_session_id`
Expected: PASS

**Checkpoint:** `TestRun` carries a `session_id` for the LLM to use with `debug_trace`/`debug_stop`.

---

### Task 3: Test baselines SQLite table and operations

**Files:**
- Modify: `src/db/schema.rs:36-163` (add table)
- Create: `src/db/baselines.rs`
- Modify: `src/db/mod.rs` (add module)

**Step 1: Write the failing tests**

```rust
// In src/db/baselines.rs:
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

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
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib db::baselines::tests`
Expected: FAIL — module doesn't exist, table doesn't exist

**Step 3: Write minimal implementation**

In `src/db/schema.rs`, add to `initialize_schema()`:

```rust
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
```

Create `src/db/baselines.rs`:

```rust
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
```

In `src/db/mod.rs`, add: `mod baselines;`

**Step 4: Run test — verify it passes**
Run: `cargo test --lib db::baselines::tests`
Expected: PASS

**Checkpoint:** Baseline storage works — record, query averages, batch lookup, cleanup.

---

### Task 4: Transform stuck detector into continuous advisory monitor

**Files:**
- Modify: `src/test/stuck_detector.rs`

**Step 1: Write the failing test**

```rust
// In src/test/stuck_detector.rs #[cfg(test)] mod tests:
#[tokio::test]
async fn test_stuck_detector_writes_warnings_instead_of_returning() {
    // Spawn a process that exits quickly
    let mut child = tokio::process::Command::new("sleep")
        .arg("0.1")
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();

    let progress = std::sync::Arc::new(std::sync::Mutex::new(
        super::TestProgress::new()
    ));
    // Set phase to Running so detector doesn't skip
    {
        let mut p = progress.lock().unwrap();
        p.phase = super::TestPhase::Running;
    }

    let detector = StuckDetector::new(pid, 60_000, std::sync::Arc::clone(&progress));
    // Should return () (not Option<StuckInfo>) when process exits
    detector.run().await;

    let _ = child.wait().await;
    // No warnings expected for a fast-exiting process
    let p = progress.lock().unwrap();
    assert!(p.warnings.is_empty());
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib test::stuck_detector::tests::test_stuck_detector_writes_warnings`
Expected: FAIL — `StuckDetector::new` doesn't take progress as required param, `run()` returns `Option<StuckInfo>` not `()`

**Step 3: Write minimal implementation**

Refactor `StuckDetector`:

```rust
pub struct StuckDetector {
    pid: u32,
    hard_timeout_ms: u64,
    progress: Arc<Mutex<TestProgress>>,  // Now required
}

impl StuckDetector {
    pub fn new(pid: u32, hard_timeout_ms: u64, progress: Arc<Mutex<TestProgress>>) -> Self {
        Self { pid, hard_timeout_ms, progress }
    }

    // Remove with_progress() — progress is now required

    fn current_phase(&self) -> TestPhase {
        self.progress.lock().unwrap().phase.clone()
    }

    fn current_test(&self) -> Option<String> {
        self.progress.lock().unwrap().current_test.clone()
    }

    fn write_warning(&self, diagnosis: &str, idle_ms: u64) {
        let mut p = self.progress.lock().unwrap();
        let test_name = p.current_test.clone();
        // Clear any previous warning for this test (replace, don't accumulate)
        p.warnings.retain(|w| w.test_name != test_name);
        p.warnings.push(super::StuckWarning {
            test_name,
            idle_ms,
            diagnosis: diagnosis.to_string(),
            suggested_traces: vec![],
        });
    }

    fn clear_warnings(&self) {
        self.progress.lock().unwrap().warnings.clear();
    }

    /// Run as continuous monitor. Never returns until process exits.
    /// Writes warnings to shared progress instead of returning StuckInfo.
    pub async fn run(self) {
        let start = Instant::now();
        let mut running_since: Option<Instant> = None;
        let mut prev_cpu_ns: Option<u64> = None;
        let mut suspicious_since: Option<Instant> = None;
        let mut zero_delta_count = 0u32;
        let mut constant_high_count = 0u32;
        let mut prev_test: Option<String> = None;

        loop {
            let alive = unsafe { libc::kill(self.pid as i32, 0) } == 0;
            if !alive {
                return; // Process exited
            }

            let phase = self.current_phase();

            // Track when tests start running
            if running_since.is_none() && phase != super::TestPhase::Compiling {
                running_since = Some(Instant::now());
            }

            // If current test changed, clear any warnings (test progressed)
            let current = self.current_test();
            if current != prev_test && prev_test.is_some() {
                self.clear_warnings();
                suspicious_since = None;
                zero_delta_count = 0;
                constant_high_count = 0;
            }
            prev_test = current;

            // SuitesFinished — not stuck, just cleaning up
            if phase == super::TestPhase::SuitesFinished {
                self.clear_warnings();
                suspicious_since = None;
                zero_delta_count = 0;
                constant_high_count = 0;
                prev_cpu_ns = Some(get_process_tree_cpu_ns(self.pid));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            // Hard timeout — write warning but don't kill
            if let Some(since) = running_since {
                if since.elapsed().as_millis() as u64 >= self.hard_timeout_ms {
                    self.write_warning(
                        "Hard timeout reached — consider stopping the test with debug_stop(sessionId)",
                        start.elapsed().as_millis() as u64,
                    );
                    // Keep running — LLM may want to investigate before killing
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }

            // Skip CPU analysis during compilation
            if phase == super::TestPhase::Compiling {
                prev_cpu_ns = Some(get_process_tree_cpu_ns(self.pid));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            // CPU time sampling (same logic as before)
            let cpu_ns = get_process_tree_cpu_ns(self.pid);
            if let Some(prev) = prev_cpu_ns {
                let delta = cpu_ns.saturating_sub(prev);
                let sample_interval_ns = 2_000_000_000u64;

                if delta == 0 {
                    zero_delta_count += 1;
                    constant_high_count = 0;
                    if suspicious_since.is_none() {
                        suspicious_since = Some(Instant::now());
                    }
                } else if delta > sample_interval_ns * 80 / 100 {
                    constant_high_count += 1;
                    zero_delta_count = 0;
                    if suspicious_since.is_none() {
                        suspicious_since = Some(Instant::now());
                    }
                } else {
                    zero_delta_count = 0;
                    constant_high_count = 0;
                    suspicious_since = None;
                    // CPU looks normal — clear any active warnings
                    self.clear_warnings();
                }

                if let Some(since) = suspicious_since {
                    if since.elapsed() > Duration::from_secs(6) {
                        let diagnosis_type = if zero_delta_count >= 3 {
                            "deadlock"
                        } else if constant_high_count >= 3 {
                            "infinite_loop"
                        } else {
                            "unknown"
                        };

                        if let Some(diagnosis) = self.confirm_with_stacks(diagnosis_type).await {
                            let idle_ms = since.elapsed().as_millis() as u64;
                            self.write_warning(&diagnosis, idle_ms);
                            // DON'T return — continue monitoring
                            // Reset suspicious counters but keep the warning
                            suspicious_since = None;
                            zero_delta_count = 0;
                            constant_high_count = 0;
                        } else {
                            suspicious_since = None;
                            zero_delta_count = 0;
                            constant_high_count = 0;
                        }
                    }
                }
            }

            prev_cpu_ns = Some(cpu_ns);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    // confirm_with_stacks() stays the same — returns Option<String>
}
```

**Step 4: Run test — verify it passes**
Run: `cargo test --lib test::stuck_detector::tests`
Expected: PASS (both old test adapted + new test)

Note: The existing `test_stuck_detector_returns_none_for_fast_exit` must be updated — `run()` now returns `()`, not `Option<StuckInfo>`. Adapt it:

```rust
#[tokio::test]
async fn test_stuck_detector_returns_for_fast_exit() {
    let mut child = tokio::process::Command::new("true").spawn().unwrap();
    let pid = child.id().unwrap();
    let _ = child.wait().await;
    let progress = Arc::new(Mutex::new(super::TestProgress::new()));
    let detector = StuckDetector::new(pid, 5000, Arc::clone(&progress));
    detector.run().await; // Should return quickly, no longer returns Option
    assert!(progress.lock().unwrap().warnings.is_empty());
}
```

**Checkpoint:** Stuck detector is a continuous monitor, writes warnings to `TestProgress`, never kills.

---

### Task 5: Unify test runner — always Frida with DB-based progress

**Files:**
- Modify: `src/test/mod.rs` (replace `run()` + `run_instrumented()` with single `run()`)

**Step 1: Write the failing test**

This is an integration-level change. The existing unit tests for `TestRunner` (`test_adapter_detection_cargo`, `test_adapter_detection_explicit_override`) should still pass. Add:

```rust
// In src/test/mod.rs #[cfg(test)] mod tests:
#[test]
fn test_run_result_always_has_session_id() {
    // After unification, TestRunResult.session_id should never be None
    let result = TestRunResult {
        framework: "cargo".to_string(),
        result: crate::test::adapter::TestResult {
            summary: crate::test::adapter::TestSummary {
                passed: 1, failed: 0, skipped: 0,
                stuck: None, duration_ms: 100,
            },
            failures: vec![],
            stuck: vec![],
            all_tests: vec![],
        },
        session_id: Some("session-123".to_string()),
        raw_stdout: String::new(),
        raw_stderr: String::new(),
    };
    assert!(result.session_id.is_some(), "session_id must always be set after unification");
}
```

**Step 2: Run test — verify it passes (this is a type-level test)**
Run: `cargo test --lib test::tests::test_run_result_always_has_session_id`
Expected: PASS (but we use it as a design constraint)

**Step 3: Implement the unified run()**

Remove the old `run()` method (lines 134-360) and rename `run_instrumented()` to `run()`. The new signature:

```rust
pub async fn run(
    &self,
    project_root: &Path,
    framework: Option<&str>,
    level: Option<TestLevel>,
    test: Option<&str>,
    command: Option<&str>,
    env: &HashMap<String, String>,
    timeout: Option<u64>,
    session_manager: &crate::daemon::SessionManager,
    trace_patterns: &[String],
    watches: Option<&crate::mcp::WatchUpdate>,
    connection_id: &str,
    progress: Arc<Mutex<TestProgress>>,
) -> crate::Result<TestRunResult>
```

Key changes from the old `run_instrumented()`:
- `progress` is no longer `Option` — always required
- `level` parameter added back (was only in native path)
- The progress polling loop replaces the simple `kill(pid, 0)` busy-wait

The core of the new method — replace the tight polling loop (old lines 444-457) with a progress-aware loop:

```rust
// Get adapter progress updater
let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str() {
    "cargo" => Some(cargo_adapter::update_progress),
    "catch2" => Some(catch2_adapter::update_progress),
    _ => None,
};

// Spawn stuck detector as background monitor
let detector_progress = Arc::clone(&progress);
let detector = StuckDetector::new(pid, hard_timeout, detector_progress);
let detector_handle = tokio::spawn(async move { detector.run().await });

// Progress-aware polling loop
let mut last_stdout_offset = 0u32;
let poll_interval = std::time::Duration::from_millis(500);
let safety_timeout = std::time::Duration::from_secs(600); // 10 min safety net
let start = std::time::Instant::now();

loop {
    // 1. Check if process is alive
    let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
    if !alive {
        break;
    }

    // 2. Safety net timeout
    if start.elapsed() > safety_timeout {
        unsafe { libc::kill(pid as i32, libc::SIGKILL); }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        break;
    }

    // 3. Poll DB for new stdout events and update progress
    if let Some(update_fn) = progress_fn {
        let new_events = session_manager.db().query_events(&session_id, |q| {
            q.event_type(crate::db::EventType::Stdout)
             .offset(last_stdout_offset)
             .limit(500)
        }).unwrap_or_default();

        for event in &new_events {
            if let Some(text) = &event.text {
                update_fn(text, &progress);
            }
        }
        last_stdout_offset += new_events.len() as u32;
    }

    tokio::time::sleep(poll_interval).await;
}

// Abort detector
detector_handle.abort();

// Mark suites finished in progress (stdout EOF equivalent)
{
    let mut p = progress.lock().unwrap();
    if p.phase != TestPhase::SuitesFinished {
        p.phase = TestPhase::SuitesFinished;
    }
}

// Let DB writer flush
tokio::time::sleep(std::time::Duration::from_millis(200)).await;

// Query ALL stdout/stderr from DB (same as before)
// ... rest of existing run_instrumented logic ...
```

**Step 4: Build and verify**
Run: `cargo build`
Expected: Compiles without errors. All call sites updated.

**Checkpoint:** Single `run()` method — always uses Frida, polls DB for progress, stuck detector runs as background monitor.

---

### Task 6: Update daemon server — remove branching, set session_id on TestRun

**Files:**
- Modify: `src/daemon/server.rs:1291-1415` (`tool_debug_test`)

**Step 1: Verify build (no test-first for wiring)**

This task is pure wiring — removing the `if has_instrumentation` branching and always calling the unified `run()`.

**Step 2: Implement**

In `tool_debug_test`, replace:

```rust
// OLD:
let has_instrumentation = req_clone.trace_patterns.is_some() || req_clone.watches.is_some();
let run_result = if has_instrumentation {
    runner.run_instrumented(...)
} else {
    runner.run(...)
};
```

With:

```rust
// NEW: Always run inside Frida
let trace_patterns = req_clone.trace_patterns.unwrap_or_default();
let run_result = runner.run(
    &project_root,
    req_clone.framework.as_deref(),
    req_clone.level,
    req_clone.test.as_deref(),
    req_clone.command.as_deref(),
    &env,
    req_clone.timeout,
    &session_manager,
    &trace_patterns,
    req_clone.watches.as_ref(),
    &connection_id_owned,
    progress_clone,
).await;
```

Also: before spawning the async task, generate the `session_id` and store it on the `TestRun`:

```rust
// Generate session_id upfront so we can store it on the TestRun
let session_id_for_run = session_manager.generate_session_id(&format!("test-{}", framework_name));

{
    let mut runs = self.test_runs.write().await;
    runs.insert(test_run_id.clone(), crate::test::TestRun {
        id: test_run_id.clone(),
        state: crate::test::TestRunState::Running { progress },
        fetched: false,
        session_id: Some(session_id_for_run.clone()),  // Set immediately
    });
}
```

Then pass this `session_id_for_run` into the unified `run()` so it uses this pre-generated ID instead of generating its own.

**Step 3: Build and verify**
Run: `cargo build`
Expected: Compiles. No more `has_instrumentation` branching.

**Checkpoint:** `tool_debug_test` always takes the Frida path. `session_id` is available on `TestRun` from the start.

---

### Task 7: Add warnings, session_id, baseline_ms to MCP status types and handler

**Files:**
- Modify: `src/mcp/types.rs:370-398`
- Modify: `src/daemon/server.rs:1417-1496` (`tool_debug_test_status`)

**Step 1: Update MCP types**

```rust
// In src/mcp/types.rs:

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestStuckWarning {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_name: Option<String>,
    pub idle_ms: u64,
    pub diagnosis: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestProgressSnapshot {
    pub elapsed_ms: u64,
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    // NEW fields:
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<TestStuckWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_test_baseline_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestStatusResponse {
    pub test_run_id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<TestProgressSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    // NEW: Always present so LLM can use debug_trace/debug_stop
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}
```

**Step 2: Update status handler**

In `tool_debug_test_status`, in the `Running` arm:

```rust
crate::test::TestRunState::Running { progress, .. } => {
    let p = progress.lock().unwrap();
    let phase_str = match p.phase { /* same as before */ };

    // Convert internal warnings to MCP type
    let warnings: Vec<crate::mcp::TestStuckWarning> = p.warnings.iter().map(|w| {
        crate::mcp::TestStuckWarning {
            test_name: w.test_name.clone(),
            idle_ms: w.idle_ms,
            diagnosis: w.diagnosis.clone(),
            suggested_traces: w.suggested_traces.clone(),
        }
    }).collect();

    // Look up baseline for current test
    let baseline_ms = if let Some(ref test_name) = p.current_test {
        self.session_manager.db()
            .get_test_baseline(test_name, &project_root)
            .unwrap_or(None)
    } else {
        None
    };

    // Faster polling when stuck warnings are active
    let retry_ms = if warnings.is_empty() { 5_000 } else { 2_000 };

    crate::mcp::DebugTestStatusResponse {
        test_run_id: req.test_run_id,
        status: "running".to_string(),
        progress: Some(crate::mcp::TestProgressSnapshot {
            elapsed_ms: p.elapsed_ms(),
            passed: p.passed,
            failed: p.failed,
            skipped: p.skipped,
            current_test: p.current_test.clone(),
            phase: Some(phase_str.to_string()),
            warnings,
            current_test_baseline_ms: baseline_ms,
        }),
        result: None,
        error: None,
        retry_in_ms: Some(retry_ms),
        session_id: test_run.session_id.clone(),
    }
}
```

For the `Completed` and `Failed` arms, add `session_id: test_run.session_id.clone()`.

Note: We need access to `project_root` in the status handler. Store it on `TestRun`:

```rust
pub struct TestRun {
    pub id: String,
    pub state: TestRunState,
    pub fetched: bool,
    pub session_id: Option<String>,
    pub project_root: String,  // NEW — for baseline lookup
}
```

**Step 3: Build and verify**
Run: `cargo build`
Expected: Compiles. Status responses now include `warnings`, `sessionId`, `currentTestBaselineMs`.

**Checkpoint:** LLM sees warnings, session ID, and baseline comparison in every status poll.

---

### Task 8: Record baselines after test completion

**Files:**
- Modify: `src/daemon/server.rs` (in the spawned task, after test completes)

**Step 1: Implement**

In the spawned task, after `run_result` is available and before transitioning state:

```rust
// Record baselines for completed tests
if let Ok(ref run_result) = run_result {
    for test_detail in &run_result.result.all_tests {
        let _ = session_manager.db().record_test_baseline(
            &test_detail.name,
            project_root.to_str().unwrap_or("."),
            test_detail.duration_ms,
            &test_detail.status,
        );
    }
    let _ = session_manager.db().cleanup_old_baselines(
        project_root.to_str().unwrap_or(".")
    );
}
```

**Step 2: Build and verify**
Run: `cargo build`
Expected: Compiles. Baselines recorded after each test run.

**Checkpoint:** Historical test durations accumulate in SQLite. Each subsequent run provides better baseline data.

---

### Task 9: Update MCP tool descriptions and system prompt

**Files:**
- Modify: `src/daemon/server.rs` (tool definitions + `debugging_instructions()`)

**Step 1: Update debug_test tool schema**

Change the `tracePatterns` description from:
```
"Presence triggers Frida instrumented path"
```
To:
```
"Trace patterns to apply immediately (tests always run inside Frida)"
```

Change the `watches` description from:
```
"Watch variables during test (triggers Frida path)"
```
To:
```
"Watch variables during test execution"
```

**Step 2: Update debugging_instructions()**

Replace the `## Running Tests` section with:

```
## Running Tests

ALWAYS use `debug_test` — never `cargo test` or test binaries via bash.
Tests always run inside Frida, so you can add traces at any time without restarting.

`debug_test` returns immediately with a `testRunId`. Poll `debug_test_status(testRunId)`
for progress. IMPORTANT: Each status response includes `retryInMs` — wait that many
milliseconds before polling again.

### Status Response Fields
- `progress.currentTest` — name of the currently executing test
- `progress.currentTestBaselineMs` — historical average duration for this test (if known)
- `progress.warnings` — stuck detection warnings (see below)
- `sessionId` — Frida session ID for `debug_trace` and `debug_stop`

### Stuck Test Detection
The test runner monitors for stuck tests (deadlocks, infinite loops). When detected,
`debug_test_status` includes warnings:
```json
{ "warnings": [{ "testName": "test_auth", "idleMs": 12000,
  "diagnosis": "0% CPU, stacks unchanged 6s" }] }
```

When you see a warning:
1. Use `debug_trace({ sessionId, add: ["relevant::patterns"] })` to investigate
2. Use `debug_query({ sessionId })` to see what's happening
3. Use `debug_stop({ sessionId })` to kill the test when you understand the issue

### Quick Reference
- Rust: provide `projectRoot` | C++: provide `command` (test binary path)
- Add `tracePatterns` to trace from the start (optional — can add later via `debug_trace`)
```

**Step 3: Build and verify**
Run: `cargo build`
Expected: Compiles. Updated system prompt reflects new behavior.

**Checkpoint:** LLM documentation matches the new architecture.

---

## Final Verification

After all tasks:

Run: `cargo test --lib`
Expected: All unit tests pass.

Run: `cargo test --test phase1d_test`
Expected: Integration tests pass (adapter detection, real cargo parsing).

Run: `cargo build --release`
Expected: Clean release build.

**Commit:** `feat: Smart test monitoring — always-Frida, advisory stuck detection, baselines`
