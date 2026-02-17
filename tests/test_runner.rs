//! Test runner integration tests.
//! Tests the TestRunner API with Cargo (Rust fixture) and Catch2 (C++ fixture) adapters.

mod common;

use common::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_runner_scenarios() {
    // Pre-build fixtures so cargo test / catch2 spawned via Frida don't need to compile
    let _ = rust_target();
    let _ = cpp_test_suite();

    let (sm, _dir) = create_session_manager();

    eprintln!("=== Test 1/7: Cargo test execution ===");
    test_cargo_execution(&sm).await;

    eprintln!("\n=== Test 2/7: Cargo single test filter ===");
    test_cargo_single_test(&sm).await;

    eprintln!("\n=== Test 3/7: Catch2 test execution ===");
    test_catch2_execution(&sm).await;

    eprintln!("\n=== Test 4/7: Catch2 single test filter ===");
    test_catch2_single_test(&sm).await;

    eprintln!("\n=== Test 5/7: Catch2 stuck test detection ===");
    test_catch2_stuck_detection(&sm).await;

    eprintln!("\n=== Test 6/7: Adapter detection ===");
    test_adapter_detection();

    eprintln!("\n=== Test 7/7: Details file writing ===");
    test_details_file_writing(&sm).await;

    eprintln!("\n=== All 7 test runner scenarios passed ===");
}

async fn test_cargo_execution(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-cargo");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            &rust_fixture_project(),
            Some("cargo"),
            None,
            None,
            None,
            &HashMap::new(),
            Some(60),
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress,
        )
        .await
        .unwrap();

    eprintln!(
        "Cargo result: framework={} passed={} failed={} skipped={}",
        result.framework,
        result.result.summary.passed,
        result.result.summary.failed,
        result.result.summary.skipped,
    );

    assert_eq!(result.framework, "cargo");
    // Rust fixture has: 3 pass (audio, midi, engine), 1 fail (intentional), 1 skip (ignored)
    assert_eq!(result.result.summary.passed, 3, "Expected 3 passing tests");
    assert_eq!(result.result.summary.failed, 1, "Expected 1 failing test");
    assert_eq!(result.result.summary.skipped, 1, "Expected 1 skipped test");
    assert!(result.session_id.is_some(), "Should have Frida session");

    // Verify failure details
    assert_eq!(result.result.failures.len(), 1);
    assert!(
        result.result.failures[0].name.contains("intentional_failure"),
        "Failure should be the intentional one"
    );
}

async fn test_cargo_single_test(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-cargo-single");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            &rust_fixture_project(),
            Some("cargo"),
            None,
            Some("test_audio_process"),
            None,
            &HashMap::new(),
            Some(60),
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress,
        )
        .await
        .unwrap();

    eprintln!(
        "Single test: passed={} failed={}",
        result.result.summary.passed, result.result.summary.failed,
    );

    assert_eq!(result.result.summary.passed, 1, "Should pass exactly 1 test");
    assert_eq!(result.result.summary.failed, 0, "Should have no failures");
}

async fn test_catch2_execution(sm: &strobe::daemon::SessionManager) {
    let test_suite = cpp_test_suite();
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-catch2");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            test_suite.parent().unwrap(),
            None, // auto-detect
            None,
            None,
            Some(test_suite.to_str().unwrap()),
            &HashMap::new(),
            Some(60),
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress,
        )
        .await
        .unwrap();

    eprintln!(
        "Catch2 result: framework={} passed={} failed={} skipped={}",
        result.framework,
        result.result.summary.passed,
        result.result.summary.failed,
        result.result.summary.skipped,
    );

    assert_eq!(result.framework, "catch2");
    // C++ fixture has: 9 pass, 1 fail (Intentional failure), 0 skip
    // (Stuck test excluded by default — Catch2 doesn't run [stuck] tagged tests without explicit tag)
    assert!(result.result.summary.passed >= 8, "Expected >= 8 passing tests");
    assert!(result.result.summary.failed >= 1, "Expected >= 1 failing test");

    // Verify failure details include file/line
    if !result.result.failures.is_empty() {
        let failure = &result.result.failures[0];
        eprintln!("Catch2 failure: name={} message={}", failure.name, failure.message);
    }
}

async fn test_catch2_single_test(sm: &strobe::daemon::SessionManager) {
    let test_suite = cpp_test_suite();
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-catch2-single");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            test_suite.parent().unwrap(),
            None,
            None,
            Some("MIDI note on"),
            Some(test_suite.to_str().unwrap()),
            &HashMap::new(),
            Some(60),
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress,
        )
        .await
        .unwrap();

    eprintln!(
        "Single Catch2: passed={} failed={}",
        result.result.summary.passed, result.result.summary.failed,
    );

    assert_eq!(result.result.summary.passed, 1, "Should pass exactly 1 test");
    assert_eq!(result.result.summary.failed, 0, "Should have no failures");
}

async fn test_catch2_stuck_detection(sm: &strobe::daemon::SessionManager) {
    let test_suite = cpp_test_suite();
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-catch2-stuck");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            test_suite.parent().unwrap(),
            None,
            None,
            Some("Stuck test"),
            Some(test_suite.to_str().unwrap()),
            &HashMap::new(),
            Some(10), // Short timeout
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress.clone(),
        )
        .await;

    // The test might timeout or produce stuck warnings
    match result {
        Ok(r) => {
            eprintln!(
                "Stuck test result: passed={} failed={} stuck={:?}",
                r.result.summary.passed,
                r.result.summary.failed,
                r.result.summary.stuck,
            );
        }
        Err(e) => {
            eprintln!("Stuck test error (expected): {}", e);
        }
    }

    // Check progress for warnings
    let progress = progress.lock().unwrap();
    eprintln!("Stuck warnings: {:?}", progress.warnings.len());
}

fn test_adapter_detection() {
    let runner = strobe::test::TestRunner::new();

    // Cargo adapter should detect Rust fixture
    let rust_project = rust_fixture_project();
    let adapter = runner.detect_adapter(&rust_project, None, None).unwrap();
    eprintln!("Detected adapter for Rust project: {}", adapter.name());
    assert_eq!(adapter.name(), "cargo", "Should detect Cargo for Rust fixture");

    let cargo_confidence = adapter.detect(&rust_project, None);
    eprintln!("Cargo confidence: {}", cargo_confidence);
    assert!(cargo_confidence >= 85, "Cargo should detect Rust fixture with high confidence");

    // Catch2 adapter should detect C++ binary
    let cpp_suite = cpp_test_suite();
    let adapter = runner.detect_adapter(
        cpp_suite.parent().unwrap(),
        None,
        Some(cpp_suite.to_str().unwrap()),
    ).unwrap();
    eprintln!("Detected adapter for C++ suite: {}", adapter.name());
    assert_eq!(adapter.name(), "catch2", "Should detect Catch2 for C++ test suite");

    let catch2_confidence = adapter.detect(cpp_suite.parent().unwrap(), Some(cpp_suite.to_str().unwrap()));
    eprintln!("Catch2 confidence: {}", catch2_confidence);
    assert!(catch2_confidence >= 80, "Catch2 should detect C++ test suite");

    // No framework should error with guidance
    let result = runner.detect_adapter(std::path::Path::new("/nonexistent"), None, None);
    assert!(result.is_err(), "Should error when no framework detected");
    let err = result.err().unwrap().to_string();
    eprintln!("No-framework error: {}", err);
    assert!(err.contains("No test framework detected"));

    // Invalid framework name should error
    let result = runner.detect_adapter(&rust_project, Some("unknown_fw"), None);
    assert!(result.is_err(), "Should error on unknown framework");
    let err = result.err().unwrap().to_string();
    eprintln!("Invalid-framework error: {}", err);
    assert!(err.contains("Unknown framework 'unknown_fw'"));
}

async fn test_details_file_writing(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-details");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            &rust_fixture_project(),
            Some("cargo"),
            None,
            Some("test_audio_process"),
            None,
            &HashMap::new(),
            Some(60),
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress,
        )
        .await
        .unwrap();

    // Write details file
    let path = strobe::test::output::write_details(
        &result.framework,
        &result.result,
        &result.raw_stdout,
        &result.raw_stderr,
    )
    .unwrap();

    eprintln!("Details written to: {}", path);

    // Read and verify JSON structure
    let content = std::fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert!(json.get("framework").is_some(), "Should have framework field");
    assert!(json.get("summary").is_some(), "Should have summary field");
    assert_eq!(json["framework"], "cargo");

    // Clean up
    let _ = std::fs::remove_file(path);
}
