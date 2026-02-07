# Phase 1d: Test Instrumentation Implementation Plan

**Spec:** `docs/specs/2026-02-07-phase-1d-test-instrumentation.md`
**Goal:** Universal, machine-readable test output via `debug_test` MCP tool with auto-detection, stuck detection, and optional Frida integration.
**Architecture:** `TestAdapter` trait with pluggable adapters (Cargo, Catch2, Generic), `TestRunner` orchestrator that delegates to adapters for detection/parsing/stack capture, `StuckDetector` engine using multi-signal analysis (output silence + CPU delta + stack sampling), details file writer for full test output.
**Tech Stack:** tokio (async subprocess + stuck detection), serde/serde_json (structured output), quick-xml (Catch2 XML parsing), libc (process CPU sampling, `kill` checks)
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Foundation):** Tasks 1, 2, 3 — types, trait, MCP types, TestRunner skeleton
- **Stream B (Adapters):** Tasks 4, 5, 6 — Cargo, Catch2, Generic adapters (after A)
- **Stream C (Engine):** Task 7 — StuckDetector (after A)
- **Stream D (Output):** Task 8 — Details file writer (after A)
- **Stream E (Integration):** Task 9 — Wire `debug_test` into daemon (after A, B, C, D)
- **Stream F (Install + Skill):** Tasks 10, 11 — independent of everything
- **Stream G (Tests):** Task 12 — integration tests (after E)

**Dependencies:** A must complete first. B, C, D run in parallel after A. E depends on all of A-D. F is fully independent. G comes last.

---

### Task 1: TestAdapter Trait & Types

**Files:**
- Create: `src/test/adapter.rs`

**Step 1: Define the types and trait**

```rust
use std::collections::HashMap;
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLevel {
    Unit,
    Integration,
    E2e,
}

#[derive(Debug, Clone)]
pub struct TestCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSummary {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stuck: Option<u32>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFailure {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rerun: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckTest {
    pub name: String,
    pub elapsed_ms: u64,
    pub diagnosis: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub threads: Vec<ThreadStack>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub suggested_traces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStack {
    pub name: String,
    pub stack: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub summary: TestSummary,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<TestFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stuck: Vec<StuckTest>,
    /// Per-test detail for the details file
    #[serde(skip)]
    pub all_tests: Vec<TestDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestDetail {
    pub name: String,
    pub status: String, // "pass", "fail", "skip", "stuck"
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub language: String,
    pub build_system: String,
    pub test_files: u32,
}

pub trait TestAdapter: Send + Sync {
    /// Scan projectRoot for signals. Returns 0-100 confidence. Highest wins.
    fn detect(&self, project_root: &Path, command: Option<&str>) -> u8;

    /// Human-readable name: "cargo", "catch2", "generic"
    fn name(&self) -> &str;

    /// Build command for running tests at a given level. None = all.
    fn suite_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
        env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand>;

    /// Build command for running a single test by name.
    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand>;

    /// Parse raw stdout + stderr into structured results.
    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> TestResult;

    /// Given a failure, suggest trace patterns for instrumented rerun.
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;

    /// Capture thread stacks for stuck detection. Language-aware.
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack>;

    /// Default hard timeout for a given test level.
    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) | None => 30_000,
            Some(TestLevel::Integration) => 120_000,
            Some(TestLevel::E2e) => 300_000,
        }
    }
}
```

**Checkpoint:** The trait and all types compile. No adapter implementations yet.

---

### Task 2: TestRunner Orchestrator & Module Setup

**Files:**
- Create: `src/test/mod.rs`
- Modify: `src/lib.rs` (add `pub mod test;`)

**Step 1: Create the module file with TestRunner**

`src/test/mod.rs`:

```rust
pub mod adapter;
pub mod cargo_adapter;
pub mod catch2_adapter;
pub mod generic_adapter;
pub mod stuck_detector;
pub mod output;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::Command;
use tokio::io::AsyncReadExt;

use adapter::*;
use cargo_adapter::CargoTestAdapter;
use catch2_adapter::Catch2Adapter;
use generic_adapter::GenericAdapter;
use stuck_detector::StuckDetector;

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
    fn detect_adapter(
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
    ) -> crate::Result<TestRunResult> {
        let adapter = self.detect_adapter(project_root, framework, command);
        let framework_name = adapter.name().to_string();

        // Build command
        let test_cmd = if let Some(test_name) = test {
            adapter.single_test_command(project_root, test_name)?
        } else {
            adapter.suite_command(project_root, level, env)?
        };

        let hard_timeout = timeout.unwrap_or_else(|| adapter.default_timeout(level));

        // Spawn subprocess
        let mut child = Command::new(&test_cmd.program)
            .args(&test_cmd.args)
            .envs(&test_cmd.env)
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
        let detector_handle = tokio::spawn({
            let adapter_name = framework_name.clone();
            async move { detector.run().await }
        });

        // Capture stdout and stderr
        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        let mut child_stdout = child.stdout.take();
        let mut child_stderr = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            let mut buf = String::new();
            if let Some(ref mut stdout) = child_stdout {
                let _ = stdout.read_to_string(&mut buf).await;
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
        tokio::select! {
            status = child.wait() => {
                // Process exited normally
                detector_handle.abort();
                stdout_buf = stdout_task.await.unwrap_or_default();
                stderr_buf = stderr_task.await.unwrap_or_default();

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
            stuck_result = detector_handle => {
                // Stuck detected — capture stacks before killing
                match stuck_result {
                    Ok(Some(stuck_info)) => {
                        // Capture stacks
                        let threads = adapter.capture_stacks(pid);

                        // Kill the process
                        let _ = child.kill().await;
                        stdout_buf = stdout_task.await.unwrap_or_default();
                        stderr_buf = stderr_task.await.unwrap_or_default();

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
                        stdout_buf = stdout_task.await.unwrap_or_default();
                        stderr_buf = stderr_task.await.unwrap_or_default();

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
        watches: Option<&crate::mcp::WatchUpdate>,
        connection_id: &str,
    ) -> crate::Result<TestRunResult> {
        let adapter = self.detect_adapter(project_root, framework, command);
        let framework_name = adapter.name().to_string();

        // Build the test command (typically single test for instrumented rerun)
        let test_cmd = if let Some(test_name) = test {
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
```

**Step 2: Register the module**

In `src/lib.rs`, add:
```rust
pub mod test;
```

**Step 3: Write failing test**

In `src/test/mod.rs`, add a `#[cfg(test)]` module:

```rust
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
```

**Step 4: Run test — verify it fails**

Run: `cargo test --lib test::tests`
Expected: FAIL (adapters not implemented yet)

**Checkpoint:** TestRunner skeleton compiles, adapter detection logic is testable.

---

### Task 3: MCP Types for debug_test

**Files:**
- Modify: `src/mcp/types.rs`

**Step 1: Add DebugTestRequest and DebugTestResponse**

Append to `src/mcp/types.rs`:

```rust
// ============ debug_test ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestRequest {
    pub project_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<crate::test::adapter::TestLevel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_patterns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watches: Option<WatchUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugTestResponse {
    pub framework: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<crate::test::adapter::TestSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<crate::test::adapter::TestFailure>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub stuck: Vec<crate::test::adapter::StuckTest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_tests: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<crate::test::adapter::ProjectInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
```

**Step 2: Write test**

```rust
#[test]
fn test_debug_test_request_serialization() {
    let req = DebugTestRequest {
        project_root: "/Users/alex/strobe".to_string(),
        framework: None,
        level: None,
        test: Some("test_foo".to_string()),
        command: None,
        trace_patterns: None,
        watches: None,
        env: None,
        timeout: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("projectRoot"));
    assert!(json.contains("test_foo"));
    // camelCase check
    assert!(!json.contains("project_root"));
}
```

**Checkpoint:** MCP types compile and serialize correctly with camelCase.

---

### Task 4: CargoTestAdapter

**Files:**
- Create: `src/test/cargo_adapter.rs`

**Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_detect_cargo_project() {
        // strobe root has Cargo.toml
        let adapter = CargoTestAdapter;
        let confidence = adapter.detect(Path::new("."), None);
        assert_eq!(confidence, 90);
    }

    #[test]
    fn test_detect_no_cargo() {
        let adapter = CargoTestAdapter;
        let confidence = adapter.detect(Path::new("/tmp"), None);
        assert_eq!(confidence, 0);
    }

    #[test]
    fn test_parse_cargo_json_all_pass() {
        let adapter = CargoTestAdapter;
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 3 }
{ "type": "test", "event": "started", "name": "tests::test_a" }
{ "type": "test", "event": "ok", "name": "tests::test_a", "exec_time": 0.001 }
{ "type": "test", "event": "started", "name": "tests::test_b" }
{ "type": "test", "event": "ok", "name": "tests::test_b", "exec_time": 0.002 }
{ "type": "test", "event": "started", "name": "tests::test_c" }
{ "type": "test", "event": "ignored", "name": "tests::test_c" }
{ "type": "suite", "event": "ok", "passed": 2, "failed": 0, "ignored": 1, "measured": 0, "filtered_out": 0, "exec_time": 0.003 }
"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_cargo_json_with_failure() {
        let adapter = CargoTestAdapter;
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 2 }
{ "type": "test", "event": "started", "name": "parser::tests::test_empty_input" }
{ "type": "test", "event": "failed", "name": "parser::tests::test_empty_input", "exec_time": 0.5, "stdout": "thread 'parser::tests::test_empty_input' panicked at src/parser.rs:142:5:\nassertion `left == right` failed\n  left: None\n  right: Some(Node { kind: Empty })\n" }
{ "type": "test", "event": "started", "name": "parser::tests::test_ok" }
{ "type": "test", "event": "ok", "name": "parser::tests::test_ok", "exec_time": 0.001 }
{ "type": "suite", "event": "failed", "passed": 1, "failed": 1, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.501 }
"#;
        let result = adapter.parse_output(stdout, "", 101);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "parser::tests::test_empty_input");
        assert_eq!(f.file.as_deref(), Some("src/parser.rs"));
        assert_eq!(f.line, Some(142));
        assert!(f.message.contains("assertion"));
    }

    #[test]
    fn test_suggest_traces_from_module_path() {
        let adapter = CargoTestAdapter;
        let failure = TestFailure {
            name: "parser::tests::test_empty_input".to_string(),
            file: Some("src/parser.rs".to_string()),
            line: Some(142),
            message: "assertion failed".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = adapter.suggest_traces(&failure);
        assert!(traces.contains(&"parser::*".to_string()));
    }

    #[test]
    fn test_suite_command_unit() {
        let adapter = CargoTestAdapter;
        let cmd = adapter.suite_command(
            Path::new("/project"),
            Some(TestLevel::Unit),
            &HashMap::new(),
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(cmd.args.contains(&"--lib".to_string()));
        assert!(cmd.args.contains(&"--format".to_string()));
    }

    #[test]
    fn test_single_test_command() {
        let adapter = CargoTestAdapter;
        let cmd = adapter.single_test_command(
            Path::new("/project"),
            "parser::tests::test_empty_input",
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(cmd.args.contains(&"--exact".to_string()));
        assert!(cmd.args.contains(&"parser::tests::test_empty_input".to_string()));
    }
}
```

**Step 2: Implement CargoTestAdapter**

```rust
use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;

pub struct CargoTestAdapter;

impl TestAdapter for CargoTestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("Cargo.toml").exists() {
            90
        } else {
            0
        }
    }

    fn name(&self) -> &str {
        "cargo"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let mut args = vec!["test".to_string()];

        match level {
            Some(TestLevel::Unit) => args.push("--lib".to_string()),
            Some(TestLevel::Integration) => {
                args.push("--test".to_string());
                args.push("*".to_string());
            }
            Some(TestLevel::E2e) => {
                args.push("--test".to_string());
                args.push("e2e*".to_string());
            }
            None => {}
        }

        args.push("--format".to_string());
        args.push("json".to_string());
        // Ensure unstable format flag is present for cargo test JSON output
        args.push("-Zunstable-options".to_string());

        Ok(TestCommand {
            program: "cargo".to_string(),
            args,
            env: HashMap::new(),
        })
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "cargo".to_string(),
            args: vec![
                "test".to_string(),
                "--format".to_string(),
                "json".to_string(),
                "-Zunstable-options".to_string(),
                "--".to_string(),
                test_name.to_string(),
                "--exact".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        _exit_code: i32,
    ) -> TestResult {
        let mut passed = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut duration_ms = 0u64;
        let mut failures = Vec::new();
        let mut all_tests = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // Skip non-JSON lines (e.g., compiler output)
            };

            let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");

            match (event_type, event) {
                ("test", "ok") => {
                    passed += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    all_tests.push(TestDetail {
                        name,
                        status: "pass".to_string(),
                        duration_ms: (exec_time * 1000.0) as u64,
                        stdout: v.get("stdout").and_then(|s| s.as_str()).map(|s| s.to_string()),
                        stderr: None,
                        message: None,
                    });
                }
                ("test", "failed") => {
                    failed += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let test_stdout = v.get("stdout").and_then(|s| s.as_str()).unwrap_or("");

                    // Extract file:line from panic message
                    let (file, line_num, message) = parse_panic_location(test_stdout);

                    failures.push(TestFailure {
                        name: name.clone(),
                        file: file.clone(),
                        line: line_num,
                        message: message.clone(),
                        rerun: Some(name.clone()),
                        suggested_traces: vec![], // Filled in by TestRunner
                    });

                    all_tests.push(TestDetail {
                        name,
                        status: "fail".to_string(),
                        duration_ms: (exec_time * 1000.0) as u64,
                        stdout: Some(test_stdout.to_string()),
                        stderr: None,
                        message: Some(message),
                    });
                }
                ("test", "ignored") => {
                    skipped += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    all_tests.push(TestDetail {
                        name,
                        status: "skip".to_string(),
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: None,
                    });
                }
                ("suite", "ok") | ("suite", "failed") => {
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    duration_ms = (exec_time * 1000.0) as u64;

                    // Use suite-level counts if our per-test counting missed anything
                    // (e.g., filtered_out tests)
                    if let Some(p) = v.get("passed").and_then(|n| n.as_u64()) {
                        passed = p as u32;
                    }
                    if let Some(f) = v.get("failed").and_then(|n| n.as_u64()) {
                        failed = f as u32;
                    }
                    if let Some(i) = v.get("ignored").and_then(|n| n.as_u64()) {
                        skipped = i as u32;
                    }
                }
                _ => {}
            }
        }

        TestResult {
            summary: TestSummary {
                passed,
                failed,
                skipped,
                stuck: None,
                duration_ms,
            },
            failures,
            stuck: vec![],
            all_tests,
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        // Extract module path from test name: "parser::tests::test_foo" → "parser::*"
        let parts: Vec<&str> = failure.name.split("::").collect();
        if parts.len() >= 2 {
            // Use the top-level module
            traces.push(format!("{}::*", parts[0]));
        }

        // If we have a source file, use @file: pattern
        if let Some(ref file) = failure.file {
            if let Some(filename) = Path::new(file).file_name().and_then(|n| n.to_str()) {
                traces.push(format!("@file:{}", filename));
            }
        }

        traces
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        // Native code → OS-level sampling
        capture_native_stacks(pid)
    }
}

/// Parse panic location from cargo test stdout.
/// Looks for patterns like: "panicked at src/parser.rs:142:5:\n<message>"
fn parse_panic_location(stdout: &str) -> (Option<String>, Option<u32>, String) {
    // Pattern: "panicked at <file>:<line>:<col>:\n"
    for line in stdout.lines() {
        if let Some(idx) = line.find("panicked at ") {
            let after = &line[idx + "panicked at ".len()..];
            // Parse "file:line:col:" or "file:line:"
            let parts: Vec<&str> = after.splitn(4, ':').collect();
            if parts.len() >= 2 {
                let file = parts[0].trim().to_string();
                let line_num = parts[1].trim().parse::<u32>().ok();
                // The message is everything after the location line
                let msg_start = stdout.find(line)
                    .map(|i| i + line.len())
                    .unwrap_or(0);
                let message = stdout[msg_start..].trim().to_string();
                let message = if message.is_empty() {
                    stdout.to_string()
                } else {
                    message
                };
                return (Some(file), line_num, message);
            }
        }
    }
    (None, None, stdout.to_string())
}

/// Capture thread stacks using OS-level tools. Works for native code (Rust, C, C++).
fn capture_native_stacks(pid: u32) -> Vec<ThreadStack> {
    #[cfg(target_os = "macos")]
    {
        capture_stacks_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        capture_stacks_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        vec![]
    }
}

#[cfg(target_os = "macos")]
fn capture_stacks_macos(pid: u32) -> Vec<ThreadStack> {
    // Use macOS `sample` command for thread stack capture
    let output = std::process::Command::new("sample")
        .args([&pid.to_string(), "1"]) // 1 second sample
        .output();

    match output {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            parse_sample_output(&text)
        }
        Err(_) => vec![],
    }
}

#[cfg(target_os = "macos")]
fn parse_sample_output(text: &str) -> Vec<ThreadStack> {
    let mut threads = Vec::new();
    let mut current_thread: Option<String> = None;
    let mut current_stack: Vec<String> = Vec::new();

    for line in text.lines() {
        // Thread headers in sample output look like: "Thread_<n>  DispatchQueue_<n>: <name>"
        // or just "Thread_<n>"
        if line.starts_with("Thread_") || line.starts_with("  Thread_") {
            // Save previous thread
            if let Some(name) = current_thread.take() {
                if !current_stack.is_empty() {
                    threads.push(ThreadStack {
                        name,
                        stack: current_stack.clone(),
                    });
                    current_stack.clear();
                }
            }
            current_thread = Some(line.trim().to_string());
        } else if current_thread.is_some() && line.contains("+") {
            // Stack frame lines typically contain "+" for offset
            let frame = line.trim().to_string();
            if !frame.is_empty() {
                current_stack.push(frame);
            }
        }
    }

    // Don't forget last thread
    if let Some(name) = current_thread {
        if !current_stack.is_empty() {
            threads.push(ThreadStack { name, stack: current_stack });
        }
    }

    threads
}

#[cfg(target_os = "linux")]
fn capture_stacks_linux(pid: u32) -> Vec<ThreadStack> {
    let mut threads = Vec::new();
    let task_dir = format!("/proc/{}/task", pid);

    if let Ok(entries) = std::fs::read_dir(&task_dir) {
        for entry in entries.flatten() {
            let tid = entry.file_name().to_string_lossy().to_string();
            let stack_path = format!("{}/{}/stack", task_dir, tid);
            if let Ok(stack) = std::fs::read_to_string(&stack_path) {
                let frames: Vec<String> = stack.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !frames.is_empty() {
                    threads.push(ThreadStack {
                        name: format!("thread-{}", tid),
                        stack: frames,
                    });
                }
            }
        }
    }

    threads
}
```

**Step 3: Run tests — verify they pass**

Run: `cargo test --lib test::cargo_adapter::tests`
Expected: All 6 tests PASS

**Checkpoint:** CargoTestAdapter detects Cargo projects, parses JSON output, extracts failure locations, and suggests trace patterns.

---

### Task 5: Catch2Adapter

**Files:**
- Create: `src/test/catch2_adapter.rs`
- Modify: `Cargo.toml` (add `quick-xml = "0.37"`)

**Step 1: Add dependency**

In `Cargo.toml`, under `[dependencies]`:
```toml
quick-xml = "0.37"
```

**Step 2: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_catch2_xml_all_pass() {
        let adapter = Catch2Adapter;
        let stdout = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="12345" catch2-version="3.5.0">
  <TestCase name="Addition works" tags="[unit][math]" filename="test_math.cpp" line="10">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <TestCase name="Subtraction works" tags="[unit][math]" filename="test_math.cpp" line="20">
    <OverallResult success="true" durationInSeconds="0.002"/>
  </TestCase>
  <OverallResults successes="4" failures="0" expectedFailures="0"/>
  <OverallResultsCases successes="2" failures="0" expectedFailures="0"/>
</Catch2TestRun>"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_catch2_xml_with_failure() {
        let adapter = Catch2Adapter;
        let stdout = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="12345" catch2-version="3.5.0">
  <TestCase name="Parser handles empty" tags="[unit]" filename="test_parser.cpp" line="15">
    <Expression success="false" type="REQUIRE" filename="test_parser.cpp" line="18">
      <Original>result == expected</Original>
      <Expanded>nullptr == 0x42</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.005"/>
  </TestCase>
  <TestCase name="Parser handles valid" tags="[unit]" filename="test_parser.cpp" line="25">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <OverallResults successes="1" failures="1" expectedFailures="0"/>
  <OverallResultsCases successes="1" failures="1" expectedFailures="0"/>
</Catch2TestRun>"#;
        let result = adapter.parse_output(stdout, "", 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Parser handles empty");
        assert_eq!(f.file.as_deref(), Some("test_parser.cpp"));
        assert_eq!(f.line, Some(18));
        assert!(f.message.contains("nullptr == 0x42"));
    }
}
```

**Step 3: Implement Catch2Adapter**

```rust
use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;
use super::cargo_adapter::capture_native_stacks;

pub struct Catch2Adapter;

impl TestAdapter for Catch2Adapter {
    fn detect(&self, _project_root: &Path, command: Option<&str>) -> u8 {
        // Catch2 detection: if a command is provided, probe it
        if let Some(cmd) = command {
            if Path::new(cmd).exists() {
                // Try running --list-tests to detect Catch2
                let output = std::process::Command::new(cmd)
                    .arg("--list-tests")
                    .output();
                match output {
                    Ok(o) if o.status.success() => return 85,
                    _ => return 0,
                }
            }
        }
        0
    }

    fn name(&self) -> &str {
        "catch2"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        // Catch2 requires a binary path — error if not available.
        // The binary is passed via `command` param, which is stored in the request.
        // This will be called with the command already set as the program.
        Err(crate::Error::Frida(
            "Catch2 adapter requires a test binary path via the 'command' parameter".to_string()
        ))
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::Frida(
            "Catch2 adapter requires a test binary path via the 'command' parameter".to_string()
        ))
    }

    fn parse_output(
        &self,
        stdout: &str,
        _stderr: &str,
        _exit_code: i32,
    ) -> TestResult {
        parse_catch2_xml(stdout)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        // Use the source file for @file: pattern
        if let Some(ref file) = failure.file {
            if let Some(filename) = Path::new(file).file_name().and_then(|n| n.to_str()) {
                traces.push(format!("@file:{}", filename));
            }
        }

        traces
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        capture_native_stacks(pid)
    }
}

impl Catch2Adapter {
    /// Build command for a given binary with Catch2 flags.
    pub fn command_for_binary(
        binary: &str,
        level: Option<TestLevel>,
    ) -> TestCommand {
        let mut args = vec!["--reporter".to_string(), "xml".to_string()];

        match level {
            Some(TestLevel::Unit) => args.push("[unit]".to_string()),
            Some(TestLevel::Integration) => args.push("[integration]".to_string()),
            Some(TestLevel::E2e) => args.push("[e2e]".to_string()),
            None => {}
        }

        TestCommand {
            program: binary.to_string(),
            args,
            env: HashMap::new(),
        }
    }

    /// Build command for running a single test in a Catch2 binary.
    pub fn single_test_for_binary(binary: &str, test_name: &str) -> TestCommand {
        TestCommand {
            program: binary.to_string(),
            args: vec![
                "--reporter".to_string(),
                "xml".to_string(),
                test_name.to_string(),
            ],
            env: HashMap::new(),
        }
    }
}

/// Parse Catch2 XML reporter output into TestResult.
fn parse_catch2_xml(xml: &str) -> TestResult {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // State for current TestCase
    let mut in_test_case = false;
    let mut tc_name = String::new();
    let mut tc_file = String::new();
    let mut tc_line = 0u32;
    let mut tc_success = true;
    let mut tc_duration_ms = 0u64;

    // State for current Expression (assertion failure)
    let mut in_expression = false;
    let mut expr_file = String::new();
    let mut expr_line = 0u32;
    let mut expr_original = String::new();
    let mut expr_expanded = String::new();
    let mut reading_original = false;
    let mut reading_expanded = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let local_name = e.local_name();
                match local_name.as_ref() {
                    b"TestCase" => {
                        in_test_case = true;
                        tc_success = true;
                        tc_name = get_attr(&e, "name");
                        tc_file = get_attr(&e, "filename");
                        tc_line = get_attr(&e, "line").parse().unwrap_or(0);
                        tc_duration_ms = 0;
                    }
                    b"Expression" => {
                        in_expression = true;
                        let success = get_attr(&e, "success");
                        if success == "false" {
                            tc_success = false;
                            expr_file = get_attr(&e, "filename");
                            expr_line = get_attr(&e, "line").parse().unwrap_or(0);
                        }
                        expr_original.clear();
                        expr_expanded.clear();
                    }
                    b"Original" => {
                        reading_original = true;
                    }
                    b"Expanded" => {
                        reading_expanded = true;
                    }
                    b"OverallResult" if in_test_case => {
                        let secs = get_attr(&e, "durationInSeconds");
                        tc_duration_ms = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                        let success = get_attr(&e, "success");
                        if success == "false" {
                            tc_success = false;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                match e.local_name().as_ref() {
                    b"TestCase" => {
                        if tc_success {
                            passed += 1;
                            all_tests.push(TestDetail {
                                name: tc_name.clone(),
                                status: "pass".to_string(),
                                duration_ms: tc_duration_ms,
                                stdout: None,
                                stderr: None,
                                message: None,
                            });
                        } else {
                            failed += 1;
                            let message = if !expr_expanded.is_empty() {
                                format!("REQUIRE( {} )\nwith expansion:\n  {}", expr_original, expr_expanded)
                            } else {
                                "Test failed".to_string()
                            };

                            failures.push(TestFailure {
                                name: tc_name.clone(),
                                file: if !expr_file.is_empty() { Some(expr_file.clone()) } else if !tc_file.is_empty() { Some(tc_file.clone()) } else { None },
                                line: if expr_line > 0 { Some(expr_line) } else if tc_line > 0 { Some(tc_line) } else { None },
                                message: message.clone(),
                                rerun: Some(tc_name.clone()),
                                suggested_traces: vec![],
                            });

                            all_tests.push(TestDetail {
                                name: tc_name.clone(),
                                status: "fail".to_string(),
                                duration_ms: tc_duration_ms,
                                stdout: None,
                                stderr: None,
                                message: Some(message),
                            });
                        }
                        in_test_case = false;
                    }
                    b"Expression" => {
                        in_expression = false;
                    }
                    b"Original" => {
                        reading_original = false;
                    }
                    b"Expanded" => {
                        reading_expanded = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if reading_original {
                    expr_original = e.unescape().unwrap_or_default().to_string();
                } else if reading_expanded {
                    expr_expanded = e.unescape().unwrap_or_default().to_string();
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let total_duration: u64 = all_tests.iter().map(|t| t.duration_ms).sum();

    TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped: 0,
            stuck: None,
            duration_ms: total_duration,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract an attribute value from an XML element.
fn get_attr(e: &quick_xml::events::BytesStart, name: &str) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name.as_bytes())
        .and_then(|a| String::from_utf8(a.value.to_vec()).ok())
        .unwrap_or_default()
}
```

**Step 3: Run tests**

Run: `cargo test --lib test::catch2_adapter::tests`
Expected: PASS

**Checkpoint:** Catch2Adapter parses XML output, extracts failures with file:line and expression details.

---

### Task 6: GenericAdapter

**Files:**
- Create: `src/test/generic_adapter.rs`

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_always_detects() {
        let adapter = GenericAdapter;
        assert_eq!(adapter.detect(Path::new("/anything"), None), 1);
    }

    #[test]
    fn test_parse_generic_pass() {
        let adapter = GenericAdapter;
        let result = adapter.parse_output("All tests passed\n", "", 0);
        assert_eq!(result.summary.passed, 0); // Can't determine from raw output
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn test_parse_generic_failure_detection() {
        let adapter = GenericAdapter;
        let stderr = "FAIL: test_something at tests/test.py:42\nAssertionError: expected 1 got 2\n";
        let result = adapter.parse_output("", stderr, 1);
        assert_eq!(result.summary.failed, 1);
        assert!(!result.failures.is_empty());
    }
}
```

**Step 2: Implement**

```rust
use std::collections::HashMap;
use std::path::Path;
use super::adapter::*;
use super::cargo_adapter::capture_native_stacks;

pub struct GenericAdapter;

impl TestAdapter for GenericAdapter {
    fn detect(&self, _project_root: &Path, _command: Option<&str>) -> u8 {
        1 // Always matches as fallback
    }

    fn name(&self) -> &str {
        "generic"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::Frida(
            "Generic adapter requires a test command. Use the 'command' parameter.".to_string()
        ))
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        _test_name: &str,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::Frida(
            "Generic adapter does not support single test reruns.".to_string()
        ))
    }

    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> TestResult {
        let combined = format!("{}\n{}", stdout, stderr);
        let mut failures = Vec::new();

        // Heuristic: look for FAIL patterns with file:line
        let fail_re = regex::Regex::new(
            r"(?i)(?:FAIL|FAILED|ERROR|FAILURE)[:\s]+(.+?)(?:\s+at\s+)?(\S+?):(\d+)"
        ).ok();

        if let Some(re) = &fail_re {
            for cap in re.captures_iter(&combined) {
                failures.push(TestFailure {
                    name: cap.get(1).map(|m| m.as_str().trim().to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    file: cap.get(2).map(|m| m.as_str().to_string()),
                    line: cap.get(3).and_then(|m| m.as_str().parse().ok()),
                    message: cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
                    rerun: None, // Generic can't determine rerun command
                    suggested_traces: vec![],
                });
            }
        }

        // If no regex failures found but exit code is non-zero, add generic failure
        if failures.is_empty() && exit_code != 0 {
            failures.push(TestFailure {
                name: "unknown".to_string(),
                file: None,
                line: None,
                message: format!("Process exited with code {}", exit_code),
                rerun: None,
                suggested_traces: vec![],
            });
        }

        let failed = failures.len() as u32;

        TestResult {
            summary: TestSummary {
                passed: 0, // Can't determine from raw output
                failed,
                skipped: 0,
                stuck: None,
                duration_ms: 0,
            },
            failures,
            stuck: vec![],
            all_tests: vec![],
        }
    }

    fn suggest_traces(&self, _failure: &TestFailure) -> Vec<String> {
        vec![] // No trace suggestions for generic adapter
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        capture_native_stacks(pid)
    }
}
```

Note: `GenericAdapter` uses `regex` for heuristic parsing. `regex` is already in `[dev-dependencies]`; move it to `[dependencies]`:

```toml
regex = "1"
```

**Checkpoint:** GenericAdapter provides a fallback with best-effort output parsing.

---

### Task 7: StuckDetector

**Files:**
- Create: `src/test/stuck_detector.rs`

**Step 1: Write tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stuck_detector_returns_none_for_fast_exit() {
        // Launch a command that exits immediately
        let child = tokio::process::Command::new("true")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let detector = StuckDetector::new(pid, 5000);
        // Process exits before stuck detection triggers
        let result = detector.run().await;
        // Should return None (process exited before stuck detection)
        assert!(result.is_none());
    }

    #[test]
    fn test_cpu_sample_parsing() {
        // Ensure we can get CPU time for current process
        let pid = std::process::id();
        let time = get_process_cpu_ns(pid);
        assert!(time > 0, "Should get non-zero CPU time for current process");
    }
}
```

**Step 2: Implement StuckDetector**

```rust
use std::time::{Duration, Instant};

/// Result of stuck detection analysis.
pub struct StuckInfo {
    pub elapsed_ms: u64,
    pub diagnosis: String,
    pub suggested_traces: Vec<String>,
}

/// Multi-signal stuck detector.
/// Runs in parallel with test subprocess, monitors:
/// 1. CPU time delta (every 2s)
/// 2. Stack sampling (triggered when suspicious)
pub struct StuckDetector {
    pid: u32,
    hard_timeout_ms: u64,
}

impl StuckDetector {
    pub fn new(pid: u32, hard_timeout_ms: u64) -> Self {
        Self { pid, hard_timeout_ms }
    }

    /// Run the detection loop. Returns Some(StuckInfo) if stuck, None if process exits first.
    pub async fn run(self) -> Option<StuckInfo> {
        let start = Instant::now();
        let mut prev_cpu_ns: Option<u64> = None;
        let mut suspicious_since: Option<Instant> = None;
        let mut zero_delta_count = 0u32;
        let mut constant_high_count = 0u32;

        loop {
            // Check if process is still alive
            let alive = unsafe { libc::kill(self.pid as i32, 0) } == 0;
            if !alive {
                return None; // Process exited — not stuck
            }

            // Check hard timeout
            let elapsed = start.elapsed();
            if elapsed.as_millis() as u64 >= self.hard_timeout_ms {
                return Some(StuckInfo {
                    elapsed_ms: elapsed.as_millis() as u64,
                    diagnosis: "Hard timeout reached".to_string(),
                    suggested_traces: vec![],
                });
            }

            // CPU time sampling
            let cpu_ns = get_process_cpu_ns(self.pid);

            if let Some(prev) = prev_cpu_ns {
                let delta = cpu_ns.saturating_sub(prev);
                let sample_interval_ns = 2_000_000_000u64; // 2 seconds

                if delta == 0 {
                    // CPU idle — potential deadlock
                    zero_delta_count += 1;
                    constant_high_count = 0;

                    if suspicious_since.is_none() {
                        suspicious_since = Some(Instant::now());
                    }
                } else if delta > sample_interval_ns * 80 / 100 {
                    // CPU near 100% — potential infinite loop
                    constant_high_count += 1;
                    zero_delta_count = 0;

                    if suspicious_since.is_none() {
                        suspicious_since = Some(Instant::now());
                    }
                } else {
                    // Normal activity — reset
                    zero_delta_count = 0;
                    constant_high_count = 0;
                    suspicious_since = None;
                }

                // Trigger stack sampling after ~6s of suspicious signals (3 samples)
                if let Some(since) = suspicious_since {
                    if since.elapsed() > Duration::from_secs(6) {
                        let diagnosis = if zero_delta_count >= 3 {
                            "Deadlock: 0% CPU, process completely blocked".to_string()
                        } else if constant_high_count >= 3 {
                            "Infinite loop: 100% CPU, no output progress".to_string()
                        } else {
                            "Process appears stuck".to_string()
                        };

                        return Some(StuckInfo {
                            elapsed_ms: start.elapsed().as_millis() as u64,
                            diagnosis,
                            suggested_traces: vec![],
                        });
                    }
                }
            }

            prev_cpu_ns = Some(cpu_ns);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Get cumulative CPU time (user + system) for a process in nanoseconds.
pub fn get_process_cpu_ns(pid: u32) -> u64 {
    #[cfg(target_os = "macos")]
    {
        get_cpu_ns_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        get_cpu_ns_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

#[cfg(target_os = "macos")]
fn get_cpu_ns_macos(pid: u32) -> u64 {
    use std::mem;

    // Use proc_pidinfo with PROC_PIDTASKINFO
    const PROC_PIDTASKINFO: i32 = 4;

    #[repr(C)]
    struct ProcTaskInfo {
        pti_virtual_size: u64,
        pti_resident_size: u64,
        pti_total_user: u64,   // nanoseconds
        pti_total_system: u64, // nanoseconds
        pti_threads_user: u64,
        pti_threads_system: u64,
        pti_policy: i32,
        pti_faults: i32,
        pti_pageins: i32,
        pti_cow_faults: i32,
        pti_messages_sent: i32,
        pti_messages_received: i32,
        pti_syscalls_mach: i32,
        pti_syscalls_unix: i32,
        pti_csw: i32,
        pti_threadnum: i32,
        pti_numrunning: i32,
        pti_priority: i32,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
    }

    unsafe {
        let mut info: ProcTaskInfo = mem::zeroed();
        let size = mem::size_of::<ProcTaskInfo>() as i32;
        let ret = proc_pidinfo(
            pid as i32,
            PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if ret > 0 {
            info.pti_total_user + info.pti_total_system
        } else {
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn get_cpu_ns_linux(pid: u32) -> u64 {
    // Read /proc/<pid>/stat, fields 14 (utime) and 15 (stime) in clock ticks
    let stat_path = format!("/proc/{}/stat", pid);
    if let Ok(content) = std::fs::read_to_string(&stat_path) {
        let fields: Vec<&str> = content.split_whitespace().collect();
        if fields.len() > 14 {
            let utime: u64 = fields[13].parse().unwrap_or(0);
            let stime: u64 = fields[14].parse().unwrap_or(0);
            let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            if ticks_per_sec > 0 {
                return (utime + stime) * 1_000_000_000 / ticks_per_sec;
            }
        }
    }
    0
}
```

**Step 3: Run tests**

Run: `cargo test --lib test::stuck_detector::tests`
Expected: PASS

**Checkpoint:** StuckDetector can monitor CPU time and detect deadlocks/infinite loops.

---

### Task 8: Details File Writer

**Files:**
- Create: `src/test/output.rs`

**Step 1: Write test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::adapter::*;

    #[test]
    fn test_write_details_file() {
        let result = TestResult {
            summary: TestSummary {
                passed: 1, failed: 0, skipped: 0, stuck: None, duration_ms: 100,
            },
            failures: vec![],
            stuck: vec![],
            all_tests: vec![TestDetail {
                name: "test_foo".to_string(),
                status: "pass".to_string(),
                duration_ms: 100,
                stdout: None,
                stderr: None,
                message: None,
            }],
        };

        let path = write_details("cargo", &result, "", "").unwrap();
        assert!(std::path::Path::new(&path).exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test_foo"));

        // Cleanup
        let _ = std::fs::remove_file(&path);
    }
}
```

**Step 2: Implement**

```rust
use std::path::PathBuf;
use crate::test::adapter::TestResult;

/// Write full test details to a temp file. Returns the file path.
pub fn write_details(
    framework: &str,
    result: &TestResult,
    raw_stdout: &str,
    raw_stderr: &str,
) -> crate::Result<String> {
    let dir = PathBuf::from("/tmp/strobe/tests");
    std::fs::create_dir_all(&dir)?;

    let session_id = uuid::Uuid::new_v4().to_string().split('-').next().unwrap_or("unknown").to_string();
    let date = chrono::Utc::now().format("%Y-%m-%d");
    let filename = format!("{}-{}.json", session_id, date);
    let path = dir.join(&filename);

    let details = serde_json::json!({
        "framework": framework,
        "summary": result.summary,
        "tests": result.all_tests,
        "failures": result.failures,
        "stuck": result.stuck,
        "rawStdout": raw_stdout,
        "rawStderr": raw_stderr,
    });

    std::fs::write(&path, serde_json::to_string_pretty(&details)?)?;

    Ok(path.to_string_lossy().to_string())
}
```

**Checkpoint:** Details files are written to `/tmp/strobe/tests/` with full test output.

---

### Task 9: Wire debug_test into Daemon

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Add debug_test to tool list**

In `handle_tools_list`, add a new `McpTool` entry after `debug_delete_session`:

```rust
McpTool {
    name: "debug_test".to_string(),
    description: "Run tests and get structured results. Auto-detects the test framework. If no tests exist, returns project info and suggests test setup. Use this instead of running test commands via bash — it provides machine-readable output, stuck detection, and failure analysis.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "projectRoot": { "type": "string", "description": "Project root for adapter detection" },
            "framework": { "type": "string", "description": "Override auto-detection: \"cargo\", \"catch2\"" },
            "level": { "type": "string", "enum": ["unit", "integration", "e2e"], "description": "Filter: unit, integration, e2e. Omit for all." },
            "test": { "type": "string", "description": "Run a single test by name" },
            "command": { "type": "string", "description": "Test binary path (required for compiled test frameworks like Catch2)" },
            "tracePatterns": { "type": "array", "items": { "type": "string" }, "description": "Presence triggers Frida instrumented path" },
            "watches": {
                "type": "object",
                "description": "Watch variables during test (triggers Frida path)",
                "properties": {
                    "add": { "type": "array", "items": { "type": "object" } },
                    "remove": { "type": "array", "items": { "type": "string" } }
                }
            },
            "env": { "type": "object", "description": "Additional environment variables" },
            "timeout": { "type": "integer", "description": "Hard timeout in ms (default varies by level)" }
        },
        "required": ["projectRoot"]
    }),
},
```

**Step 2: Add to handle_tools_call dispatch**

In the `match call.name.as_str()` block:

```rust
"debug_test" => self.tool_debug_test(&call.arguments, connection_id).await,
```

**Step 3: Implement tool_debug_test**

```rust
async fn tool_debug_test(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
    let req: crate::mcp::DebugTestRequest = serde_json::from_value(args.clone())?;
    let project_root = std::path::Path::new(&req.project_root);

    let runner = crate::test::TestRunner::new();
    let env = req.env.unwrap_or_default();

    let level = req.level;

    // Determine execution path: Frida or direct
    let has_instrumentation = req.trace_patterns.is_some() || req.watches.is_some();

    let run_result = if has_instrumentation {
        // Frida path
        let trace_patterns = req.trace_patterns.unwrap_or_default();
        runner.run_instrumented(
            project_root,
            req.framework.as_deref(),
            req.test.as_deref(),
            req.command.as_deref(),
            &env,
            req.timeout,
            &self.session_manager,
            &trace_patterns,
            req.watches.as_ref(),
            connection_id,
        ).await?
    } else {
        // Fast path — direct subprocess
        runner.run(
            project_root,
            req.framework.as_deref(),
            level,
            req.test.as_deref(),
            req.command.as_deref(),
            &env,
            req.timeout,
        ).await?
    };

    // Write details file
    let details_path = crate::test::output::write_details(
        &run_result.framework,
        &run_result.result,
        &run_result.raw_stdout,
        &run_result.raw_stderr,
    ).ok();

    // Build response
    let response = crate::mcp::DebugTestResponse {
        framework: run_result.framework,
        summary: Some(run_result.result.summary),
        failures: run_result.result.failures,
        stuck: run_result.result.stuck,
        session_id: run_result.session_id,
        details: details_path,
        no_tests: None,
        project: None,
        hint: None,
    };

    Ok(serde_json::to_value(response)?)
}
```

**Step 4: Handle the Catch2 `command` parameter override**

In the `TestRunner`, extend `run()` and `run_instrumented()` to handle the case where `command` is provided (Catch2 binary). Add a check early in `run()`:

In `src/test/mod.rs`, modify `run()` to check for the `command` parameter and build the right command:

```rust
// If a command is provided, use it as the binary path for the adapter
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
```

**Checkpoint:** `debug_test` is callable via MCP. Cargo projects auto-detected. Results include structured failures, details file path, and optional sessionId for Frida path.

---

### Task 10: strobe install Command

**Files:**
- Create: `src/install.rs`
- Modify: `src/lib.rs` (add `pub mod install;`)
- Modify: `src/main.rs` (add "install" subcommand)

**Step 1: Implement agent detection and MCP config installation**

`src/install.rs`:

```rust
use std::path::{Path, PathBuf};
use crate::Result;

#[derive(Debug)]
enum AgentSystem {
    ClaudeCode { config_dir: PathBuf },
}

/// Detect which coding agent system is installed.
fn detect_agent() -> Option<AgentSystem> {
    let home = dirs::home_dir()?;

    // Claude Code
    let claude_dir = home.join(".claude");
    if claude_dir.exists() {
        return Some(AgentSystem::ClaudeCode { config_dir: claude_dir });
    }

    None
}

/// Get the path to the strobe binary.
fn strobe_binary_path() -> Result<String> {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| crate::Error::Io(e))
}

/// Install Strobe MCP config + TDD skill for the detected agent.
pub fn install() -> Result<()> {
    let agent = detect_agent();

    match agent {
        Some(AgentSystem::ClaudeCode { config_dir }) => {
            install_claude_code(&config_dir)?;
            println!("Strobe installed for Claude Code.");
        }
        None => {
            println!("No supported coding agent detected.");
            println!("Supported: Claude Code (~/.claude/)");
            println!("\nManual setup: add strobe to your MCP config with:");
            println!("  command: \"strobe\"");
            println!("  args: [\"mcp\"]");
        }
    }

    Ok(())
}

fn install_claude_code(config_dir: &Path) -> Result<()> {
    let binary = strobe_binary_path()?;

    // Write/update MCP config
    let mcp_path = config_dir.join("mcp.json");
    let mut config: serde_json::Value = if mcp_path.exists() {
        let content = std::fs::read_to_string(&mcp_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let servers = config.as_object_mut()
        .and_then(|o| o.entry("mcpServers").or_insert(serde_json::json!({})).as_object_mut());

    if let Some(servers) = servers {
        servers.insert("strobe".to_string(), serde_json::json!({
            "command": binary,
            "args": ["mcp"]
        }));
    }

    std::fs::write(&mcp_path, serde_json::to_string_pretty(&config)?)?;

    // Install TDD skill
    let skills_dir = config_dir.join("skills").join("strobe-tdd");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("strobe-tdd.md"),
        include_str!("../skills/strobe-tdd.md"),
    )?;

    Ok(())
}
```

**Step 2: Add install subcommand to main.rs**

In `src/main.rs`, add to the match:

```rust
Some("install") => {
    strobe::install::install()
}
```

**Step 3: Register module**

In `src/lib.rs`, add:
```rust
pub mod install;
```

**Checkpoint:** `strobe install` detects Claude Code and writes MCP config + TDD skill.

---

### Task 11: TDD Skill Markdown

**Files:**
- Create: `skills/strobe-tdd.md`

```markdown
---
name: strobe-tdd
description: Guide TDD workflow using Strobe's debug_test tool
---

# Strobe TDD Workflow

When a user reports a bug and wants to fix it:

1. **Reproduce first**: Create a minimal test case that demonstrates the bug
2. **Confirm failure**: Run `debug_test({ projectRoot: "..." })` to verify the test fails
3. **Fix the bug**: Make the minimal change to fix the issue
4. **Confirm fix**: Run `debug_test` again to verify the test passes
5. **Check for regressions**: Run the full test suite with `debug_test` (no test filter)

When `debug_test` returns failures with `suggested_traces`, offer to rerun with instrumentation:
- Call `debug_test({ test: "<failed_test>", tracePatterns: [...suggested_traces] })`
- Use the returned `sessionId` with `debug_query` to inspect runtime behavior

When `debug_test` returns `no_tests: true`:
- The project has no test infrastructure
- Guide the user to create their first test
- For Rust: suggest adding a `#[test]` function
- For C++: suggest setting up Catch2
- Run `debug_test` to confirm it works

Always prefer `debug_test` over running test commands via bash — it provides:
- Structured failure information (file, line, message)
- Stuck test detection (deadlocks, infinite loops)
- Suggested trace patterns for deeper investigation
- Optional Frida instrumentation for runtime inspection
```

**Checkpoint:** TDD skill file exists for `strobe install` to deploy.

---

### Task 12: Integration Tests

**Files:**
- Create: `tests/phase1d_test.rs`

**Step 1: Test cargo adapter on strobe itself**

```rust
use std::collections::HashMap;
use std::path::Path;

/// Test that CargoTestAdapter correctly detects strobe as a Cargo project
#[test]
fn test_cargo_detection_on_strobe() {
    let runner = strobe::test::TestRunner::new();
    // We can't call detect_adapter directly (private), but we can verify
    // the adapter types work
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;
    use strobe::test::adapter::TestAdapter;
    assert_eq!(adapter.detect(Path::new("."), None), 90);
}

/// Test CargoTestAdapter command generation
#[test]
fn test_cargo_suite_commands() {
    use strobe::test::adapter::{TestAdapter, TestLevel};
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;

    let unit_cmd = adapter.suite_command(Path::new("."), Some(TestLevel::Unit), &HashMap::new()).unwrap();
    assert!(unit_cmd.args.contains(&"--lib".to_string()));

    let int_cmd = adapter.suite_command(Path::new("."), Some(TestLevel::Integration), &HashMap::new()).unwrap();
    assert!(int_cmd.args.iter().any(|a| a == "--test"));

    let all_cmd = adapter.suite_command(Path::new("."), None, &HashMap::new()).unwrap();
    assert!(!all_cmd.args.contains(&"--lib".to_string()));
}

/// Test that parsing real cargo test output works
/// (Run cargo test --format json on a subset and parse the output)
#[tokio::test]
async fn test_cargo_parse_real_output() {
    use strobe::test::adapter::TestAdapter;
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;

    // Run a real cargo test with JSON output on a known-passing test
    let output = tokio::process::Command::new("cargo")
        .args(["test", "--lib", "--format", "json", "-Zunstable-options", "--", "test::cargo_adapter::tests::test_detect_cargo_project", "--exact"])
        .current_dir(".")
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let result = adapter.parse_output(&stdout, &stderr, output.status.code().unwrap_or(-1));
    assert!(result.summary.passed >= 1, "Expected at least 1 pass, got: {:?}", result.summary);
    assert_eq!(result.summary.failed, 0, "Expected 0 failures");
}

/// Test MCP type serialization
#[test]
fn test_debug_test_request_camelcase() {
    let req = strobe::mcp::DebugTestRequest {
        project_root: "/test".to_string(),
        framework: None,
        level: None,
        test: None,
        command: None,
        trace_patterns: Some(vec!["foo::*".to_string()]),
        watches: None,
        env: None,
        timeout: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("projectRoot"));
    assert!(json.contains("tracePatterns"));
    assert!(!json.contains("project_root"));
    assert!(!json.contains("trace_patterns"));
}

/// Test details file writing
#[test]
fn test_details_file_roundtrip() {
    use strobe::test::adapter::*;
    let result = TestResult {
        summary: TestSummary {
            passed: 5, failed: 1, skipped: 0, stuck: None, duration_ms: 250,
        },
        failures: vec![TestFailure {
            name: "test_foo".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(42),
            message: "assertion failed".to_string(),
            rerun: Some("test_foo".to_string()),
            suggested_traces: vec!["foo::*".to_string()],
        }],
        stuck: vec![],
        all_tests: vec![],
    };

    let path = strobe::test::output::write_details("cargo", &result, "stdout", "stderr").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["framework"], "cargo");
    assert_eq!(parsed["summary"]["passed"], 5);
    assert_eq!(parsed["failures"][0]["name"], "test_foo");
    assert_eq!(parsed["rawStdout"], "stdout");

    let _ = std::fs::remove_file(&path);
}

/// Test StuckDetector CPU sampling
#[test]
fn test_cpu_sampling_current_process() {
    let pid = std::process::id();
    let cpu = strobe::test::stuck_detector::get_process_cpu_ns(pid);
    assert!(cpu > 0, "Current process should have non-zero CPU time");
}

/// Test Catch2 XML parsing with realistic output
#[test]
fn test_catch2_parse_realistic_xml() {
    use strobe::test::adapter::TestAdapter;
    let adapter = strobe::test::catch2_adapter::Catch2Adapter;

    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="erae_tests" rng-seed="1234" catch2-version="3.5.0">
  <TestCase name="MIDI note on" tags="[midi][unit]" filename="test_midi.cpp" line="15">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <TestCase name="Audio buffer size" tags="[audio][unit]" filename="test_audio.cpp" line="30">
    <Expression success="false" type="REQUIRE" filename="test_audio.cpp" line="35">
      <Original>buffer.size() == 512</Original>
      <Expanded>256 == 512</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.002"/>
  </TestCase>
  <TestCase name="Engine init" tags="[engine][integration]" filename="test_engine.cpp" line="50">
    <OverallResult success="true" durationInSeconds="0.010"/>
  </TestCase>
  <OverallResults successes="3" failures="1" expectedFailures="0"/>
  <OverallResultsCases successes="2" failures="1" expectedFailures="0"/>
</Catch2TestRun>"#;

    let result = adapter.parse_output(xml, "", 1);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].name, "Audio buffer size");
    assert_eq!(result.failures[0].file.as_deref(), Some("test_audio.cpp"));
    assert_eq!(result.failures[0].line, Some(35));
    assert!(result.failures[0].message.contains("256 == 512"));
}
```

**Step 2: Run all tests**

Run: `cargo test --test phase1d_test`
Expected: All tests PASS

Run: `cargo test` (full suite)
Expected: All existing tests still pass + new tests pass

**Checkpoint:** Phase 1d is fully integrated and validated.

---

## New Dependencies

Add to `Cargo.toml` under `[dependencies]`:
```toml
quick-xml = "0.37"
regex = "1"  # Move from [dev-dependencies] to [dependencies]
```

Remove `regex = "1"` from `[dev-dependencies]` (now in main deps).

---

## Verification Scenarios

After all tasks are complete, validate these scenarios manually:

**A. Cargo fast path**: Build strobe, run `debug_test({ projectRoot: "." })` via MCP — should get structured JSON with pass/fail counts for strobe's own tests.

**B. Single test rerun**: Take a test name from scenario A's results, call `debug_test({ projectRoot: ".", test: "<name>" })` — should run just that test.

**C. Stuck detection**: Write a test that deliberately deadlocks (two mutexes), run via `debug_test` — should detect stuck within ~8s and return thread stacks.

**D. Frida path**: Call `debug_test({ projectRoot: ".", test: "<name>", tracePatterns: ["test::*"] })` — should return a sessionId that works with `debug_query`.

**E. strobe install**: Run `strobe install` — should write to `~/.claude/mcp.json` and `~/.claude/skills/strobe-tdd/`.
