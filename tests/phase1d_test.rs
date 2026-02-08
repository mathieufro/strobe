use std::collections::HashMap;
use std::path::Path;

/// Test that CargoTestAdapter correctly detects strobe as a Cargo project
#[test]
fn test_cargo_detection_on_strobe() {
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;
    use strobe::test::adapter::TestAdapter;
    assert_eq!(adapter.detect(Path::new("."), None), 90);
}

/// Test CargoTestAdapter command generation
#[test]
fn test_cargo_suite_commands() {
    use strobe::test::adapter::{TestAdapter, TestLevel};
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;

    let unit_cmd = adapter.suite_command(Path::new("."), Some(TestLevel::Unit), &HashMap::new()).unwrap();
    assert!(unit_cmd.args.contains(&"--lib".to_string()));

    let int_cmd = adapter.suite_command(Path::new("."), Some(TestLevel::Integration), &HashMap::new()).unwrap();
    assert!(int_cmd.args.iter().any(|a| a == "--test"));

    let all_cmd = adapter.suite_command(Path::new("."), None, &HashMap::new()).unwrap();
    assert!(!all_cmd.args.contains(&"--lib".to_string()));
}

/// Test that parsing real cargo test output works
/// (Run cargo test with JSON format using RUSTC_BOOTSTRAP=1)
#[tokio::test]
async fn test_cargo_parse_real_output() {
    use strobe::test::adapter::TestAdapter;
    let adapter = strobe::test::cargo_adapter::CargoTestAdapter;

    // --format json and -Zunstable-options are test harness flags (after --)
    // RUSTC_BOOTSTRAP=1 enables unstable features on stable toolchain
    let output = tokio::process::Command::new("cargo")
        .args(["test", "--lib", "--", "test::cargo_adapter::tests::test_detect_cargo_project", "--exact", "-Zunstable-options", "--format", "json"])
        .env("RUSTC_BOOTSTRAP", "1")
        .current_dir(".")
        .output()
        .await
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let result = adapter.parse_output(&stdout, &stderr, output.status.code().unwrap_or(-1));
    assert!(result.summary.passed >= 1, "Expected at least 1 pass, got: {:?}\nstdout: {}\nstderr: {}", result.summary, stdout, stderr);
    assert_eq!(result.summary.failed, 0, "Expected 0 failures");
}

/// Test MCP type serialization
#[test]
fn test_debug_test_request_camelcase() {
    let req = strobe::mcp::DebugTestRequest {
        project_root: "/test".to_string(),
        framework: None,
        level: None,
        test: None,
        command: None,
        trace_patterns: Some(vec!["foo::*".to_string()]),
        watches: None,
        env: None,
    };
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("projectRoot"));
    assert!(json.contains("tracePatterns"));
    assert!(!json.contains("project_root"));
    assert!(!json.contains("trace_patterns"));
}

/// Test details file writing
#[test]
fn test_details_file_roundtrip() {
    use strobe::test::adapter::*;
    let result = TestResult {
        summary: TestSummary {
            passed: 5, failed: 1, skipped: 0, stuck: None, duration_ms: 250,
        },
        failures: vec![TestFailure {
            name: "test_foo".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(42),
            message: "assertion failed".to_string(),
            rerun: Some("test_foo".to_string()),
            suggested_traces: vec!["foo::*".to_string()],
        }],
        stuck: vec![],
        all_tests: vec![],
    };

    let path = strobe::test::output::write_details("cargo", &result, "stdout", "stderr").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["framework"], "cargo");
    assert_eq!(parsed["summary"]["passed"], 5);
    assert_eq!(parsed["failures"][0]["name"], "test_foo");
    assert_eq!(parsed["rawStdout"], "stdout");

    let _ = std::fs::remove_file(&path);
}

/// Test StuckDetector CPU sampling
#[test]
fn test_cpu_sampling_current_process() {
    let pid = std::process::id();
    let cpu = strobe::test::stuck_detector::get_process_cpu_ns(pid);
    assert!(cpu > 0, "Current process should have non-zero CPU time");
}

/// Test Catch2 XML parsing with realistic output
#[test]
fn test_catch2_parse_realistic_xml() {
    use strobe::test::adapter::TestAdapter;
    let adapter = strobe::test::catch2_adapter::Catch2Adapter;

    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="erae_tests" rng-seed="1234" catch2-version="3.5.0">
  <TestCase name="MIDI note on" tags="[midi][unit]" filename="test_midi.cpp" line="15">
    <OverallResult success="true" durationInSeconds="0.001"/>
  </TestCase>
  <TestCase name="Audio buffer size" tags="[audio][unit]" filename="test_audio.cpp" line="30">
    <Expression success="false" type="REQUIRE" filename="test_audio.cpp" line="35">
      <Original>buffer.size() == 512</Original>
      <Expanded>256 == 512</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.002"/>
  </TestCase>
  <TestCase name="Engine init" tags="[engine][integration]" filename="test_engine.cpp" line="50">
    <OverallResult success="true" durationInSeconds="0.010"/>
  </TestCase>
  <OverallResults successes="3" failures="1" expectedFailures="0"/>
  <OverallResultsCases successes="2" failures="1" expectedFailures="0"/>
</Catch2TestRun>"#;

    let result = adapter.parse_output(xml, "", 1);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].name, "Audio buffer size");
    assert_eq!(result.failures[0].file.as_deref(), Some("test_audio.cpp"));
    assert_eq!(result.failures[0].line, Some(35));
    assert!(result.failures[0].message.contains("256 == 512"));
}
