use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;
use super::adapter::*;
use super::cargo_adapter::capture_native_stacks;

static FAIL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:FAIL|FAILED|ERROR|FAILURE)[:\s]+(.+?)(?:\s+at\s+)?(\S+?):(\d+)"
    ).expect("Invalid failure regex pattern")
});

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
        for cap in FAIL_RE.captures_iter(&combined) {
            failures.push(TestFailure {
                name: cap.get(1).map(|m| m.as_str().trim().to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
                file: cap.get(2).map(|m| m.as_str().to_string()),
                line: cap.get(3).and_then(|m| m.as_str().parse().ok()),
                message: cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
                rerun: None,
                suggested_traces: vec![],
            });
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
                passed: 0,
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
        vec![]
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        capture_native_stacks(pid)
    }
}

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
