pub mod adapter;
pub mod cargo_adapter;
pub mod catch2_adapter;
pub mod generic_adapter;
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
use generic_adapter::GenericAdapter;
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
    pub current_test: Option<String>,
    pub current_test_started_at: Option<Instant>,
    pub started_at: Instant,
    pub phase: TestPhase,
    pub warnings: Vec<StuckWarning>,
}

impl TestProgress {
    pub fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
            skipped: 0,
            current_test: None,
            current_test_started_at: None,
            started_at: Instant::now(),
            phase: TestPhase::Compiling,
            warnings: Vec::new(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
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
                Box::new(GenericAdapter),
            ],
        }
    }

    /// Detect the best adapter for this project.
    pub fn detect_adapter(
        &self,
        project_root: &Path,
        framework: Option<&str>,
        command: Option<&str>,
    ) -> &dyn TestAdapter {
        // Explicit framework override
        if let Some(name) = framework {
            for adapter in &self.adapters {
                if adapter.name() == name {
                    return adapter.as_ref();
                }
            }
        }

        // Auto-detect: highest confidence wins
        let mut best: Option<(&dyn TestAdapter, u8)> = None;
        for adapter in &self.adapters {
            let confidence = adapter.detect(project_root, command);
            if let Some((_, best_conf)) = best {
                if confidence > best_conf {
                    best = Some((adapter.as_ref(), confidence));
                }
            } else {
                best = Some((adapter.as_ref(), confidence));
            }
        }

        best.map(|(a, _)| a).unwrap_or(self.adapters.last().unwrap().as_ref())
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
        let adapter = self.detect_adapter(project_root, framework, command);
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

        // Spawn via Frida
        let pid = session_manager.spawn_with_frida(
            session_id,
            &program,
            &test_cmd.args,
            Some(project_root.to_str().unwrap_or(".")),
            project_root.to_str().unwrap_or("."),
            Some(&combined_env),
        ).await?;

        session_manager.create_session(
            session_id,
            &test_cmd.program,
            project_root.to_str().unwrap_or("."),
            pid,
        )?;

        // Apply trace patterns if any
        if !trace_patterns.is_empty() {
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
        }

        // Select progress updater based on adapter
        let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str() {
            "cargo" => Some(cargo_adapter::update_progress),
            "catch2" => Some(catch2_adapter::update_progress),
            _ => None,
        };

        // Spawn stuck detector as background monitor
        let detector_progress = Arc::clone(&progress);
        let detector = StuckDetector::new(pid, hard_timeout, detector_progress);
        let detector_handle = tokio::spawn(async move { detector.run().await });

        // Progress-aware polling loop — poll DB for stdout events
        let mut last_stdout_offset = 0u32;
        let poll_interval = std::time::Duration::from_millis(500);
        // Hard timeout kills the process; add grace period for stuck detector to write warnings
        let kill_timeout = std::time::Duration::from_secs(hard_timeout + 5);
        let safety_timeout = std::time::Duration::from_secs(600); // 10 min safety net
        let start = std::time::Instant::now();

        loop {
            if !stacks::is_process_alive(pid) {
                break;
            }

            // Hard timeout — kill the process (stuck detector has already written warnings)
            if start.elapsed() > kill_timeout {
                unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                // Reap zombie so is_process_alive returns false
                unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG); }
                break;
            }

            // Safety net timeout
            if start.elapsed() > safety_timeout {
                unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG); }
                break;
            }

            // Poll DB for new stdout events and update progress
            if let Some(update_fn) = progress_fn {
                let new_events = session_manager.db().query_events(session_id, |q| {
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

        // Mark suites finished in progress
        {
            let mut p = progress.lock().unwrap();
            if p.phase != TestPhase::SuitesFinished {
                p.phase = TestPhase::SuitesFinished;
            }
        }

        // Let DB writer flush
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

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
    db.query_events(session_id, |q| {
        q.event_type(event_type).limit_uncapped(50000)
    })
    .unwrap_or_default()
    .into_iter()
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
        let adapter = runner.detect_adapter(Path::new("."), None, None);
        assert_eq!(adapter.name(), "cargo");
    }

    #[test]
    fn test_adapter_detection_explicit_override() {
        let runner = TestRunner::new();
        let adapter = runner.detect_adapter(Path::new("/nonexistent"), Some("generic"), None);
        assert_eq!(adapter.name(), "generic");
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
