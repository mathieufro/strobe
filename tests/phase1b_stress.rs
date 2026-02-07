/// Phase 1b Stress Test Integration
///
/// This test suite validates Phase 1b features under realistic load conditions.
///
/// ## Stress Test Binary
///
/// Simulates a complete audio DSP application with:
/// - **Multiple audio processing threads** (default: 4) - HOT PATH generating >10k calls/sec
/// - **MIDI processing thread** - Medium frequency event bursts
/// - **Parameter automation thread** - Continuous global state updates
/// - **Statistics thread** - Monitoring and global state modulation
///
/// ### Global Variables (for watch testing):
/// - `G_SAMPLE_RATE`, `G_BUFFER_SIZE` - Audio configuration (modified from multiple threads)
/// - `G_TEMPO` - Musical tempo (modulated by stats thread)
/// - `G_AUDIO_BUFFER_COUNT`, `G_MIDI_NOTE_ON_COUNT`, `G_PARAMETER_UPDATES` - Performance counters
/// - `G_EFFECT_CHAIN_DEPTH` - Current effect chain depth
///
/// ### Namespaces (for pattern matching testing):
/// - `audio::*` - DSP processing functions (HOT)
/// - `midi::*` - MIDI event processing (medium)
/// - `engine::*` - State management (cold)
///
/// ### Data Structures (for serialization testing):
/// - `AudioBuffer` - 512 f32 samples + metadata
/// - `EffectChain` - Recursive linked list (depth 5)
/// - `MidiMessage` - 3-byte MIDI data + timestamp
///
/// ### Cross-Module State Dependencies (Contextual Watch Testing):
/// - `audio::process_audio_buffer` reads: G_TEMPO, G_MIDI_NOTE_ON_COUNT (MIDI state during audio)
/// - `midi::process_note_on` reads: G_SAMPLE_RATE, G_AUDIO_BUFFER_COUNT (audio state during MIDI)
/// - `midi::process_control_change` reads: G_AUDIO_BUFFER_COUNT, G_MIDI_NOTE_ON_COUNT, G_TEMPO
///
/// ### Expected Behavior:
/// - `audio::process_audio_buffer` triggers auto-sampling (>100k calls/sec)
/// - Multiple thread names visible: audio-0, audio-1, midi-processor, automation, stats
/// - Watch variables show cross-module reads (e.g., G_TEMPO read from audio threads)
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
fn test_stress_binary_runs() {
    let binary = Path::new("tests/stress_test_phase1b/target/debug/stress_tester");

    if !binary.exists() {
        eprintln!("Binary not found, skipping test. Run: cargo test test_stress_binary_compiles");
        return;
    }

    let output = Command::new(binary)
        .args(&["--duration", "2"])
        .output()
        .expect("Failed to run stress tester");

    assert!(output.status.success(), "Stress tester failed");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("AUDIO DSP STRESS TEST"), "Expected stress test output");
    assert!(stdout.contains("audio-"), "Expected audio thread output");
    assert!(stdout.contains("ENGINE STATS"), "Expected engine stats output");
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
        serialization_depth: None,
    };

    let result = req.validate();
    assert!(result.is_err(), "Validation should reject 100M event limit");
    assert!(result.unwrap_err().to_string().contains("10000000"));
}

// ============ Serialization Depth Stress Validation ============

/// Validate serialization depth rejects extreme values under stress-like parameters
#[test]
fn test_validation_prevents_extreme_serialization_depth() {
    use strobe::mcp::DebugTraceRequest;

    // Try depth 0 (below minimum)
    let req = DebugTraceRequest {
        session_id: Some("stress-test".to_string()),
        add: Some(vec!["audio::*".to_string(), "midi::*".to_string()]),
        remove: None,
        watches: None,
        event_limit: Some(500_000),
        serialization_depth: Some(0),
    };
    assert!(req.validate().is_err(), "Depth 0 should be rejected");

    // Try depth 100 (way above maximum)
    let req = DebugTraceRequest {
        session_id: Some("stress-test".to_string()),
        add: Some(vec!["audio::*".to_string()]),
        remove: None,
        watches: None,
        event_limit: Some(1_000_000),
        serialization_depth: Some(100),
    };
    let err = req.validate().unwrap_err();
    assert!(err.to_string().contains("serialization_depth must be between 1 and 10"));
}

/// Validate serialization depth works correctly combined with all other stress parameters
#[test]
fn test_serialization_depth_with_full_stress_params() {
    use strobe::mcp::{DebugTraceRequest, WatchTarget, WatchUpdate};

    // Full stress configuration: patterns + watches + event limit + serialization depth
    let req = DebugTraceRequest {
        session_id: Some("stress-session".to_string()),
        add: Some(vec![
            "audio::process_audio_buffer".to_string(),
            "audio::apply_effect_chain".to_string(),
            "midi::process_note_on".to_string(),
            "midi::process_control_change".to_string(),
            "engine::*".to_string(),
        ]),
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![
                WatchTarget {
                    variable: Some("G_TEMPO".to_string()),
                    address: None,
                    type_hint: None,
                    label: Some("tempo".to_string()),
                    expr: None,
                    on: Some(vec!["audio::process_audio_buffer".to_string()]),
                },
                WatchTarget {
                    variable: Some("G_MIDI_NOTE_ON_COUNT".to_string()),
                    address: None,
                    type_hint: None,
                    label: Some("midi_notes".to_string()),
                    expr: None,
                    on: Some(vec!["audio::*".to_string()]),
                },
                WatchTarget {
                    variable: Some("G_SAMPLE_RATE".to_string()),
                    address: None,
                    type_hint: None,
                    label: Some("sample_rate".to_string()),
                    expr: None,
                    on: Some(vec!["midi::process_note_on".to_string()]),
                },
                WatchTarget {
                    variable: Some("G_PARAMETER_UPDATES".to_string()),
                    address: None,
                    type_hint: None,
                    label: Some("param_updates".to_string()),
                    expr: None,
                    on: None, // global watch
                },
            ]),
            remove: None,
        }),
        event_limit: Some(500_000),
        serialization_depth: Some(5), // Deep enough for EffectChain recursive struct
    };

    assert!(req.validate().is_ok(), "Full stress config should validate");
}

/// Validate that invalid depth fails even when other stress params are valid
#[test]
fn test_serialization_depth_invalid_with_valid_stress_params() {
    use strobe::mcp::{DebugTraceRequest, WatchTarget, WatchUpdate};

    let req = DebugTraceRequest {
        session_id: Some("stress-session".to_string()),
        add: Some(vec!["audio::*".to_string(), "midi::*".to_string()]),
        remove: None,
        watches: Some(WatchUpdate {
            add: Some(vec![WatchTarget {
                variable: Some("G_TEMPO".to_string()),
                address: None,
                type_hint: None,
                label: Some("tempo".to_string()),
                expr: None,
                on: Some(vec!["audio::*".to_string()]),
            }]),
            remove: None,
        }),
        event_limit: Some(200_000),
        serialization_depth: Some(11), // Just above max
    };

    assert!(req.validate().is_err(), "Depth 11 should fail even with valid watches/limits");
}

/// Validate serialization depth JSON roundtrip in stress-like MCP messages
#[test]
fn test_serialization_depth_mcp_json_stress() {
    use strobe::mcp::DebugTraceRequest;

    // Simulate full MCP message with serializationDepth (camelCase from client)
    let json = r#"{
        "sessionId": "stress-123",
        "add": ["audio::process_audio_buffer", "audio::apply_effect_chain", "midi::*"],
        "serializationDepth": 5,
        "eventLimit": 500000
    }"#;

    let req: DebugTraceRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.serialization_depth, Some(5));
    assert_eq!(req.event_limit, Some(500_000));
    assert_eq!(req.add.as_ref().unwrap().len(), 3);
    assert!(req.validate().is_ok());

    // Roundtrip: serialize back and verify camelCase field names
    let re_serialized = serde_json::to_string(&req).unwrap();
    assert!(re_serialized.contains("serializationDepth"));
    assert!(re_serialized.contains(":5") || re_serialized.contains(": 5"));

    // MCP message without serializationDepth (should default to None)
    let json_no_depth = r#"{"sessionId": "stress-456", "add": ["audio::*"]}"#;
    let req2: DebugTraceRequest = serde_json::from_str(json_no_depth).unwrap();
    assert_eq!(req2.serialization_depth, None);
    assert!(req2.validate().is_ok());
}

/// Validate all boundary values for serialization depth
#[test]
fn test_serialization_depth_boundary_stress() {
    use strobe::mcp::DebugTraceRequest;

    let make_req = |depth: u32| DebugTraceRequest {
        session_id: Some("stress".to_string()),
        add: Some(vec!["audio::*".to_string(), "midi::*".to_string(), "engine::*".to_string()]),
        remove: None,
        watches: None,
        event_limit: Some(1_000_000),
        serialization_depth: Some(depth),
    };

    // All valid values pass
    for depth in 1..=10 {
        assert!(make_req(depth).validate().is_ok(), "depth={} should pass", depth);
    }

    // Invalid values fail
    for depth in [0, 11, 50, 100, 255, u32::MAX] {
        assert!(make_req(depth).validate().is_err(), "depth={} should fail", depth);
    }
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
///      args: ["--duration", "30"],
///      projectRoot: "/path/to/strobe"
///    })
///
/// 4. Add trace patterns with serialization depth and contextual watches:
///    debug_trace({
///      sessionId: "<session-id>",
///      add: ["audio::process_audio_buffer", "audio::apply_effect_chain",
///            "midi::process_note_on", "midi::process_control_change",
///            "engine::*"],
///      serializationDepth: 5,  // Deep enough for EffectChain (recursive depth 5)
///      watches: {
///        add: [
///          // Cross-module contextual watches:
///          { variable: "G_TEMPO", on: ["audio::process_audio_buffer"] },
///          { variable: "G_MIDI_NOTE_ON_COUNT", on: ["audio::process_audio_buffer"] },
///          { variable: "G_SAMPLE_RATE", on: ["midi::process_note_on"] },
///          { variable: "G_AUDIO_BUFFER_COUNT", on: ["midi::process_note_on"] },
///          // Global watches:
///          { variable: "G_PARAMETER_UPDATES" }
///        ]
///      }
///    })
///
/// 5. Wait for execution to complete (30 seconds)
///
/// 6. Query events with verbose output (includes serialized arguments):
///    debug_query({
///      sessionId: "<session-id>",
///      function: { contains: "apply_effect_chain" },
///      verbose: true,
///      limit: 20
///    })
///
/// 7. Verify serialization depth behavior:
///    - Arguments should show structured data up to depth 5
///    - EffectChain recursive pointers resolved through linked list
///    - Circular references detected and reported as "<circular ref to 0x...>"
///    - Depth exceeded shown as "<max depth 5 reached>"
///    - AudioBuffer struct members visible: sample_count, channel_count, etc.
///
/// 8. Test with different depths for comparison:
///    debug_trace({
///      sessionId: "<session-id>",
///      serializationDepth: 2  // Shallow - only top-level struct visible
///    })
///
/// Expected Observations:
/// - audio::process_audio_buffer should be HOT (>10k calls/sec), trigger sampling
/// - audio::apply_effect_chain called recursively (depth 5)
/// - Multiple thread names visible: audio-0, audio-1, audio-2, audio-3, midi-processor, automation, stats
/// - **SERIALIZATION DEPTH**: Arguments show structured data, not raw hex pointers
/// - **CIRCULAR REFS**: Recursive EffectChain stops at circular reference or max depth
/// - **CONTEXTUAL WATCHES**: G_TEMPO captured only during audio::process_audio_buffer
/// - **CROSS-MODULE READS**: Audio threads reading MIDI state (G_TEMPO, G_MIDI_NOTE_ON_COUNT)
/// - Deep struct serialization in EffectChain and AudioBuffer arguments
/// - MIDI events appear in bursts (realistic pattern)
/// - No crashes or hangs under sustained load
/// - Event limit enforcement if millions of events generated
///
/// Alternative patterns to test:
/// - Broad namespace: ["audio::*", "midi::*"] - tests pattern matching
/// - File-based: ["@file:main.rs"] - tests file pattern matching
/// - Wildcard: ["*::process*"] - tests cross-namespace wildcards
/// - Varying depth: serializationDepth 1 vs 3 vs 10 for performance comparison
#[test]
fn test_stress_documentation_exists() {
    // This test exists to document the manual stress testing procedure above
    // Actual stress testing requires running Strobe daemon with the stress binary
    assert!(true);
}
