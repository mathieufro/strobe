use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

use super::adapter::*;

pub struct JestAdapter;

#[derive(Deserialize)]
struct JestReport {
    #[serde(rename = "numPassedTests", default)]
    num_passed: u32,
    #[serde(rename = "numFailedTests", default)]
    num_failed: u32,
    #[serde(rename = "numPendingTests", default)]
    num_pending: u32,
    #[serde(rename = "testResults", default)]
    test_results: Vec<JestSuite>,
}

#[derive(Deserialize)]
struct JestSuite {
    #[serde(alias = "testFilePath", alias = "name", default)]
    file_path: String,
    #[serde(rename = "testResults", default)]
    tests: Vec<JestAssertion>,
}

#[derive(Deserialize)]
struct JestAssertion {
    #[serde(rename = "ancestorTitles", default)]
    ancestors: Vec<String>,
    title: String,
    status: String,
    duration: Option<f64>,
    #[serde(rename = "failureMessages", default)]
    failure_messages: Vec<String>,
}

impl TestAdapter for JestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        for cfg in &["jest.config.js", "jest.config.ts", "jest.config.cjs", "jest.config.mjs"] {
            if project_root.join(cfg).exists() { return 92; }
        }
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            // Check for jest in dependencies (but NOT vitest — vitest takes priority)
            if pkg.contains("\"jest\"") && !pkg.contains("\"vitest\"") { return 88; }
            if pkg.contains("\"jest\"") { return 70; }
        }
        0
    }

    fn name(&self) -> &str { "jest" }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "jest".to_string(),
                "--json".to_string(),
                "--no-coverage".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "jest".to_string(),
                "--json".to_string(),
                "--no-coverage".to_string(),
                "-t".to_string(), test_name.to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        let json_start = stdout.find('{').unwrap_or(0);
        let json_str = &stdout[json_start..];

        let report: JestReport = match serde_json::from_str(json_str) {
            Ok(r) => r,
            Err(_) => {
                let failures = if exit_code != 0 {
                    vec![TestFailure {
                        name: "Test run crashed".to_string(),
                        file: None, line: None,
                        message: format!("Could not parse jest output.\nstderr: {}", &stderr[..stderr.len().min(500)]),
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
        // Also match Jest's "at Object.<anonymous> (file:line:col)" format
        let stack_re2 = regex::Regex::new(r"at\s+\S+\s+\(([^)]+\.(?:test|spec)\.\w+):(\d+):\d+\)").unwrap();
        let mut failures = vec![];
        let mut all_tests = vec![];
        let mut total_duration_ms = 0u64;

        for suite in &report.test_results {
            for a in &suite.tests {
                let full_name = {
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
                        .or_else(|| stack_re2.captures(&msg))
                        .map(|c| (Some(c[1].to_string()), c[2].parse().ok()))
                        .unwrap_or_else(|| {
                            // Fall back to the suite-level file path
                            let f = if suite.file_path.is_empty() { None } else { Some(suite.file_path.clone()) };
                            (f, None)
                        });

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
                skipped: report.num_pending,
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

    const JEST_PASS: &str = r#"{
        "success": true, "startTime": 1000000,
        "numTotalTests": 2, "numPassedTests": 2, "numFailedTests": 0, "numPendingTests": 0,
        "testResults": [{
            "testFilePath": "/project/src/__tests__/calc.test.js",
            "numPassingTests": 2, "numFailingTests": 0,
            "perfStats": {"start": 1000100, "end": 1000300, "runtime": 200},
            "testResults": [
                {"title": "adds", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 5, "failureMessages": []},
                {"title": "subs", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 3, "failureMessages": []}
            ]
        }]
    }"#;

    const JEST_FAIL: &str = r#"{
        "success": false, "startTime": 1000000,
        "numTotalTests": 2, "numPassedTests": 1, "numFailedTests": 1, "numPendingTests": 0,
        "testResults": [{
            "testFilePath": "/project/src/__tests__/calc.test.js",
            "numPassingTests": 1, "numFailingTests": 1,
            "perfStats": {"start": 1000100, "end": 1000400, "runtime": 300},
            "testResults": [
                {
                    "title": "multiplies",
                    "status": "failed",
                    "ancestorTitles": ["Calculator", "multiply"],
                    "duration": 8,
                    "failureMessages": [
                        "Error: expect(received).toBe(expected)\nExpected: 6\nReceived: 5\n    at Object.<anonymous> (/project/src/__tests__/calc.test.js:15:5)"
                    ]
                },
                {"title": "adds", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 5, "failureMessages": []}
            ]
        }]
    }"#;

    #[test]
    fn test_detect_jest() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = JestAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("jest.config.js"), "module.exports = {}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_parse_passing() {
        let result = JestAdapter.parse_output(JEST_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing_with_location() {
        let result = JestAdapter.parse_output(JEST_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures[0].name, "Calculator multiply multiplies");
        assert!(result.failures[0].file.as_deref().unwrap_or("").ends_with("calc.test.js"));
        assert_eq!(result.failures[0].line, Some(15));
    }

    #[test]
    fn test_suite_command_uses_json_flag() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = JestAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert!(cmd.args.iter().any(|a| a == "--json"));
    }
}
