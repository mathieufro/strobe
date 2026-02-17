use std::collections::HashMap;
use std::path::Path;
use regex::Regex;

use super::adapter::*;

pub struct UnittestAdapter;

impl TestAdapter for UnittestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Lower priority than pytest — only detect if no pytest config found
        if project_root.join("pytest.ini").exists()
            || project_root.join("conftest.py").exists()
        {
            return 0; // pytest takes priority
        }

        // Look for test_*.py files using unittest patterns
        if has_unittest_test_files(project_root) {
            return 70;
        }

        0
    }

    fn name(&self) -> &str {
        "unittest"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        // Run unittest discover with verbose output
        Ok(TestCommand {
            program: "python3".into(),
            args: vec![
                "-m".into(),
                "unittest".into(),
                "discover".into(),
                "-v".into(),
                "-s".into(),
                ".".into(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        // test_name format: "test_module.TestClass.test_method"
        Ok(TestCommand {
            program: "python3".into(),
            args: vec!["-m".into(), "unittest".into(), test_name.into(), "-v".into()],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, _exit_code: i32) -> TestResult {
        parse_unittest_output(stdout, stderr)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        // Same logic as pytest
        let mut traces = Vec::new();

        if let Some(ref file) = failure.file {
            if let Some(filename) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
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

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
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

fn has_unittest_test_files(root: &Path) -> bool {
    // Check for test_*.py files that use unittest
    for dir in ["tests", "test", "."] {
        let test_dir = root.join(dir);
        if test_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&test_dir) {
                for entry in entries.flatten() {
                    if let Ok(file_type) = entry.file_type() {
                        if file_type.is_file() {
                            let name = entry.file_name();
                            let name = name.to_string_lossy();
                            if name.starts_with("test_") && name.ends_with(".py") {
                                // Quick check if file contains "unittest" import
                                if let Ok(content) = std::fs::read_to_string(entry.path()) {
                                    if content.contains("import unittest")
                                        || content.contains("from unittest")
                                    {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

/// Parse unittest verbose output.
/// Format:
/// ```
/// test_method (test_module.TestClass) ... ok
/// test_another (test_module.TestClass) ... FAIL
/// test_skip (test_module.TestClass) ... skipped 'reason'
///
/// ======================================================================
/// FAIL: test_another (test_module.TestClass)
/// ----------------------------------------------------------------------
/// Traceback (most recent call last):
///   File "/path/to/test.py", line 15, in test_another
///     self.assertEqual(1, 2)
/// AssertionError: 1 != 2
///
/// ----------------------------------------------------------------------
/// Ran 3 tests in 0.001s
///
/// FAILED (failures=1, skipped=1)
/// ```
fn parse_unittest_output(stdout: &str, stderr: &str) -> TestResult {
    let combined = format!("{}\n{}", stdout, stderr);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // Parse test result lines
    let test_line_re = Regex::new(r"^(\w+)\s+\(([^)]+)\)\s+\.\.\.\s+(ok|FAIL|ERROR|skipped)").unwrap();

    for line in combined.lines() {
        if let Some(caps) = test_line_re.captures(line) {
            let test_method = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let test_class = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let outcome = caps.get(3).map(|m| m.as_str()).unwrap_or("");

            let test_name = format!("{}.{}", test_class, test_method);

            match outcome {
                "ok" => {
                    passed += 1;
                    all_tests.push(TestDetail {
                        name: test_name,
                        status: TestStatus::Pass,
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: None,
                    });
                }
                "FAIL" | "ERROR" => {
                    failed += 1;
                    all_tests.push(TestDetail {
                        name: test_name.clone(),
                        status: TestStatus::Fail,
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: Some(format!("{} test", outcome)),
                    });
                }
                "skipped" => {
                    skipped += 1;
                    all_tests.push(TestDetail {
                        name: test_name,
                        status: TestStatus::Skip,
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: Some("skipped".to_string()),
                    });
                }
                _ => {}
            }
        }
    }

    // Parse failure details
    let fail_block_re = Regex::new(
        r"(?s)(?:FAIL|ERROR):\s+(\w+)\s+\(([^)]+)\)\s*\n-+\n(.*?)(?:\n-+|\n=+|$)",
    )
    .unwrap();

    for caps in fail_block_re.captures_iter(&combined) {
        let test_method = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let test_class = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let message_block = caps.get(3).map(|m| m.as_str()).unwrap_or("");

        let test_name = format!("{}.{}", test_class, test_method);

        // Try to extract file and line from traceback
        let file_line_re = Regex::new(r#"File "([^"]+)", line (\d+)"#).unwrap();
        let (file, line) = if let Some(caps) = file_line_re.captures(message_block) {
            (
                caps.get(1).map(|m| m.as_str().to_string()),
                caps.get(2)
                    .and_then(|m| m.as_str().parse::<u32>().ok()),
            )
        } else {
            (None, None)
        };

        failures.push(TestFailure {
            name: test_name.clone(),
            file,
            line,
            message: message_block.trim().to_string(),
            rerun: Some(test_name),
            suggested_traces: vec![],
        });
    }

    // Add suggested traces to failures
    for failure in &mut failures {
        failure.suggested_traces = extract_python_traces_from_unittest(failure);
    }

    // Extract duration from summary line
    let duration_re = Regex::new(r"Ran \d+ tests? in ([\d.]+)s").unwrap();
    let duration_ms = if let Some(caps) = duration_re.captures(&combined) {
        if let Some(secs) = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()) {
            (secs * 1000.0) as u64
        } else {
            0
        }
    } else {
        0
    };

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

fn extract_python_traces_from_unittest(failure: &TestFailure) -> Vec<String> {
    let mut traces = Vec::new();

    // Extract module from class name: "test_module.TestClass"
    if let Some(dot_pos) = failure.name.rfind('.') {
        let module_class = &failure.name[..dot_pos];
        if let Some(module_name) = module_class.split('.').next() {
            // "test_module" → trace corresponding module
            let target_module = module_name.strip_prefix("test_").unwrap_or(module_name);
            traces.push(format!("{}.*", target_module));
        }
    }

    if let Some(ref file) = failure.file {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unittest_output() {
        let output = r#"
test_generate (test_audio.TestAudio) ... ok
test_process (test_audio.TestAudio) ... FAIL
test_skipped (test_audio.TestAudio) ... skipped 'not implemented'

======================================================================
FAIL: test_process (test_audio.TestAudio)
----------------------------------------------------------------------
Traceback (most recent call last):
  File "/path/to/test_audio.py", line 15, in test_process
    self.assertEqual(result, 1.0)
AssertionError: 0.0 != 1.0

----------------------------------------------------------------------
Ran 3 tests in 0.012s

FAILED (failures=1, skipped=1)
"#;

        let result = parse_unittest_output(output, "");
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].name.contains("test_process"));
        assert!(result.failures[0].file.is_some());
    }

    #[test]
    fn test_suggest_traces_unittest() {
        let failure = TestFailure {
            name: "test_audio.TestAudio.test_process".to_string(),
            file: Some("/path/to/test_audio.py".to_string()),
            line: Some(15),
            message: "AssertionError".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = extract_python_traces_from_unittest(&failure);
        assert!(!traces.is_empty());
        assert!(traces.iter().any(|t| t.contains("audio")));
    }
}
