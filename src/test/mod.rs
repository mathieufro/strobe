pub mod adapter;
pub mod cargo_adapter;
pub mod catch2_adapter;
pub mod generic_adapter;
pub mod stuck_detector;
pub mod output;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio::pin;

use adapter::*;
use cargo_adapter::CargoTestAdapter;
use catch2_adapter::Catch2Adapter;
use generic_adapter::GenericAdapter;
use stuck_detector::StuckDetector;

/// Live progress state for an in-flight test run, updated incrementally.
#[derive(Debug, Clone)]
pub struct TestProgress {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub current_test: Option<String>,
    pub started_at: Instant,
}

impl TestProgress {
    pub fn new() -> Self {
        Self {
            passed: 0,
            failed: 0,
            skipped: 0,
            current_test: None,
            started_at: Instant::now(),
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

    /// Run tests via direct subprocess (fast path — no Frida).
    pub async fn run(
        &self,
        project_root: &Path,
        framework: Option<&str>,
        level: Option<TestLevel>,
        test: Option<&str>,
        command: Option<&str>,
        env: &HashMap<String, String>,
        timeout: Option<u64>,
        progress: Option<Arc<Mutex<TestProgress>>>,
    ) -> crate::Result<TestRunResult> {
        let adapter = self.detect_adapter(project_root, framework, command);
        let framework_name = adapter.name().to_string();

        // Build command — if a command is provided, use it as the binary path
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

        // Merge user env with adapter env (user env takes precedence)
        let mut combined_env = test_cmd.env.clone();
        combined_env.extend(env.clone());

        // Spawn subprocess
        let mut child = tokio::process::Command::new(&test_cmd.program)
            .args(&test_cmd.args)
            .envs(&combined_env)
            .current_dir(project_root)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| crate::Error::Frida(format!(
                "Failed to spawn test command '{}': {}", test_cmd.program, e
            )))?;

        let pid = child.id().unwrap_or(0);

        // Run stuck detector in parallel
        let detector = StuckDetector::new(pid, hard_timeout);
        let detector_handle = tokio::spawn(async move { detector.run().await });

        // Capture stdout and stderr
        let child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        // Select the right progress updater based on adapter
        let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str() {
            "cargo" => Some(cargo_adapter::update_progress),
            "catch2" => Some(catch2_adapter::update_progress),
            _ => None,
        };

        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(stdout) = child_stdout {
                let mut reader = tokio::io::BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break,
                        Ok(_) => {
                            if let (Some(f), Some(ref p)) = (progress_fn, &progress) {
                                f(&line, p);
                            }
                            buf.push_str(&line);
                        }
                        Err(_) => break,
                    }
                }
            }
            buf
        });

        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(ref mut stderr) = child_stderr {
                let _ = stderr.read_to_string(&mut buf).await;
            }
            buf
        });

        // Wait for exit or stuck detection
        pin!(detector_handle);
        tokio::select! {
            status = child.wait() => {
                // Process exited normally
                detector_handle.abort();
                let stdout_buf = stdout_task.await.unwrap_or_default();
                let stderr_buf = stderr_task.await.unwrap_or_default();

                let exit_code = status
                    .map(|s| s.code().unwrap_or(-1))
                    .unwrap_or(-1);

                let mut result = adapter.parse_output(&stdout_buf, &stderr_buf, exit_code);

                // Add suggested traces to failures that don't have them
                for failure in &mut result.failures {
                    if failure.suggested_traces.is_empty() {
                        failure.suggested_traces = adapter.suggest_traces(failure);
                    }
                }

                Ok(TestRunResult {
                    framework: framework_name,
                    result,
                    session_id: None,
                    raw_stdout: stdout_buf,
                    raw_stderr: stderr_buf,
                })
            }
            stuck_result = &mut detector_handle => {
                // Stuck detected — capture stacks before killing
                match stuck_result {
                    Ok(Some(stuck_info)) => {
                        // Capture stacks
                        let threads = adapter.capture_stacks(pid);

                        // Kill the process
                        let _ = child.kill().await;
                        let stdout_buf = stdout_task.await.unwrap_or_default();
                        let stderr_buf = stderr_task.await.unwrap_or_default();

                        let stuck_test = StuckTest {
                            name: test.unwrap_or("unknown").to_string(),
                            elapsed_ms: stuck_info.elapsed_ms,
                            diagnosis: stuck_info.diagnosis,
                            threads: threads.clone(),
                            suggested_traces: stuck_info.suggested_traces,
                        };

                        let result = TestResult {
                            summary: TestSummary {
                                passed: 0,
                                failed: 0,
                                skipped: 0,
                                stuck: Some(1),
                                duration_ms: stuck_info.elapsed_ms,
                            },
                            failures: vec![],
                            stuck: vec![stuck_test],
                            all_tests: vec![],
                        };

                        Ok(TestRunResult {
                            framework: framework_name,
                            result,
                            session_id: None,
                            raw_stdout: stdout_buf,
                            raw_stderr: stderr_buf,
                        })
                    }
                    _ => {
                        // Hard timeout with no stuck diagnosis
                        let _ = child.kill().await;
                        let stdout_buf = stdout_task.await.unwrap_or_default();
                        let stderr_buf = stderr_task.await.unwrap_or_default();

                        let result = TestResult {
                            summary: TestSummary {
                                passed: 0,
                                failed: 0,
                                skipped: 0,
                                stuck: Some(1),
                                duration_ms: hard_timeout,
                            },
                            failures: vec![],
                            stuck: vec![StuckTest {
                                name: test.unwrap_or("unknown").to_string(),
                                elapsed_ms: hard_timeout,
                                diagnosis: "Hard timeout reached".to_string(),
                                threads: vec![],
                                suggested_traces: vec![],
                            }],
                            all_tests: vec![],
                        };

                        Ok(TestRunResult {
                            framework: framework_name,
                            result,
                            session_id: None,
                            raw_stdout: stdout_buf,
                            raw_stderr: stderr_buf,
                        })
                    }
                }
            }
        }
    }

    /// Run tests with Frida instrumentation (instrumented path).
    /// Returns results + sessionId for debug_query.
    pub async fn run_instrumented(
        &self,
        project_root: &Path,
        framework: Option<&str>,
        test: Option<&str>,
        command: Option<&str>,
        env: &HashMap<String, String>,
        timeout: Option<u64>,
        session_manager: &crate::daemon::SessionManager,
        trace_patterns: &[String],
        _watches: Option<&crate::mcp::WatchUpdate>,
        _connection_id: &str,
        _progress: Option<Arc<Mutex<TestProgress>>>,
    ) -> crate::Result<TestRunResult> {
        let adapter = self.detect_adapter(project_root, framework, command);
        let framework_name = adapter.name().to_string();

        // Build the test command (typically single test for instrumented rerun)
        let test_cmd = if let Some(cmd) = command {
            if let Some(test_name) = test {
                Catch2Adapter::single_test_for_binary(cmd, test_name)
            } else {
                Catch2Adapter::command_for_binary(cmd, None)
            }
        } else if let Some(test_name) = test {
            adapter.single_test_command(project_root, test_name)?
        } else {
            adapter.suite_command(project_root, None, env)?
        };

        let hard_timeout = timeout.unwrap_or_else(|| adapter.default_timeout(None));

        // Generate session ID and spawn with Frida
        let binary = &test_cmd.program;
        let binary_name = Path::new(binary)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("test");
        let session_id = session_manager.generate_session_id(
            &format!("test-{}", binary_name)
        );

        let mut combined_env = test_cmd.env.clone();
        combined_env.extend(env.clone());

        let pid = session_manager.spawn_with_frida(
            &session_id,
            &test_cmd.program,
            &test_cmd.args,
            Some(project_root.to_str().unwrap_or(".")),
            project_root.to_str().unwrap_or("."),
            Some(&combined_env),
        ).await?;

        session_manager.create_session(
            &session_id,
            &test_cmd.program,
            project_root.to_str().unwrap_or("."),
            pid,
        )?;

        // Apply trace patterns
        if !trace_patterns.is_empty() {
            session_manager.add_patterns(&session_id, trace_patterns)?;
            match session_manager.update_frida_patterns(
                &session_id,
                Some(trace_patterns),
                None,
                None,
            ).await {
                Ok(result) => {
                    session_manager.set_hook_count(&session_id, result.installed);
                }
                Err(e) => {
                    tracing::warn!("Failed to apply trace patterns for test session: {}", e);
                }
            }
        }

        // Wait for process to exit (poll kill(pid, 0))
        let start = std::time::Instant::now();
        loop {
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            if !alive {
                break;
            }
            if start.elapsed().as_millis() as u64 > hard_timeout {
                // Timeout — kill and report as stuck
                unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // Let DB writer flush
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Query captured stdout/stderr from session DB
        let stdout_events = session_manager.db().query_events(&session_id, |q| {
            q.event_type(crate::db::EventType::Stdout).limit(10000)
        }).unwrap_or_default();

        let stderr_events = session_manager.db().query_events(&session_id, |q| {
            q.event_type(crate::db::EventType::Stderr).limit(10000)
        }).unwrap_or_default();

        let stdout_buf: String = stdout_events.iter()
            .filter_map(|e| e.text.as_deref())
            .collect::<Vec<_>>()
            .join("");
        let stderr_buf: String = stderr_events.iter()
            .filter_map(|e| e.text.as_deref())
            .collect::<Vec<_>>()
            .join("");

        let exit_code = session_manager.get_session(&session_id)?
            .map(|s| match s.status {
                crate::db::SessionStatus::Stopped => 0,
                _ => -1,
            })
            .unwrap_or(-1);

        let mut result = adapter.parse_output(&stdout_buf, &stderr_buf, exit_code);

        for failure in &mut result.failures {
            if failure.suggested_traces.is_empty() {
                failure.suggested_traces = adapter.suggest_traces(failure);
            }
        }

        Ok(TestRunResult {
            framework: framework_name,
            result,
            session_id: Some(session_id),
            raw_stdout: stdout_buf,
            raw_stderr: stderr_buf,
        })
    }
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
}
