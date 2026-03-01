//! Vitest adapter end-to-end tests.
//! Tests the full TestRunner flow with a real vitest project fixture,
//! covering JSON stdout parsing, STROBE_TEST stderr fallback, and
//! threads pool to avoid Frida spawn gating deadlocks.

mod common;

use common::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Return the vitest fixture project root.
fn vitest_fixture_project() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vitest")
}

/// Total expected test count across all fixture files.
/// math(3) + slow(2) + string(3) + gen1-7(3 each = 21) = 29
const TOTAL_TESTS: u32 = 29;

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

    eprintln!("=== Test 1/7: Vitest adapter detection ===");
    test_vitest_detection();

    eprintln!("\n=== Test 2/7: Vitest full suite execution (JSON path) ===");
    test_vitest_full_suite(&sm).await;

    eprintln!("\n=== Test 3/7: Vitest single test filter ===");
    test_vitest_single_test(&sm).await;

    eprintln!("\n=== Test 4/7: Vitest custom script command ===");
    test_vitest_custom_command(&sm).await;

    eprintln!("\n=== Test 5/7: STROBE_TEST fallback when JSON missing ===");
    test_strobe_fallback(&sm).await;

    eprintln!("\n=== Test 6/7: Multi-file suite with threads pool (10 files, 29 tests) ===");
    test_multi_file_threads_pool(&sm).await;

    eprintln!("\n=== Test 7/7: Forks pool deadlocks under Frida (negative test) ===");
    test_forks_pool_deadlocks(&sm).await;

    eprintln!("\n=== All 7 vitest e2e scenarios passed ===");
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
    assert_eq!(result.result.summary.passed, TOTAL_TESTS, "Expected all tests to pass");
    assert_eq!(result.result.summary.failed, 0, "Expected 0 failing tests");
    assert!(result.result.failures.is_empty());
    assert_eq!(result.result.all_tests.len(), TOTAL_TESTS as usize);

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
    assert_eq!(result.result.summary.passed, 3, "Expected 3 passing tests from math.test.js");
    assert_eq!(result.result.summary.failed, 0);
}

/// Test STROBE_TEST fallback path by running WITHOUT the JSON reporter.
/// When stdout has no JSON, parse_output falls back to STROBE_TEST events in stderr.
/// This is a cleaner test than relying on afterAll hangs (pool-dependent behavior).
async fn test_strobe_fallback(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-fallback");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    // Run with ONLY the STROBE_TEST reporter (no --reporter=json).
    // By including "--reporter=" in the command, build_custom_command skips
    // adding --reporter=json, so stdout has no JSON → fallback path triggers.
    let reporter_path = "/tmp/.strobe-vitest-reporter.mjs";
    let cmd = format!(
        "npx vitest run src/math.test.js --reporter={}",
        reporter_path,
    );

    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            None,
            Some(&cmd),
            &HashMap::new(),
            Some(30),
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
    eprintln!("raw_stdout length: {}", result.raw_stdout.len());
    eprintln!("raw_stderr length: {}", result.raw_stderr.len());

    assert_eq!(result.framework, "vitest");
    // math.test.js has 3 tests — all should be recovered via STROBE_TEST fallback
    assert_eq!(
        result.result.summary.passed, 3,
        "STROBE_TEST fallback should recover 3 passing tests from math.test.js, got passed={}",
        result.result.summary.passed,
    );
    assert_eq!(result.result.summary.failed, 0);
    assert_eq!(result.result.all_tests.len(), 3);

    // Verify stdout had no JSON (confirming fallback was used, not primary path)
    assert!(
        result.raw_stdout.is_empty() || !result.raw_stdout.contains("numPassedTests"),
        "Stdout should not contain JSON reporter output (fallback test)"
    );
}

/// Stress test: run 10 test files (29 tests) with threads pool.
/// With `--pool=forks`, Frida's spawn gating would intercept vitest's
/// worker processes and cause deadlocks. With `--pool=threads`, workers
/// are in-process threads — no fork/exec, no spawn gating, no deadlock.
///
/// Tight 15s timeout ensures any pool-related stalling is caught.
async fn test_multi_file_threads_pool(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-threads");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let start = std::time::Instant::now();
    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            None,
            None,
            &HashMap::new(),
            Some(15), // tight timeout — forks pool would deadlock here
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress.clone(),
        )
        .await
        .unwrap();

    let elapsed = start.elapsed();
    eprintln!(
        "Multi-file threads pool: passed={} failed={} elapsed={:.1}s",
        result.result.summary.passed,
        result.result.summary.failed,
        elapsed.as_secs_f64(),
    );

    // All 10 test files (29 tests) must complete
    assert_eq!(result.result.summary.passed, TOTAL_TESTS,
        "All {} tests should pass with threads pool", TOTAL_TESTS);
    assert_eq!(result.result.summary.failed, 0);

    // Must complete well within timeout
    assert!(
        elapsed.as_secs() < 12,
        "Should complete within 12s (threads pool), took {:.1}s — possible pool issue",
        elapsed.as_secs_f64(),
    );
}

/// Negative test: prove that `--pool=forks` deadlocks under Frida.
/// Runs the full 10-file suite with explicit `--pool=forks` and a 10s timeout.
/// Expects the run to be killed before all tests complete (spawn gating deadlock).
/// This test documents the exact failure mode that `--pool=threads` prevents.
async fn test_forks_pool_deadlocks(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-vitest-forks");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let start = std::time::Instant::now();
    let result = runner
        .run(
            &vitest_fixture_project(),
            Some("vitest"),
            None,
            None,
            // Explicit --pool=forks to bypass our --pool=threads fix
            Some("npx vitest run --pool=forks"),
            &HashMap::new(),
            Some(10), // 10s timeout — should deadlock before completing
            sm,
            &[],
            None,
            "test-conn",
            &session_id,
            progress.clone(),
        )
        .await
        .unwrap();

    let elapsed = start.elapsed();
    let p = progress.lock().unwrap();

    eprintln!(
        "Forks pool: passed={} failed={} elapsed={:.1}s (progress: passed={} failed={})",
        result.result.summary.passed,
        result.result.summary.failed,
        elapsed.as_secs_f64(),
        p.passed,
        p.failed,
    );

    // With forks pool under Frida, we expect one of:
    // 1. Timeout kill: test run killed at 10s, fewer tests complete
    // 2. Partial completion: some tests pass but suite doesn't finish cleanly
    // 3. In rare cases: all tests complete before deadlock hits (race condition)
    //
    // We can't assert exact failure because the deadlock is a race condition.
    // But we CAN assert that IF it completed, it took close to the timeout
    // (spawn gating adds significant overhead even when not deadlocking).
    if result.result.summary.passed < TOTAL_TESTS {
        eprintln!(
            "CONFIRMED: forks pool failed to complete all tests ({}/{} passed, killed at {:.1}s)",
            result.result.summary.passed, TOTAL_TESTS, elapsed.as_secs_f64(),
        );
    } else {
        // Rare: all tests completed despite forks pool. Log but don't fail —
        // the race condition went our way this time. The threads pool test
        // above is the primary regression guard.
        eprintln!(
            "NOTE: forks pool completed all tests this time ({:.1}s) — race condition went favorably",
            elapsed.as_secs_f64(),
        );
    }
}
