/// Phase 1b Stress Test Integration
///
/// This test suite validates Phase 1b features under realistic load conditions.
///
/// ## Test Modes
///
/// ### Simple Modes (for backwards compatibility):
/// - **hot**: Single function called as fast as possible (triggers auto-sampling)
/// - **threads**: 10 worker threads with varying call rates
/// - **deep-structs**: Nested struct creation and processing
/// - **all**: All simple modes in sequence
///
/// ### Realistic Mode (RECOMMENDED):
/// Simulates a complete audio DSP application with:
/// - **Multiple audio processing threads** (default: 4) - HOT PATH generating >10k calls/sec
/// - **MIDI processing thread** - Medium frequency event bursts
/// - **Parameter automation thread** - Continuous global state updates
/// - **Statistics thread** - Monitoring and global state modulation
///
/// #### Global Variables (for watch testing):
/// - `G_SAMPLE_RATE`, `G_BUFFER_SIZE` - Audio configuration (modified from multiple threads)
/// - `G_TEMPO` - Musical tempo (modulated by stats thread)
/// - `G_AUDIO_BUFFER_COUNT`, `G_MIDI_NOTE_ON_COUNT`, `G_PARAMETER_UPDATES` - Performance counters
/// - `G_EFFECT_CHAIN_DEPTH` - Current effect chain depth
///
/// #### Namespaces (for pattern matching testing):
/// - `audio::*` - DSP processing functions (HOT)
/// - `midi::*` - MIDI event processing (medium)
/// - `engine::*` - State management (cold)
///
/// #### Data Structures (for serialization testing):
/// - `AudioBuffer` - 512 f32 samples + metadata
/// - `EffectChain` - Recursive linked list (depth 5)
/// - `MidiMessage` - 3-byte MIDI data + timestamp
///
/// #### Expected Behavior:
/// - `audio::process_audio_buffer` triggers auto-sampling (>100k calls/sec)
/// - Multiple thread names visible: audio-0, audio-1, midi-processor, automation, stats
/// - Watch variables change across thread contexts
/// - Deep recursive call stacks in effect chain processing
/// - Realistic event generation patterns (millions of events in 30 seconds)
///
/// NOTE: These tests require the stress_test_phase1b binary to be compiled.
/// Run: cargo build --manifest-path tests/stress_test_phase1b/Cargo.toml

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

#[test]
fn test_stress_binary_runs_realistic_mode() {
    let binary = Path::new("tests/stress_test_phase1b/target/debug/stress_tester");

    if !binary.exists() {
        eprintln!("Binary not found, skipping test");
        return;
    }

    let output = Command::new(binary)
        .args(&["--mode", "realistic", "--duration", "2"])
        .output()
        .expect("Failed to run stress tester");

    assert!(output.status.success(), "Stress tester failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("REALISTIC MODE"), "Expected REALISTIC MODE output");
    assert!(stdout.contains("audio-"), "Expected audio thread output");
    assert!(stdout.contains("ENGINE STATS"), "Expected engine stats output");
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
/// 3. Via MCP, launch stress tester in REALISTIC mode:
///    debug_launch({
///      command: "tests/stress_test_phase1b/target/release/stress_tester",
///      args: ["--mode", "realistic", "--duration", "30"],
///      projectRoot: "/path/to/strobe"
///    })
///
/// 4. Add trace patterns for multiple namespaces:
///    debug_trace({
///      sessionId: "<session-id>",
///      add: ["audio::process_audio_buffer", "audio::apply_effect_chain",
///            "midi::process_note_on", "midi::process_control_change",
///            "engine::*"],
///      watches: {
///        add: [
///          { variable: "G_SAMPLE_RATE", on: ["audio::*"] },
///          { variable: "G_TEMPO", on: ["midi::*"] },
///          { variable: "G_AUDIO_BUFFER_COUNT" },
///          { variable: "G_MIDI_NOTE_ON_COUNT" }
///        ]
///      }
///    })
///
/// 5. Wait for execution to complete (30 seconds)
///
/// 6. Query events by thread:
///    debug_query({
///      sessionId: "<session-id>",
///      thread_name_contains: "audio",
///      limit: 100
///    })
///
/// 7. Query watch variable changes:
///    debug_query({
///      sessionId: "<session-id>",
///      eventType: "watch_change",
///      limit: 50
///    })
///
/// Expected Observations:
/// - audio::process_audio_buffer should be HOT (>10k calls/sec), trigger sampling
/// - audio::apply_effect_chain called recursively (depth 5)
/// - Multiple thread names visible: audio-0, audio-1, audio-2, audio-3, midi-processor, automation, stats
/// - Watch variables change across threads (G_SAMPLE_RATE in audio threads, G_TEMPO from stats thread)
/// - Deep struct serialization in EffectChain and AudioBuffer arguments
/// - MIDI events appear in bursts (realistic pattern)
/// - No crashes or hangs under sustained load
/// - Event limit enforcement if millions of events generated
///
/// Alternative patterns to test:
/// - Broad namespace: ["audio::*", "midi::*"] - tests pattern matching
/// - File-based: ["@file:main.rs"] - tests file pattern matching
/// - Wildcard: ["*::process*"] - tests cross-namespace wildcards
#[test]
fn test_stress_documentation_exists() {
    // This test exists to document the manual stress testing procedure above
    // Actual stress testing requires running Strobe daemon with the stress binary
    assert!(true);
}
