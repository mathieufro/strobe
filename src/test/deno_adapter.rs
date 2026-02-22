use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::adapter::*;
use super::TestProgress;

pub struct DenoAdapter;

impl TestAdapter for DenoAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("deno.json").exists() {
            return 92;
        }
        if project_root.join("deno.jsonc").exists() {
            return 92;
        }
        if project_root.join("deno.lock").exists() {
            return 85;
        }
        0
    }

    fn name(&self) -> &str {
        "deno"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "deno".to_string(),
            args: vec![
                "test".to_string(),
                "--reporter=junit".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "deno".to_string(),
            args: vec![
                "test".to_string(),
                "--reporter=junit".to_string(),
                format!("--filter={}", test_name),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, _exit_code: i32) -> TestResult {
        // Deno may mix human-readable output before the JUnit XML.
        // Try stdout first, then stderr (some Deno versions output XML to stderr).
        if let Some(xml) = extract_xml(stdout) {
            let mut result = super::bun_adapter::parse_junit_xml(xml);
            set_rerun_on_failures(&mut result.failures);
            return result;
        }
        if let Some(xml) = extract_xml(stderr) {
            let mut result = super::bun_adapter::parse_junit_xml(xml);
            set_rerun_on_failures(&mut result.failures);
            return result;
        }

        // Fallback: no XML found — report a generic failure with stderr preview
        let preview: String = stderr.chars().take(500).collect();
        TestResult {
            summary: TestSummary {
                passed: 0,
                failed: if preview.is_empty() { 0 } else { 1 },
                skipped: 0,
                stuck: None,
                duration_ms: 0,
            },
            failures: if preview.is_empty() {
                vec![]
            } else {
                vec![TestFailure {
                    name: "Deno test run".to_string(),
                    file: None,
                    line: None,
                    message: format!("Could not parse Deno test output (no JUnit XML found).\nstderr: {}", preview),
                    rerun: None,
                    suggested_traces: vec![],
                }]
            },
            stuck: vec![],
            all_tests: vec![],
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            let module = stem
                .trim_end_matches(".test")
                .trim_end_matches(".spec")
                .trim_end_matches("_test");
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

/// Extract JUnit XML from mixed output by finding the XML start marker.
/// Deno may prepend human-readable test progress lines before the XML.
fn extract_xml(output: &str) -> Option<&str> {
    if let Some(pos) = output.find("<?xml") {
        return Some(&output[pos..]);
    }
    if let Some(pos) = output.find("<testsuites") {
        return Some(&output[pos..]);
    }
    None
}

/// Set `rerun` on all failures so the LLM can re-run individual failed tests.
fn set_rerun_on_failures(failures: &mut [TestFailure]) {
    for failure in failures.iter_mut() {
        if failure.rerun.is_none() {
            failure.rerun = Some(failure.name.clone());
        }
    }
}

/// Parse Deno test output lines and update progress incrementally.
/// Deno's human-readable output uses lines like:
///   "test <name> ..." to indicate a test is starting
///   "... ok (Xms)" or "... FAILED (Xms)" for completion
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    // Transition from Compiling to Running on first test output
    if trimmed.starts_with("test ") || trimmed.starts_with("running ") {
        let mut p = progress.lock().unwrap();
        if p.phase == super::TestPhase::Compiling {
            p.phase = super::TestPhase::Running;
        }
    }

    // Handle "test <name> ..." lines.
    // Deno outputs either:
    //   "test <name> ..."          (start only, result on next line)
    //   "test <name> ... ok (Xms)" (single-line start + result)
    //   "test <name> ... FAILED (Xms)"
    if let Some(rest) = trimmed.strip_prefix("test ") {
        if let Some(dots_pos) = rest.rfind(" ...") {
            let name = &rest[..dots_pos];
            let after_dots = rest[dots_pos + 4..].trim();

            let mut p = progress.lock().unwrap();
            if after_dots.is_empty() {
                // Test started, no result yet
                p.running_tests.insert(name.to_string(), Instant::now());
            } else if after_dots.starts_with("ok") {
                p.passed += 1;
                p.running_tests.remove(name);
            } else if after_dots.starts_with("FAILED") {
                p.failed += 1;
                p.running_tests.remove(name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNIT_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="deno test" tests="2" failures="0" time="0.035">
  <testsuite name="math_test.ts" tests="2" failures="0" time="0.030">
    <testcase name="adds two numbers" classname="math_test.ts" time="0.010"/>
    <testcase name="subtracts two numbers" classname="math_test.ts" time="0.008"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="deno test" tests="2" failures="1" time="0.045">
  <testsuite name="math_test.ts" tests="2" failures="1" time="0.040">
    <testcase name="multiplies correctly" classname="math_test.ts" time="0.012">
      <failure message="Expected 6, got 5" type="AssertionError">
AssertionError: Expected 6, got 5
    at math_test.ts:15:7
      </failure>
    </testcase>
    <testcase name="adds correctly" classname="math_test.ts" time="0.006"/>
  </testsuite>
</testsuites>"#;

    #[test]
    fn test_detect_deno() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DenoAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("deno.json"), "{}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90, "deno.json should yield high confidence");
    }

    #[test]
    fn test_detect_deno_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DenoAdapter;

        std::fs::write(dir.path().join("deno.jsonc"), "{}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90, "deno.jsonc should yield high confidence");
    }

    #[test]
    fn test_parse_passing() {
        let result = DenoAdapter.parse_output(JUNIT_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
    }

    #[test]
    fn test_parse_failing() {
        let result = DenoAdapter.parse_output(JUNIT_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "multiplies correctly");
        assert!(f.message.contains("Expected 6"));
        assert!(f.rerun.is_some(), "rerun should be set for failures");
        assert_eq!(f.rerun.as_deref(), Some("multiplies correctly"));
    }

    #[test]
    fn test_parse_with_preamble() {
        // Deno may output human-readable progress before the JUnit XML
        let output = format!(
            "running 2 tests from ./math_test.ts\ntest adds ... ok (5ms)\ntest subs ... ok (3ms)\n\n{}",
            JUNIT_PASS
        );
        let result = DenoAdapter.parse_output(&output, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn test_parse_xml_in_stderr() {
        // Some Deno versions may output JUnit XML to stderr
        let result = DenoAdapter.parse_output("", JUNIT_PASS, 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn test_parse_no_xml_fallback() {
        let result = DenoAdapter.parse_output("", "error: Module not found", 1);
        assert_eq!(result.summary.failed, 1);
        assert!(result.failures[0].message.contains("Module not found"));
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = DenoAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert_eq!(cmd.program, "deno");
        assert!(cmd.args.iter().any(|a| a.contains("junit")));
    }

    #[test]
    fn test_single_test_command_filter() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = DenoAdapter.single_test_command(dir.path(), "adds two numbers").unwrap();
        assert_eq!(cmd.program, "deno");
        assert!(cmd.args.iter().any(|a| a.contains("filter")));
        assert!(cmd.args.iter().any(|a| a.contains("adds two numbers")));
    }

    #[test]
    fn test_suggest_traces() {
        let failure = TestFailure {
            name: "multiplies correctly".to_string(),
            file: Some("math_test.ts".to_string()),
            line: Some(15),
            message: "AssertionError".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = DenoAdapter.suggest_traces(&failure);
        assert!(traces.iter().any(|t| t.contains("@file:math_test")));
        assert!(traces.iter().any(|t| t.contains("math.*")));
    }

    #[test]
    fn test_default_timeouts() {
        let adapter = DenoAdapter;
        assert_eq!(adapter.default_timeout(Some(TestLevel::Unit)), 60_000);
        assert_eq!(adapter.default_timeout(Some(TestLevel::Integration)), 180_000);
        assert_eq!(adapter.default_timeout(Some(TestLevel::E2e)), 300_000);
        assert_eq!(adapter.default_timeout(None), 120_000);
    }

    #[test]
    fn test_update_progress_running_transition() {
        let progress = Arc::new(Mutex::new(TestProgress::new()));
        assert_eq!(progress.lock().unwrap().phase, super::super::TestPhase::Compiling);

        update_progress("running 3 tests from ./math_test.ts", &progress);
        assert_eq!(progress.lock().unwrap().phase, super::super::TestPhase::Running);
    }

    #[test]
    fn test_update_progress_tracks_tests() {
        let progress = Arc::new(Mutex::new(TestProgress::new()));

        update_progress("test adds two numbers ... ok (5ms)", &progress);
        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1);
        assert!(!p.running_tests.contains_key("adds two numbers"));
    }

    #[test]
    fn test_update_progress_tracks_failures() {
        let progress = Arc::new(Mutex::new(TestProgress::new()));

        update_progress("test multiply ... FAILED (12ms)", &progress);
        let p = progress.lock().unwrap();
        assert_eq!(p.failed, 1);
        assert!(!p.running_tests.contains_key("multiply"));
    }

    #[test]
    fn test_extract_xml_with_preamble() {
        let input = "some output\nmore output\n<?xml version=\"1.0\"?><testsuites/>";
        let xml = extract_xml(input);
        assert!(xml.is_some());
        assert!(xml.unwrap().starts_with("<?xml"));
    }

    #[test]
    fn test_extract_xml_testsuites_start() {
        let input = "output\n<testsuites name=\"deno\"></testsuites>";
        let xml = extract_xml(input);
        assert!(xml.is_some());
        assert!(xml.unwrap().starts_with("<testsuites"));
    }

    #[test]
    fn test_extract_xml_no_xml() {
        assert!(extract_xml("no xml here").is_none());
    }
}
