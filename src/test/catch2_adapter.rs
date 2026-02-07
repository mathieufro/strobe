use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;
use super::cargo_adapter::capture_native_stacks;

pub struct Catch2Adapter;

impl TestAdapter for Catch2Adapter {
    fn detect(&self, _project_root: &Path, command: Option<&str>) -> u8 {
        if let Some(cmd) = command {
            if Path::new(cmd).exists() {
                let output = std::process::Command::new(cmd)
                    .arg("--list-tests")
                    .output();
                match output {
                    Ok(o) if o.status.success() => return 85,
                    _ => return 0,
                }
            }
        }
        0
    }

    fn name(&self) -> &str {
        "catch2"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::Frida(
            "Catch2 adapter requires a test binary path via the 'command' parameter".to_string()
        ))
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        _test_name: &str,
    ) -> crate::Result<TestCommand> {
        Err(crate::Error::Frida(
            "Catch2 adapter requires a test binary path via the 'command' parameter".to_string()
        ))
    }

    fn parse_output(
        &self,
        stdout: &str,
        _stderr: &str,
        _exit_code: i32,
    ) -> TestResult {
        parse_catch2_xml(stdout)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        if let Some(ref file) = failure.file {
            if let Some(filename) = Path::new(file).file_name().and_then(|n| n.to_str()) {
                traces.push(format!("@file:{}", filename));
            }
        }

        traces
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        capture_native_stacks(pid)
    }
}

impl Catch2Adapter {
    /// Build command for a given binary with Catch2 flags.
    pub fn command_for_binary(
        binary: &str,
        level: Option<TestLevel>,
    ) -> TestCommand {
        let mut args = vec!["--reporter".to_string(), "xml".to_string()];

        match level {
            Some(TestLevel::Unit) => args.push("[unit]".to_string()),
            Some(TestLevel::Integration) => args.push("[integration]".to_string()),
            Some(TestLevel::E2e) => args.push("[e2e]".to_string()),
            None => {}
        }

        TestCommand {
            program: binary.to_string(),
            args,
            env: HashMap::new(),
        }
    }

    /// Build command for running a single test in a Catch2 binary.
    pub fn single_test_for_binary(binary: &str, test_name: &str) -> TestCommand {
        TestCommand {
            program: binary.to_string(),
            args: vec![
                "--reporter".to_string(),
                "xml".to_string(),
                test_name.to_string(),
            ],
            env: HashMap::new(),
        }
    }
}

/// Parse Catch2 XML reporter output into TestResult.
fn parse_catch2_xml(xml: &str) -> TestResult {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // State for current TestCase
    let mut in_test_case = false;
    let mut tc_name = String::new();
    let mut tc_file = String::new();
    let mut tc_line = 0u32;
    let mut tc_success = true;
    let mut tc_duration_ms = 0u64;

    // State for current Expression (assertion failure)
    let mut _in_expression = false;
    let mut expr_file = String::new();
    let mut expr_line = 0u32;
    let mut expr_original = String::new();
    let mut expr_expanded = String::new();
    let mut reading_original = false;
    let mut reading_expanded = false;

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                let local_name = e.local_name();
                match local_name.as_ref() {
                    b"TestCase" => {
                        in_test_case = true;
                        tc_success = true;
                        tc_name = get_attr(e, "name");
                        tc_file = get_attr(e, "filename");
                        tc_line = get_attr(e, "line").parse().unwrap_or(0);
                        tc_duration_ms = 0;
                        expr_original.clear();
                        expr_expanded.clear();
                        expr_file.clear();
                        expr_line = 0;
                    }
                    b"Expression" => {
                        _in_expression = true;
                        let success = get_attr(e, "success");
                        if success == "false" {
                            tc_success = false;
                            expr_file = get_attr(e, "filename");
                            expr_line = get_attr(e, "line").parse().unwrap_or(0);
                        }
                        expr_original.clear();
                        expr_expanded.clear();
                    }
                    b"Original" => {
                        reading_original = true;
                    }
                    b"Expanded" => {
                        reading_expanded = true;
                    }
                    b"OverallResult" if in_test_case => {
                        let secs = get_attr(e, "durationInSeconds");
                        tc_duration_ms = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                        let success = get_attr(e, "success");
                        if success == "false" {
                            tc_success = false;
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(ref e)) => {
                match e.local_name().as_ref() {
                    b"TestCase" => {
                        if tc_success {
                            passed += 1;
                            all_tests.push(TestDetail {
                                name: tc_name.clone(),
                                status: "pass".to_string(),
                                duration_ms: tc_duration_ms,
                                stdout: None,
                                stderr: None,
                                message: None,
                            });
                        } else {
                            failed += 1;
                            let message = if !expr_expanded.is_empty() {
                                format!("REQUIRE( {} )\nwith expansion:\n  {}", expr_original, expr_expanded)
                            } else {
                                "Test failed".to_string()
                            };

                            failures.push(TestFailure {
                                name: tc_name.clone(),
                                file: if !expr_file.is_empty() { Some(expr_file.clone()) } else if !tc_file.is_empty() { Some(tc_file.clone()) } else { None },
                                line: if expr_line > 0 { Some(expr_line) } else if tc_line > 0 { Some(tc_line) } else { None },
                                message: message.clone(),
                                rerun: Some(tc_name.clone()),
                                suggested_traces: vec![],
                            });

                            all_tests.push(TestDetail {
                                name: tc_name.clone(),
                                status: "fail".to_string(),
                                duration_ms: tc_duration_ms,
                                stdout: None,
                                stderr: None,
                                message: Some(message),
                            });
                        }
                        in_test_case = false;
                    }
                    b"Expression" => {
                        _in_expression = false;
                    }
                    b"Original" => {
                        reading_original = false;
                    }
                    b"Expanded" => {
                        reading_expanded = false;
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(ref e)) => {
                if reading_original {
                    expr_original = e.unescape().unwrap_or_default().to_string();
                } else if reading_expanded {
                    expr_expanded = e.unescape().unwrap_or_default().to_string();
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
            skipped: 0,
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
        .and_then(|a| String::from_utf8(a.value.to_vec()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_catch2_xml_all_pass() {
        let adapter = Catch2Adapter;
        let stdout = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="12345" catch2-version="3.5.0">
  <TestCase name="Addition works" tags="[unit][math]" filename="test_math.cpp" line="10">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <TestCase name="Subtraction works" tags="[unit][math]" filename="test_math.cpp" line="20">
    <OverallResult success="true" durationInSeconds="0.002"/>
  </TestCase>
  <OverallResults successes="4" failures="0" expectedFailures="0"/>
  <OverallResultsCases successes="2" failures="0" expectedFailures="0"/>
</Catch2TestRun>"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_catch2_xml_with_failure() {
        let adapter = Catch2Adapter;
        let stdout = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="12345" catch2-version="3.5.0">
  <TestCase name="Parser handles empty" tags="[unit]" filename="test_parser.cpp" line="15">
    <Expression success="false" type="REQUIRE" filename="test_parser.cpp" line="18">
      <Original>result == expected</Original>
      <Expanded>nullptr == 0x42</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.005"/>
  </TestCase>
  <TestCase name="Parser handles valid" tags="[unit]" filename="test_parser.cpp" line="25">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <OverallResults successes="1" failures="1" expectedFailures="0"/>
  <OverallResultsCases successes="1" failures="1" expectedFailures="0"/>
</Catch2TestRun>"#;
        let result = adapter.parse_output(stdout, "", 1);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Parser handles empty");
        assert_eq!(f.file.as_deref(), Some("test_parser.cpp"));
        assert_eq!(f.line, Some(18));
        assert!(f.message.contains("nullptr == 0x42"));
    }
}
