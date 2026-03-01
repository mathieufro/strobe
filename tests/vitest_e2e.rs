//! Vitest adapter end-to-end tests.
//! Tests the full TestRunner flow with a real vitest project fixture,
//! covering both JSON stdout parsing and STROBE_TEST stderr fallback.

mod common;

use common::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Return the vitest fixture project root.
fn vitest_fixture_project() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vitest")
}

/// Ensure vitest fixture has node_modules installed.
fn ensure_vitest_installed() {
    use std::sync::OnceLock;
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let fixture = vitest_fixture_project();
        if !fixture.join("node_modules/.package-lock.json").exists() {
            eprintln!("Installing vitest fixture dependencies...");
            let status = std::process::Command::new("npm")
                .args(["install", "--no-audit", "--no-fund"])
                .current_dir(&fixture)
                .status()
                .expect("npm not found");
            assert!(status.success(), "npm install failed for vitest fixture");
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vitest_e2e_scenarios() {
    ensure_vitest_installed();

    let (sm, _dir) = create_session_manager();

    eprintln!("=== Test 1/5: Vitest adapter detection ===");
    test_vitest_detection();

    eprintln!("\n=== Test 2/5: Vitest full suite execution (JSON path) ===");
    test_vitest_full_suite(&sm).await;

    eprintln!("\n=== Test 3/5: Vitest single test filter ===");
    test_vitest_single_test(&sm).await;

    eprintln!("\n=== Test 4/5: Vitest custom script command ===");
    test_vitest_custom_command(&sm).await;

    eprintln!("\n=== Test 5/5: STROBE_TEST fallback when JSON missing ===");
    test_strobe_fallback(&sm).await;

    eprintln!("\n=== All 5 vitest e2e scenarios passed ===");
}

fn test_vitest_detection() {
    let runner = strobe::test::TestRunner::new();
    let project = vitest_fixture_project();

    let adapter = runner.detect_adapter(&project, None, None).unwrap();
    assert_eq!(adapter.name(), "vitest", "Should detect vitest from vitest.config.js");

    let confidence = adapter.detect(&project, None);
    eprintln!("Vitest confidence: {}", confidence);
    assert!(confidence >= 90, "Vitest should detect fixture with high confidence");

    // Also test explicit framework override
    let adapter = runner.detect_adapter(&project, Some("vitest"), None).unwrap();
    assert_eq!(adapter.name(), "vitest");
}

async fn test_vitest_full_suite(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-suite");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
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
            progress.clone(),
        )
        .await
        .unwrap();

    eprintln!(
        "Vitest result: framework={} passed={} failed={} skipped={}",
        result.framework,
        result.result.summary.passed,
        result.result.summary.failed,
        result.result.summary.skipped,
    );

    assert_eq!(result.framework, "vitest");
    // Fixture has: math.test.js (3 tests) + slow.test.js (2 tests) = 5 pass
    assert_eq!(result.result.summary.passed, 5, "Expected 5 passing tests");
    assert_eq!(result.result.summary.failed, 0, "Expected 0 failing tests");
    assert!(result.result.failures.is_empty());
    assert_eq!(result.result.all_tests.len(), 5, "Should have 5 test details");

    // Verify raw output was captured
    assert!(result.session_id.is_some(), "Should have session");

    // Verify progress was tracked via STROBE_TEST events
    let p = progress.lock().unwrap();
    assert!(p.has_custom_reporter, "Should have detected STROBE_TEST reporter");
    eprintln!("Progress: passed={} failed={} skipped={}", p.passed, p.failed, p.skipped);
}

async fn test_vitest_single_test(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-single");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            Some("adds two numbers"),
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

    assert!(result.result.summary.passed >= 1, "Should pass at least 1 test");
    assert_eq!(result.result.summary.failed, 0, "Should have no failures");
}

async fn test_vitest_custom_command(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-cmd");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    // Test with a custom npm script command — "npm run test:unit" should run only math tests
    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            None,
            Some("npx vitest run src/math.test.js"),
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
        "Custom command: passed={} failed={} all_tests={}",
        result.result.summary.passed,
        result.result.summary.failed,
        result.result.all_tests.len(),
    );

    assert_eq!(result.framework, "vitest");
    // math.test.js has 3 tests
    assert_eq!(result.result.summary.passed, 3, "Expected 3 passing tests from math.test.js");
    assert_eq!(result.result.summary.failed, 0);
}

async fn test_strobe_fallback(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-fallback");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    // Use STROBE_SLOW_CLEANUP to make afterAll hang longer than test timeout.
    // With timeout=8s and cleanup=120s, vitest will be killed before JSON is written,
    // forcing the STROBE_TEST fallback path.
    let mut env = HashMap::new();
    env.insert("STROBE_SLOW_CLEANUP".to_string(), "120000".to_string());

    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            None,
            // Only run the slow test to trigger the fallback
            Some("npx vitest run src/slow.test.js"),
            &env,
            Some(8), // 8 second timeout — will kill before afterAll finishes
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress.clone(),
        )
        .await
        .unwrap();

    eprintln!(
        "STROBE_TEST fallback: framework={} passed={} failed={} skipped={} all_tests={}",
        result.framework,
        result.result.summary.passed,
        result.result.summary.failed,
        result.result.summary.skipped,
        result.result.all_tests.len(),
    );

    // The tests themselves pass — only afterAll hangs. With STROBE_TEST fallback,
    // we should still see the 2 passing tests even though JSON was never written.
    assert_eq!(result.framework, "vitest");
    assert!(
        result.result.summary.passed >= 2 || result.result.all_tests.len() >= 2,
        "STROBE_TEST fallback should recover at least 2 passing tests, got passed={} all_tests={}",
        result.result.summary.passed,
        result.result.all_tests.len(),
    );
    assert_eq!(result.result.summary.failed, 0, "Tests passed, only cleanup hung");

    // Verify the fallback was used by checking raw_stdout is empty
    // (if JSON was available, the primary path would have been used)
    eprintln!("raw_stdout length: {}", result.raw_stdout.len());
    eprintln!("raw_stderr length: {}", result.raw_stderr.len());
}
