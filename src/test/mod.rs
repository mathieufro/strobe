pub mod adapter;
pub mod cargo_adapter;
pub mod catch2_adapter;
pub mod pytest_adapter;
pub mod unittest_adapter;
pub mod stacks;
pub mod stuck_detector;
pub mod output;

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use adapter::*;
use cargo_adapter::CargoTestAdapter;
use catch2_adapter::Catch2Adapter;
use pytest_adapter::PytestAdapter;
use unittest_adapter::UnittestAdapter;
use stuck_detector::StuckDetector;

/// Phase of a test run lifecycle.
#[derive(Debug, Clone, PartialEq)]
pub enum TestPhase {
    /// Building/compiling (no test output yet)
    Compiling,
    /// Tests are executing
    Running,
    /// All test suites have reported final results
    SuitesFinished,
}

/// Advisory warning from the stuck detector — informs the LLM, does not kill.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckWarning {
    pub test_name: Option<String>,
    pub idle_ms: u64,
    pub diagnosis: String,
    pub suggested_traces: Vec<String>,
}

/// Live progress state for an in-flight test run, updated incrementally.
#[derive(Debug, Clone)]
pub struct TestProgress {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    /// All currently running tests and when they started.
    /// Populated on "test started", removed on "test ok"/"failed"/"timeout".
    pub running_tests: HashMap<String, Instant>,
    pub started_at: Instant,
    pub phase: TestPhase,
    pub warnings: Vec<StuckWarning>,
    /// Wall-clock durations per test (name → ms), populated as tests complete.
    pub test_durations: HashMap<String, u64>,
}

impl TestProgress {
    pub fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
            skipped: 0,
            running_tests: HashMap::new(),
            started_at: Instant::now(),
            phase: TestPhase::Compiling,
            warnings: Vec::new(),
            test_durations: HashMap::new(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Get the "current test" — the one that's been running longest (for stuck detection).
    pub fn current_test(&self) -> Option<String> {
        self.running_tests.iter()
            .min_by_key(|(_, started)| *started)
            .map(|(name, _)| name.clone())
    }

    /// Get when the current (longest-running) test started.
    pub fn current_test_started_at(&self) -> Option<Instant> {
        self.running_tests.values().min().copied()
    }
}

/// State of a tracked test run stored in daemon.
pub enum TestRunState {
    /// Test is still running.
    Running {
        progress: Arc<Mutex<TestProgress>>,
    },
    /// Test completed with results (pre-serialized DebugTestResponse).
    Completed {
        response: serde_json::Value,
        completed_at: Instant,
    },
    /// Test run failed before producing results.
    Failed {
        error: String,
        completed_at: Instant,
    },
}

/// A tracked test run in the daemon.
pub struct TestRun {
    pub id: String,
    pub state: TestRunState,
    /// Whether results have been fetched (eligible for cleanup).
    pub fetched: bool,
    /// Frida session ID — always set when running inside Frida.
    pub session_id: Option<String>,
    /// Project root for baseline lookup.
    pub project_root: String,
}

pub struct TestRunner {
    adapters: Vec<Box<dyn TestAdapter>>,
}

impl TestRunner {
    pub fn new() -> Self {
        Self {
            adapters: vec![
                Box::new(CargoTestAdapter),
                Box::new(Catch2Adapter),
                Box::new(PytestAdapter),
                Box::new(UnittestAdapter),
            ],
        }
    }

    /// Detect the best adapter for this project.
    /// Returns an error if no framework is detected or an invalid framework name is given.
    pub fn detect_adapter(
        &self,
        project_root: &Path,
        framework: Option<&str>,
        command: Option<&str>,
    ) -> crate::Result<&dyn TestAdapter> {
        // Explicit framework override
        if let Some(name) = framework {
            for adapter in &self.adapters {
                if adapter.name() == name {
                    return Ok(adapter.as_ref());
                }
            }
            return Err(crate::Error::ValidationError(
                format!("Unknown framework '{}'. Supported: 'cargo', 'catch2'", name)
            ));
        }

        // Auto-detect: highest confidence wins
        let mut best: Option<(&dyn TestAdapter, u8)> = None;
        for adapter in &self.adapters {
            let confidence = adapter.detect(project_root, command);
            if confidence > 0 {
                if let Some((_, best_conf)) = best {
                    if confidence > best_conf {
                        best = Some((adapter.as_ref(), confidence));
                    }
                } else {
                    best = Some((adapter.as_ref(), confidence));
                }
            }
        }

        match best {
            Some((adapter, _)) => Ok(adapter),
            None => Err(crate::Error::ValidationError(
                "No test framework detected. Supported frameworks:\n\
                 - Cargo (Rust): provide projectRoot pointing to a directory with Cargo.toml\n\
                 - Catch2 (C++): provide command with path to a test binary".to_string()
            )),
        }
    }

    /// Run tests inside Frida with DB-based progress polling.
    /// Always spawns via Frida — the LLM can add trace patterns at any time via debug_trace.
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
        _watches: Option<&crate::mcp::WatchUpdate>,
        _connection_id: &str,
        session_id: &str,
        progress: Arc<Mutex<TestProgress>>,
    ) -> crate::Result<TestRunResult> {
        let adapter = self.detect_adapter(project_root, framework, command)?;
        let framework_name = adapter.name().to_string();

        // Build command
        let test_cmd = if let Some(cmd) = command {
            if let Some(test_name) = test {
                Catch2Adapter::single_test_for_binary(cmd, test_name)
            } else {
                Catch2Adapter::command_for_binary(cmd, level)
            }
        } else if let Some(test_name) = test {
            adapter.single_test_command(project_root, test_name)?
        } else {
            adapter.suite_command(project_root, level, env)?
        };

        let hard_timeout = timeout.unwrap_or_else(|| adapter.default_timeout(level));

        // Resolve program to absolute path (Frida's Device.spawn doesn't do PATH lookup)
        let program = resolve_program(&test_cmd.program);

        // Inherit parent environment, then overlay test-specific and user-provided vars.
        // envp() replaces the environment entirely, so we must include everything.
        let mut combined_env: HashMap<String, String> = std::env::vars().collect();
        combined_env.extend(test_cmd.env.clone());
        combined_env.extend(env.clone());

        // Create session BEFORE spawning — writer task needs FK to exist
        // when events arrive (interpreted processes start immediately)
        session_manager.create_session(
            session_id,
            &test_cmd.program,
            project_root.to_str().unwrap_or("."),
            0,
        )?;

        // Spawn via Frida — defer resume if we need to install hooks first
        let has_trace_patterns = !trace_patterns.is_empty();
        let pid = session_manager.spawn_with_frida(
            session_id,
            &program,
            &test_cmd.args,
            Some(project_root.to_str().unwrap_or(".")),
            project_root.to_str().unwrap_or("."),
            Some(&combined_env),
            has_trace_patterns, // defer_resume: install hooks before running
        ).await?;

        // Apply trace patterns BEFORE resuming the process
        if has_trace_patterns {
            session_manager.add_patterns(session_id, trace_patterns)?;
            match session_manager.update_frida_patterns(
                session_id,
                Some(trace_patterns),
                None,
                None,
            ).await {
                Ok(result) => {
                    session_manager.set_hook_count(session_id, result.installed);
                }
                Err(e) => {
                    tracing::warn!("Failed to apply trace patterns for test session: {}", e);
                }
            }
            // NOW resume — hooks are installed
            session_manager.resume_process(pid).await?;
        }

        // Select progress updater based on adapter
        let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str() {
            "cargo" => Some(cargo_adapter::update_progress),
            "catch2" => Some(catch2_adapter::update_progress),
            _ => None,
        };

        // Spawn stuck detector as background monitor
        let detector_progress = Arc::clone(&progress);
        let mut detector = StuckDetector::new(pid, hard_timeout, detector_progress);

        // Phase 2: Wire up breakpoint pause awareness so the stuck detector
        // doesn't false-positive on threads paused at breakpoints.
        {
            let paused = session_manager.paused_threads_ref();
            let sid = session_id.to_string();
            detector = detector.with_pause_check(Arc::new(move || {
                paused.read().unwrap_or_else(|e| e.into_inner())
                    .get(&sid)
                    .map_or(false, |m| !m.is_empty())
            }));
        }

        let detector_handle = tokio::spawn(async move { detector.run().await });

        // Progress-aware polling loop — poll DB for stdout events
        let mut last_seen_timestamp_ns: i64 = 0;
        let poll_interval = std::time::Duration::from_millis(500);
        // Hard timeout kills the process; add grace period for stuck detector to write warnings.
        // hard_timeout is in milliseconds (from adapter.default_timeout).
        let kill_timeout = std::time::Duration::from_millis(hard_timeout + 5_000);
        let safety_timeout = std::time::Duration::from_secs(600); // 10 min safety net
        let start = std::time::Instant::now();

        loop {
            if !stacks::is_process_alive(pid) {
                break;
            }

            // Hard timeout — kill the process tree (stuck detector has already written warnings)
            if start.elapsed() > kill_timeout {
                stacks::kill_process_tree(pid);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                break;
            }

            // Safety net timeout
            if start.elapsed() > safety_timeout {
                stacks::kill_process_tree(pid);
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                break;
            }

            // Poll DB for new stdout events and update progress.
            // Use timestamp-based filtering: only fetch events newer than last seen.
            // (Offset-based pagination is broken with DESC ordering — new events
            // arrive at the "top" but offset skips from the top, missing them.)
            if let Some(update_fn) = progress_fn {
                let mut new_events = session_manager.db().query_events(session_id, |q| {
                    let mut q = q.event_type(crate::db::EventType::Stdout).limit(500);
                    if last_seen_timestamp_ns > 0 {
                        q.timestamp_from_ns = Some(last_seen_timestamp_ns + 1);
                    }
                    q
                }).unwrap_or_default();

                // DB returns newest-first; reverse to chronological so "test started"
                // is processed before "test ok".
                new_events.reverse();

                for event in &new_events {
                    if event.timestamp_ns > last_seen_timestamp_ns {
                        last_seen_timestamp_ns = event.timestamp_ns;
                    }
                    if let Some(text) = &event.text {
                        update_fn(text, &progress);
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }

        // Abort detector
        detector_handle.abort();

        // Mark suites finished in progress
        {
            let mut p = progress.lock().unwrap();
            if p.phase != TestPhase::SuitesFinished {
                p.phase = TestPhase::SuitesFinished;
            }
        }

        // Let DB writer flush
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Final progress drain — process any remaining stdout events that arrived
        // after the last poll (e.g., test "ok" events emitted just before process exit).
        if let Some(update_fn) = progress_fn {
            let mut remaining = session_manager.db().query_events(session_id, |q| {
                let mut q = q.event_type(crate::db::EventType::Stdout).limit_uncapped(5000);
                if last_seen_timestamp_ns > 0 {
                    q.timestamp_from_ns = Some(last_seen_timestamp_ns + 1);
                }
                q
            }).unwrap_or_default();

            remaining.reverse(); // Chronological order

            for event in &remaining {
                if let Some(text) = &event.text {
                    update_fn(text, &progress);
                }
            }
        }

        // Query ALL stdout/stderr from DB
        let stdout_buf = collect_output(session_manager.db(), session_id, crate::db::EventType::Stdout);
        let stderr_buf = collect_output(session_manager.db(), session_id, crate::db::EventType::Stderr);

        // Get exit code: try waitpid for real exit code, fall back to test results
        let exit_code = {
            let mut status: i32 = 0;
            let result = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
            if result > 0 {
                if libc::WIFEXITED(status) {
                    libc::WEXITSTATUS(status)
                } else if libc::WIFSIGNALED(status) {
                    128 + libc::WTERMSIG(status)
                } else {
                    -1
                }
            } else {
                // Already reaped or not our child — infer from test results
                let p = progress.lock().unwrap();
                if p.failed > 0 { 1 } else { 0 }
            }
        };

        let mut result = adapter.parse_output(&stdout_buf, &stderr_buf, exit_code);

        // Compute per-test durations from DB event timestamps.
        // Cargo's JSON format doesn't include exec_time on individual test events,
        // so we derive durations from the time between "started" and "ok"/"failed"
        // stdout events in the database.
        {
            let mut test_start_times: HashMap<String, i64> = HashMap::new();
            let mut test_durations: HashMap<String, u64> = HashMap::new();

            let mut all_stdout = session_manager.db().query_events(session_id, |q| {
                q.event_type(crate::db::EventType::Stdout).limit_uncapped(50000)
            }).unwrap_or_default();
            all_stdout.reverse(); // Chronological order

            for event in &all_stdout {
                if let Some(text) = &event.text {
                    for line in text.lines() {
                        let line = line.trim();
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                            let etype = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            let eevent = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
                            let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            match (etype, eevent) {
                                ("test", "started") if !name.is_empty() => {
                                    test_start_times.insert(name.to_string(), event.timestamp_ns);
                                }
                                ("test", "ok") | ("test", "failed") if !name.is_empty() => {
                                    if let Some(start_ns) = test_start_times.get(name) {
                                        let dur_ms = (event.timestamp_ns - start_ns).max(0) as u64 / 1_000_000;
                                        test_durations.insert(name.to_string(), dur_ms);
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }

            for test in &mut result.all_tests {
                if test.duration_ms == 0 {
                    if let Some(&dur) = test_durations.get(&test.name) {
                        test.duration_ms = dur;
                    }
                }
            }
        }

        for failure in &mut result.failures {
            if failure.suggested_traces.is_empty() {
                failure.suggested_traces = adapter.suggest_traces(failure);
            }
        }

        Ok(TestRunResult {
            framework: framework_name,
            result,
            session_id: Some(session_id.to_string()),
            raw_stdout: stdout_buf,
            raw_stderr: stderr_buf,
        })
    }
}

/// Resolve a program name to an absolute path via PATH lookup.
/// Frida's Device.spawn() doesn't do PATH resolution like a shell.
fn resolve_program(program: &str) -> String {
    if program.contains('/') {
        return program.to_string();
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(':') {
            let full = format!("{}/{}", dir, program);
            if Path::new(&full).exists() {
                return full;
            }
        }
    }
    program.to_string()
}

/// Collect all output events of a given type from the database into a single string.
fn collect_output(db: &crate::db::Database, session_id: &str, event_type: crate::db::EventType) -> String {
    let mut events = db.query_events(session_id, |q| {
        q.event_type(event_type).limit_uncapped(50000)
    })
    .unwrap_or_default();
    // Query returns newest-first; reverse to chronological for output concatenation
    events.reverse();
    events.into_iter()
        .filter_map(|e| e.text)
        .collect::<Vec<_>>()
        .join("")
}

/// Combined result from test run, used by MCP tool handler.
pub struct TestRunResult {
    pub framework: String,
    pub result: TestResult,
    pub session_id: Option<String>,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adapter_detection_cargo() {
        let runner = TestRunner::new();
        // strobe project root has Cargo.toml → should detect cargo
        let adapter = runner.detect_adapter(Path::new("."), None, None).unwrap();
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn test_adapter_detection_no_match() {
        let runner = TestRunner::new();
        let result = runner.detect_adapter(Path::new("/nonexistent"), None, None);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("No test framework detected"), "got: {}", err);
        assert!(err.contains("Cargo"));
        assert!(err.contains("Catch2"));
    }

    #[test]
    fn test_adapter_detection_invalid_framework() {
        let runner = TestRunner::new();
        let result = runner.detect_adapter(Path::new("."), Some("generic"), None);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("Unknown framework 'generic'"), "got: {}", err);
        assert!(err.contains("'cargo', 'catch2'"));
    }

    #[test]
    fn test_run_has_session_id() {
        let progress = std::sync::Arc::new(std::sync::Mutex::new(TestProgress::new()));
        let run = TestRun {
            id: "test-abc123".to_string(),
            state: TestRunState::Running { progress },
            fetched: false,
            session_id: Some("session-xyz".to_string()),
            project_root: "/project".to_string(),
        };
        assert_eq!(run.session_id.as_deref(), Some("session-xyz"));
    }

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

        progress.warnings.clear();
        assert!(progress.warnings.is_empty());
    }
}
