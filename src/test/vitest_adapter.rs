use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

use super::adapter::*;

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
}
