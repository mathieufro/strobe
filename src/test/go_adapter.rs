use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use regex::Regex;
use serde::Deserialize;

use super::adapter::*;

/// Streaming JSON event emitted by `go test -json`.
#[derive(Debug, Deserialize)]
struct GoTestEvent {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package", default)]
    package: String,
    #[serde(rename = "Test")]
    test: Option<String>,
    #[serde(rename = "Output")]
    output: Option<String>,
    #[serde(rename = "Elapsed")]
    elapsed: Option<f64>,
}

pub struct GoTestAdapter;

impl TestAdapter for GoTestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("go.mod").exists() {
            90
        } else if project_root.join("go.sum").exists() {
            80
        } else {
            0
        }
    }

    fn name(&self) -> &str {
        "go"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "go".to_string(),
            args: vec![
                "test".to_string(),
                "-v".to_string(),
                "-json".to_string(),
                "./...".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        let escaped = regex::escape(test_name);
        let filter = format!("^{}$", escaped);
        Ok(TestCommand {
            program: "go".to_string(),
            args: vec![
                "test".to_string(),
                "-v".to_string(),
                "-json".to_string(),
                "-run".to_string(),
                filter,
                "./...".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> TestResult {
        parse_go_json(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        if let Some(ref file) = failure.file {
            if let Some(stem) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
                traces.push(format!("@file:{}", stem));
                let module = stem.trim_end_matches("_test");
                traces.push(format!("{}.*", module));
            }
        }

        traces
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 120_000,
            Some(TestLevel::Integration) => 300_000,
            Some(TestLevel::E2e) => 600_000,
            None => 300_000,
        }
    }
}

/// Parse `go test -json` streaming output into structured results.
fn parse_go_json(stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut duration_ms = 0u64;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // Accumulate output per test (package::test_name → output lines).
    let mut test_output: HashMap<String, String> = HashMap::new();
    // Track which tests had a build-fail action.
    let mut build_failed = false;

    let file_line_re = Regex::new(r"([\w/.:\-]+\.go):(\d+):").unwrap();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let event: GoTestEvent = match serde_json::from_str(line) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match event.action.as_str() {
            "output" => {
                if let (Some(ref test_name), Some(ref output)) = (&event.test, &event.output) {
                    let key = format!("{}::{}", event.package, test_name);
                    test_output
                        .entry(key)
                        .or_default()
                        .push_str(output);
                }
            }
            "pass" => {
                if let Some(ref test_name) = event.test {
                    passed += 1;
                    let dur_ms = event.elapsed.map(|e| (e * 1000.0) as u64).unwrap_or(0);
                    let key = format!("{}::{}", event.package, test_name);
                    all_tests.push(TestDetail {
                        name: test_name.clone(),
                        status: TestStatus::Pass,
                        duration_ms: dur_ms,
                        stdout: test_output.remove(&key),
                        stderr: None,
                        message: None,
                    });
                } else {
                    // Package-level pass — accumulate duration.
                    if let Some(elapsed) = event.elapsed {
                        duration_ms += (elapsed * 1000.0) as u64;
                    }
                }
            }
            "fail" => {
                if let Some(ref test_name) = event.test {
                    failed += 1;
                    let dur_ms = event.elapsed.map(|e| (e * 1000.0) as u64).unwrap_or(0);
                    let key = format!("{}::{}", event.package, test_name);
                    let output_text = test_output.remove(&key).unwrap_or_default();

                    let (file, line_num) = extract_go_file_line(&file_line_re, &output_text);
                    // Extract the actual assertion message, skipping test framework lines
                    // like "=== RUN", "--- FAIL:", and empty lines
                    let message = output_text.lines()
                        .map(|l| l.trim())
                        .filter(|l| {
                            !l.is_empty()
                                && !l.starts_with("=== RUN")
                                && !l.starts_with("--- FAIL:")
                                && !l.starts_with("--- PASS:")
                        })
                        .last()
                        .unwrap_or("Test failed")
                        .to_string();

                    failures.push(TestFailure {
                        name: test_name.clone(),
                        file: file.clone(),
                        line: line_num,
                        message: message.clone(),
                        rerun: Some(test_name.clone()),
                        suggested_traces: vec![],
                    });

                    all_tests.push(TestDetail {
                        name: test_name.clone(),
                        status: TestStatus::Fail,
                        duration_ms: dur_ms,
                        stdout: Some(output_text),
                        stderr: None,
                        message: Some(message),
                    });
                } else {
                    // Package-level fail — accumulate duration.
                    if let Some(elapsed) = event.elapsed {
                        duration_ms += (elapsed * 1000.0) as u64;
                    }
                }
            }
            "skip" => {
                if let Some(ref test_name) = event.test {
                    skipped += 1;
                    let key = format!("{}::{}", event.package, test_name);
                    all_tests.push(TestDetail {
                        name: test_name.clone(),
                        status: TestStatus::Skip,
                        duration_ms: 0,
                        stdout: test_output.remove(&key),
                        stderr: None,
                        message: None,
                    });
                }
            }
            "build-fail" => {
                build_failed = true;
            }
            // "run" and other actions — no result accounting needed.
            _ => {}
        }
    }

    // Handle build failure: report as a compilation error.
    if build_failed && failed == 0 {
        failed += 1;
        let stderr_truncated = stderr.chars().take(500).collect::<String>();
        let message = if stderr_truncated.is_empty() {
            "Compilation failed (build-fail)".to_string()
        } else {
            format!("Compilation failed:\n{}", stderr_truncated)
        };
        failures.push(TestFailure {
            name: "(compilation)".to_string(),
            file: None,
            line: None,
            message: message.clone(),
            rerun: None,
            suggested_traces: vec![],
        });
        all_tests.push(TestDetail {
            name: "(compilation)".to_string(),
            status: TestStatus::Fail,
            duration_ms: 0,
            stdout: None,
            stderr: Some(stderr_truncated),
            message: Some(message),
        });
    }

    // Detect process crash when exit code is non-zero but no test-level failures
    // were recorded (and no build-fail either).
    if exit_code != 0 && failed == 0 && !build_failed {
        if exit_code >= 128 {
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
                stderr: Some(stderr.chars().take(500).collect::<String>()),
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

/// Extract the best file:line location from Go test output.
/// Prefers _test.go files over other matches (vendor, stdlib).
fn extract_go_file_line(re: &Regex, output: &str) -> (Option<String>, Option<u32>) {
    let mut first_match: Option<(String, u32)> = None;

    for caps in re.captures_iter(output) {
        let file = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let line = caps.get(2).and_then(|m| m.as_str().parse::<u32>().ok()).unwrap_or(0);

        // Prefer _test.go files — these are the user's test assertions
        if file.ends_with("_test.go") {
            return (Some(file.to_string()), Some(line));
        }

        if first_match.is_none() && !file.contains("/vendor/") {
            first_match = Some((file.to_string(), line));
        }
    }

    match first_match {
        Some((file, line)) => (Some(file), Some(line)),
        None => (None, None),
    }
}

/// Parse `go test -json` output and update progress incrementally.
/// Input may contain multiple JSON lines (stdout chunks from Frida can batch lines).
pub fn update_progress(text: &str, progress: &Arc<Mutex<super::TestProgress>>) {
    for line in text.lines() {
        update_progress_line(line, progress);
    }
}

fn update_progress_line(line: &str, progress: &Arc<Mutex<super::TestProgress>>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    let event: GoTestEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut p = progress.lock().unwrap();

    match event.action.as_str() {
        "run" => {
            if let Some(ref test_name) = event.test {
                p.phase = super::TestPhase::Running;
                p.running_tests
                    .insert(test_name.clone(), std::time::Instant::now());
            }
        }
        "pass" => {
            if event.test.is_some() {
                p.passed += 1;
            }
            if let Some(ref name) = event.test {
                if let Some(started) = p.running_tests.remove(name) {
                    p.test_durations
                        .insert(name.clone(), started.elapsed().as_millis() as u64);
                }
            }
        }
        "fail" => {
            if event.test.is_some() {
                p.failed += 1;
            }
            if let Some(ref name) = event.test {
                if let Some(started) = p.running_tests.remove(name) {
                    p.test_durations
                        .insert(name.clone(), started.elapsed().as_millis() as u64);
                }
            }
        }
        "skip" => {
            if event.test.is_some() {
                p.skipped += 1;
            }
            if let Some(ref name) = event.test {
                p.running_tests.remove(name);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_go() {
        let adapter = GoTestAdapter;

        // Empty/nonexistent dir → 0
        let empty = tempfile::tempdir().unwrap();
        let confidence = adapter.detect(empty.path(), None);
        assert_eq!(confidence, 0);

        // A dir with go.mod → 90
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("go.mod"), "module example.com/foo\n\ngo 1.21\n").unwrap();
        let confidence = adapter.detect(tmp.path(), None);
        assert!(confidence >= 90, "Expected >= 90 for go.mod, got {}", confidence);
    }

    #[test]
    fn test_detect_go_sum_only() {
        let adapter = GoTestAdapter;
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("go.sum"), "example.com/foo v1.0.0\n").unwrap();
        let confidence = adapter.detect(tmp.path(), None);
        assert!(confidence >= 80, "Expected >= 80 for go.sum, got {}", confidence);
    }

    #[test]
    fn test_parse_passing() {
        let adapter = GoTestAdapter;
        let stdout = r#"{"Time":"2026-01-15T10:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestAdd"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestAdd","Output":"=== RUN   TestAdd\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestAdd","Output":"--- PASS: TestAdd (0.00s)\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"pass","Package":"example.com/pkg","Test":"TestAdd","Elapsed":0.001}
{"Time":"2026-01-15T10:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestSub"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestSub","Output":"=== RUN   TestSub\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestSub","Output":"--- PASS: TestSub (0.00s)\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"pass","Package":"example.com/pkg","Test":"TestSub","Elapsed":0.002}
{"Time":"2026-01-15T10:00:00Z","Action":"pass","Package":"example.com/pkg","Elapsed":0.005}
"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
    }

    #[test]
    fn test_parse_failing() {
        let adapter = GoTestAdapter;
        let stdout = r#"{"Time":"2026-01-15T10:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestBroken"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestBroken","Output":"=== RUN   TestBroken\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestBroken","Output":"    calc_test.go:18: expected 4, got 5\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestBroken","Output":"--- FAIL: TestBroken (0.00s)\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"fail","Package":"example.com/pkg","Test":"TestBroken","Elapsed":0.001}
{"Time":"2026-01-15T10:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestOk"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestOk","Output":"=== RUN   TestOk\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestOk","Output":"--- PASS: TestOk (0.00s)\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"pass","Package":"example.com/pkg","Test":"TestOk","Elapsed":0.001}
{"Time":"2026-01-15T10:00:00Z","Action":"fail","Package":"example.com/pkg","Elapsed":0.003}
"#;
        let result = adapter.parse_output(stdout, "", 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "TestBroken");
        assert_eq!(f.file.as_deref(), Some("calc_test.go"));
        assert_eq!(f.line, Some(18));
        assert!(f.message.contains("expected 4, got 5") || f.message.contains("FAIL"));
        assert_eq!(f.rerun.as_deref(), Some("TestBroken"));
    }

    #[test]
    fn test_parse_skipped() {
        let adapter = GoTestAdapter;
        let stdout = r#"{"Time":"2026-01-15T10:00:00Z","Action":"run","Package":"example.com/pkg","Test":"TestSkipped"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestSkipped","Output":"=== RUN   TestSkipped\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"output","Package":"example.com/pkg","Test":"TestSkipped","Output":"--- SKIP: TestSkipped (0.00s)\n"}
{"Time":"2026-01-15T10:00:00Z","Action":"skip","Package":"example.com/pkg","Test":"TestSkipped","Elapsed":0.0}
{"Time":"2026-01-15T10:00:00Z","Action":"pass","Package":"example.com/pkg","Elapsed":0.001}
"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 0);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 1);
        assert_eq!(result.all_tests[0].status, TestStatus::Skip);
    }

    #[test]
    fn test_parse_build_fail() {
        let adapter = GoTestAdapter;
        let stdout = r#"{"Time":"2026-01-15T10:00:00Z","Action":"build-fail","Package":"example.com/pkg"}
"#;
        let stderr = "# example.com/pkg\n./main.go:10:5: undefined: Foo\n";
        let result = adapter.parse_output(stdout, stderr, 2);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].message.contains("Compilation failed"));
        assert!(result.failures[0].name.contains("compilation"));
    }

    #[test]
    fn test_suite_command() {
        let adapter = GoTestAdapter;
        let cmd = adapter
            .suite_command(Path::new("/project"), None, &HashMap::new())
            .unwrap();
        assert_eq!(cmd.program, "go");
        assert!(cmd.args.contains(&"-json".to_string()));
        assert!(cmd.args.contains(&"./...".to_string()));
    }

    #[test]
    fn test_single_test_escapes_regex() {
        let adapter = GoTestAdapter;
        let cmd = adapter
            .single_test_command(Path::new("/project"), "TestFoo.Bar")
            .unwrap();
        assert_eq!(cmd.program, "go");
        assert!(cmd.args.contains(&"-run".to_string()));
        // regex::escape("TestFoo.Bar") → "TestFoo\\.Bar", then anchored with ^...$
        let filter = cmd.args.iter().find(|a| a.contains("TestFoo")).unwrap();
        assert_eq!(filter, r"^TestFoo\.Bar$");
    }

    #[test]
    fn test_suggest_traces() {
        let adapter = GoTestAdapter;
        let failure = TestFailure {
            name: "TestBroken".to_string(),
            file: Some("calc_test.go".to_string()),
            line: Some(18),
            message: "expected 4, got 5".to_string(),
            rerun: Some("TestBroken".to_string()),
            suggested_traces: vec![],
        };
        let traces = adapter.suggest_traces(&failure);
        assert!(traces.contains(&"@file:calc_test".to_string()));
        assert!(traces.contains(&"calc.*".to_string()));
    }

    #[test]
    fn test_update_progress() {
        let progress = Arc::new(Mutex::new(super::super::TestProgress::new()));
        let lines = r#"{"Action":"run","Package":"example.com/pkg","Test":"TestA"}
{"Action":"pass","Package":"example.com/pkg","Test":"TestA","Elapsed":0.001}
{"Action":"run","Package":"example.com/pkg","Test":"TestB"}
{"Action":"fail","Package":"example.com/pkg","Test":"TestB","Elapsed":0.002}
"#;
        update_progress(lines, &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1);
        assert_eq!(p.failed, 1);
        assert_eq!(p.phase, super::super::TestPhase::Running);
        // TestA was removed on pass, TestB was removed on fail
        assert!(p.running_tests.is_empty());
    }

    #[test]
    fn test_default_timeout() {
        let adapter = GoTestAdapter;
        assert_eq!(adapter.default_timeout(Some(TestLevel::Unit)), 120_000);
        assert_eq!(adapter.default_timeout(Some(TestLevel::Integration)), 300_000);
        assert_eq!(adapter.default_timeout(Some(TestLevel::E2e)), 600_000);
        assert_eq!(adapter.default_timeout(None), 300_000);
    }
}
