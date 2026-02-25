use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use serde::Deserialize;

use super::adapter::*;
use super::TestProgress;

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
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "vitest".to_string(), "run".to_string(),
                "--reporter=json".to_string(),
                "--no-coverage".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "vitest".to_string(), "run".to_string(),
                "--reporter=json".to_string(),
                "--no-coverage".to_string(),
                "-t".to_string(), test_name.to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        // Find JSON in stdout (may have non-JSON prefix from npx)
        let json_start = stdout.find('{').unwrap_or(0);
        let json_str = &stdout[json_start..];

        let report: VitestReport = match serde_json::from_str(json_str) {
            Ok(r) => r,
            Err(_) => {
                let failures = if exit_code != 0 {
                    vec![TestFailure {
                        name: "Test run crashed".to_string(),
                        file: None, line: None,
                        message: format!("Could not parse vitest output.\nstderr: {}", &stderr[..stderr.len().min(500)]),
                        rerun: None,
                        suggested_traces: vec![],
                    }]
                } else { vec![] };
                return TestResult {
                    summary: TestSummary { passed: 0, failed: 0, skipped: 0, stuck: None, duration_ms: 0 },
                    failures,
                    stuck: vec![],
                    all_tests: vec![],
                };
            }
        };

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

        TestResult {
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

/// Progress updater for vitest. Vitest with --reporter=json streams the full result JSON
/// to stdout in OS pipe-buffer-sized chunks as tests complete. We count occurrences of
/// `"status":"passed"` etc. in each chunk for live progress tracking.
///
/// Also handles verbose reporter-style stderr lines (✓/× suite lines) when
/// --reporter=verbose is used instead of --reporter=json.
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();

    // Transition from Compiling to Running on first output from either stream
    if p.phase == super::TestPhase::Compiling {
        p.phase = super::TestPhase::Running;
    }

    // Parse verbose reporter lines emitted to stderr during test execution.
    // Format: "✓ src/foo.test.ts (N tests) Xms"  or  "× src/foo.test.ts (N tests | M failed) Xms"
    let trimmed = line.trim();
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

    // Count individual test results from JSON reporter chunks.
    // Vitest --reporter=json outputs compact JSON (no spaces after colons).
    p.passed += count_occurrences(line, "\"status\":\"passed\"");
    p.failed += count_occurrences(line, "\"status\":\"failed\"");
    p.skipped += count_occurrences(line, "\"status\":\"pending\"")
        + count_occurrences(line, "\"status\":\"todo\"")
        + count_occurrences(line, "\"status\":\"skipped\"");
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
    }

    #[test]
    fn test_single_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        let cmd = adapter.single_test_command(dir.path(), "Math addition adds correctly").unwrap();
        assert!(cmd.args.iter().any(|a| a.contains("Math")));
    }

    #[test]
    fn test_update_progress_json_chunk_counting() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let mut p0 = TestProgress::new();
        p0.phase = TestPhase::Running;
        let progress = Arc::new(Mutex::new(p0));

        // Simulate a JSON chunk with 3 passed and 1 failed assertion
        let chunk = r#"{"status":"passed","title":"a"},{"status":"passed","title":"b"},{"status":"failed","title":"c"},{"status":"passed","title":"d"}"#;
        update_progress(chunk, &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 3, "should count 3 passed");
        assert_eq!(p.failed, 1, "should count 1 failed");
        assert_eq!(p.skipped, 0);
    }

    #[test]
    fn test_update_progress_json_chunk_skipped() {
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
    fn test_update_progress_phase_transition() {
        use std::sync::{Arc, Mutex};
        use super::super::{TestProgress, TestPhase};

        let progress = Arc::new(Mutex::new(TestProgress::new()));
        assert_eq!(progress.lock().unwrap().phase, TestPhase::Compiling);

        update_progress("any output", &progress);
        assert_eq!(progress.lock().unwrap().phase, TestPhase::Running);
    }

    #[test]
    fn test_count_occurrences() {
        assert_eq!(count_occurrences("", "x"), 0);
        assert_eq!(count_occurrences("aaa", "a"), 3);
        assert_eq!(count_occurrences(r#""status":"passed","status":"passed""#, r#""status":"passed""#), 2);
        assert_eq!(count_occurrences("abab", "ab"), 2);
    }
}
