use std::collections::HashMap;
use std::path::Path;
use serde::Deserialize;

use super::adapter::*;

pub struct PytestAdapter;

impl TestAdapter for PytestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // pyproject.toml with [tool.pytest] section
        if project_root.join("pyproject.toml").exists() {
            if let Ok(content) = std::fs::read_to_string(project_root.join("pyproject.toml")) {
                if content.contains("[tool.pytest") {
                    return 90;
                }
            }
        }
        // pytest.ini or setup.cfg with [tool:pytest]
        if project_root.join("pytest.ini").exists() {
            return 90;
        }
        if project_root.join("conftest.py").exists() {
            return 85;
        }
        // requirements.txt with pytest
        if let Ok(content) = std::fs::read_to_string(project_root.join("requirements.txt")) {
            if content.contains("pytest") {
                return 80;
            }
        }
        // Any test_*.py files
        if has_python_test_files(project_root) {
            return 60;
        }
        0
    }

    fn name(&self) -> &str {
        "pytest"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let mut args = vec![
            "-m".into(),
            "pytest".into(),
            "--tb=short".into(),
            "-q".into(),
            "--json-report".into(),
            "--json-report-file=-".into(),
        ];
        match level {
            Some(TestLevel::Unit) => {
                args.extend(["-m".into(), "not integration and not e2e".into()]);
            }
            Some(TestLevel::Integration) => {
                args.extend(["-m".into(), "integration".into()]);
            }
            Some(TestLevel::E2e) => {
                args.extend(["-m".into(), "e2e".into()]);
            }
            None => {}
        }
        Ok(TestCommand {
            program: "python3".into(),
            args,
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "python3".into(),
            args: vec![
                "-m".into(),
                "pytest".into(),
                "-k".into(),
                test_name.into(),
                "--json-report".into(),
                "--json-report-file=-".into(),
                "--tb=short".into(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_pytest_json_report(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        extract_python_traces(failure)
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        // Fall back to native stacks (py-spy integration is future work)
        super::stacks::capture_native_stacks(pid)
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 60_000,
            Some(TestLevel::Integration) => 180_000,
            Some(TestLevel::E2e) => 300_000,
            None => 120_000,
        }
    }
}

fn has_python_test_files(root: &Path) -> bool {
    // Check for test_*.py files in common locations
    for dir in ["tests", "test", "."] {
        let test_dir = root.join(dir);
        if test_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&test_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("test_") && name.ends_with(".py") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Pytest JSON report structure (subset of fields we care about)
#[derive(Debug, Deserialize)]
struct PytestJsonReport {
    #[serde(default)]
    summary: PytestSummary,
    #[serde(default)]
    tests: Vec<PytestTest>,
}

#[derive(Debug, Deserialize, Default)]
struct PytestSummary {
    #[serde(default)]
    passed: u32,
    #[serde(default)]
    failed: u32,
    #[serde(default)]
    skipped: u32,
    #[serde(default)]
    total: u32,
}

#[derive(Debug, Deserialize)]
struct PytestTest {
    nodeid: String,
    outcome: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    lineno: Option<u32>,
    #[serde(default)]
    call: Option<PytestCall>,
}

#[derive(Debug, Deserialize)]
struct PytestCall {
    #[serde(default)]
    longrepr: Option<String>,
}

/// Parse pytest-json-report output.
fn parse_pytest_json_report(stdout: &str, _stderr: &str, _exit_code: i32) -> TestResult {
    // pytest-json-report writes JSON to stdout when --json-report-file=-
    // Try parsing the entire output first (in tests, it's pure JSON)
    let json_str = stdout.trim();

    let report: PytestJsonReport = if let Ok(r) = serde_json::from_str(json_str) {
        r
    } else {
        // If that fails, look for JSON content (starts with { and ends with })
        let json_start = json_str.find('{').unwrap_or(0);
        let json_end = json_str.rfind('}').map(|i| i + 1).unwrap_or(json_str.len());
        let json_substr = &json_str[json_start..json_end];

        match serde_json::from_str(json_substr) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Failed to parse pytest JSON report: {}", e);
                // Return empty result on parse failure
                return TestResult {
                    summary: TestSummary {
                        passed: 0,
                        failed: 0,
                        skipped: 0,
                        stuck: None,
                        duration_ms: 0,
                    },
                    failures: vec![],
                    stuck: vec![],
                    all_tests: vec![],
                };
            }
        }
    };

    let mut failures = Vec::new();
    let mut all_tests = Vec::new();
    let mut total_duration_ms = 0u64;

    for test in &report.tests {
        let duration_ms = (test.duration * 1000.0) as u64;
        total_duration_ms += duration_ms;

        let status = match test.outcome.as_str() {
            "passed" => TestStatus::Pass,
            "failed" => TestStatus::Fail,
            "skipped" => TestStatus::Skip,
            _ => TestStatus::Skip,
        };

        let detail = TestDetail {
            name: test.nodeid.clone(),
            status: status.clone(),
            duration_ms,
            stdout: None,
            stderr: None,
            message: test
                .call
                .as_ref()
                .and_then(|c| c.longrepr.clone())
                .or_else(|| Some(format!("{} test", test.outcome))),
        };
        all_tests.push(detail);

        if test.outcome == "failed" {
            // Extract file name from nodeid: "tests/test_audio.py::test_function"
            let file = test
                .nodeid
                .split("::")
                .next()
                .map(|s| s.to_string());

            let message = test
                .call
                .as_ref()
                .and_then(|c| c.longrepr.clone())
                .unwrap_or_else(|| "Test failed".to_string());

            failures.push(TestFailure {
                name: test.nodeid.clone(),
                file,
                line: test.lineno,
                message,
                rerun: Some(test.nodeid.split("::").last().unwrap_or(&test.nodeid).to_string()),
                suggested_traces: vec![],
            });
        }
    }

    // Add suggested traces to failures
    for failure in &mut failures {
        failure.suggested_traces = extract_python_traces(failure);
    }

    TestResult {
        summary: TestSummary {
            passed: report.summary.passed,
            failed: report.summary.failed,
            skipped: report.summary.skipped,
            stuck: None,
            duration_ms: total_duration_ms,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract suggested trace patterns from a Python test failure.
fn extract_python_traces(failure: &TestFailure) -> Vec<String> {
    let mut traces = Vec::new();

    // Extract module name from test path: "tests/test_audio.py::TestAudio::test_process"
    if let Some(ref file) = failure.file {
        if let Some(filename) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
            // "test_audio" → trace "audio.*"
            let module = filename.strip_prefix("test_").unwrap_or(filename);
            traces.push(format!("{}.*", module));
        }
        traces.push(format!(
            "@file:{}",
            Path::new(file)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
        ));
    }

    traces
}

/// Update progress from pytest output (line-by-line incremental parsing).
pub fn update_progress(
    line: &str,
    progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>,
) {
    let trimmed = line.trim();

    // Detect test collection phase
    if trimmed.starts_with("collecting") || trimmed.starts_with("collected") {
        let mut p = progress.lock().unwrap();
        if p.phase == super::TestPhase::Compiling {
            p.phase = super::TestPhase::Running;
        }
    }

    // Detect individual test results from verbose output
    // "tests/test_audio.py::test_generate PASSED"
    if trimmed.contains(" PASSED") {
        let mut p = progress.lock().unwrap();
        p.passed += 1;
    } else if trimmed.contains(" FAILED") {
        let mut p = progress.lock().unwrap();
        p.failed += 1;
    } else if trimmed.contains(" SKIPPED") || trimmed.contains(" XFAIL") {
        let mut p = progress.lock().unwrap();
        p.skipped += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_detect_pytest_config() {
        let adapter = PytestAdapter;
        let fixture_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
        if fixture_dir.exists() {
            let confidence = adapter.detect(&fixture_dir, None);
            assert!(
                confidence >= 80,
                "Should detect pytest in fixture dir: got {}",
                confidence
            );
        }
    }

    #[test]
    fn test_parse_pytest_json_report() {
        let json_output = r#"{"summary":{"passed":3,"failed":1,"total":4,"collected":4,"skipped":0},"tests":[{"nodeid":"tests/test_audio.py::test_audio_generate_sine","outcome":"passed","duration":0.001},{"nodeid":"tests/test_audio.py::test_audio_intentional_failure","outcome":"failed","duration":0.002,"call":{"longrepr":"AssertionError: assert 0.0 == 1.0"},"lineno":15}]}"#;
        let result = parse_pytest_json_report(json_output, "", 1);
        assert_eq!(result.summary.passed, 3);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].name.contains("intentional_failure"));
    }

    #[test]
    fn test_suggest_traces_python() {
        let failure = TestFailure {
            name: "tests/test_audio.py::TestAudio::test_process".to_string(),
            file: Some("tests/test_audio.py".to_string()),
            line: Some(15),
            message: "AssertionError".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = extract_python_traces(&failure);
        assert!(!traces.is_empty());
        assert!(traces.iter().any(|t| t.contains("audio")));
    }
}
