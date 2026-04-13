pub mod adapter;
pub mod bun_adapter;
pub mod cargo_adapter;
pub mod catch2_adapter;
pub mod deno_adapter;
pub mod go_adapter;
pub mod gtest_adapter;
pub mod jest_adapter;
pub mod mocha_adapter;
pub mod output;
pub mod playwright_adapter;
pub mod pytest_adapter;
pub mod stacks;
pub mod stuck_detector;
pub mod unittest_adapter;
pub mod vitest_adapter;

use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use adapter::*;
use bun_adapter::BunAdapter;
use cargo_adapter::CargoTestAdapter;
use catch2_adapter::Catch2Adapter;
use deno_adapter::DenoAdapter;
use go_adapter::GoTestAdapter;
use gtest_adapter::GTestAdapter;
use jest_adapter::JestAdapter;
use mocha_adapter::MochaAdapter;
use playwright_adapter::PlaywrightAdapter;
use pytest_adapter::PytestAdapter;
use stuck_detector::StuckDetector;
use unittest_adapter::UnittestAdapter;
use vitest_adapter::VitestAdapter;

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
    /// Whether the custom STROBE_TEST reporter has been detected.
    /// When true, disables fallback JSON chunk counting to avoid double-counting.
    pub has_custom_reporter: bool,
    /// Last compilation message (e.g., "Compiling strobe v0.1.0"), for progress display.
    pub compile_message: Option<String>,
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
            has_custom_reporter: false,
            compile_message: None,
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    /// Record a test starting (for stuck detection).
    pub fn start_test(&mut self, name: String) {
        self.running_tests.insert(name, Instant::now());
    }

    /// Record a test finishing (for stuck detection).
    pub fn finish_test(&mut self, name: &str) {
        if let Some(started) = self.running_tests.remove(name) {
            let dur_ms = started.elapsed().as_millis() as u64;
            self.test_durations.insert(name.to_string(), dur_ms);
        }
    }

    /// Get the "current test" — the one that's been running longest (for stuck detection).
    pub fn current_test(&self) -> Option<String> {
        self.running_tests
            .iter()
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
    Running { progress: Arc<Mutex<TestProgress>> },
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
    /// Connection that owns this test run (for per-connection isolation).
    pub connection_id: String,
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
                Box::new(VitestAdapter),
                Box::new(JestAdapter),
                Box::new(BunAdapter),
                Box::new(DenoAdapter),
                Box::new(GoTestAdapter),
                Box::new(GTestAdapter),
                Box::new(MochaAdapter),
                Box::new(PlaywrightAdapter),
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
                format!("Unknown framework '{}'. Supported: 'cargo', 'catch2', 'pytest', 'unittest', 'vitest', 'jest', 'bun', 'deno', 'go', 'mocha', 'gtest', 'playwright'", name)
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
                 - Cargo (Rust): provide projectRoot with Cargo.toml\n\
                 - Catch2 (C++): provide command with path to test binary\n\
                 - pytest/unittest (Python): provide projectRoot with pyproject.toml or test files\n\
                 - Vitest/Jest (Node.js): provide projectRoot with package.json\n\
                 - Bun: provide projectRoot with bunfig.toml\n\
                 - Deno: provide projectRoot with deno.json\n\
                 - Go: provide projectRoot with go.mod\n\
                 - Mocha: provide projectRoot with .mocharc.* or mocha in package.json\n\
                 - Google Test (C++): provide command with path to gtest binary".to_string()
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

        // Build command — dispatch through trait methods for binary-based adapters
        let test_cmd = if let Some(cmd) = command {
            if let Some(test_name) = test {
                adapter.single_test_for_binary(cmd, test_name)?
            } else {
                adapter.command_for_binary(cmd, level)?
            }
        } else if let Some(test_name) = test {
            adapter.single_test_command(project_root, test_name)?
        } else {
            adapter.suite_command(project_root, level, env)?
        };

        // Timeout priority: explicit param > settings.json > adapter default
        let settings = crate::config::resolve(Some(project_root));
        let hard_timeout = timeout
            .or(settings.test_timeout_ms)
            .unwrap_or_else(|| adapter.default_timeout(level));

        // Run pretest script if one exists (e.g. pretest:e2e for DB setup).
        // Executed outside Frida — these are setup commands, not test code.
        if let Some(pretest) = adapter.pretest_command(project_root, level) {
            let pretest_cwd = pretest
                .cwd
                .as_deref()
                .unwrap_or(project_root.to_str().unwrap_or("."));
            tracing::info!(
                cmd = %pretest.program,
                args = ?pretest.args,
                cwd = %pretest_cwd,
                "Running pretest script"
            );
            let status = std::process::Command::new(&pretest.program)
                .args(&pretest.args)
                .current_dir(pretest_cwd)
                .envs(&pretest.env)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .status();
            match status {
                Ok(s) if !s.success() => {
                    let code = s.code().unwrap_or(-1);
                    return Err(crate::Error::ValidationError(format!(
                        "Pretest script '{}' failed (exit code {}). \
                         Fix the script or remove it from package.json scripts.",
                        pretest.args.last().unwrap_or(&pretest.program),
                        code
                    )));
                }
                Err(e) => {
                    tracing::warn!(err = %e, "Pretest script failed to execute, continuing anyway");
                }
                _ => {}
            }
        }

        // Resolve program to absolute path (Frida's Device.spawn doesn't do PATH lookup)
        let program = resolve_program(&test_cmd.program);

        // Inherit parent environment, then overlay test-specific and user-provided vars.
        // envp() replaces the environment entirely, so we must include everything.
        let mut combined_env: HashMap<String, String> = std::env::vars().collect();
        for key in &test_cmd.remove_env {
            combined_env.remove(key);
        }
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
        let spawn_cwd = test_cmd
            .cwd
            .as_deref()
            .unwrap_or(project_root.to_str().unwrap_or("."));
        let pid = session_manager
            .spawn_with_frida(
                session_id,
                &program,
                &test_cmd.args,
                Some(spawn_cwd),
                project_root.to_str().unwrap_or("."),
                Some(&combined_env),
                has_trace_patterns, // defer_resume: install hooks before running
                None,               // symbols_path: test runner uses automatic resolution
            )
            .await?;

        // Apply trace patterns BEFORE resuming the process
        if has_trace_patterns {
            session_manager.add_patterns(session_id, trace_patterns)?;
            match session_manager
                .update_frida_patterns(session_id, Some(trace_patterns), None, None)
                .await
            {
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
        let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str()
        {
            "cargo" => Some(cargo_adapter::update_progress),
            "catch2" => Some(catch2_adapter::update_progress),
            "deno" => Some(deno_adapter::update_progress),
            "go" => Some(go_adapter::update_progress),
            "gtest" => Some(gtest_adapter::update_progress),
            "mocha" => Some(mocha_adapter::update_progress),
            "pytest" => Some(pytest_adapter::update_progress),
            "unittest" => Some(unittest_adapter::update_progress),
            "playwright" => {
                playwright_adapter::reset_progress();
                // Playwright: use the DB event loop callback to poll the progress file.
                // The callback is invoked every 500ms (even with no DB events thanks to
                // the empty-string fallback path). Inside, it reads the progress file.
                Some(playwright_adapter::update_progress as fn(&str, &Arc<Mutex<TestProgress>>))
            }
            "vitest" | "jest" => Some(vitest_adapter::update_progress),
            "bun" => Some(bun_adapter::update_progress),
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
                paused
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
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
        // Safety net: always exceeds kill_timeout so the stuck detector's grace period isn't bypassed.
        let safety_timeout = std::time::Duration::from_millis(hard_timeout + 60_000);
        let start = std::time::Instant::now();
        let mut reaped_status: Option<i32> = None;

        // For Playwright: the spawned bun process exec's into node (playwright runner)
        // and exits immediately. We need to detect when the REAL test runner finishes,
        // not when the bun wrapper exits. Use a child process group check.
        let is_playwright = framework_name == "playwright";

        loop {
            let process_alive = stacks::is_process_alive(pid);

            // Try to reap zombie — kill(pid, 0) returns true for zombies but
            // waitpid detects actual exit. Without this, the loop runs until
            // hard_timeout for every normal test completion.
            let mut reaped = false;
            {
                let mut status: i32 = 0;
                let wp = unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) };
                if wp > 0 {
                    reaped_status = Some(status);
                    reaped = true;
                }
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

            // Poll DB for new text events (stdout + stderr) and update progress.
            if let Some(update_fn) = progress_fn {
                let mut new_events = session_manager
                    .db()
                    .query_events(session_id, |q| {
                        let mut q = q.text_output().limit(500);
                        if last_seen_timestamp_ns > 0 {
                            q.timestamp_from_ns = Some(last_seen_timestamp_ns + 1);
                        }
                        q
                    })
                    .unwrap_or_default();

                new_events.reverse();

                if new_events.is_empty() {
                    // No DB events — still call update_fn with empty string so
                    // file-polling adapters (Playwright) can check their progress file.
                    update_fn("", &progress);
                } else {
                    for event in &new_events {
                        if event.timestamp_ns > last_seen_timestamp_ns {
                            last_seen_timestamp_ns = event.timestamp_ns;
                        }
                        if let Some(text) = &event.text {
                            update_fn(text, &progress);
                        }
                    }
                }
            }

            // For JS frameworks with buffered reporters, force transition
            // from Compiling to Running after 3s.
            if matches!(
                framework_name.as_str(),
                "vitest" | "jest" | "bun" | "playwright"
            ) {
                if let Ok(mut p) = progress.lock() {
                    if p.phase == TestPhase::Compiling
                        && !p.has_custom_reporter
                        && start.elapsed().as_secs() >= 3
                    {
                        p.phase = TestPhase::Running;
                    }
                }
            }

            // Process exit check — AFTER the progress poll so we don't miss the last events.
            if !process_alive || reaped {
                if is_playwright {
                    // Playwright: the bun wrapper exits early but tests continue in a child
                    // node process. Keep polling the progress file until it stops growing
                    // or all tests have reported results, up to hard_timeout.
                    let last_file_size = std::fs::metadata(playwright_adapter::PROGRESS_FILE)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let new_file_size = std::fs::metadata(playwright_adapter::PROGRESS_FILE)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    if new_file_size > last_file_size {
                        // File still growing — tests are still running, keep looping
                        continue;
                    }
                    // File stopped growing — drain remaining events
                    if let Some(update_fn) = progress_fn {
                        update_fn("", &progress);
                    }
                }
                break;
            }

            tokio::time::sleep(poll_interval).await;
        }

        // Abort detector
        detector_handle.abort();

        // Playwright: the spawned process exits before tests finish (exec-replacement).
        // Wait for the progress file to stop growing, polling + updating progress as we go.
        if is_playwright {
            if let Some(update_fn) = progress_fn {
                let poll_wait = std::time::Duration::from_secs(3);
                for _ in 0..60 {
                    // max 3 minutes wait
                    update_fn("", &progress);
                    let size_before = std::fs::metadata(playwright_adapter::PROGRESS_FILE)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    tokio::time::sleep(poll_wait).await;
                    update_fn("", &progress);
                    let size_after = std::fs::metadata(playwright_adapter::PROGRESS_FILE)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    if size_after == size_before && size_after > 0 {
                        break; // File stopped growing — tests done
                    }
                }
            }
        }

        // Mark suites finished in progress
        {
            let mut p = progress.lock().unwrap();
            if p.phase != TestPhase::SuitesFinished {
                p.phase = TestPhase::SuitesFinished;
            }
        }

        // Let DB writer flush
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Final progress drain — process any remaining text events (stdout + stderr) that
        // arrived after the last poll (e.g., test "ok" events emitted just before process exit).
        if let Some(update_fn) = progress_fn {
            let mut remaining = session_manager
                .db()
                .query_events(session_id, |q| {
                    let mut q = q.text_output().limit_uncapped(5000);
                    if last_seen_timestamp_ns > 0 {
                        q.timestamp_from_ns = Some(last_seen_timestamp_ns + 1);
                    }
                    q
                })
                .unwrap_or_default();

            remaining.reverse(); // Chronological order

            for event in &remaining {
                if let Some(text) = &event.text {
                    update_fn(text, &progress);
                }
            }
        }

        // Query ALL stdout/stderr from DB
        let stdout_buf = collect_output(
            session_manager.db(),
            session_id,
            crate::db::EventType::Stdout,
        );
        let stderr_buf = collect_output(
            session_manager.db(),
            session_id,
            crate::db::EventType::Stderr,
        );

        // Get exit code: use already-reaped status from polling loop, or try waitpid
        let exit_code = {
            let status = reaped_status.unwrap_or_else(|| {
                let mut s: i32 = 0;
                let r = unsafe { libc::waitpid(pid as i32, &mut s, libc::WNOHANG) };
                if r > 0 {
                    s
                } else {
                    -1
                }
            });
            if status == -1 {
                // Not our child or already reaped — infer from test results
                let p = progress.lock().unwrap();
                if p.failed > 0 {
                    1
                } else {
                    0
                }
            } else if unsafe { libc::WIFEXITED(status) } {
                unsafe { libc::WEXITSTATUS(status) }
            } else if unsafe { libc::WIFSIGNALED(status) } {
                128 + unsafe { libc::WTERMSIG(status) }
            } else {
                -1
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

            let mut all_stdout = session_manager
                .db()
                .query_events(session_id, |q| {
                    q.event_type(crate::db::EventType::Stdout)
                        .limit_uncapped(50000)
                })
                .unwrap_or_default();
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
                                        let dur_ms = (event.timestamp_ns - start_ns).max(0) as u64
                                            / 1_000_000;
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
    let resolved = if program.contains('/') {
        program.to_string()
    } else if let Ok(path_var) = std::env::var("PATH") {
        let mut found = None;
        for dir in path_var.split(':') {
            let full = format!("{}/{}", dir, program);
            if Path::new(&full).exists() {
                found = Some(full);
                break;
            }
        }
        found.unwrap_or_else(|| program.to_string())
    } else {
        program.to_string()
    };

    #[cfg(target_os = "macos")]
    {
        return prepare_debuggable_bun(&resolved).unwrap_or(resolved);
    }

    #[cfg(not(target_os = "macos"))]
    {
        resolved
    }
}

#[cfg(target_os = "macos")]
fn prepare_debuggable_bun(program: &str) -> Option<String> {
    const REQUIRED_ENTITLEMENTS: &[&str] = &[
        "com.apple.security.get-task-allow",
        "com.apple.security.cs.disable-library-validation",
    ];

    fn read_entitlements(path: &Path) -> Option<String> {
        let output = std::process::Command::new("codesign")
            .args([
                "-d",
                "--entitlements",
                ":-",
                path.to_string_lossy().as_ref(),
            ])
            .output()
            .ok()?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let start = combined.find("<?xml")?;
        Some(combined[start..].trim().to_string())
    }

    fn extract_entitlement_keys(xml: &str) -> std::collections::BTreeSet<String> {
        let mut keys = std::collections::BTreeSet::new();
        let mut rest = xml;
        while let Some(start) = rest.find("<key>") {
            let after_start = &rest[start + 5..];
            if let Some(end) = after_start.find("</key>") {
                keys.insert(after_start[..end].trim().to_string());
                rest = &after_start[end + 6..];
            } else {
                break;
            }
        }
        keys
    }

    fn merged_entitlements_xml(existing: Option<&str>) -> String {
        let mut xml = existing.unwrap_or(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
</dict></plist>"#,
        )
        .to_string();

        for key in REQUIRED_ENTITLEMENTS {
            let marker = format!("<key>{}</key>", key);
            if xml.contains(&marker) {
                continue;
            }
            if let Some(idx) = xml.find("</dict>") {
                xml.insert_str(idx, &format!("<key>{}</key><true/>", key));
            }
        }

        xml
    }

    let path = Path::new(program);
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if !name.contains("bun") || name == "strobe-bun-debug" {
        return None;
    }

    let debug_path = std::env::temp_dir().join("strobe-bun-debug");
    let ent_path = std::env::temp_dir().join("strobe-bun-debug.entitlements");
    let source_entitlements = read_entitlements(path);
    let required_keys: std::collections::BTreeSet<String> = source_entitlements
        .as_deref()
        .map(extract_entitlement_keys)
        .unwrap_or_default()
        .into_iter()
        .chain(REQUIRED_ENTITLEMENTS.iter().map(|key| key.to_string()))
        .collect();

    let needs_refresh = match (std::fs::metadata(path), std::fs::metadata(&debug_path)) {
        (Ok(src), Ok(dst)) => {
            let src_mtime = src.modified().ok();
            let dst_mtime = dst.modified().ok();
            let stale_binary = src_mtime
                .zip(dst_mtime)
                .map(|(src, dst)| src > dst)
                .unwrap_or(true);
            let existing_keys = read_entitlements(&debug_path)
                .as_deref()
                .map(extract_entitlement_keys)
                .unwrap_or_default();
            stale_binary || !required_keys.is_subset(&existing_keys)
        }
        (Ok(_), Err(_)) => true,
        _ => return None,
    };

    if !needs_refresh {
        return Some(debug_path.to_string_lossy().into_owned());
    }

    let entitlements = merged_entitlements_xml(source_entitlements.as_deref());

    if let Err(err) = std::fs::write(&ent_path, entitlements) {
        tracing::warn!("Failed to write Bun entitlements file: {}", err);
        return None;
    }
    if let Err(err) = std::fs::copy(path, &debug_path) {
        tracing::warn!("Failed to copy Bun binary for debug_test: {}", err);
        return None;
    }

    match std::process::Command::new("codesign")
        .args([
            "-f",
            "-s",
            "-",
            "--entitlements",
            ent_path.to_string_lossy().as_ref(),
            debug_path.to_string_lossy().as_ref(),
        ])
        .status()
    {
        Ok(status) if status.success() => Some(debug_path.to_string_lossy().into_owned()),
        Ok(status) => {
            tracing::warn!(
                "codesign returned {} while preparing Bun for Frida; falling back to original binary",
                status
            );
            None
        }
        Err(err) => {
            tracing::warn!("Failed to run codesign for Bun debug_test: {}", err);
            None
        }
    }
}

/// Collect all output events of a given type from the database into a single string.
fn collect_output(
    db: &crate::db::Database,
    session_id: &str,
    event_type: crate::db::EventType,
) -> String {
    let mut events = db
        .query_events(session_id, |q| {
            q.event_type(event_type).limit_uncapped(50000)
        })
        .unwrap_or_default();
    // Query returns newest-first; reverse to chronological for output concatenation
    events.reverse();
    events
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
        assert!(err.contains("Cargo"), "should mention Cargo: {}", err);
        assert!(err.contains("Catch2"), "should mention Catch2: {}", err);
        assert!(err.contains("pytest"), "should mention pytest: {}", err);
        assert!(err.contains("Deno"), "should mention Deno: {}", err);
        assert!(err.contains("Go"), "should mention Go: {}", err);
        assert!(err.contains("Mocha"), "should mention Mocha: {}", err);
        assert!(
            err.contains("Google Test"),
            "should mention Google Test: {}",
            err
        );
    }

    #[test]
    fn test_adapter_detection_deno() {
        let runner = TestRunner::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("deno.json"), "{}").unwrap();
        let adapter = runner.detect_adapter(dir.path(), None, None).unwrap();
        assert_eq!(adapter.name(), "deno");
    }

    #[test]
    fn test_adapter_detection_go() {
        let runner = TestRunner::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module example.com/foo\ngo 1.21\n",
        )
        .unwrap();
        let adapter = runner.detect_adapter(dir.path(), None, None).unwrap();
        assert_eq!(adapter.name(), "go");
    }

    #[test]
    fn test_adapter_detection_mocha() {
        let runner = TestRunner::new();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".mocharc.yml"), "timeout: 5000\n").unwrap();
        let adapter = runner.detect_adapter(dir.path(), None, None).unwrap();
        assert_eq!(adapter.name(), "mocha");
    }

    #[test]
    fn test_adapter_detection_gtest_by_name() {
        let runner = TestRunner::new();
        let adapter = runner
            .detect_adapter(Path::new("/nonexistent"), Some("gtest"), None)
            .unwrap();
        assert_eq!(adapter.name(), "gtest");
    }

    #[test]
    fn test_adapter_detection_invalid_framework() {
        let runner = TestRunner::new();
        let result = runner.detect_adapter(Path::new("."), Some("generic"), None);
        assert!(result.is_err());
        let err = result.err().unwrap().to_string();
        assert!(err.contains("Unknown framework 'generic'"), "got: {}", err);
        assert!(err.contains("Supported:"), "got: {}", err);
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
            connection_id: "conn-1".to_string(),
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
