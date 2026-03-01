use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use serde::Deserialize;

use super::adapter::*;
use super::TestProgress;

/// Custom Vitest 3.x reporter that streams per-test events to stderr.
/// Written to a temp file and passed via `--reporter=<path>`.
/// Vitest 2.x silently ignores the unknown hooks, so this is safe for all versions.
const REPORTER_JS: &str = include_str!("reporters/vitest-reporter.mjs");

/// Write the custom reporter to a temp file, returning the path.
/// Content is static so concurrent writes are safe.
fn ensure_reporter_file() -> String {
    let path = "/tmp/.strobe-vitest-reporter.mjs";
    let _ = std::fs::write(path, REPORTER_JS);
    path.to_string()
}

pub struct VitestAdapter;

#[derive(Deserialize)]
struct VitestReport {
    #[serde(rename = "numPassedTests", default)]
    num_passed: u32,
    #[serde(rename = "numFailedTests", default)]
    num_failed: u32,
    #[serde(rename = "numPendingTests", default)]
    num_pending: u32,
    #[serde(rename = "numTodoTests", default)]
    num_todo: u32,
    #[serde(rename = "testResults", default)]
    test_results: Vec<VitestSuite>,
}

#[derive(Deserialize)]
struct VitestSuite {
    #[allow(dead_code)]
    name: String,
    #[serde(rename = "assertionResults", default)]
    assertions: Vec<VitestAssertion>,
}

#[derive(Deserialize)]
struct VitestAssertion {
    #[serde(rename = "ancestorTitles", default)]
    ancestors: Vec<String>,
    title: String,
    #[serde(rename = "fullName", default)]
    full_name: String,
    status: String,
    duration: Option<f64>,
    #[serde(rename = "failureMessages", default)]
    failure_messages: Vec<String>,
}

impl TestAdapter for VitestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Highest: explicit vitest config file
        for cfg in &["vitest.config.ts", "vitest.config.js", "vitest.config.mts", "vitest.config.mjs"] {
            if project_root.join(cfg).exists() { return 95; }
        }
        // High: vitest in package.json devDependencies or dependencies
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"vitest\"") { return 90; }
        }
        // Medium: vite.config with test key (Vitest is Vite-native)
        for cfg in &["vite.config.ts", "vite.config.js"] {
            if let Ok(c) = std::fs::read_to_string(project_root.join(cfg)) {
                if c.contains("\"test\"") || c.contains("'test'") { return 70; }
            }
        }
        0
    }

    fn name(&self) -> &str { "vitest" }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let reporter_path = ensure_reporter_file();
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "vitest".to_string(), "run".to_string(),
                "--reporter=json".to_string(),
                format!("--reporter={}", reporter_path),
                "--no-coverage".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        let reporter_path = ensure_reporter_file();
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "vitest".to_string(), "run".to_string(),
                "--reporter=json".to_string(),
                format!("--reporter={}", reporter_path),
                "--no-coverage".to_string(),
                "-t".to_string(), test_name.to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        // Try JSON from stdout first (primary path)
        if let Some(result) = parse_json_output(stdout) {
            return result;
        }

        // Fallback: build results from STROBE_TEST events in stderr.
        // This handles cases where vitest hangs during finalization (e.g., afterAll hook)
        // and never writes JSON to stdout, but the custom reporter already streamed
        // per-test results to stderr.
        if let Some(result) = parse_strobe_events(stderr) {
            return result;
        }

        // Neither JSON nor STROBE_TEST events — report crash or empty
        let failures = if exit_code != 0 {
            vec![TestFailure {
                name: "Test run crashed".to_string(),
                file: None, line: None,
                message: format!("Could not parse vitest output.\nstderr: {}", &stderr[..stderr.len().min(500)]),
                rerun: None,
                suggested_traces: vec![],
            }]
        } else { vec![] };
        TestResult {
            summary: TestSummary { passed: 0, failed: 0, skipped: 0, stuck: None, duration_ms: 0 },
            failures,
            stuck: vec![],
            all_tests: vec![],
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file).file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            let module = stem.trim_end_matches(".test").trim_end_matches(".spec");
            traces.push(format!("@file:{}", stem));
            traces.push(format!("{}.*", module));
        }
        traces
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 120_000,
            Some(TestLevel::Integration) => 300_000,
            Some(TestLevel::E2e) => 600_000,
            None => 180_000,
        }
    }
}

/// Parse vitest JSON reporter output from stdout.
/// Returns None if stdout has no valid JSON or the JSON has no test results.
fn parse_json_output(stdout: &str) -> Option<TestResult> {
    let json_start = stdout.find('{')?;
    let json_str = &stdout[json_start..];

    let report: VitestReport = serde_json::from_str(json_str).ok()?;

    // Valid JSON but empty testResults — don't treat as success, let caller try fallback
    if report.test_results.is_empty()
        && report.num_passed == 0
        && report.num_failed == 0
    {
        return None;
    }

    let stack_re = regex::Regex::new(r"\(([^)]+\.(?:test|spec)\.\w+):(\d+):\d+\)").unwrap();
    let mut failures = vec![];
    let mut all_tests = vec![];
    let mut total_duration_ms = 0u64;

    for suite in &report.test_results {
        for a in &suite.assertions {
            let full_name = if !a.full_name.is_empty() {
                a.full_name.clone()
            } else {
                let mut parts = a.ancestors.clone();
                parts.push(a.title.clone());
                parts.join(" ")
            };

            let duration_ms = a.duration.map(|d| d as u64).unwrap_or(0);
            total_duration_ms += duration_ms;

            let status = match a.status.as_str() {
                "passed" => TestStatus::Pass,
                "failed" => TestStatus::Fail,
                "todo" | "pending" | "skipped" => TestStatus::Skip,
                _ => TestStatus::Skip,
            };

            all_tests.push(TestDetail {
                name: full_name.clone(),
                status: status.clone(),
                duration_ms,
                stdout: None, stderr: None, message: None,
            });

            if matches!(status, TestStatus::Fail) {
                let msg = a.failure_messages.first().cloned().unwrap_or_default();
                let (file, line) = stack_re.captures(&msg)
                    .map(|c| (Some(c[1].to_string()), c[2].parse().ok()))
                    .unwrap_or((None, None));

                failures.push(TestFailure {
                    name: full_name,
                    file,
                    line,
                    message: msg,
                    rerun: None,
                    suggested_traces: vec![],
                });
            }
        }
    }

    Some(TestResult {
        summary: TestSummary {
            passed: report.num_passed,
            failed: report.num_failed,
            skipped: report.num_pending + report.num_todo,
            stuck: None,
            duration_ms: total_duration_ms,
        },
        failures,
        stuck: vec![],
        all_tests,
    })
}

/// Build TestResult from STROBE_TEST: protocol events in stderr.
/// Returns None if no STROBE_TEST events are found.
fn parse_strobe_events(stderr: &str) -> Option<TestResult> {
    let mut all_tests = vec![];
    let mut failures = vec![];
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut total_duration_ms = 0u64;

    for segment in stderr.split("STROBE_TEST:") {
        let json_str = segment.trim();
        if json_str.is_empty() || !json_str.starts_with('{') {
            continue;
        }
        let json_end = json_str.find('\n').unwrap_or(json_str.len());
        let json = &json_str[..json_end];

        let v: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
        let name = v.get("n").and_then(|n| n.as_str()).unwrap_or("").to_string();
        let duration_ms = v.get("d").and_then(|d| d.as_f64()).map(|d| d as u64).unwrap_or(0);

        let status = match event {
            "pass" => { passed += 1; TestStatus::Pass }
            "fail" => { failed += 1; TestStatus::Fail }
            "skip" => { skipped += 1; TestStatus::Skip }
            _ => continue, // module_start, module_end, start — not result events
        };

        total_duration_ms += duration_ms;

        all_tests.push(TestDetail {
            name: name.clone(),
            status: status.clone(),
            duration_ms,
            stdout: None, stderr: None, message: None,
        });

        if matches!(status, TestStatus::Fail) {
            failures.push(TestFailure {
                name,
                file: None,
                line: None,
                message: String::new(),
                rerun: None,
                suggested_traces: vec![],
            });
        }
    }

    if all_tests.is_empty() {
        return None;
    }

    Some(TestResult {
        summary: TestSummary { passed, failed, skipped, stuck: None, duration_ms: total_duration_ms },
        failures,
        stuck: vec![],
        all_tests,
    })
}

/// Progress updater for vitest/jest/bun. Parses STROBE_TEST: protocol events from the
/// custom reporter (Vitest 3.x) for real-time per-test tracking. Falls back to JSON
/// chunk counting for older Vitest versions, Jest, and Bun.
pub fn update_progress(text: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();

    // Parse STROBE_TEST: protocol events from custom reporter.
    // Each event is an atomic process.stderr.write() call (<PIPE_BUF), so events
    // won't be split across chunks. Multiple events may appear in one chunk.
    let mut found_strobe = false;
    for segment in text.split("STROBE_TEST:") {
        let json_str = segment.trim();
        if json_str.is_empty() || !json_str.starts_with('{') {
            continue;
        }
        // Extract just the JSON object (stop at newline or end)
        let json_end = json_str.find('\n').unwrap_or(json_str.len());
        let json = &json_str[..json_end];

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
            found_strobe = true;
            p.has_custom_reporter = true;

            let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
            let name = v.get("n").and_then(|n| n.as_str()).unwrap_or("").to_string();

            match event {
                "module_start" => {
                    // File-level execution started — transition from Compiling
                    if p.phase == super::TestPhase::Compiling {
                        p.phase = super::TestPhase::Running;
                    }
                }
                "module_end" => {
                    // File-level execution finished (informational)
                }
                "start" => { p.start_test(name); }
                "pass"  => { p.passed += 1; p.finish_test(&name); }
                "fail"  => { p.failed += 1; p.finish_test(&name); }
                "skip"  => { p.skipped += 1; p.finish_test(&name); }
                _ => {}
            }
        }
    }

    // Fallback: old counting for Vitest 2.x, Jest, and Bun (no STROBE_TEST events)
    if !found_strobe && !p.has_custom_reporter {
        let trimmed = text.trim();
        if trimmed.starts_with('✓') || trimmed.starts_with('\u{2713}') {
            if let Some(n) = parse_suite_test_count(trimmed) {
                p.passed += n;
                return;
            }
        } else if trimmed.starts_with('×') || trimmed.starts_with('\u{00d7}') || trimmed.starts_with('❯') {
            if let Some((passed, failed)) = parse_suite_failed_count(trimmed) {
                p.passed += passed;
                p.failed += failed;
                return;
            }
        }

        p.passed += count_occurrences(text, "\"status\":\"passed\"");
        p.failed += count_occurrences(text, "\"status\":\"failed\"");
        p.skipped += count_occurrences(text, "\"status\":\"pending\"")
            + count_occurrences(text, "\"status\":\"todo\"")
            + count_occurrences(text, "\"status\":\"skipped\"");
    }
}

fn count_occurrences(haystack: &str, needle: &str) -> u32 {
    let mut count = 0u32;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

/// Parse "(N tests) Xms" → N from a vitest verbose suite line.
fn parse_suite_test_count(line: &str) -> Option<u32> {
    let open = line.find('(')?;
    let close = line[open..].find(')')?;
    let inner = &line[open + 1..open + close];
    // e.g. "5 tests" or "1 test"
    inner.split_whitespace().next()?.parse().ok()
}

/// Parse "(N tests | M failed)" → (passed, failed) from a failed suite line.
fn parse_suite_failed_count(line: &str) -> Option<(u32, u32)> {
    let open = line.find('(')?;
    let close = line[open..].find(')')?;
    let inner = &line[open + 1..open + close];
    if inner.contains('|') {
        let mut parts = inner.splitn(2, '|');
        let total: u32 = parts.next()?.split_whitespace().next()?.parse().ok()?;
        let failed_part = parts.next()?.trim();
        let failed: u32 = failed_part.split_whitespace().next()?.parse().ok()?;
        Some((total.saturating_sub(failed), failed))
    } else {
        // No pipe — all failed
        let total: u32 = inner.split_whitespace().next()?.parse().ok()?;
        Some((0, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASS_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 1, "numFailedTestSuites": 0,
        "numTotalTests": 2, "numPassedTests": 2, "numFailedTests": 0,
        "success": true, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/math.test.ts",
            "status": "passed",
            "startTime": 1000100, "endTime": 1000250,
            "assertionResults": [
                {"ancestorTitles": ["Math"], "title": "adds", "fullName": "Math adds", "status": "passed", "duration": 5},
                {"ancestorTitles": ["Math"], "title": "subs", "fullName": "Math subs", "status": "passed", "duration": 3}
            ]
        }]
    }"#;

    const FAIL_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 0, "numFailedTestSuites": 1,
        "numTotalTests": 2, "numPassedTests": 1, "numFailedTests": 1,
        "success": false, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/math.test.ts",
            "status": "failed",
            "startTime": 1000100, "endTime": 1000300,
            "assertionResults": [
                {
                    "ancestorTitles": ["Math", "addition"],
                    "title": "adds correctly",
                    "fullName": "Math addition adds correctly",
                    "status": "failed",
                    "duration": 12,
                    "failureMessages": ["AssertionError: expected 3 to deeply equal 4\n    at Object.<anonymous> (/project/src/math.test.ts:8:12)"]
                },
                {"ancestorTitles": ["Math"], "title": "subs", "fullName": "Math subs", "status": "passed", "duration": 3}
            ]
        }]
    }"#;

    const PENDING_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 0, "numFailedTestSuites": 0,
        "numTotalTests": 1, "numPassedTests": 0, "numFailedTests": 0, "numPendingTests": 1,
        "success": true, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/todo.test.ts",
            "status": "passed",
            "startTime": 1000100, "endTime": 1000110,
            "assertionResults": [
                {"ancestorTitles": [], "title": "todo test", "fullName": "todo test", "status": "todo", "duration": 0}
            ]
        }]
    }"#;

    #[test]
    fn test_detect_vitest_project() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("package.json"),
            r#"{"devDependencies": {"vitest": "^1.0.0"}}"#).unwrap();
        assert!(adapter.detect(dir.path(), None) > 0, "should detect vitest in package.json");
    }

    #[test]
    fn test_detect_vitest_config() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;

        std::fs::write(dir.path().join("vitest.config.ts"), "export default {}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90, "vitest.config.ts = max confidence");
    }

    #[test]
    fn test_parse_all_passed() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(PASS_JSON, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
        assert!(result.all_tests.iter().all(|t| t.status == TestStatus::Pass));
    }

    #[test]
    fn test_parse_failure_extracts_message_and_location() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(FAIL_JSON, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Math addition adds correctly");
        assert!(f.message.contains("expected 3"), "failure message extracted");
        assert!(f.file.as_deref().unwrap_or("").ends_with("math.test.ts"));
    }

    #[test]
    fn test_parse_nested_describe_full_name() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(FAIL_JSON, "", 1);
        assert_eq!(result.failures[0].name, "Math addition adds correctly");
    }

    #[test]
    fn test_parse_pending_tests() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(PENDING_JSON, "", 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_non_json_output_graceful() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output("not json at all", "stderr line", 1);
        assert!(result.failures.len() <= 1);
    }

    #[test]
    fn test_suggest_traces_from_failure() {
        let adapter = VitestAdapter;
        let failure = TestFailure {
            name: "Math addition adds correctly".to_string(),
            file: Some("/project/src/math.test.ts".to_string()),
            line: Some(8),
            message: "AssertionError".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = adapter.suggest_traces(&failure);
        assert!(!traces.is_empty(), "should suggest traces");
        assert!(traces.iter().any(|t| t.contains("@file:math.test")));
    }

    #[test]
    fn test_suite_command_structure() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        let cmd = adapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert!(cmd.args.iter().any(|a| a.contains("vitest")));
        assert!(cmd.args.iter().any(|a| a.contains("json")), "should use json reporter");
        assert!(cmd.args.iter().any(|a| a.contains(".strobe-vitest-reporter")),
            "should include custom reporter");
    }

    #[test]
    fn test_single_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        let cmd = adapter.single_test_command(dir.path(), "Math addition adds correctly").unwrap();
        assert!(cmd.args.iter().any(|a| a.contains("Math")));
        assert!(cmd.args.iter().any(|a| a.contains(".strobe-vitest-reporter")),
            "should include custom reporter");
    }

    // --- STROBE_TEST protocol tests ---

    #[test]
    fn test_update_progress_strobe_start_and_pass() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        // Test start event
        update_progress("\nSTROBE_TEST:{\"e\":\"start\",\"n\":\"Math adds\"}\n", &progress);
        {
            let p = progress.lock().unwrap();
            assert!(p.running_tests.contains_key("Math adds"), "start should populate running_tests");
            assert!(p.has_custom_reporter);
            assert_eq!(p.passed, 0);
        }

        // Test pass event
        update_progress("\nSTROBE_TEST:{\"e\":\"pass\",\"n\":\"Math adds\",\"d\":5}\n", &progress);
        {
            let p = progress.lock().unwrap();
            assert_eq!(p.passed, 1);
            assert!(!p.running_tests.contains_key("Math adds"), "pass should remove from running_tests");
            assert!(p.test_durations.contains_key("Math adds"), "should record duration");
        }
    }

    #[test]
    fn test_update_progress_strobe_multiple_events_in_chunk() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        let chunk = "\nSTROBE_TEST:{\"e\":\"start\",\"n\":\"test1\"}\n\
                     STROBE_TEST:{\"e\":\"pass\",\"n\":\"test1\",\"d\":3}\n\
                     STROBE_TEST:{\"e\":\"start\",\"n\":\"test2\"}\n\
                     STROBE_TEST:{\"e\":\"fail\",\"n\":\"test2\",\"d\":10}\n";
        update_progress(chunk, &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1);
        assert_eq!(p.failed, 1);
        assert!(p.running_tests.is_empty());
        assert!(p.has_custom_reporter);
    }

    #[test]
    fn test_update_progress_strobe_skip() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        // Simulate start then skip — skip should remove from running_tests
        update_progress("\nSTROBE_TEST:{\"e\":\"start\",\"n\":\"todo test\"}\n", &progress);
        assert!(progress.lock().unwrap().running_tests.contains_key("todo test"));

        update_progress("\nSTROBE_TEST:{\"e\":\"skip\",\"n\":\"todo test\"}\n", &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.skipped, 1);
        assert!(p.has_custom_reporter);
        assert!(!p.running_tests.contains_key("todo test"), "skip should remove from running_tests");
    }

    #[test]
    fn test_update_progress_strobe_disables_fallback() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        // First: strobe event sets has_custom_reporter
        update_progress("\nSTROBE_TEST:{\"e\":\"pass\",\"n\":\"a\"}\n", &progress);
        assert!(progress.lock().unwrap().has_custom_reporter);

        // Second: JSON chunk should be ignored (no double-counting)
        update_progress(r#"{"status":"passed","title":"b"}"#, &progress);
        assert_eq!(progress.lock().unwrap().passed, 1, "fallback should be disabled");
    }

    // --- Fallback path tests (Jest/Bun/Vitest 2.x) ---

    #[test]
    fn test_update_progress_fallback_json_chunk_counting() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        let chunk = r#"{"status":"passed","title":"a"},{"status":"passed","title":"b"},{"status":"failed","title":"c"},{"status":"passed","title":"d"}"#;
        update_progress(chunk, &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 3, "should count 3 passed");
        assert_eq!(p.failed, 1, "should count 1 failed");
        assert_eq!(p.skipped, 0);
        assert!(!p.has_custom_reporter, "should not set custom reporter flag");
    }

    #[test]
    fn test_update_progress_fallback_skipped() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        let chunk = r#"{"status":"pending","title":"a"},{"status":"todo","title":"b"},{"status":"skipped","title":"c"}"#;
        update_progress(chunk, &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.skipped, 3, "should count pending+todo+skipped");
    }

    #[test]
    fn test_update_progress_phase_transition_via_module_start() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let progress = Arc::new(Mutex::new(TestProgress::new()));
        assert_eq!(progress.lock().unwrap().phase, TestPhase::Compiling);

        // Random output should NOT transition phase (no more blanket transition)
        update_progress("any output", &progress);
        assert_eq!(progress.lock().unwrap().phase, TestPhase::Compiling);

        // module_start event should transition from Compiling to Running
        update_progress("\nSTROBE_TEST:{\"e\":\"module_start\",\"n\":\"src/math.test.ts\"}\n", &progress);
        assert_eq!(progress.lock().unwrap().phase, TestPhase::Running);
        assert!(progress.lock().unwrap().has_custom_reporter);
    }

    #[test]
    fn test_count_occurrences() {
        assert_eq!(count_occurrences("", "x"), 0);
        assert_eq!(count_occurrences("aaa", "a"), 3);
        assert_eq!(count_occurrences(r#""status":"passed","status":"passed""#, r#""status":"passed""#), 2);
        assert_eq!(count_occurrences("abab", "ab"), 2);
    }

    // --- STROBE_TEST fallback tests ---

    #[test]
    fn test_parse_output_falls_back_to_strobe_events_when_stdout_empty() {
        let adapter = VitestAdapter;
        let stderr = r#"
STROBE_TEST:{"e":"module_start","n":"src/math.test.ts"}
STROBE_TEST:{"e":"start","n":"Math adds"}
STROBE_TEST:{"e":"pass","n":"Math adds","d":5}
STROBE_TEST:{"e":"start","n":"Math subs"}
STROBE_TEST:{"e":"pass","n":"Math subs","d":3}
STROBE_TEST:{"e":"module_end","n":"src/math.test.ts","d":10}
"#;
        let result = adapter.parse_output("", stderr, 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.all_tests.len(), 2);
        assert!(result.all_tests.iter().all(|t| t.status == TestStatus::Pass));
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_output_falls_back_with_mixed_results() {
        let adapter = VitestAdapter;
        let stderr = r#"
STROBE_TEST:{"e":"pass","n":"test A","d":10}
STROBE_TEST:{"e":"fail","n":"test B","d":5}
STROBE_TEST:{"e":"skip","n":"test C"}
"#;
        let result = adapter.parse_output("", stderr, 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.all_tests.len(), 3);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].name, "test B");
    }

    #[test]
    fn test_parse_output_prefers_json_over_strobe_events() {
        let adapter = VitestAdapter;
        // Both JSON stdout and STROBE_TEST events available — JSON wins
        let stderr = "\nSTROBE_TEST:{\"e\":\"pass\",\"n\":\"extra test\",\"d\":1}\n";
        let result = adapter.parse_output(PASS_JSON, stderr, 0);
        assert_eq!(result.summary.passed, 2, "should use JSON counts, not STROBE_TEST");
        assert_eq!(result.all_tests.len(), 2);
    }

    #[test]
    fn test_parse_output_no_json_no_strobe_events() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output("", "some random stderr", 1);
        assert!(result.all_tests.is_empty());
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].name.contains("crashed"));
    }

    #[test]
    fn test_parse_strobe_events_ignores_non_result_events() {
        let stderr = r#"
STROBE_TEST:{"e":"module_start","n":"file.ts"}
STROBE_TEST:{"e":"start","n":"test A"}
STROBE_TEST:{"e":"pass","n":"test A","d":5}
STROBE_TEST:{"e":"module_end","n":"file.ts","d":10}
"#;
        let result = parse_strobe_events(stderr).unwrap();
        assert_eq!(result.all_tests.len(), 1, "only pass/fail/skip should produce test entries");
        assert_eq!(result.summary.passed, 1);
    }
}
