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
            // Skip doctests by default — they're slow to compile and often
            // fail in isolation due to missing feature flags or link issues.
            None => args.push("--tests".to_string()),
        }

        // --format json and -Zunstable-options are test harness flags (after --)
        args.push("--".to_string());
        args.push("-Zunstable-options".to_string());
        args.push("--format".to_string());
        args.push("json".to_string());

        Ok(TestCommand {
            program: "cargo".to_string(),
            args,
            env: HashMap::from([("RUSTC_BOOTSTRAP".to_string(), "1".to_string())]),
        })
    }

    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        let mut args = vec!["test".to_string()];

        // Check if test_name matches an integration test binary (tests/<name>.rs).
        // If so, use `--test <name>` to only compile that specific binary instead
        // of all test targets — this avoids recompiling doctests and unrelated binaries.
        let test_file = project_root.join("tests").join(format!("{}.rs", test_name));
        if test_file.exists() {
            args.push("--test".to_string());
            args.push(test_name.to_string());
        } else {
            // Not a test binary name — treat as a function name filter.
            // Use --tests to skip doctests (which are slow and often fail in isolation).
            args.push("--tests".to_string());
        }

        args.push("--".to_string());

        // If not targeting a specific binary, pass the name as a test function filter
        if !test_file.exists() {
            args.push(test_name.to_string());
        }

        args.push("-Zunstable-options".to_string());
        args.push("--format".to_string());
        args.push("json".to_string());

        Ok(TestCommand {
            program: "cargo".to_string(),
            args,
            env: HashMap::from([("RUSTC_BOOTSTRAP".to_string(), "1".to_string())]),
        })
    }

    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
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
                Err(_) => continue,
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
                        status: TestStatus::Pass,
                        duration_ms: (exec_time * 1000.0) as u64,
                        stdout: v.get("stdout").and_then(|s| s.as_str()).map(|s| s.to_string()),
                        stderr: None,
                        message: None,
                    });
                }
                ("test", "failed") | ("test", "timeout") => {
                    failed += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let test_stdout = v.get("stdout").and_then(|s| s.as_str()).unwrap_or("");

                    let (file, line_num, message) = if event == "timeout" {
                        (None, None, format!("Test '{}' timed out", name))
                    } else {
                        parse_panic_location(test_stdout)
                    };

                    failures.push(TestFailure {
                        name: name.clone(),
                        file: file.clone(),
                        line: line_num,
                        message: message.clone(),
                        rerun: Some(name.clone()),
                        suggested_traces: vec![],
                    });

                    all_tests.push(TestDetail {
                        name,
                        status: TestStatus::Fail,
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
                        status: TestStatus::Skip,
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: None,
                    });
                }
                ("suite", "ok") | ("suite", "failed") => {
                    // Only accumulate duration from suite summaries.
                    // Pass/fail/skip counts come from individual test events above,
                    // since multi-target runs emit multiple suite summaries.
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    duration_ms += (exec_time * 1000.0) as u64;
                }
                _ => {}
            }
        }

        // Detect test binaries that crashed (SIGSEGV, SIGABRT, etc.).
        // Cargo reports these in stderr but never emits JSON test events for
        // the crashed binary, so they silently vanish from the results.
        if exit_code != 0 && failed == 0 {
            // Parse stderr for crash indicators from cargo
            // Pattern: "process didn't exit successfully: <path> (signal: N, ...)"
            let crash_failures = parse_crash_from_stderr(stderr);
            if !crash_failures.is_empty() {
                for crash in crash_failures {
                    failed += 1;
                    all_tests.push(TestDetail {
                        name: crash.name.clone(),
                        status: TestStatus::Fail,
                        duration_ms: 0,
                        stdout: None,
                        stderr: Some(stderr.to_string()),
                        message: Some(crash.message.clone()),
                    });
                    failures.push(crash);
                }
            } else if exit_code >= 128 {
                // Process killed by signal but no cargo crash message found
                let signal = exit_code - 128;
                let signal_name = match signal {
                    6 => "SIGABRT",
                    9 => "SIGKILL",
                    11 => "SIGSEGV",
                    15 => "SIGTERM",
                    _ => "signal",
                };
                let message = format!(
                    "Test process crashed with {} (signal {}, exit code {})",
                    signal_name, signal, exit_code
                );
                failed += 1;
                failures.push(TestFailure {
                    name: "(crash)".to_string(),
                    file: None,
                    line: None,
                    message: message.clone(),
                    rerun: None,
                    suggested_traces: vec![],
                });
                all_tests.push(TestDetail {
                    name: "(crash)".to_string(),
                    status: TestStatus::Fail,
                    duration_ms: 0,
                    stdout: None,
                    stderr: Some(stderr.to_string()),
                    message: Some(message),
                });
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

}

/// Parse crash messages from cargo stderr.
/// Cargo reports crashed test binaries like:
///   error: test failed, to rerun pass `--test phase2a_gaps`
///   Caused by:
///     process didn't exit successfully: `<path>` (signal: 11, SIGSEGV: invalid memory reference)
fn parse_crash_from_stderr(stderr: &str) -> Vec<TestFailure> {
    let mut failures = Vec::new();
    let mut rerun_hint: Option<String> = None;

    for line in stderr.lines() {
        // Capture "to rerun pass `--test <name>`" hint
        if let Some(idx) = line.find("to rerun pass `--test ") {
            let after = &line[idx + "to rerun pass `--test ".len()..];
            if let Some(end) = after.find('`') {
                rerun_hint = Some(after[..end].to_string());
            }
        }

        // Detect "process didn't exit successfully: ... (signal: N, ...)"
        if line.contains("process didn't exit successfully") {
            if let Some(sig_idx) = line.find("(signal: ") {
                let after = &line[sig_idx + "(signal: ".len()..];
                let signal_desc = after.split(')').next().unwrap_or("unknown");

                let binary_name = rerun_hint.take().unwrap_or_else(|| {
                    // Try to extract binary name from the path
                    if let Some(path_start) = line.find('`') {
                        let after_tick = &line[path_start + 1..];
                        if let Some(path_end) = after_tick.find('`') {
                            let path = &after_tick[..path_end];
                            // Split on / or space, take the binary name
                            return path.split(&['/', ' '][..])
                                .find(|s| !s.is_empty() && !s.starts_with('-'))
                                .unwrap_or("(unknown binary)")
                                .to_string();
                        }
                    }
                    "(unknown binary)".to_string()
                });

                let message = format!(
                    "Test binary '{}' crashed: signal: {}",
                    binary_name, signal_desc
                );

                failures.push(TestFailure {
                    name: format!("(crash: {})", binary_name),
                    file: None,
                    line: None,
                    message,
                    rerun: Some(binary_name),
                    suggested_traces: vec![],
                });
            }
        }
    }

    failures
}

/// Parse panic location from cargo test stdout.
/// Looks for patterns like: "panicked at src/parser.rs:142:5:\n<message>"
fn parse_panic_location(stdout: &str) -> (Option<String>, Option<u32>, String) {
    for line in stdout.lines() {
        if let Some(idx) = line.find("panicked at ") {
            let after = &line[idx + "panicked at ".len()..];
            let parts: Vec<&str> = after.splitn(4, ':').collect();
            if parts.len() >= 2 {
                let file = parts[0].trim().to_string();
                let line_num = parts[1].trim().parse::<u32>().ok();
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

/// Parse Cargo JSON output and update progress incrementally.
/// Input may contain multiple JSON lines (stdout chunks from Frida can batch lines).
pub fn update_progress(text: &str, progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>) {
    for line in text.lines() {
        update_progress_line(line, progress);
    }
}

fn update_progress_line(line: &str, progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let mut p = progress.lock().unwrap();
    match (event_type, event) {
        ("suite", "started") => {
            // First suite started means compilation is done, tests are running
            if p.phase == super::TestPhase::Compiling {
                p.phase = super::TestPhase::Running;
            }
        }
        ("suite", "ok") | ("suite", "failed") => {
            // Suite finished — mark SuitesFinished so stuck detector knows tests
            // completed. If another suite starts, ("suite", "started") won't
            // regress this since it only transitions from Compiling.
            p.phase = super::TestPhase::SuitesFinished;
            p.current_test = None;
        }
        ("test", "started") => {
            p.phase = super::TestPhase::Running;
            p.current_test = v.get("name").and_then(|n| n.as_str()).map(String::from);
            p.current_test_started_at = Some(std::time::Instant::now());
        }
        ("test", "ok") => {
            p.passed += 1;
            // Record wall-clock duration for this test
            if let Some(started) = p.current_test_started_at {
                if let Some(name) = p.current_test.clone() {
                    p.test_durations.insert(name, started.elapsed().as_millis() as u64);
                }
            }
            p.current_test_started_at = None;
        }
        ("test", "failed") | ("test", "timeout") => {
            p.failed += 1;
            if let Some(started) = p.current_test_started_at {
                if let Some(name) = p.current_test.clone() {
                    p.test_durations.insert(name, started.elapsed().as_millis() as u64);
                }
            }
            p.current_test_started_at = None;
        }
        ("test", "ignored") => {
            p.skipped += 1;
            p.current_test_started_at = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_detect_cargo_project() {
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
    fn test_single_test_command_function_name() {
        let adapter = CargoTestAdapter;
        // Function name filter (no matching tests/<name>.rs) → --tests + filter
        let cmd = adapter.single_test_command(
            Path::new("/tmp"),
            "parser::tests::test_empty_input",
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(cmd.args.contains(&"--tests".to_string()));
        assert!(cmd.args.contains(&"parser::tests::test_empty_input".to_string()));
    }

    #[test]
    fn test_single_test_command_binary_name() {
        let adapter = CargoTestAdapter;
        // If tests/<name>.rs exists, use --test <name> (no function filter)
        // We test this with the actual project root which has tests/
        let cmd = adapter.single_test_command(
            Path::new("."),
            "frida_e2e",
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(cmd.args.contains(&"--test".to_string()));
        assert!(cmd.args.contains(&"frida_e2e".to_string()));
        // Should NOT have --tests flag (we're targeting a specific binary)
        assert!(!cmd.args.contains(&"--tests".to_string()));
    }

    #[test]
    fn test_parse_crash_sigsegv_detected() {
        let adapter = CargoTestAdapter;
        // Simulate: some tests passed, then a binary SIGSEGV'd (no JSON failures)
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 2 }
{ "type": "test", "event": "started", "name": "tests::test_ok" }
{ "type": "test", "event": "ok", "name": "tests::test_ok" }
{ "type": "suite", "event": "ok", "passed": 2, "failed": 0, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.1 }
"#;
        let stderr = "   Compiling strobe v0.1.0 (/Users/alex/strobe)\n\
            error: test failed, to rerun pass `--test phase2a_gaps`\n\
            \n\
            Caused by:\n  \
            process didn't exit successfully: `/path/to/phase2a_gaps` (signal: 11, SIGSEGV: invalid memory reference)\n";
        // exit_code 101 = cargo's "test failed" code
        let result = adapter.parse_output(stdout, stderr, 101);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1, "Crash should be counted as a failure");
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].name.contains("phase2a_gaps"));
        assert!(result.failures[0].message.contains("SIGSEGV"));
        assert_eq!(result.failures[0].rerun.as_deref(), Some("phase2a_gaps"));
    }

    #[test]
    fn test_parse_crash_not_triggered_when_json_has_failures() {
        let adapter = CargoTestAdapter;
        // If JSON already reported a failure, don't double-count from exit code
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 1 }
{ "type": "test", "event": "started", "name": "tests::test_bad" }
{ "type": "test", "event": "failed", "name": "tests::test_bad", "stdout": "thread 'tests::test_bad' panicked at tests/bad.rs:10:5:\nassert failed\n" }
{ "type": "suite", "event": "failed", "passed": 0, "failed": 1, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.01 }
"#;
        let result = adapter.parse_output(stdout, "", 101);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        // Should NOT have a synthetic crash entry
        assert!(!result.failures[0].name.contains("crash"));
    }

    #[test]
    fn test_parse_crash_signal_only() {
        let adapter = CargoTestAdapter;
        // Process killed by signal directly (no cargo crash message in stderr)
        let result = adapter.parse_output("", "", 139); // 128 + 11 = SIGSEGV
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].message.contains("SIGSEGV"));
    }
}
