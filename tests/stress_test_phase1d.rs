//! Stress tests for Phase 1d — Test Instrumentation subsystem.
//!
//! Tests both Rust (Cargo) and C++ (Catch2) adapter paths heavily,
//! exercises the full TestRunner pipeline, parser edge cases,
//! stuck detector, output writer, generic adapter, and MCP type contracts.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use strobe::test::adapter::*;
use strobe::test::cargo_adapter::CargoTestAdapter;
use strobe::test::catch2_adapter::Catch2Adapter;
use strobe::test::generic_adapter::GenericAdapter;
use strobe::test::stuck_detector::{StuckDetector, get_process_cpu_ns};
use strobe::test::TestProgress;

/// Create a SessionManager with a temp DB for integration tests.
fn test_session_manager() -> Arc<strobe::daemon::SessionManager> {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test.db");
    // Leak the tempdir so it lives for the test duration
    std::mem::forget(tmp);
    Arc::new(strobe::daemon::SessionManager::new(&db_path).unwrap())
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 1: CargoTestAdapter — parse_output stress
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_cargo_parse_empty_output() {
    let adapter = CargoTestAdapter;
    let result = adapter.parse_output("", "", 0);
    assert_eq!(result.summary.passed, 0);
    assert_eq!(result.summary.failed, 0);
    assert!(result.failures.is_empty());
}

#[test]
fn test_cargo_parse_garbage_json() {
    let adapter = CargoTestAdapter;
    let stdout = "this is not json\n{invalid json too}\n\x00\x01\x02\n";
    let result = adapter.parse_output(stdout, "", 1);
    assert_eq!(result.summary.passed, 0);
    assert_eq!(result.summary.failed, 0);
}

#[test]
fn test_cargo_parse_mixed_json_and_garbage() {
    let adapter = CargoTestAdapter;
    let stdout = r#"
Compiling something v0.1.0
warning: unused variable
{ "type": "suite", "event": "started", "test_count": 2 }
   = help: consider using `_x` instead
{ "type": "test", "event": "started", "name": "tests::test_a" }
{ "type": "test", "event": "ok", "name": "tests::test_a", "exec_time": 0.001 }
{ "type": "test", "event": "started", "name": "tests::test_b" }
{ "type": "test", "event": "ok", "name": "tests::test_b", "exec_time": 0.002 }
{ "type": "suite", "event": "ok", "passed": 2, "failed": 0, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.003 }
"#;
    let result = adapter.parse_output(stdout, "cargo warnings here", 0);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.failed, 0);
    assert_eq!(result.summary.skipped, 0);
}

#[test]
fn test_cargo_parse_many_failures() {
    let adapter = CargoTestAdapter;
    let mut lines = vec![
        r#"{ "type": "suite", "event": "started", "test_count": 50 }"#.to_string(),
    ];
    for i in 0..50 {
        lines.push(format!(
            r#"{{ "type": "test", "event": "started", "name": "mod{}::tests::test_{}" }}"#,
            i / 10, i
        ));
        if i % 3 == 0 {
            lines.push(format!(
                r#"{{ "type": "test", "event": "failed", "name": "mod{}::tests::test_{}", "exec_time": 0.1, "stdout": "thread 'mod{}::tests::test_{}' panicked at src/mod{}.rs:{}:5:\nassertion `left == right` failed\n  left: 0\n  right: {}\n" }}"#,
                i / 10, i, i / 10, i, i / 10, 100 + i, i
            ));
        } else {
            lines.push(format!(
                r#"{{ "type": "test", "event": "ok", "name": "mod{}::tests::test_{}", "exec_time": 0.05 }}"#,
                i / 10, i
            ));
        }
    }
    lines.push(r#"{ "type": "suite", "event": "failed", "passed": 33, "failed": 17, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 5.0 }"#.to_string());

    let stdout = lines.join("\n");
    let result = adapter.parse_output(&stdout, "", 101);

    assert_eq!(result.summary.passed, 33);
    assert_eq!(result.summary.failed, 17);
    assert_eq!(result.failures.len(), 17);

    // Verify all failures have file locations parsed
    for f in &result.failures {
        assert!(f.file.is_some(), "Failure '{}' missing file", f.name);
        assert!(f.line.is_some(), "Failure '{}' missing line", f.name);
        assert!(f.rerun.is_some(), "Failure '{}' missing rerun", f.name);
    }

    // Verify all_tests captured everything
    assert_eq!(result.all_tests.len(), 50);
}

#[test]
fn test_cargo_parse_with_ignored_tests() {
    let adapter = CargoTestAdapter;
    let stdout = r#"{ "type": "suite", "event": "started", "test_count": 5 }
{ "type": "test", "event": "started", "name": "tests::test_a" }
{ "type": "test", "event": "ok", "name": "tests::test_a", "exec_time": 0.001 }
{ "type": "test", "event": "ignored", "name": "tests::test_b" }
{ "type": "test", "event": "ignored", "name": "tests::test_c" }
{ "type": "test", "event": "ignored", "name": "tests::test_d" }
{ "type": "test", "event": "started", "name": "tests::test_e" }
{ "type": "test", "event": "ok", "name": "tests::test_e", "exec_time": 0.002 }
{ "type": "suite", "event": "ok", "passed": 2, "failed": 0, "ignored": 3, "measured": 0, "filtered_out": 0, "exec_time": 0.003 }
"#;
    let result = adapter.parse_output(stdout, "", 0);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.skipped, 3);
    assert_eq!(result.summary.failed, 0);
    assert_eq!(result.all_tests.len(), 5);

    let skipped_tests: Vec<_> = result.all_tests.iter().filter(|t| t.status == "skip").collect();
    assert_eq!(skipped_tests.len(), 3);
}

#[test]
fn test_cargo_parse_panic_without_location() {
    let adapter = CargoTestAdapter;
    // Some panics don't have file:line format
    let stdout = r#"{ "type": "suite", "event": "started", "test_count": 1 }
{ "type": "test", "event": "started", "name": "tests::test_oops" }
{ "type": "test", "event": "failed", "name": "tests::test_oops", "exec_time": 0.1, "stdout": "some random output without panic location info\nmaybe an error message\n" }
{ "type": "suite", "event": "failed", "passed": 0, "failed": 1, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.1 }
"#;
    let result = adapter.parse_output(stdout, "", 101);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);
    // Without "panicked at" we don't extract file/line
    assert!(result.failures[0].file.is_none());
    assert!(result.failures[0].line.is_none());
    // But message still gets the raw stdout
    assert!(!result.failures[0].message.is_empty());
}

#[test]
fn test_cargo_suite_command_levels() {
    let adapter = CargoTestAdapter;

    let levels = [
        (Some(TestLevel::Unit), "--lib"),
        (Some(TestLevel::Integration), "--test"),
        (Some(TestLevel::E2e), "--test"),
    ];

    for (level, expected_flag) in levels {
        let cmd = adapter.suite_command(Path::new("."), level, &HashMap::new()).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(
            cmd.args.contains(&expected_flag.to_string()),
            "Level {:?} should contain '{}', got: {:?}",
            level, expected_flag, cmd.args
        );
        // All commands should have JSON format flags after --
        let dash_idx = cmd.args.iter().position(|a| a == "--").unwrap();
        assert!(cmd.args[dash_idx..].contains(&"--format".to_string()));
        assert!(cmd.args[dash_idx..].contains(&"json".to_string()));
        // RUSTC_BOOTSTRAP must be set
        assert_eq!(cmd.env.get("RUSTC_BOOTSTRAP"), Some(&"1".to_string()));
    }
}

#[test]
fn test_cargo_suggest_traces_deep_module() {
    let adapter = CargoTestAdapter;

    let failure = TestFailure {
        name: "deeply::nested::module::tests::test_foo".to_string(),
        file: Some("src/deeply/nested/module.rs".to_string()),
        line: Some(42),
        message: "failed".to_string(),
        rerun: None,
        suggested_traces: vec![],
    };

    let traces = adapter.suggest_traces(&failure);
    // Should suggest module-level pattern from first segment
    assert!(traces.contains(&"deeply::*".to_string()));
    // Should suggest @file: pattern from the filename
    assert!(traces.contains(&"@file:module.rs".to_string()));
}

#[test]
fn test_cargo_suggest_traces_no_file() {
    let adapter = CargoTestAdapter;

    let failure = TestFailure {
        name: "tests::test_bare".to_string(),
        file: None,
        line: None,
        message: "failed".to_string(),
        rerun: None,
        suggested_traces: vec![],
    };

    let traces = adapter.suggest_traces(&failure);
    assert!(traces.contains(&"tests::*".to_string()));
    assert_eq!(traces.len(), 1); // No @file: when file is None
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 2: Catch2Adapter — parse_output stress
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_catch2_parse_empty_xml() {
    let adapter = Catch2Adapter;
    let result = adapter.parse_output("", "", 0);
    assert_eq!(result.summary.passed, 0);
    assert_eq!(result.summary.failed, 0);
}

#[test]
fn test_catch2_parse_garbage_input() {
    let adapter = Catch2Adapter;
    let result = adapter.parse_output("this is not xml at all", "some stderr", 1);
    assert_eq!(result.summary.passed, 0);
    assert_eq!(result.summary.failed, 0);
}

#[test]
fn test_catch2_parse_many_test_cases() {
    let adapter = Catch2Adapter;
    let mut xml = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="stress_tests" rng-seed="42" catch2-version="3.5.0">
"#);

    let total = 100;
    let fail_every = 7; // every 7th test fails

    for i in 0..total {
        let name = format!("Test case #{}", i);
        let file = format!("test_{}.cpp", i % 10);
        let line = 10 + i;

        if i % fail_every == 0 {
            xml.push_str(&format!(
                r#"  <TestCase name="{name}" tags="[unit]" filename="{file}" line="{line}">
    <Expression success="false" type="REQUIRE" filename="{file}" line="{}">
      <Original>result_{i} == expected_{i}</Original>
      <Expanded>{} == {}</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.00{}"/>
  </TestCase>
"#,
                line + 5, i * 2, i * 2 + 1, i % 10
            ));
        } else {
            xml.push_str(&format!(
                r#"  <TestCase name="{name}" tags="[unit]" filename="{file}" line="{line}">
    <OverallResult success="true" durationInSeconds="0.00{}"/>
  </TestCase>
"#,
                i % 10
            ));
        }
    }

    let expected_failures = (0..total).filter(|i| i % fail_every == 0).count() as u32;
    let expected_passes = total as u32 - expected_failures;

    xml.push_str(&format!(
        r#"  <OverallResults successes="{}" failures="{}" expectedFailures="0"/>
  <OverallResultsCases successes="{}" failures="{}" expectedFailures="0"/>
</Catch2TestRun>"#,
        expected_passes * 2, // assertions != test cases, but doesn't matter for parse
        expected_failures,
        expected_passes,
        expected_failures
    ));

    let result = adapter.parse_output(&xml, "", 1);
    assert_eq!(result.summary.passed, expected_passes);
    assert_eq!(result.summary.failed, expected_failures);
    assert_eq!(result.failures.len(), expected_failures as usize);
    assert_eq!(result.all_tests.len(), total);

    // Every failure should have file, line, and expanded expression in message
    for f in &result.failures {
        assert!(f.file.is_some(), "Failure '{}' missing file", f.name);
        assert!(f.line.is_some(), "Failure '{}' missing line", f.name);
        assert!(f.message.contains("=="), "Failure '{}' missing expanded expression", f.name);
        assert!(f.rerun.is_some());
    }
}

#[test]
fn test_catch2_parse_multiple_expressions_in_single_test() {
    let adapter = Catch2Adapter;
    // A single test with multiple assertion failures — Catch2 REQUIRE stops at first,
    // but CHECK continues. The XML can have multiple Expression elements.
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="42" catch2-version="3.5.0">
  <TestCase name="Multiple checks" tags="[unit]" filename="test.cpp" line="10">
    <Expression success="false" type="CHECK" filename="test.cpp" line="12">
      <Original>a == 1</Original>
      <Expanded>0 == 1</Expanded>
    </Expression>
    <Expression success="false" type="CHECK" filename="test.cpp" line="13">
      <Original>b == 2</Original>
      <Expanded>99 == 2</Expanded>
    </Expression>
    <OverallResult success="false" durationInSeconds="0.001"/>
  </TestCase>
</Catch2TestRun>"#;

    let result = adapter.parse_output(xml, "", 1);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);
    // Should pick up the last expression's expanded form
    let f = &result.failures[0];
    assert!(f.message.contains("99 == 2"), "Expected last expression: {}", f.message);
}

#[test]
fn test_catch2_parse_nested_sections() {
    let adapter = Catch2Adapter;
    // Catch2 SECTIONs appear as nested elements
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<Catch2TestRun name="tests" rng-seed="42" catch2-version="3.5.0">
  <TestCase name="Sections test" tags="[unit]" filename="test.cpp" line="1">
    <Section name="Section A" filename="test.cpp" line="3">
      <OverallResults successes="1" failures="0" expectedFailures="0"/>
    </Section>
    <Section name="Section B" filename="test.cpp" line="8">
      <Expression success="false" type="REQUIRE" filename="test.cpp" line="10">
        <Original>x &gt; 0</Original>
        <Expanded>-1 &gt; 0</Expanded>
      </Expression>
      <OverallResults successes="0" failures="1" expectedFailures="0"/>
    </Section>
    <OverallResult success="false" durationInSeconds="0.003"/>
  </TestCase>
</Catch2TestRun>"#;

    let result = adapter.parse_output(xml, "", 1);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures[0].name, "Sections test");
    // XML entities should be unescaped
    assert!(result.failures[0].message.contains("> 0"), "Message: {}", result.failures[0].message);
}

#[test]
fn test_catch2_command_for_binary_levels() {
    let cmd = Catch2Adapter::command_for_binary("/path/to/tests", None);
    assert_eq!(cmd.program, "/path/to/tests");
    assert!(cmd.args.contains(&"--reporter".to_string()));
    assert!(cmd.args.contains(&"xml".to_string()));

    let unit_cmd = Catch2Adapter::command_for_binary("/path/to/tests", Some(TestLevel::Unit));
    assert!(unit_cmd.args.contains(&"[unit]".to_string()));

    let int_cmd = Catch2Adapter::command_for_binary("/path/to/tests", Some(TestLevel::Integration));
    assert!(int_cmd.args.contains(&"[integration]".to_string()));

    let e2e_cmd = Catch2Adapter::command_for_binary("/path/to/tests", Some(TestLevel::E2e));
    assert!(e2e_cmd.args.contains(&"[e2e]".to_string()));
}

#[test]
fn test_catch2_single_test_for_binary() {
    let cmd = Catch2Adapter::single_test_for_binary(
        "/path/to/tests",
        "Test case with spaces and [tags]",
    );
    assert_eq!(cmd.program, "/path/to/tests");
    assert!(cmd.args.contains(&"Test case with spaces and [tags]".to_string()));
    assert!(cmd.args.contains(&"--reporter".to_string()));
}

#[test]
fn test_catch2_suggest_traces() {
    let adapter = Catch2Adapter;

    let failure = TestFailure {
        name: "Audio buffer test".to_string(),
        file: Some("test_audio.cpp".to_string()),
        line: Some(35),
        message: "failed".to_string(),
        rerun: None,
        suggested_traces: vec![],
    };

    let traces = adapter.suggest_traces(&failure);
    assert!(traces.contains(&"@file:test_audio.cpp".to_string()));

    // Without file → no traces
    let no_file_failure = TestFailure {
        name: "bare test".to_string(),
        file: None,
        line: None,
        message: "failed".to_string(),
        rerun: None,
        suggested_traces: vec![],
    };
    let traces2 = adapter.suggest_traces(&no_file_failure);
    assert!(traces2.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 3: GenericAdapter — heuristic parsing stress
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_generic_parse_various_failure_formats() {
    let adapter = GenericAdapter;

    // Various failure patterns from different frameworks
    let stderr = r#"
FAIL: test_authentication at tests/test_auth.py:42
FAILED test_login tests/login.cpp:100
ERROR: validation_check at src/validator.rs:55
"#;
    let result = adapter.parse_output("", stderr, 1);
    assert!(result.summary.failed >= 1, "Should detect failures in stderr");
    assert!(!result.failures.is_empty());
}

#[test]
fn test_generic_parse_exit_code_only() {
    let adapter = GenericAdapter;
    // No recognizable patterns, but non-zero exit
    let result = adapter.parse_output("everything looks fine", "no errors", 1);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);
    assert!(result.failures[0].message.contains("exited with code 1"));
}

#[test]
fn test_generic_parse_zero_exit_no_failures() {
    let adapter = GenericAdapter;
    let result = adapter.parse_output("all good", "some warnings", 0);
    assert_eq!(result.summary.failed, 0);
    assert!(result.failures.is_empty());
}

#[test]
fn test_generic_parse_signal_exit() {
    let adapter = GenericAdapter;
    let result = adapter.parse_output("", "Segmentation fault", -11);
    assert_eq!(result.summary.failed, 1);
    assert!(result.failures[0].message.contains("-11"));
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 4: TestRunner — adapter detection
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_runner_detects_cargo_for_strobe() {
    // Strobe root has Cargo.toml → cargo adapter should be selected
    let adapter = CargoTestAdapter;
    assert_eq!(adapter.detect(Path::new("."), None), 90);
    let cmd = adapter.suite_command(Path::new("."), None, &HashMap::new()).unwrap();
    assert_eq!(cmd.program, "cargo");
}

#[test]
fn test_runner_generic_fallback_for_unknown_project() {
    let adapter = GenericAdapter;
    let confidence = adapter.detect(Path::new("/nonexistent/project"), None);
    assert_eq!(confidence, 1); // always matches as fallback
}

#[test]
fn test_adapter_confidence_ordering() {
    // Cargo should always beat generic for a Rust project
    let cargo_conf = CargoTestAdapter.detect(Path::new("."), None);
    let generic_conf = GenericAdapter.detect(Path::new("."), None);
    let catch2_conf = Catch2Adapter.detect(Path::new("."), None);

    assert!(cargo_conf > generic_conf, "Cargo ({}) should beat generic ({})", cargo_conf, generic_conf);
    assert!(cargo_conf > catch2_conf, "Cargo ({}) should beat Catch2 ({}) for Rust project", cargo_conf, catch2_conf);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 5: TestRunner — real execution (Rust)
// ═══════════════════════════════════════════════════════════════════════════

/// Full TestRunner pipeline for Rust: single test + lib suite, run sequentially
/// to avoid cargo lock contention. Uses default target dir (already compiled).
#[tokio::test]
async fn test_runner_run_real_cargo_tests() {
    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();

    // Part 1: Single test by name
    let progress1 = Arc::new(Mutex::new(TestProgress::new()));
    let single_result = runner.run(
        Path::new("."),
        Some("cargo"),
        None,
        Some("test::cargo_adapter::tests::test_detect_cargo_project"),
        None,
        &HashMap::new(),
        Some(120_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-single-cargo",
        progress1,
    ).await.unwrap();

    assert_eq!(single_result.framework, "cargo");
    assert!(single_result.result.summary.passed >= 1, "Expected pass, got: {:?}", single_result.result.summary);
    assert_eq!(single_result.result.summary.failed, 0);
    assert!(single_result.session_id.is_some()); // always has session now

    // Part 2: Full lib suite
    let progress2 = Arc::new(Mutex::new(TestProgress::new()));
    let lib_result = runner.run(
        Path::new("."),
        Some("cargo"),
        Some(TestLevel::Unit),
        None,
        None,
        &HashMap::new(),
        Some(120_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-lib-cargo",
        progress2,
    ).await.unwrap();

    assert_eq!(lib_result.framework, "cargo");
    assert!(
        lib_result.result.summary.passed >= 10,
        "Expected >= 10 passes, got: {:?}",
        lib_result.result.summary,
    );
    assert!(!lib_result.raw_stdout.is_empty(), "raw_stdout should not be empty");

    // Verify all_tests details match summary counts
    let pass_count = lib_result.result.all_tests.iter().filter(|t| t.status == "pass").count();
    let fail_count = lib_result.result.all_tests.iter().filter(|t| t.status == "fail").count();
    assert_eq!(pass_count, lib_result.result.summary.passed as usize);
    assert_eq!(fail_count, lib_result.result.summary.failed as usize);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 6: TestRunner — real execution (C++/Catch2)
// ═══════════════════════════════════════════════════════════════════════════

const ERAE_FW_TEST_BINARY: &str =
    "/Users/alex/erae_touch_mk2_fw/build/simulator/tests/erae_mk2_fw_tests";
const ERAE_DATA_TEST_BINARY: &str =
    "/Users/alex/erae_touch_mk2_fw/build/erae_data/tests/erae_data_tests_x86_64";

fn catch2_binary_exists(path: &str) -> bool {
    Path::new(path).exists()
}

#[tokio::test]
async fn test_runner_run_catch2_erae_fw_tests() {
    if !catch2_binary_exists(ERAE_FW_TEST_BINARY) {
        eprintln!("Skipping: erae_mk2_fw_tests not found");
        return;
    }

    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();
    let progress = Arc::new(Mutex::new(TestProgress::new()));

    let result = runner.run(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        None, // auto-detect should pick catch2 since command is provided
        None,
        None,
        Some(ERAE_FW_TEST_BINARY),
        &HashMap::new(),
        Some(30_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-catch2-fw",
        progress,
    ).await.unwrap();

    assert_eq!(result.framework, "catch2");
    assert!(
        result.result.summary.passed > 0,
        "Expected at least some passes, got: {:?}\nstdout: {}\nstderr: {}",
        result.result.summary, &result.raw_stdout[..result.raw_stdout.len().min(500)], &result.raw_stderr[..result.raw_stderr.len().min(500)]
    );
    // All erae fw tests should pass
    assert_eq!(
        result.result.summary.failed, 0,
        "Expected 0 failures, got {} failures: {:?}",
        result.result.summary.failed,
        result.result.failures.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn test_runner_run_catch2_erae_data_tests() {
    if !catch2_binary_exists(ERAE_DATA_TEST_BINARY) {
        eprintln!("Skipping: erae_data_tests not found");
        return;
    }

    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();
    let progress = Arc::new(Mutex::new(TestProgress::new()));

    let result = runner.run(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        None,
        None,
        None,
        Some(ERAE_DATA_TEST_BINARY),
        &HashMap::new(),
        Some(30_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-catch2-data",
        progress,
    ).await.unwrap();

    assert_eq!(result.framework, "catch2");
    assert!(
        result.result.summary.passed > 10,
        "Expected many passes for erae_data, got: {:?}",
        result.result.summary
    );
    assert_eq!(result.result.summary.failed, 0);

    // Verify details
    assert!(!result.result.all_tests.is_empty());
    for t in &result.result.all_tests {
        assert!(!t.name.is_empty());
        assert!(t.status == "pass" || t.status == "fail" || t.status == "skip");
    }
}

#[tokio::test]
async fn test_runner_run_catch2_single_test() {
    if !catch2_binary_exists(ERAE_FW_TEST_BINARY) {
        eprintln!("Skipping: erae_mk2_fw_tests not found");
        return;
    }

    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();
    let progress = Arc::new(Mutex::new(TestProgress::new()));

    let result = runner.run(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        None,
        None,
        Some("Project serialization/deserialization roundtrip"),
        Some(ERAE_FW_TEST_BINARY),
        &HashMap::new(),
        Some(10_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-catch2-single",
        progress,
    ).await.unwrap();

    assert_eq!(result.framework, "catch2");
    assert_eq!(result.result.summary.passed, 1, "Single test should produce 1 pass");
    assert_eq!(result.result.summary.failed, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 7: Stuck detector stress
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_stuck_detector_fast_exit_process() {
    // "echo hello" should exit instantly — detector should return None
    let mut child = tokio::process::Command::new("echo")
        .arg("hello")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let _ = child.wait().await;

    let progress = std::sync::Arc::new(std::sync::Mutex::new(strobe::test::TestProgress::new()));
    let detector = StuckDetector::new(pid, 5000, std::sync::Arc::clone(&progress));
    detector.run().await;
    assert!(progress.lock().unwrap().warnings.is_empty(), "Fast process should have no warnings");
}

#[tokio::test]
async fn test_stuck_detector_hard_timeout() {
    // sleep 100 with a 2s timeout should trigger hard timeout warning
    let mut child = tokio::process::Command::new("sleep")
        .arg("100")
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();

    let progress = std::sync::Arc::new(std::sync::Mutex::new(strobe::test::TestProgress::new()));
    {
        let mut p = progress.lock().unwrap();
        p.phase = strobe::test::TestPhase::Running;
    }
    let progress_clone = std::sync::Arc::clone(&progress);
    let detector = StuckDetector::new(pid, 2000, progress_clone);
    let detector_handle = tokio::spawn(async move { detector.run().await });

    // Wait for warning to appear (timeout is 2s, check every 500ms)
    let start = std::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let p = progress.lock().unwrap();
        if !p.warnings.is_empty() {
            let warning = &p.warnings[0];
            assert!(warning.diagnosis.contains("timeout") || warning.diagnosis.contains("Timeout")
                || warning.diagnosis.contains("stopping"),
                "Diagnosis should mention timeout: {}", warning.diagnosis
            );
            break;
        }
        if start.elapsed() > std::time::Duration::from_secs(10) {
            panic!("Timed out waiting for stuck warning");
        }
    }

    // Clean up
    let _ = child.kill().await;
    detector_handle.abort();
}

#[test]
fn test_cpu_sampling_stress() {
    let pid = std::process::id();

    // Sample multiple times and verify monotonically increasing
    let mut prev = 0;
    for _ in 0..10 {
        let cpu = get_process_cpu_ns(pid);
        assert!(cpu >= prev, "CPU time should be monotonically increasing");
        prev = cpu;
    }
}

#[test]
fn test_cpu_sampling_dead_process() {
    // PID 999999 almost certainly doesn't exist
    let cpu = get_process_cpu_ns(999999);
    assert_eq!(cpu, 0, "Dead process should return 0");
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 8: Output writer stress
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_details_writer_large_output() {
    let result = TestResult {
        summary: TestSummary {
            passed: 1000,
            failed: 50,
            skipped: 10,
            stuck: Some(2),
            duration_ms: 60_000,
        },
        failures: (0..50).map(|i| TestFailure {
            name: format!("test_failure_{}", i),
            file: Some(format!("src/mod_{}.rs", i % 10)),
            line: Some(100 + i),
            message: format!("assertion failed: expected {} got {}", i, i + 1),
            rerun: Some(format!("test_failure_{}", i)),
            suggested_traces: vec![format!("mod_{}::*", i % 10)],
        }).collect(),
        stuck: vec![
            StuckTest {
                name: "stuck_test_1".to_string(),
                elapsed_ms: 30_000,
                diagnosis: "Deadlock: 0% CPU".to_string(),
                threads: vec![ThreadStack {
                    name: "main".to_string(),
                    stack: vec!["frame1".to_string(), "frame2".to_string()],
                }],
                suggested_traces: vec!["main::*".to_string()],
            },
        ],
        all_tests: (0..1060).map(|i| TestDetail {
            name: format!("test_{}", i),
            status: if i < 1000 { "pass" } else if i < 1050 { "fail" } else { "skip" }.to_string(),
            duration_ms: 50 + (i % 100) as u64,
            stdout: if i % 100 == 0 { Some("some output".to_string()) } else { None },
            stderr: None,
            message: if i >= 1000 && i < 1050 { Some(format!("failed at {}", i)) } else { None },
        }).collect(),
    };

    let raw_stdout = "x".repeat(100_000); // 100KB of stdout
    let raw_stderr = "y".repeat(50_000);  // 50KB of stderr

    let path = strobe::test::output::write_details("cargo", &result, &raw_stdout, &raw_stderr).unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["framework"], "cargo");
    assert_eq!(parsed["summary"]["passed"], 1000);
    assert_eq!(parsed["summary"]["failed"], 50);
    assert_eq!(parsed["summary"]["stuck"], 2);
    assert_eq!(parsed["failures"].as_array().unwrap().len(), 50);
    assert_eq!(parsed["tests"].as_array().unwrap().len(), 1060);
    assert_eq!(parsed["rawStdout"].as_str().unwrap().len(), 100_000);
    assert_eq!(parsed["rawStderr"].as_str().unwrap().len(), 50_000);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_details_writer_unicode_content() {
    let result = TestResult {
        summary: TestSummary {
            passed: 1, failed: 1, skipped: 0, stuck: None, duration_ms: 100,
        },
        failures: vec![TestFailure {
            name: "test_unicode_handling".to_string(),
            file: Some("src/i18n.rs".to_string()),
            line: Some(42),
            message: "assertion failed: expected '\u{1F600}' got '\u{2764}'".to_string(),
            rerun: Some("test_unicode_handling".to_string()),
            suggested_traces: vec![],
        }],
        stuck: vec![],
        all_tests: vec![],
    };

    let path = strobe::test::output::write_details("cargo", &result, "stdout \u{1F4A9}", "stderr \u{00E9}").unwrap();
    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(parsed["failures"][0]["message"].as_str().unwrap().contains('\u{1F600}'));
    assert!(parsed["rawStdout"].as_str().unwrap().contains('\u{1F4A9}'));
    assert!(parsed["rawStderr"].as_str().unwrap().contains('\u{00E9}'));

    let _ = std::fs::remove_file(&path);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 9: MCP types serialization/deserialization stress
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_debug_test_request_full_roundtrip() {
    let req = strobe::mcp::DebugTestRequest {
        project_root: "/my/project".to_string(),
        framework: Some("cargo".to_string()),
        level: Some(TestLevel::Unit),
        test: Some("test_foo".to_string()),
        command: Some("/path/to/binary".to_string()),
        trace_patterns: Some(vec!["foo::*".to_string(), "@file:bar.rs".to_string()]),
        watches: None,
        env: Some(HashMap::from([("RUST_LOG".to_string(), "debug".to_string())])),
        timeout: Some(60_000),
    };

    let json = serde_json::to_string(&req).unwrap();

    // camelCase verification
    assert!(json.contains("projectRoot"));
    assert!(json.contains("tracePatterns"));
    assert!(!json.contains("project_root"));
    assert!(!json.contains("trace_patterns"));

    // Roundtrip
    let back: strobe::mcp::DebugTestRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.project_root, "/my/project");
    assert_eq!(back.framework.as_deref(), Some("cargo"));
    assert_eq!(back.test.as_deref(), Some("test_foo"));
    assert_eq!(back.timeout, Some(60_000));
}

#[test]
fn test_debug_test_request_minimal() {
    let json = r#"{"projectRoot": "/test"}"#;
    let req: strobe::mcp::DebugTestRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.project_root, "/test");
    assert!(req.framework.is_none());
    assert!(req.level.is_none());
    assert!(req.test.is_none());
    assert!(req.command.is_none());
    assert!(req.trace_patterns.is_none());
    assert!(req.watches.is_none());
    assert!(req.env.is_none());
    assert!(req.timeout.is_none());
}

#[test]
fn test_debug_test_response_full_roundtrip() {
    let resp = strobe::mcp::DebugTestResponse {
        framework: "catch2".to_string(),
        summary: Some(TestSummary {
            passed: 42,
            failed: 3,
            skipped: 1,
            stuck: Some(1),
            duration_ms: 5000,
        }),
        failures: vec![TestFailure {
            name: "Audio buffer test".to_string(),
            file: Some("test_audio.cpp".to_string()),
            line: Some(35),
            message: "REQUIRE( buf.size() == 512 )\nwith expansion:\n  256 == 512".to_string(),
            rerun: Some("Audio buffer test".to_string()),
            suggested_traces: vec!["@file:test_audio.cpp".to_string()],
        }],
        stuck: vec![StuckTest {
            name: "Deadlock test".to_string(),
            elapsed_ms: 30_000,
            diagnosis: "Deadlock: 0% CPU".to_string(),
            threads: vec![],
            suggested_traces: vec![],
        }],
        session_id: Some("test-session-123".to_string()),
        details: Some("/tmp/strobe/tests/abc-2026-02-07.json".to_string()),
        no_tests: None,
        project: None,
        hint: Some("Run with tracePatterns to instrument".to_string()),
    };

    let json = serde_json::to_string_pretty(&resp).unwrap();
    let back: strobe::mcp::DebugTestResponse = serde_json::from_str(&json).unwrap();

    assert_eq!(back.framework, "catch2");
    assert_eq!(back.summary.unwrap().passed, 42);
    assert_eq!(back.failures.len(), 1);
    assert_eq!(back.stuck.len(), 1);
    assert_eq!(back.session_id.as_deref(), Some("test-session-123"));
}

#[test]
fn test_debug_test_response_empty_no_tests() {
    let resp = strobe::mcp::DebugTestResponse {
        framework: "cargo".to_string(),
        summary: None,
        failures: vec![],
        stuck: vec![],
        session_id: None,
        details: None,
        no_tests: Some(true),
        project: Some(ProjectInfo {
            language: "rust".to_string(),
            build_system: "cargo".to_string(),
            test_files: 0,
        }),
        hint: Some("No tests found. Create a tests/ directory.".to_string()),
    };

    let json = serde_json::to_string(&resp).unwrap();
    // Empty vecs should be omitted
    assert!(!json.contains("failures"), "Empty failures should be omitted");
    assert!(!json.contains("stuck"), "Empty stuck should be omitted");
}

#[test]
fn test_test_level_serde() {
    let levels = [TestLevel::Unit, TestLevel::Integration, TestLevel::E2e];
    let expected = ["\"unit\"", "\"integration\"", "\"e2e\""];

    for (level, expected_json) in levels.iter().zip(expected.iter()) {
        let json = serde_json::to_string(level).unwrap();
        assert_eq!(&json, expected_json);
        let back: TestLevel = serde_json::from_str(expected_json).unwrap();
        assert_eq!(
            serde_json::to_string(&back).unwrap(),
            serde_json::to_string(level).unwrap()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 10: Default timeout values
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_default_timeouts() {
    let adapter = CargoTestAdapter;
    assert_eq!(adapter.default_timeout(None), 30_000);
    assert_eq!(adapter.default_timeout(Some(TestLevel::Unit)), 30_000);
    assert_eq!(adapter.default_timeout(Some(TestLevel::Integration)), 120_000);
    assert_eq!(adapter.default_timeout(Some(TestLevel::E2e)), 300_000);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 11: Catch2 detection with real binary
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_catch2_detection_with_real_binary() {
    if !catch2_binary_exists(ERAE_FW_TEST_BINARY) {
        eprintln!("Skipping: erae_mk2_fw_tests not found");
        return;
    }

    let adapter = Catch2Adapter;
    let confidence = adapter.detect(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        Some(ERAE_FW_TEST_BINARY),
    );
    assert_eq!(confidence, 85, "Catch2 binary should be detected with confidence 85");

    // Without command, Catch2 shouldn't detect
    let no_cmd_confidence = adapter.detect(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        None,
    );
    assert_eq!(no_cmd_confidence, 0);
}

#[test]
fn test_catch2_detection_with_nonexistent_binary() {
    let adapter = Catch2Adapter;
    let confidence = adapter.detect(
        Path::new("/tmp"),
        Some("/nonexistent/binary"),
    );
    assert_eq!(confidence, 0);
}

// ═══════════════════════════════════════════════════════════════════════════
// SECTION 12: End-to-end details file from real test run
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn test_e2e_cargo_run_and_write_details() {
    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();
    let progress = Arc::new(Mutex::new(TestProgress::new()));

    let run_result = runner.run(
        Path::new("."),
        Some("cargo"),
        None,
        Some("test::cargo_adapter::tests::test_detect_cargo_project"),
        None,
        &HashMap::new(),
        Some(120_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-e2e-cargo",
        progress,
    ).await.unwrap();

    // Write details
    let path = strobe::test::output::write_details(
        &run_result.framework,
        &run_result.result,
        &run_result.raw_stdout,
        &run_result.raw_stderr,
    ).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["framework"], "cargo");
    assert!(parsed["summary"]["passed"].as_u64().unwrap() >= 1);

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_e2e_catch2_run_and_write_details() {
    if !catch2_binary_exists(ERAE_FW_TEST_BINARY) {
        eprintln!("Skipping: erae_mk2_fw_tests not found");
        return;
    }

    let runner = strobe::test::TestRunner::new();
    let sm = test_session_manager();
    let progress = Arc::new(Mutex::new(TestProgress::new()));

    let run_result = runner.run(
        Path::new("/Users/alex/erae_touch_mk2_fw"),
        None,
        None,
        None,
        Some(ERAE_FW_TEST_BINARY),
        &HashMap::new(),
        Some(30_000),
        &sm,
        &[],
        None,
        "test-conn",
        "test-e2e-catch2",
        progress,
    ).await.unwrap();

    // Write details
    let path = strobe::test::output::write_details(
        &run_result.framework,
        &run_result.result,
        &run_result.raw_stdout,
        &run_result.raw_stderr,
    ).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["framework"], "catch2");
    assert!(parsed["summary"]["passed"].as_u64().unwrap() > 0);
    assert!(parsed["tests"].as_array().unwrap().len() > 0);

    // Verify test names are present
    let first_test = &parsed["tests"][0];
    assert!(!first_test["name"].as_str().unwrap().is_empty());

    let _ = std::fs::remove_file(&path);
}
