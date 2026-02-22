use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde::Deserialize;

use super::adapter::*;
use super::TestProgress;

pub struct MochaAdapter;

#[derive(Deserialize)]
struct MochaReport {
    stats: MochaStat,
    #[serde(default)]
    passes: Vec<MochaTest>,
    #[serde(default)]
    failures: Vec<MochaFailure>,
    #[serde(default)]
    pending: Vec<MochaTest>,
}

#[derive(Deserialize)]
struct MochaStat {
    #[serde(default)]
    passes: u32,
    #[serde(default)]
    failures: u32,
    #[serde(default)]
    pending: u32,
    #[serde(default)]
    duration: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MochaTest {
    title: String,
    #[serde(default)]
    full_title: String,
    #[serde(default)]
    duration: Option<u64>,
    #[allow(dead_code)]
    #[serde(default)]
    file: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MochaFailure {
    title: String,
    #[serde(default)]
    full_title: String,
    #[serde(default)]
    duration: Option<u64>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    err: Option<MochaError>,
}

#[derive(Deserialize)]
struct MochaError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    stack: Option<String>,
}

impl TestAdapter for MochaAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Check for vitest or jest in package.json first -- they take priority
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                return 0;
            }
        }

        // Check for mocharc config files
        for cfg in &[
            ".mocharc.yml",
            ".mocharc.yaml",
            ".mocharc.json",
            ".mocharc.js",
            ".mocharc.cjs",
        ] {
            if project_root.join(cfg).exists() {
                return 90;
            }
        }

        // Check for mocha in package.json
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"mocha\"") {
                return 80;
            }
        }

        0
    }

    fn name(&self) -> &str {
        "mocha"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "mocha".to_string(),
                "--reporter".to_string(),
                "json".to_string(),
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
            program: "npx".to_string(),
            args: vec![
                "mocha".to_string(),
                "--reporter".to_string(),
                "json".to_string(),
                "--grep".to_string(),
                test_name.to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        if let Some(report) = extract_mocha_json(stdout).or_else(|| extract_mocha_json(stderr)) {
            return build_result_from_report(report);
        }

        // Fallback: could not parse JSON
        let failures = if exit_code != 0 {
            let preview: String = stderr.chars().take(500).collect();
            vec![TestFailure {
                name: "Test run crashed".to_string(),
                file: None,
                line: None,
                message: format!("Could not parse mocha JSON output.\nstderr: {}", preview),
                rerun: None,
                suggested_traces: vec![],
            }]
        } else {
            vec![]
        };

        TestResult {
            summary: TestSummary {
                passed: 0,
                failed: 0,
                skipped: 0,
                stuck: None,
                duration_ms: 0,
            },
            failures,
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
                .trim_end_matches(".spec");
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

/// Try to extract a MochaReport from a string that may contain non-JSON noise.
///
/// Strategy: find the last occurrence of `"stats"` in the text, then walk backwards
/// to find the enclosing `{`. Try to parse from there. This handles both minified
/// JSON (`{"stats":...}`) and pretty-printed JSON with whitespace.
fn extract_mocha_json(text: &str) -> Option<MochaReport> {
    // Find the last occurrence of `"stats"` -- Mocha's JSON reporter always
    // includes a "stats" key at the top level. Using the last occurrence skips
    // any noise that tests may have printed to stdout.
    let stats_pos = text.rfind("\"stats\"")?;

    // Walk backwards from "stats" to find the opening `{`
    let before = &text[..stats_pos];
    let brace_pos = before.rfind('{')?;
    let candidate = &text[brace_pos..];

    serde_json::from_str(candidate).ok()
}

/// Build a TestResult from a successfully parsed MochaReport.
fn build_result_from_report(report: MochaReport) -> TestResult {
    let stack_re =
        regex::Regex::new(r"at\s+\S+\s+\(([^)]+):(\d+):\d+\)").unwrap();

    let mut all_tests = Vec::new();
    let mut failures = Vec::new();

    // Passing tests
    for test in &report.passes {
        let name = if test.full_title.is_empty() {
            test.title.clone()
        } else {
            test.full_title.clone()
        };
        all_tests.push(TestDetail {
            name,
            status: TestStatus::Pass,
            duration_ms: test.duration.unwrap_or(0),
            stdout: None,
            stderr: None,
            message: None,
        });
    }

    // Pending (skipped) tests
    for test in &report.pending {
        let name = if test.full_title.is_empty() {
            test.title.clone()
        } else {
            test.full_title.clone()
        };
        all_tests.push(TestDetail {
            name,
            status: TestStatus::Skip,
            duration_ms: test.duration.unwrap_or(0),
            stdout: None,
            stderr: None,
            message: None,
        });
    }

    // Failing tests
    for fail in &report.failures {
        let name = if fail.full_title.is_empty() {
            fail.title.clone()
        } else {
            fail.full_title.clone()
        };

        let (message, file, line) = match &fail.err {
            Some(err) => {
                let msg = err.message.clone();
                let (f, l) = extract_location_from_stack(
                    err.stack.as_deref(),
                    fail.file.as_deref(),
                    &stack_re,
                );
                (msg, f, l)
            }
            None => {
                let f = fail.file.clone();
                (String::new(), f, None)
            }
        };

        failures.push(TestFailure {
            name: name.clone(),
            file: file.clone(),
            line,
            message: message.clone(),
            rerun: Some(name.clone()),
            suggested_traces: vec![],
        });

        all_tests.push(TestDetail {
            name,
            status: TestStatus::Fail,
            duration_ms: fail.duration.unwrap_or(0),
            stdout: None,
            stderr: None,
            message: Some(message),
        });
    }

    TestResult {
        summary: TestSummary {
            passed: report.stats.passes,
            failed: report.stats.failures,
            skipped: report.stats.pending,
            stuck: None,
            duration_ms: report.stats.duration,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract file and line from an error stack trace or fall back to the test's file field.
fn extract_location_from_stack(
    stack: Option<&str>,
    test_file: Option<&str>,
    stack_re: &regex::Regex,
) -> (Option<String>, Option<u32>) {
    if let Some(stack) = stack {
        if let Some(caps) = stack_re.captures(stack) {
            let file = caps[1].to_string();
            let line = caps[2].parse().ok();
            return (Some(file), line);
        }
    }
    (test_file.map(|s| s.to_string()), None)
}

/// Update progress from Mocha output.
///
/// Mocha's JSON reporter outputs everything at the end, so real-time progress
/// tracking is limited. We transition to Running on first output and try to
/// detect summary lines in stderr.
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();

    // Transition to Running on first output
    if p.phase == super::TestPhase::Compiling {
        p.phase = super::TestPhase::Running;
    }

    // Try to detect passing/failing counts from stderr summary lines
    let trimmed = line.trim();
    if trimmed.contains("passing") {
        // e.g. "  2 passing (50ms)"
        if let Some(count) = trimmed.split_whitespace().next().and_then(|s| s.parse::<u32>().ok())
        {
            p.passed = count;
        }
    } else if trimmed.contains("failing") {
        // e.g. "  1 failing"
        if let Some(count) = trimmed.split_whitespace().next().and_then(|s| s.parse::<u32>().ok())
        {
            p.failed = count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOCHA_PASS: &str = r#"{
        "stats": { "suites": 1, "tests": 2, "passes": 2, "failures": 0, "pending": 0, "duration": 50 },
        "passes": [
            { "title": "adds numbers", "fullTitle": "Calculator adds numbers", "duration": 5, "file": "test/calc.test.js" },
            { "title": "subtracts", "fullTitle": "Calculator subtracts", "duration": 3, "file": "test/calc.test.js" }
        ],
        "failures": [],
        "pending": []
    }"#;

    const MOCHA_FAIL: &str = r#"{
        "stats": { "suites": 1, "tests": 2, "passes": 1, "failures": 1, "pending": 0, "duration": 80 },
        "passes": [
            { "title": "adds numbers", "fullTitle": "Calculator adds numbers", "duration": 5, "file": "test/calc.test.js" }
        ],
        "failures": [
            {
                "title": "multiplies",
                "fullTitle": "Calculator multiplies",
                "duration": 8,
                "file": "test/calc.test.js",
                "err": {
                    "message": "expected 5 to equal 6",
                    "stack": "AssertionError: expected 5 to equal 6\n    at Context.<anonymous> (test/calc.test.js:15:10)"
                }
            }
        ],
        "pending": []
    }"#;

    #[test]
    fn test_detect_mocha() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = MochaAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join(".mocharc.yml"), "timeout: 5000").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_detect_mocha_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies": {"mocha": "^10.0.0"}}"#,
        )
        .unwrap();
        let adapter = MochaAdapter;
        assert!(adapter.detect(dir.path(), None) >= 80);
    }

    #[test]
    fn test_detect_mocha_yields_to_vitest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies": {"mocha": "^10.0.0", "vitest": "^1.0.0"}}"#,
        )
        .unwrap();
        let adapter = MochaAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);
    }

    #[test]
    fn test_parse_passing() {
        let result = MochaAdapter.parse_output(MOCHA_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
        assert!(result.all_tests.iter().all(|t| t.status == TestStatus::Pass));
    }

    #[test]
    fn test_parse_failing() {
        let result = MochaAdapter.parse_output(MOCHA_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Calculator multiplies");
        assert!(f.message.contains("expected 5 to equal 6"));
        assert_eq!(f.rerun.as_deref(), Some("Calculator multiplies"));
        assert!(
            f.file.as_deref().unwrap_or("").contains("calc.test.js"),
            "file should be extracted from stack, got: {:?}",
            f.file
        );
        assert_eq!(f.line, Some(15));
    }

    #[test]
    fn test_parse_with_noise() {
        // Console output mixed before the JSON report
        let noisy = format!(
            "Starting tests...\nSome debug output\nconsole.log('hello')\n{}",
            MOCHA_PASS
        );
        let result = MochaAdapter.parse_output(&noisy, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = MochaAdapter
            .suite_command(dir.path(), None, &Default::default())
            .unwrap();
        assert_eq!(cmd.program, "npx");
        assert!(cmd.args.iter().any(|a| a == "mocha"));
        assert!(cmd.args.iter().any(|a| a == "json"));
    }
}
