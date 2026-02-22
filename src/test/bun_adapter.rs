use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;

pub struct BunAdapter;

impl TestAdapter for BunAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Check for Vitest/Jest first — Bun as package manager doesn't mean Bun as test runner
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                // Other framework present — only claim high confidence if bun:test is explicit
                if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") { return 90; }
                return 0; // Let Vitest/Jest adapters handle it
            }
        }
        if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
            return 85;
        }
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") { return 90; }
            if pkg.contains("\"bun\"") { return 75; }
        }
        0
    }

    fn name(&self) -> &str { "bun" }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "test".to_string(),
                "--reporter=junit".to_string(),
                "--reporter-outfile=/dev/stdout".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "test".to_string(),
                "--reporter=junit".to_string(),
                "--reporter-outfile=/dev/stdout".to_string(),
                "--test-name-pattern".to_string(),
                test_name.to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, _stderr: &str, _exit_code: i32) -> TestResult {
        parse_junit_xml(stdout)
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
            Some(TestLevel::Unit) => 60_000,
            Some(TestLevel::Integration) => 180_000,
            Some(TestLevel::E2e) => 300_000,
            None => 120_000,
        }
    }
}

/// Parse JUnit XML (bun:test --reporter=junit) into TestResult.
pub(crate) fn parse_junit_xml(xml: &str) -> TestResult {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // State for current testcase
    let mut in_testcase = false;
    let mut tc_name = String::new();
    let mut tc_classname = String::new();
    let mut tc_duration_ms = 0u64;
    let mut tc_failure_msg = String::new();
    let mut tc_failure_body = String::new();
    let mut tc_skipped = false;
    let mut tc_failed = false;
    let mut reading_failure = false;

    // Helper closure to finalize a testcase
    let finalize_testcase = |passed: &mut u32, failed: &mut u32, skipped: &mut u32,
                             failures: &mut Vec<TestFailure>, all_tests: &mut Vec<TestDetail>,
                             tc_name: &str, tc_classname: &str, tc_duration_ms: u64,
                             tc_skipped: bool, tc_failed: bool,
                             tc_failure_msg: &str, tc_failure_body: &str| {
        if tc_skipped {
            *skipped += 1;
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Skip,
                duration_ms: tc_duration_ms,
                stdout: None, stderr: None, message: None,
            });
        } else if tc_failed {
            *failed += 1;
            let message = if !tc_failure_body.is_empty() {
                tc_failure_body.to_string()
            } else {
                tc_failure_msg.to_string()
            };
            let file = if !tc_classname.is_empty() { Some(tc_classname.to_string()) } else { None };

            failures.push(TestFailure {
                name: tc_name.to_string(),
                file: file.clone(),
                line: None,
                message: message.clone(),
                rerun: None,
                suggested_traces: vec![],
            });
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Fail,
                duration_ms: tc_duration_ms,
                stdout: None, stderr: None,
                message: Some(message),
            });
        } else {
            *passed += 1;
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Pass,
                duration_ms: tc_duration_ms,
                stdout: None, stderr: None, message: None,
            });
        }
    };

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) => {
                match e.local_name().as_ref() {
                    b"testcase" => {
                        // Self-closing <testcase .../> — a passing test
                        let name = get_attr(e, "name");
                        let classname = get_attr(e, "classname");
                        let secs = get_attr(e, "time");
                        let dur = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                        finalize_testcase(&mut passed, &mut failed, &mut skipped,
                            &mut failures, &mut all_tests,
                            &name, &classname, dur, false, false, "", "");
                    }
                    b"skipped" if in_testcase => {
                        tc_skipped = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Start(ref e)) => {
                match e.local_name().as_ref() {
                    b"testcase" => {
                        in_testcase = true;
                        tc_name = get_attr(e, "name");
                        tc_classname = get_attr(e, "classname");
                        let secs = get_attr(e, "time");
                        tc_duration_ms = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                        tc_failure_msg.clear();
                        tc_failure_body.clear();
                        tc_skipped = false;
                        tc_failed = false;
                        reading_failure = false;
                    }
                    b"failure" if in_testcase => {
                        tc_failed = true;
                        tc_failure_msg = get_attr(e, "message");
                        reading_failure = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if reading_failure {
                    tc_failure_body = e.unescape().unwrap_or_default().to_string();
                }
            }
            Ok(Event::End(ref e)) => {
                match e.local_name().as_ref() {
                    b"failure" => {
                        reading_failure = false;
                    }
                    b"testcase" => {
                        finalize_testcase(&mut passed, &mut failed, &mut skipped,
                            &mut failures, &mut all_tests,
                            &tc_name, &tc_classname, tc_duration_ms,
                            tc_skipped, tc_failed, &tc_failure_msg, &tc_failure_body);
                        in_testcase = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let total_duration: u64 = all_tests.iter().map(|t| t.duration_ms).sum();

    TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped,
            stuck: None,
            duration_ms: total_duration,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract an attribute value from an XML element.
fn get_attr(e: &quick_xml::events::BytesStart, name: &str) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name.as_bytes())
        .and_then(|a| {
            a.unescape_value().ok().map(|s| s.to_string())
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNIT_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="0" time="0.050">
  <testsuite name="calc.test.ts" tests="2" failures="0" time="0.040">
    <testcase name="Math > adds two numbers" classname="calc.test.ts" time="0.005"/>
    <testcase name="Math > subs two numbers" classname="calc.test.ts" time="0.003"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="1" time="0.060">
  <testsuite name="calc.test.ts" tests="2" failures="1" time="0.050">
    <testcase name="Math > multiplies" classname="calc.test.ts" time="0.008">
      <failure message="Expected 6, got 5" type="AssertionError">
AssertionError: Expected 6, got 5
    at &lt;anonymous&gt; (calc.test.ts:12:7)
      </failure>
    </testcase>
    <testcase name="Math > adds" classname="calc.test.ts" time="0.005"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_SKIP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="1" failures="0" time="0.010">
  <testsuite name="todo.test.ts" tests="1" failures="0" time="0.005">
    <testcase name="todo test" classname="todo.test.ts" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn test_detect_bun() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = BunAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("bun.lockb"), b"").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 85, "bun.lockb → high confidence");
    }

    #[test]
    fn test_detect_bun_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            r#"{"scripts": {"test": "bun test"}}"#).unwrap();
        let adapter = BunAdapter;
        assert!(adapter.detect(dir.path(), None) >= 80);
    }

    #[test]
    fn test_parse_passing_junit() {
        let result = BunAdapter.parse_output(JUNIT_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing_junit() {
        let result = BunAdapter.parse_output(JUNIT_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        let f = &result.failures[0];
        assert_eq!(f.name, "Math > multiplies");
        assert!(f.message.contains("Expected 6"));
        assert!(f.file.as_deref().unwrap_or("").ends_with("calc.test.ts"));
    }

    #[test]
    fn test_parse_skipped_junit() {
        let result = BunAdapter.parse_output(JUNIT_SKIP, "", 0);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.summary.passed, 0);
    }

    #[test]
    fn test_parse_xml_entities_unescaped() {
        let result = BunAdapter.parse_output(JUNIT_FAIL, "", 1);
        let msg = &result.failures[0].message;
        assert!(msg.contains("<anonymous>"), "XML entities should be decoded, got: {}", msg);
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert_eq!(cmd.program, "bun");
        assert!(cmd.args.iter().any(|a| a.contains("junit")));
        assert!(cmd.args.iter().any(|a| a.contains("reporter-outfile")));
    }
}
