use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::adapter::*;
use super::TestProgress;

pub struct GTestAdapter;

#[derive(Deserialize)]
struct GTestOutput {
    #[serde(default)]
    testsuites: Vec<GTestSuite>,
}

#[derive(Deserialize)]
struct GTestSuite {
    #[serde(default)]
    name: String,
    #[serde(default)]
    testsuite: Vec<GTestCase>,
}

#[derive(Deserialize)]
struct GTestCase {
    #[serde(default)]
    name: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    time: String,
    #[serde(default)]
    classname: String,
    #[serde(default)]
    failures: Option<Vec<GTestFailure>>,
}

#[derive(Deserialize)]
struct GTestFailure {
    #[serde(default)]
    failure: String,
}

impl TestAdapter for GTestAdapter {
    fn detect(&self, project_root: &Path, command: Option<&str>) -> u8 {
        if let Some(cmd) = command {
            if Path::new(cmd).exists() {
                let output = std::process::Command::new(cmd)
                    .arg("--gtest_list_tests")
                    .output();
                if let Ok(o) = output {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.contains('.') {
                        return 90;
                    }
                }
            }
        }

        let cmake_path = project_root.join("CMakeLists.txt");
        if let Ok(contents) = std::fs::read_to_string(cmake_path) {
            if contents.contains("gtest") || contents.contains("gmock") || contents.contains("GTest") {
                return 85;
            }
        }

        0
    }

    fn name(&self) -> &str {
        "gtest"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::ValidationError(
            "GTest adapter requires a test binary path via the 'command' parameter".to_string(),
        ))
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        _test_name: &str,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::ValidationError(
            "GTest adapter requires a test binary path via the 'command' parameter".to_string(),
        ))
    }

    fn parse_output(
        &self,
        stdout: &str,
        _stderr: &str,
        _exit_code: i32,
    ) -> TestResult {
        if let Some(result) = parse_gtest_json(stdout) {
            return result;
        }

        parse_gtest_text_fallback(stdout)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        if let Some(ref file) = failure.file {
            if let Some(stem) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
                traces.push(format!("@file:{}", stem));
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

impl GTestAdapter {
    /// Build command for running all tests in a GTest binary.
    pub fn command_for_binary(cmd: &str, _level: Option<TestLevel>) -> TestCommand {
        TestCommand {
            program: cmd.to_string(),
            args: vec!["--gtest_output=json".to_string()],
            env: HashMap::new(),
        }
    }

    /// Build command for running a single test in a GTest binary.
    pub fn single_test_for_binary(cmd: &str, test_name: &str) -> TestCommand {
        TestCommand {
            program: cmd.to_string(),
            args: vec![
                "--gtest_output=json".to_string(),
                format!("--gtest_filter={}", test_name),
            ],
            env: HashMap::new(),
        }
    }
}

/// Parse GTest JSON output (from --gtest_output=json) into TestResult.
/// Returns None if the output is not valid GTest JSON.
fn parse_gtest_json(stdout: &str) -> Option<TestResult> {
    let output: GTestOutput = serde_json::from_str(stdout).ok()?;

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut total_duration_ms = 0u64;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    for suite in &output.testsuites {
        for case in &suite.testsuite {
            let duration_ms = parse_gtest_time(&case.time);
            total_duration_ms += duration_ms;

            let suite_name = if case.classname.is_empty() {
                &suite.name
            } else {
                &case.classname
            };
            let full_name = format!("{}.{}", suite_name, case.name);

            if case.result == "SKIPPED" {
                skipped += 1;
                all_tests.push(TestDetail {
                    name: full_name,
                    status: TestStatus::Skip,
                    duration_ms,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
                continue;
            }

            let has_failures = case.failures.as_ref().map_or(false, |f| !f.is_empty());

            if has_failures {
                failed += 1;
                let failure_messages: Vec<&str> = case
                    .failures
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|f| f.failure.as_str())
                    .collect();
                let message = failure_messages.join("\n");

                let (file, line) = extract_file_line(&message);

                failures.push(TestFailure {
                    name: full_name.clone(),
                    file: file.clone(),
                    line,
                    message: message.clone(),
                    rerun: Some(full_name.clone()),
                    suggested_traces: vec![],
                });

                all_tests.push(TestDetail {
                    name: full_name,
                    status: TestStatus::Fail,
                    duration_ms,
                    stdout: None,
                    stderr: None,
                    message: Some(message),
                });
            } else {
                passed += 1;
                all_tests.push(TestDetail {
                    name: full_name,
                    status: TestStatus::Pass,
                    duration_ms,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
            }
        }
    }

    Some(TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped,
            stuck: None,
            duration_ms: total_duration_ms,
        },
        failures,
        stuck: vec![],
        all_tests,
    })
}

/// Parse GTest time string (e.g., "0.001s") into milliseconds.
fn parse_gtest_time(time_str: &str) -> u64 {
    let stripped = time_str.trim_end_matches('s');
    let secs: f64 = stripped.parse().unwrap_or(0.0);
    (secs * 1000.0) as u64
}

/// Extract file:line from GTest failure text.
/// The first line often contains "path/file.cpp:42".
fn extract_file_line(message: &str) -> (Option<String>, Option<u32>) {
    let first_line = message.lines().next().unwrap_or("");

    // Look for pattern: path/file.cpp:NN
    for segment in first_line.split_whitespace() {
        // Must contain a colon and end with a digit (file:line pattern)
        if let Some(colon_pos) = segment.rfind(':') {
            let path_part = &segment[..colon_pos];
            let line_part = &segment[colon_pos + 1..];
            // Strip trailing non-digit characters (e.g., trailing newline chars)
            let line_part = line_part.trim_end_matches(|c: char| !c.is_ascii_digit());
            if let Ok(line_num) = line_part.parse::<u32>() {
                if path_part.contains('.') {
                    return (Some(path_part.to_string()), Some(line_num));
                }
            }
        }
    }

    (None, None)
}

/// Fallback parser for GTest text output (non-JSON mode).
/// Counts [  PASSED  ] and [  FAILED  ] summary lines.
fn parse_gtest_text_fallback(stdout: &str) -> TestResult {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();
    let mut in_summary = false;

    for line in stdout.lines() {
        let trimmed = line.trim();

        // The final summary separator contains "ran." — after it, FAILED lines are the footer list
        if trimmed.starts_with("[==========]") && trimmed.contains("ran.") {
            in_summary = true;
        }

        // Individual test results
        if trimmed.starts_with("[       OK ]") {
            passed += 1;
            let name = extract_test_name_from_text(trimmed, "[       OK ]");
            all_tests.push(TestDetail {
                name,
                status: TestStatus::Pass,
                duration_ms: 0,
                stdout: None,
                stderr: None,
                message: None,
            });
        } else if !in_summary && trimmed.starts_with("[  FAILED  ]") && !trimmed.contains("tests listed below") && !trimmed.contains("test,") {
            let name = extract_test_name_from_text(trimmed, "[  FAILED  ]");
            // Avoid counting the summary line "N FAILED TESTS" or test list footer
            if !name.is_empty() && !name.starts_with(char::is_numeric) {
                failed += 1;
                failures.push(TestFailure {
                    name: name.clone(),
                    file: None,
                    line: None,
                    message: "Test failed (see output for details)".to_string(),
                    rerun: Some(name.clone()),
                    suggested_traces: vec![],
                });
                all_tests.push(TestDetail {
                    name,
                    status: TestStatus::Fail,
                    duration_ms: 0,
                    stdout: None,
                    stderr: None,
                    message: Some("Test failed (see output for details)".to_string()),
                });
            }
        }
    }

    TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped: 0,
            stuck: None,
            duration_ms: 0,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract test name from a GTest text output line like "[       OK ] SuiteName.TestName (1 ms)".
fn extract_test_name_from_text(line: &str, prefix: &str) -> String {
    let after = line.trim_start_matches(|c: char| c == ' ' || c == '[')
        .strip_prefix(prefix.trim_start_matches(|c: char| c == ' ' || c == '['))
        .unwrap_or(line);

    // Alternative: find after the bracket prefix
    let name_part = if let Some(idx) = line.find(']') {
        line[idx + 1..].trim()
    } else {
        after.trim()
    };

    // Remove trailing duration like "(1 ms)"
    if let Some(paren) = name_part.find('(') {
        name_part[..paren].trim().to_string()
    } else {
        name_part.trim().to_string()
    }
}

/// Parse GTest text output lines and update progress incrementally.
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    let trimmed = line.trim();

    // [ RUN      ] SuiteName.TestName
    if trimmed.starts_with("[ RUN      ]") {
        let name = if let Some(idx) = trimmed.find(']') {
            trimmed[idx + 1..].trim().to_string()
        } else {
            return;
        };
        let mut p = progress.lock().unwrap();
        if p.phase == super::TestPhase::Compiling {
            p.phase = super::TestPhase::Running;
        }
        p.running_tests.insert(name, std::time::Instant::now());
    }
    // [       OK ] SuiteName.TestName
    else if trimmed.starts_with("[       OK ]") {
        let name = if let Some(idx) = trimmed.find(']') {
            let full = trimmed[idx + 1..].trim();
            if let Some(paren) = full.find('(') {
                full[..paren].trim().to_string()
            } else {
                full.to_string()
            }
        } else {
            return;
        };
        let mut p = progress.lock().unwrap();
        p.passed += 1;
        if let Some(started) = p.running_tests.remove(&name) {
            p.test_durations
                .insert(name, started.elapsed().as_millis() as u64);
        }
    }
    // [  FAILED  ] SuiteName.TestName
    else if trimmed.starts_with("[  FAILED  ]")
        && !trimmed.contains("tests listed below")
        && !trimmed.contains("test,")
    {
        let name = if let Some(idx) = trimmed.find(']') {
            let full = trimmed[idx + 1..].trim();
            if let Some(paren) = full.find('(') {
                full[..paren].trim().to_string()
            } else {
                full.to_string()
            }
        } else {
            return;
        };
        if name.is_empty() || name.starts_with(char::is_numeric) {
            return;
        }
        let mut p = progress.lock().unwrap();
        p.failed += 1;
        if let Some(started) = p.running_tests.remove(&name) {
            p.test_durations
                .insert(name, started.elapsed().as_millis() as u64);
        }
    }
    // [==========] N tests ran
    else if trimmed.starts_with("[==========]") && trimmed.contains("tests ran") {
        let mut p = progress.lock().unwrap();
        p.phase = super::TestPhase::SuitesFinished;
        p.running_tests.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_gtest_cmake() {
        let dir = std::env::temp_dir().join("strobe_gtest_detect_cmake");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("CMakeLists.txt"),
            "find_package(GTest REQUIRED)\ntarget_link_libraries(tests gtest_main)",
        )
        .unwrap();

        let adapter = GTestAdapter;
        let confidence = adapter.detect(&dir, None);
        assert!(confidence >= 85, "Expected >= 85, got {}", confidence);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_detect_gtest_no_match() {
        let dir = std::env::temp_dir().join("strobe_gtest_detect_empty");
        let _ = std::fs::create_dir_all(&dir);
        // Ensure no CMakeLists.txt
        let _ = std::fs::remove_file(dir.join("CMakeLists.txt"));

        let adapter = GTestAdapter;
        let confidence = adapter.detect(&dir, None);
        assert_eq!(confidence, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_passing() {
        let adapter = GTestAdapter;
        let json = r#"{
            "testsuites": [{
                "name": "MathTest",
                "tests": 2,
                "failures": 0,
                "testsuite": [
                    {
                        "name": "Addition",
                        "result": "COMPLETED",
                        "time": "0.001s",
                        "classname": "MathTest"
                    },
                    {
                        "name": "Subtraction",
                        "result": "COMPLETED",
                        "time": "0.002s",
                        "classname": "MathTest"
                    }
                ]
            }]
        }"#;
        let result = adapter.parse_output(json, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
        assert_eq!(result.all_tests[0].name, "MathTest.Addition");
        assert_eq!(result.all_tests[0].duration_ms, 1);
        assert_eq!(result.all_tests[1].duration_ms, 2);
    }

    #[test]
    fn test_parse_failing() {
        let adapter = GTestAdapter;
        let json = r#"{
            "testsuites": [{
                "name": "MathTest",
                "tests": 2,
                "failures": 1,
                "testsuite": [
                    {
                        "name": "Addition",
                        "result": "COMPLETED",
                        "time": "0.001s",
                        "classname": "MathTest"
                    },
                    {
                        "name": "BadMath",
                        "result": "COMPLETED",
                        "time": "0.003s",
                        "classname": "MathTest",
                        "failures": [{
                            "failure": "path/math_test.cpp:42\nExpected: 6\nActual: 5"
                        }]
                    }
                ]
            }]
        }"#;
        let result = adapter.parse_output(json, "", 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "MathTest.BadMath");
        assert_eq!(f.rerun.as_deref(), Some("MathTest.BadMath"));
        assert_eq!(f.file.as_deref(), Some("path/math_test.cpp"));
        assert_eq!(f.line, Some(42));
        assert!(f.message.contains("Expected: 6"));
        assert!(f.message.contains("Actual: 5"));
    }

    #[test]
    fn test_parse_skipped() {
        let adapter = GTestAdapter;
        let json = r#"{
            "testsuites": [{
                "name": "FeatureTest",
                "tests": 1,
                "failures": 0,
                "testsuite": [
                    {
                        "name": "NeedsGPU",
                        "result": "SKIPPED",
                        "time": "0s",
                        "classname": "FeatureTest"
                    }
                ]
            }]
        }"#;
        let result = adapter.parse_output(json, "", 0);
        assert_eq!(result.summary.passed, 0);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 1);
        assert_eq!(result.all_tests[0].status, TestStatus::Skip);
    }

    #[test]
    fn test_command_for_binary() {
        let cmd = GTestAdapter::command_for_binary("/path/to/test_binary", None);
        assert_eq!(cmd.program, "/path/to/test_binary");
        assert!(cmd.args.contains(&"--gtest_output=json".to_string()));
        assert!(cmd.env.is_empty());
    }

    #[test]
    fn test_single_test_for_binary() {
        let cmd = GTestAdapter::single_test_for_binary(
            "/path/to/test_binary",
            "MathTest.Addition",
        );
        assert_eq!(cmd.program, "/path/to/test_binary");
        assert!(cmd.args.contains(&"--gtest_output=json".to_string()));
        assert!(cmd
            .args
            .contains(&"--gtest_filter=MathTest.Addition".to_string()));
    }

    #[test]
    fn test_text_fallback() {
        let adapter = GTestAdapter;
        let text_output = "\
[==========] Running 3 tests from 1 test suite.
[----------] 3 tests from MathTest
[ RUN      ] MathTest.Addition
[       OK ] MathTest.Addition (0 ms)
[ RUN      ] MathTest.Subtraction
[       OK ] MathTest.Subtraction (0 ms)
[ RUN      ] MathTest.BadDivision
[  FAILED  ] MathTest.BadDivision (1 ms)
[----------] 3 tests from MathTest (1 ms total)
[==========] 3 tests from 1 test suite ran. (1 ms total)
[  PASSED  ] 2 tests.
[  FAILED  ] 1 test, listed below:
[  FAILED  ] MathTest.BadDivision
 1 FAILED TEST";

        let result = adapter.parse_output(text_output, "", 1);
        assert_eq!(result.summary.passed, 2);
        // The individual [  FAILED  ] line for the test result + the summary list entry
        // Only the inline result line should count, not the summary footer
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].name, "MathTest.BadDivision");
    }
}
