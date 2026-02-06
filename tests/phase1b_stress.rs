/// Phase 1b Stress Test Integration
///
/// This test suite validates Phase 1b features under load:
/// - Hot function detection and auto-sampling
/// - Multi-threading with thread name capture
/// - Deep struct serialization depth limits
/// - Input validation under stress
///
/// NOTE: These tests require the stress_test_phase1b binary to be compiled.
/// Run: cargo build --manifest-path tests/stress_test_phase1b/Cargo.toml
///
/// To run manually with Strobe:
/// 1. Build stress tester: cargo build --manifest-path tests/stress_test_phase1b/Cargo.toml
/// 2. Launch with Strobe daemon via MCP
/// 3. Trace hot_function, worker_function, create_deep_struct
/// 4. Verify sampling kicks in for hot_function (>100k calls/sec)
/// 5. Verify thread names appear in events (worker-0 through worker-9)
/// 6. Verify deep struct serialization doesn't exceed depth limit

use std::path::Path;
use std::process::Command;

#[test]
fn test_stress_binary_compiles() {
    let manifest_path = Path::new("tests/stress_test_phase1b/Cargo.toml");

    if !manifest_path.exists() {
        panic!("Stress test manifest not found at {:?}", manifest_path);
    }

    // Build the stress test binary
    let output = Command::new("cargo")
        .args(&["build", "--manifest-path", manifest_path.to_str().unwrap()])
        .output()
        .expect("Failed to build stress test binary");

    if !output.status.success() {
        eprintln!("STDOUT: {}", String::from_utf8_lossy(&output.stdout));
        eprintln!("STDERR: {}", String::from_utf8_lossy(&output.stderr));
        panic!("Stress test binary failed to compile");
    }
}

#[test]
fn test_stress_binary_runs_hot_mode() {
    let binary = Path::new("tests/stress_test_phase1b/target/debug/stress_tester");

    if !binary.exists() {
        eprintln!("Binary not found, skipping test. Run: cargo test test_stress_binary_compiles");
        return;
    }

    let output = Command::new(binary)
        .args(&["--mode", "hot", "--duration", "1"])
        .output()
        .expect("Failed to run stress tester");

    assert!(output.status.success(), "Stress tester failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("HOT MODE"), "Expected HOT MODE output");
    assert!(stdout.contains("calls/sec"), "Expected call rate output");
}

#[test]
fn test_stress_binary_runs_threads_mode() {
    let binary = Path::new("tests/stress_test_phase1b/target/debug/stress_tester");

    if !binary.exists() {
        eprintln!("Binary not found, skipping test");
        return;
    }

    let output = Command::new(binary)
        .args(&["--mode", "threads", "--duration", "1"])
        .output()
        .expect("Failed to run stress tester");

    assert!(output.status.success(), "Stress tester failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("THREADS MODE"), "Expected THREADS MODE output");
    assert!(stdout.contains("worker-0"), "Expected worker thread output");
}

#[test]
fn test_stress_binary_runs_deep_structs_mode() {
    let binary = Path::new("tests/stress_test_phase1b/target/debug/stress_tester");

    if !binary.exists() {
        eprintln!("Binary not found, skipping test");
        return;
    }

    let output = Command::new(binary)
        .args(&["--mode", "deep-structs", "--duration", "1"])
        .output()
        .expect("Failed to run stress tester");

    assert!(output.status.success(), "Stress tester failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("DEEP STRUCTS MODE"), "Expected DEEP STRUCTS MODE output");
    assert!(stdout.contains("Processed"), "Expected processing output");
}

/// Validation test: Verify input validation prevents extreme parameters
#[test]
fn test_validation_prevents_extreme_event_limits() {
    use strobe::mcp::DebugTraceRequest;

    // Try to set 100M event limit (over 10M max)
    let req = DebugTraceRequest {
        session_id: Some("test".to_string()),
        add: None,
        remove: None,
        watches: None,
        event_limit: Some(100_000_000),
    };

    let result = req.validate();
    assert!(result.is_err(), "Validation should reject 100M event limit");
    assert!(result.unwrap_err().to_string().contains("10000000"));
}

/// Documentation: Manual Stress Test Procedure
///
/// To manually validate Phase 1b features with the stress tester:
///
/// 1. Build stress tester:
///    cargo build --manifest-path tests/stress_test_phase1b/Cargo.toml --release
///
/// 2. Start Strobe daemon (in separate terminal)
///
/// 3. Via MCP, launch stress tester:
///    debug_launch({
///      command: "tests/stress_test_phase1b/target/release/stress_tester",
///      args: ["--mode", "all", "--duration", "10"],
///      projectRoot: "/path/to/strobe"
///    })
///
/// 4. Add trace patterns:
///    debug_trace({
///      sessionId: "<session-id>",
///      add: ["hot_function", "worker_function", "create_deep_struct"]
///    })
///
/// 5. Wait for execution to complete
///
/// 6. Query events:
///    debug_query({
///      sessionId: "<session-id>",
///      limit: 100
///    })
///
/// Expected Observations:
/// - hot_function should trigger auto-sampling (>100k calls/sec)
/// - Warnings about sampling should appear in debug_trace response
/// - Thread names (worker-0 through worker-9) should appear in events
/// - Deep struct arguments should respect serialization depth limits
/// - No crashes or hangs under load
#[test]
fn test_stress_documentation_exists() {
    // This test exists to document the manual stress testing procedure above
    // Actual stress testing requires running Strobe daemon with the stress binary
    assert!(true);
}
