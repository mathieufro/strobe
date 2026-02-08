# Integration Test Restructure — Implementation Plan

**Spec:** `docs/specs/2026-02-08-integration-test-restructure.md`
**Goal:** Replace phase-named, duplicated integration test files with clean, feature-based tests backed by purpose-built C++ and Rust fixture programs.
**Architecture:** Two fixture programs (C++/CMake + Rust/Cargo) auto-built on first use via `tests/common/mod.rs`. Three integration test files organized by feature: `frida_e2e.rs`, `test_runner.rs`, `stress.rs`. Unit tests migrated from `tests/` back into source files.
**Commit strategy:** Commit at checkpoints (4 commits total)

## Workstreams

- **Stream A (Fixtures):** Tasks 1, 2 — independent, parallelizable
- **Stream B (Infrastructure):** Task 3 — depends on A (needs fixture paths)
- **Stream C (Unit test migration):** Task 4 — independent of A/B
- **Serial:** Tasks 5, 6, 7 — depend on A, B, C
- **Serial:** Task 8 — depends on everything above

---

### Task 1: Create C++ Fixture Program

**Files:**
- Create: `tests/fixtures/cpp/CMakeLists.txt`
- Create: `tests/fixtures/cpp/src/globals.h`
- Create: `tests/fixtures/cpp/src/audio.h`
- Create: `tests/fixtures/cpp/src/audio.cpp`
- Create: `tests/fixtures/cpp/src/midi.h`
- Create: `tests/fixtures/cpp/src/midi.cpp`
- Create: `tests/fixtures/cpp/src/crash.h`
- Create: `tests/fixtures/cpp/src/crash.cpp`
- Create: `tests/fixtures/cpp/src/timing.h`
- Create: `tests/fixtures/cpp/src/timing.cpp`
- Create: `tests/fixtures/cpp/src/main.cpp`
- Create: `tests/fixtures/cpp/tests/test_main.cpp`

**Step 1: Create globals.h**

Shared global variables for watch expression testing. These need to be `extern` in the header, defined in one `.cpp` file.

```cpp
// tests/fixtures/cpp/src/globals.h
#pragma once
#include <cstdint>

struct Point {
    int32_t x, y;
    double value;
};

extern uint32_t g_counter;
extern double g_tempo;
extern int64_t g_sample_rate;
extern Point* g_point_ptr;
```

**Step 2: Create audio module**

`audio.h` + `audio.cpp` — namespace `audio` with buffer processing functions. The key requirement is realistic C++ namespaced function names for pattern matching tests (`audio::*`, `audio::**`, `@file:audio.cpp`).

Functions:
- `audio::process_buffer(AudioBuffer*)` → returns RMS as float
- `audio::generate_sine(float freq)` → returns AudioBuffer
- `audio::apply_effect(AudioBuffer*, float gain)` → modifies buffer in-place

`AudioBuffer` struct: `float samples[512]; uint32_t sample_rate; uint32_t size;`

Implementation should do real (simple) math so the functions aren't optimized away.

**Step 3: Create midi module**

`midi.h` + `midi.cpp` — namespace `midi`.

Functions:
- `midi::note_on(uint8_t note, uint8_t velocity)` → returns bool
- `midi::control_change(uint8_t cc, uint8_t value)` → returns bool
- `midi::generate_sequence(int length)` → returns vector of MidiMessage

Touches `g_counter` to make global watch testing meaningful.

**Step 4: Create crash module**

`crash.h` + `crash.cpp` — namespace `crash`. Port from existing `tests/stress_test_phase1c/main.c`:

- `crash::null_deref()` — NULL dereference with interesting locals (counter, ratio, buffer)
- `crash::abort_signal()` — calls `abort()`
- `crash::stack_overflow(int depth)` — recursive stack overflow

Key: `null_deref()` must have meaningful local variables (int, float, char[]) that DWARF can resolve, since existing tests verify locals in crash frames.

**Step 5: Create timing module**

`timing.h` + `timing.cpp` — namespace `timing`.

- `timing::fast()` — ~0ms (volatile loop)
- `timing::medium()` — ~5ms (computation loop)
- `timing::slow()` — ~50ms (usleep)
- `timing::very_slow()` — ~500ms (usleep)

These durations match the existing test expectations in `phase1c_e2e.rs` scenario_duration_query (checks for `>= 40ms`).

**Step 6: Create main.cpp CLI dispatcher**

Port from existing `tests/stress_test_phase1c/main.c` but add new modes:

```cpp
// tests/fixtures/cpp/src/main.cpp
#include "globals.h"
#include "audio.h"
#include "midi.h"
#include "crash.h"
#include "timing.h"
#include <cstdio>
#include <cstring>
#include <unistd.h>
#include <thread>
#include <sys/wait.h>

// Define globals
uint32_t g_counter = 0;
double g_tempo = 120.0;
int64_t g_sample_rate = 44100;
static Point g_point = {10, 20, 99.9};
Point* g_point_ptr = &g_point;

int main(int argc, char* argv[]) {
    const char* mode = (argc > 1) ? argv[1] : "hello";

    if (strcmp(mode, "hello") == 0) {
        printf("Hello from strobe_test_target\n");
        fprintf(stderr, "Debug output on stderr\n");
    } else if (strcmp(mode, "crash-null") == 0) {
        printf("[TARGET] About to crash (null deref)\n");
        fflush(stdout);
        crash::null_deref();
    } else if (strcmp(mode, "crash-abort") == 0) {
        printf("[TARGET] About to abort\n");
        fflush(stdout);
        crash::abort_signal();
    } else if (strcmp(mode, "crash-stack") == 0) {
        crash::stack_overflow(0);
    } else if (strcmp(mode, "fork-workers") == 0) {
        // Port fork_workers() from stress_test_phase1c/main.c
        // Fork 3 children, each doing work, parent waits
        ...
    } else if (strcmp(mode, "fork-exec") == 0) {
        // Port fork_exec() from stress_test_phase1c/main.c
        ...
    } else if (strcmp(mode, "slow-functions") == 0) {
        printf("[TIMING] Running functions with varied durations...\n");
        for (int round = 0; round < 5; round++) {
            timing::fast(); timing::fast(); timing::fast();
            timing::medium();
            timing::slow();
            if (round == 2) timing::very_slow();
        }
        printf("[TIMING] Done\n");
    } else if (strcmp(mode, "threads") == 0) {
        // Spawn named threads doing work with globals
        ...
    } else if (strcmp(mode, "globals") == 0) {
        // Initialize globals, loop updating them, sleep
        printf("[GLOBALS] Starting global variable updates\n");
        for (int i = 0; i < 50; i++) {
            g_counter = i;
            g_tempo = 120.0 + (i % 10);
            g_point.x = i;
            g_point.y = i * 2;
            audio::AudioBuffer buf = audio::generate_sine(440.0f);
            audio::process_buffer(&buf);
            usleep(100000); // 100ms
        }
        printf("[GLOBALS] Done\n");
    } else {
        fprintf(stderr, "Unknown mode: %s\n", mode);
        return 1;
    }
    return 0;
}
```

The `fork-workers`, `fork-exec`, and `threads` modes should be ported from the existing `tests/stress_test_phase1c/main.c` implementation, which is known to work with Frida.

**Step 7: Create CMakeLists.txt**

Use the exact CMakeLists.txt from the spec. Key points:
- `CMAKE_BUILD_TYPE Debug` always (need DWARF)
- FetchContent for Catch2 v3.5.0
- Static library `fixture_lib` shared between target and test suite
- Two executables: `strobe_test_target` and `strobe_test_suite`

**Step 8: Create Catch2 test suite**

`tests/fixtures/cpp/tests/test_main.cpp` — ~10 normal tests + 1 stuck + 1 intentional failure.

```cpp
#include <catch2/catch_test_macros.hpp>
#include "audio.h"
#include "midi.h"
#include "timing.h"

TEST_CASE("Audio buffer processing", "[unit][audio]") {
    auto buf = audio::generate_sine(440.0f);
    float rms = audio::process_buffer(&buf);
    REQUIRE(rms > 0.0f);
}

TEST_CASE("Audio apply effect", "[unit][audio]") {
    auto buf = audio::generate_sine(440.0f);
    audio::apply_effect(&buf, 2.0f);
    float rms = audio::process_buffer(&buf);
    REQUIRE(rms > 0.0f);
}

TEST_CASE("MIDI note on", "[unit][midi]") {
    REQUIRE(midi::note_on(60, 100));
}

TEST_CASE("MIDI control change", "[unit][midi]") {
    REQUIRE(midi::control_change(1, 64));
}

TEST_CASE("MIDI sequence generation", "[unit][midi]") {
    auto seq = midi::generate_sequence(8);
    REQUIRE(seq.size() == 8);
}

TEST_CASE("Timing fast function", "[integration][timing]") {
    timing::fast();
    REQUIRE(true);
}

TEST_CASE("Timing medium function", "[integration][timing]") {
    timing::medium();
    REQUIRE(true);
}

TEST_CASE("Timing slow function", "[integration][timing]") {
    timing::slow();
    REQUIRE(true);
}

// Intentionally failing test (for adapter validation)
TEST_CASE("Intentional failure", "[unit][expected-fail]") {
    REQUIRE(1 == 2);
}

// Intentionally stuck test (for stuck detector validation)
TEST_CASE("Stuck test - infinite loop", "[stuck]") {
    volatile bool done = false;
    while (!done) { }
}
```

Tags: `[unit]`, `[integration]`, `[stuck]`, `[expected-fail]` — enable Catch2 tag filtering.

**Step 9: Verify C++ fixture builds**

Run:
```bash
cd tests/fixtures/cpp && cmake -B build -DCMAKE_BUILD_TYPE=Debug && cmake --build build --parallel
```
Expected: Both `strobe_test_target` and `strobe_test_suite` binaries produced in `build/`.

Verify the target binary works:
```bash
./build/strobe_test_target hello
# Expected: "Hello from strobe_test_target" on stdout, "Debug output on stderr" on stderr
```

**Checkpoint:** C++ fixture builds and `strobe_test_target hello` prints expected output.

---

### Task 2: Create Rust Fixture Program

**Files:**
- Create: `tests/fixtures/rust/Cargo.toml`
- Create: `tests/fixtures/rust/src/main.rs`
- Create: `tests/fixtures/rust/src/lib.rs`
- Create: `tests/fixtures/rust/src/audio.rs`
- Create: `tests/fixtures/rust/src/midi.rs`
- Create: `tests/fixtures/rust/src/engine.rs`

**Step 1: Create Cargo.toml**

```toml
[package]
name = "strobe_test_fixture"
version = "0.1.0"
edition = "2021"
```

No dependencies needed — keep it simple.

**Step 2: Create audio.rs**

Port from existing `tests/stress_test_phase1b/src/main.rs` audio module but simplify. Key functions:
- `pub fn process_buffer(samples: &[f32]) -> f32` — compute RMS
- `pub fn generate_sine(freq: f32) -> Vec<f32>` — generate 512 samples of sine wave
- `pub fn apply_effect(samples: &mut [f32], gain: f32)` — multiply by gain

These give us `strobe_test_fixture::audio::process_buffer` etc. for pattern matching.

**Step 3: Create midi.rs**

- `pub fn note_on(note: u8, velocity: u8) -> bool`
- `pub fn control_change(cc: u8, value: u8) -> bool`
- `pub fn generate_sequence(length: usize) -> Vec<(u8, u8)>`

**Step 4: Create engine.rs**

- `pub fn update_state()` — touches global atomics
- `pub fn print_stats()` — prints current state to stdout

**Step 5: Create lib.rs**

```rust
pub mod audio;
pub mod midi;
pub mod engine;

use std::sync::atomic::{AtomicU64, Ordering};

pub static G_SAMPLE_RATE: AtomicU64 = AtomicU64::new(44100);
pub static G_TEMPO: AtomicU64 = AtomicU64::new(120_000);
pub static G_BUFFER_COUNT: AtomicU64 = AtomicU64::new(0);
pub static G_NOTE_COUNT: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_process() {
        let buf = audio::generate_sine(440.0);
        let rms = audio::process_buffer(&buf);
        assert!(rms > 0.0);
    }

    #[test]
    fn test_midi_note_on() {
        assert!(midi::note_on(60, 100));
    }

    #[test]
    fn test_engine_update() {
        engine::update_state();
    }

    #[test]
    #[ignore]
    fn test_ignored_for_now() {
        todo!("not implemented yet");
    }

    #[test]
    fn test_intentional_failure() {
        assert_eq!(1, 2, "intentional failure for adapter testing");
    }
}
```

The test results are deterministic: 3 pass, 1 fail, 1 skip (ignored). This is what `test_runner.rs` will verify.

**Step 6: Create main.rs**

```rust
use strobe_test_fixture::*;
use std::sync::atomic::Ordering;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "basic".to_string());

    match mode.as_str() {
        "basic" => {
            println!("Running basic mode");
            let buf = audio::generate_sine(440.0);
            let rms = audio::process_buffer(&buf);
            println!("Audio RMS: {}", rms);
            midi::note_on(60, 100);
            midi::control_change(1, 64);
            engine::update_state();
            engine::print_stats();
            println!("Done");
        }
        "threads" => {
            // Spawn named threads doing work with globals
            let handles: Vec<_> = (0..2).map(|i| {
                std::thread::Builder::new()
                    .name(format!("audio-{}", i))
                    .spawn(move || {
                        for _ in 0..100 {
                            let buf = audio::generate_sine(440.0);
                            audio::process_buffer(&buf);
                            G_BUFFER_COUNT.fetch_add(1, Ordering::Relaxed);
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }).unwrap()
            }).collect();

            let midi_handle = std::thread::Builder::new()
                .name("midi-processor".to_string())
                .spawn(|| {
                    for note in 0..100u8 {
                        midi::note_on(note % 128, 100);
                        G_NOTE_COUNT.fetch_add(1, Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }).unwrap();

            for h in handles { h.join().unwrap(); }
            midi_handle.join().unwrap();
            engine::print_stats();
        }
        "globals" => {
            // Initialize atomics, sleep (for watch variable testing on running process)
            println!("[GLOBALS] Starting");
            for i in 0..50u64 {
                G_BUFFER_COUNT.store(i, Ordering::Relaxed);
                G_NOTE_COUNT.store(i * 2, Ordering::Relaxed);
                G_TEMPO.store(120_000 + i * 100, Ordering::Relaxed);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            println!("[GLOBALS] Done");
        }
        _ => {
            eprintln!("Unknown mode: {}", mode);
            std::process::exit(1);
        }
    }
}
```

**Step 7: Build and verify**

Run:
```bash
cd tests/fixtures/rust && cargo build
dsymutil target/debug/strobe_test_fixture  # macOS only
./target/debug/strobe_test_fixture basic
```
Expected: Prints audio RMS, "Done" message.

Run tests:
```bash
cd tests/fixtures/rust && cargo test 2>&1
```
Expected: 3 passed, 1 failed (`test_intentional_failure`), 1 ignored.

**Checkpoint:** Rust fixture builds, runs `basic` mode, `cargo test` shows expected results (3 pass, 1 fail, 1 ignored).

**COMMIT 1:** `feat: Add C++ and Rust test fixture programs`

---

### Task 3: Create tests/common/mod.rs

**Files:**
- Create: `tests/common/mod.rs`

This module provides auto-build, fixture discovery, and shared helpers. It's the spine of the new test infrastructure.

**Step 1: Write the common module**

```rust
// tests/common/mod.rs
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// Auto-build and return the C++ CLI target binary path.
/// Builds on first call, caches via OnceLock.
pub fn cpp_target() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cpp");
        let binary = fixture_dir.join("build/strobe_test_target");

        if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
            build_cpp_fixtures(&fixture_dir);
        }

        assert!(binary.exists(), "C++ target binary not found after build: {:?}", binary);
        binary
    }).clone()
}

/// Auto-build and return the C++ Catch2 test suite binary path.
pub fn cpp_test_suite() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/cpp");
        let binary = fixture_dir.join("build/strobe_test_suite");

        if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
            build_cpp_fixtures(&fixture_dir);
        }

        assert!(binary.exists(), "C++ test suite binary not found after build: {:?}", binary);
        binary
    }).clone()
}

/// Auto-build and return the Rust fixture binary path (with dsymutil on macOS).
pub fn rust_target() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED.get_or_init(|| {
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/rust");
        let binary = fixture_dir.join("target/debug/strobe_test_fixture");

        if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
            build_rust_fixture(&fixture_dir);
        }

        assert!(binary.exists(), "Rust fixture binary not found after build: {:?}", binary);
        binary
    }).clone()
}

/// Return the Rust fixture project root (for debug_test Cargo adapter).
pub fn rust_fixture_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust")
}

/// Create a SessionManager with a temp database.
pub fn create_session_manager() -> (strobe::daemon::SessionManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let sm = strobe::daemon::SessionManager::new(&db_path).unwrap();
    (sm, dir)
}

/// Poll DB until predicate returns true or timeout.
pub async fn poll_events(
    sm: &strobe::daemon::SessionManager,
    session_id: &str,
    timeout: Duration,
    predicate: impl Fn(&[strobe::db::Event]) -> bool,
) -> Vec<strobe::db::Event> {
    let start = Instant::now();
    loop {
        let events = sm.db().query_events(session_id, |q| q.limit(500)).unwrap();
        if predicate(&events) || start.elapsed() >= timeout {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll with event type filter.
pub async fn poll_events_typed(
    sm: &strobe::daemon::SessionManager,
    session_id: &str,
    timeout: Duration,
    event_type: strobe::db::EventType,
    predicate: impl Fn(&[strobe::db::Event]) -> bool,
) -> Vec<strobe::db::Event> {
    let start = Instant::now();
    loop {
        let events = sm
            .db()
            .query_events(session_id, |q| q.event_type(event_type.clone()).limit(500))
            .unwrap();
        if predicate(&events) || start.elapsed() >= timeout {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Collect all stdout text from events.
pub fn collect_stdout(events: &[strobe::db::Event]) -> String {
    events.iter()
        .filter(|e| e.event_type == strobe::db::EventType::Stdout)
        .filter_map(|e| e.text.as_deref())
        .collect()
}

/// Check if sources are newer than binary (for rebuild detection).
fn needs_rebuild(src_dir: &Path, binary: &Path) -> bool {
    let binary_mtime = match std::fs::metadata(binary) {
        Ok(m) => m.modified().unwrap(),
        Err(_) => return true,
    };

    fn newest_in_dir(dir: &Path) -> Option<std::time::SystemTime> {
        let mut newest = None;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(t) = newest_in_dir(&path) {
                        newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
                    }
                } else if let Ok(m) = path.metadata() {
                    if let Ok(t) = m.modified() {
                        newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
                    }
                }
            }
        }
        newest
    }

    match newest_in_dir(src_dir) {
        Some(src_time) => src_time > binary_mtime,
        None => true,
    }
}

/// Build C++ fixtures via cmake.
fn build_cpp_fixtures(fixture_dir: &Path) {
    eprintln!("Building C++ fixtures in {:?}...", fixture_dir);

    let status = Command::new("cmake")
        .args(["-B", "build", "-DCMAKE_BUILD_TYPE=Debug"])
        .current_dir(fixture_dir)
        .status()
        .expect("cmake not found. Install with: xcode-select --install");
    assert!(status.success(), "cmake configure failed");

    let status = Command::new("cmake")
        .args(["--build", "build", "--parallel"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "cmake build failed");
}

/// Build Rust fixture via cargo + dsymutil.
fn build_rust_fixture(fixture_dir: &Path) {
    eprintln!("Building Rust fixture in {:?}...", fixture_dir);

    let status = Command::new("cargo")
        .args(["build"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "Rust fixture build failed");

    if cfg!(target_os = "macos") {
        let binary = fixture_dir.join("target/debug/strobe_test_fixture");
        let _ = Command::new("dsymutil")
            .arg(&binary)
            .status();
    }
}
```

**Step 2: Verify it compiles**

Run:
```bash
cargo check --tests
```
Expected: Compiles without errors (though no test files use it yet).

**Checkpoint:** `tests/common/mod.rs` compiles. Fixture auto-build functions are ready.

---

### Task 4: Migrate Unit Tests to Source Files

Move tests that don't need Frida from `tests/*.rs` into `#[cfg(test)]` modules in their source files.

**Files:**
- Modify: `src/db/mod.rs` — add DB roundtrip tests from `integration.rs` and `phase1c_e2e.rs`
- Modify: `src/mcp/mod.rs` — add validation tests from `validation.rs`
- Modify: `src/dwarf/parser.rs` — add pattern matching tests from `integration.rs`
- Modify: `src/frida_collector/mod.rs` — add HookManager test from `integration.rs`
- Modify: `src/error.rs` — add error type test from `integration.rs`

**Step 1: Migrate DB tests to `src/db/mod.rs`**

Add the following tests to the existing `#[cfg(test)]` module in `src/db/mod.rs`:

From `integration.rs`:
- `test_database_roundtrip` (lines 22-70) — event CRUD
- `test_event_with_watch_values` (lines 111-148) — watch values roundtrip
- `test_output_event_insertion_and_query` (lines 326-400) — stdout/stderr events
- `test_mixed_event_types_in_unified_timeline` (lines 402-514) — timeline ordering
- `test_batch_insert_with_output_events` (lines 516-586) — batch insert
- `test_session_cleanup_on_stop` (lines 648-668) — session lifecycle

From `phase1c_e2e.rs`:
- `test_crash_event_db_roundtrip_with_real_data` (lines 658-753) — crash event fields
- `test_crash_event_in_unified_timeline` (lines 757-874) — crash in timeline
- `test_batch_insert_with_crash_events` (lines 1246-1321) — batch crash insert
- `test_update_event_locals` (lines 1325-1371) — locals update
- `test_pid_filter_on_events` (lines 1025-1094) — PID filter
- `test_min_duration_filter` (lines 1098-1175) — duration filter
- `test_time_range_filter` (lines 1179-1242) — time range filter

These tests use `tempfile::tempdir()` and `Database::open()` directly — no Frida needed. They fit perfectly as `#[cfg(test)]` unit tests in `src/db/mod.rs`.

Adapt imports: change `strobe::db::*` to `super::*` since they'll be inside the module.

**Step 2: Migrate validation tests to `src/mcp/mod.rs`**

All 14 tests from `tests/validation.rs` test `DebugTraceRequest::validate()`. Move them into the existing `#[cfg(test)]` module in `src/mcp/mod.rs` (or `src/mcp/types.rs` if that has its own test module).

Tests to move:
- `test_too_many_watches`
- `test_watch_expression_too_long`
- `test_watch_expression_too_deep`
- `test_valid_requests_pass`
- `test_serialization_depth_zero_rejected`
- `test_serialization_depth_exceeds_max`
- `test_serialization_depth_valid_range`
- `test_serialization_depth_none_is_valid`
- `test_serialization_depth_boundary_values`
- `test_serialization_depth_with_other_params`
- `test_serialization_depth_invalid_with_valid_params`
- `test_serialization_depth_json_roundtrip`
- `test_serialization_depth_omitted_from_json_when_none`
- `test_serialization_depth_deserialization_from_mcp`
- `test_serialization_depth_large_values_rejected`

Also move from `integration.rs`:
- `test_mcp_types_serialization` (lines 72-86)
- `test_watch_types_serialization` (lines 88-109)
- `test_mcp_initialize_response_has_instructions` (lines 588-621)
- `test_watch_on_field_patterns` (lines 770-809)
- `test_watch_pattern_matching_with_real_names` (lines 811-849) — pure pattern matching, no binary needed

Adapt imports from `strobe::mcp::*` to `super::*`.

**Step 3: Migrate DWARF/pattern tests to `src/dwarf/parser.rs`**

From `integration.rs`:
- `test_pattern_matching_real_rust_names` (lines 150-182) — PatternMatcher unit test
- `test_symbol_demangling_real_rust_symbols` (lines 184-214) — demangle unit test (move to `src/symbols/mod.rs`)

From `phase1c_e2e.rs`:
- `test_resolve_crash_locals_synthetic` (lines 913-994) — synthetic frame data
- `test_resolve_crash_locals_register_based` (lines 998-1021) — register-based local

These are pure unit tests with no binary dependencies.

**Step 4: Migrate HookManager and error tests**

From `integration.rs`:
- `test_hook_manager` (lines 302-324) → `src/frida_collector/mod.rs` existing `#[cfg(test)]`
- `test_error_types` (lines 285-300) → `src/error.rs` (add `#[cfg(test)]` module if not present)
- `test_session_status_serialization` (lines 255-267) → `src/db/mod.rs` (DB types)
- `test_event_type_serialization` (lines 269-283) → `src/db/mod.rs`
- `test_hook_count_accuracy` (lines 891-909) → `src/frida_collector/mod.rs`
- `test_pending_patterns_isolation` (lines 623-646) → `src/daemon/server.rs` (tests pending pattern state)

**Step 5: Verify all migrated tests pass**

Run:
```bash
cargo test --lib
```
Expected: All migrated unit tests pass. Should see the test count increase by ~35+ tests compared to before migration.

**Step 6: Delete the now-redundant test files**

Don't delete yet — we'll do that in Task 8 after the new integration tests are in place.

**Checkpoint:** `cargo test --lib` passes with all migrated unit tests. No tests lost.

**COMMIT 2:** `refactor: Migrate unit tests from integration files to source modules`

---

### Task 5: Create tests/frida_e2e.rs

**Files:**
- Create: `tests/frida_e2e.rs`

This is the main Frida integration test file. One `#[tokio::test]` orchestrator running all scenarios sequentially through a shared `SessionManager` (proven pattern from `phase1c_e2e.rs`).

**Step 1: Write the test file**

```rust
//! Frida end-to-end integration tests.
//!
//! All scenarios run sequentially in ONE tokio test to avoid Frida/GLib
//! teardown races between concurrent sessions.

mod common;

use std::time::Duration;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_frida_e2e_scenarios() {
    let cpp_bin = cpp_target();
    let rust_bin = rust_target();
    let (sm, _dir) = create_session_manager();
    let cpp_str = cpp_bin.to_str().unwrap();
    let rust_str = rust_bin.to_str().unwrap();
    let cpp_project = cpp_bin.parent().unwrap().to_str().unwrap();
    let rust_project = rust_bin.parent().unwrap().to_str().unwrap();

    eprintln!("=== Scenario 1/12: Output capture ===");
    scenario_output_capture(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 2/12: Function tracing — C++ namespaces ===");
    scenario_cpp_tracing(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 3/12: Function tracing — Rust namespaces ===");
    scenario_rust_tracing(&sm, rust_str, rust_project).await;

    eprintln!("\n=== Scenario 4/12: Crash capture (SIGSEGV) ===");
    scenario_crash_null(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 5/12: Crash capture (SIGABRT) ===");
    scenario_crash_abort(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 6/12: Fork workers ===");
    scenario_fork_workers(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 7/12: Fork exec ===");
    scenario_fork_exec(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 8/12: Duration query filter ===");
    scenario_duration_query(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 9/12: Time range query filter ===");
    scenario_time_range_query(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 10/12: Pattern add/remove ===");
    scenario_pattern_add_remove(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 11/12: Watch variables ===");
    scenario_watch_variables(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 12/12: Multi-threaded tracing ===");
    scenario_multithreaded(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== All 12 Frida E2E scenarios passed ===");
}
```

**Step 2: Implement each scenario function**

Each scenario follows this pattern:
1. Generate unique session_id
2. `spawn_with_frida()` with appropriate mode
3. `create_session()` to register in DB
4. Optionally `add_patterns()` + `update_frida_patterns()`
5. `poll_events()` / `poll_events_typed()` until predicate
6. Assert expected events/values
7. `stop_session()` to clean up

**Scenario implementations to port:**

| New Scenario | Port From | Key Changes |
|---|---|---|
| `scenario_output_capture` | `phase1c_e2e.rs:scenario_output_capture` | Change binary mode arg, update expected strings |
| `scenario_cpp_tracing` | New — launch `slow-functions`, add `timing::*` patterns, verify FunctionEnter/Exit | |
| `scenario_rust_tracing` | New — launch Rust `basic`, add `strobe_test_fixture::audio::**` pattern, verify events | |
| `scenario_crash_null` | `phase1c_e2e.rs:scenario_crash_null_deref` | Same logic, new binary path |
| `scenario_crash_abort` | `phase1c_e2e.rs:scenario_crash_abort` | Same logic, new binary path |
| `scenario_fork_workers` | `phase1c_e2e.rs:scenario_fork_workers` | Same logic |
| `scenario_fork_exec` | `phase1c_e2e.rs:scenario_fork_exec` | Same logic |
| `scenario_duration_query` | `phase1c_e2e.rs:scenario_duration_query` | Use `timing::*` patterns instead of bare function names |
| `scenario_time_range_query` | Extracted from `scenario_duration_query` time range section | Standalone scenario |
| `scenario_pattern_add_remove` | New — launch, add patterns, verify count, remove, verify count decreased | |
| `scenario_watch_variables` | New — launch `globals` mode, add watch for `g_counter`, verify watch_values in events | |
| `scenario_multithreaded` | New — launch C++ `threads` mode, trace `audio::*`, verify multiple thread_name values | |

**Key differences from existing code:**

1. **Binary paths:** Use `cpp_target()` / `rust_target()` from common module (auto-builds)
2. **Output strings:** Change from `"STRESS TEST 1C"` to new fixture output (`"Hello from strobe_test_target"`, `"[TARGET]"`, `"[TIMING]"`)
3. **Namespaced patterns:** Use `timing::fast`, `timing::slow` etc. (C++ namespaces) instead of bare `fast_function`, `slow_function`
4. **New scenarios 10-12:** Pattern management, watches, and threading are NEW coverage

**Step 3: Verify**

Run:
```bash
cargo test --test frida_e2e -- --nocapture
```
Expected: All 12 scenarios pass. This will auto-build fixtures on first run.

**Checkpoint:** `cargo test --test frida_e2e` passes all 12 scenarios.

---

### Task 6: Create tests/test_runner.rs

**Files:**
- Create: `tests/test_runner.rs`

Tests the `TestRunner` API against both fixture programs.

**Step 1: Write test file**

```rust
//! Test runner integration tests.
//! Tests the TestRunner API with Cargo (Rust fixture) and Catch2 (C++ fixture) adapters.

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use common::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_runner_scenarios() {
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
```

**Step 2: Implement scenario functions**

Each test calls `TestRunner::new().run(...)` with appropriate params:

- **`test_cargo_execution`**: Run on `rust_fixture_project()`, verify summary: 3 passed, 1 failed, 1 skipped
- **`test_cargo_single_test`**: Run with `test: Some("test_audio_process")`, verify 1 passed, 0 failed
- **`test_catch2_execution`**: Run on C++ test suite binary (`cpp_test_suite()`), verify counts match expectations (~8 pass, 1 fail for `[expected-fail]`)
- **`test_catch2_single_test`**: Run with `test: Some("MIDI note on")`, verify 1 passed
- **`test_catch2_stuck_detection`**: Run `test: Some("Stuck test")` with short timeout, verify stuck warning appears
- **`test_adapter_detection`**: Create `TestRunner::new()`, call adapter detection, verify Cargo=90 for rust project, Catch2=85 for C++ binary
- **`test_details_file_writing`**: Run a test, call `output::write_details()`, read file, parse JSON, verify structure

**Key pattern for TestRunner calls:**

```rust
async fn test_cargo_execution(sm: &strobe::daemon::SessionManager) {
    let runner = strobe::test::TestRunner::new();
    let session_id = sm.generate_session_id("test-cargo");
    let progress = Arc::new(Mutex::new(strobe::test::TestProgress::new()));

    let result = runner.run(
        &rust_fixture_project(),
        Some("cargo"),
        None,       // all levels
        None,       // all tests
        None,       // auto command
        &HashMap::new(),
        Some(60),   // 60s timeout
        sm,
        &[],        // no trace patterns
        None,       // no watches
        "test-conn",
        &session_id,
        progress,
    ).await.unwrap();

    assert_eq!(result.framework, "cargo");
    assert_eq!(result.result.summary.passed, 3);
    assert_eq!(result.result.summary.failed, 1);
    assert_eq!(result.result.summary.skipped, 1);
    assert!(result.session_id.is_some());
}
```

**Step 3: Verify**

Run:
```bash
cargo test --test test_runner -- --nocapture
```
Expected: All 7 tests pass.

**Checkpoint:** `cargo test --test test_runner` passes.

---

### Task 7: Create tests/stress.rs

**Files:**
- Create: `tests/stress.rs`

Performance benchmarks, all `#[ignore]`.

**Step 1: Write test file**

Port from existing `tests/stress_test_limits.rs` with minor cleanup:
- Remove the `format_number` helper (or keep it for readability)
- Same test logic: fill, query, cleanup, sustained load phases
- Test limits: 10k, 50k, 100k, 200k, 500k, 1M

```rust
//! Performance benchmarks for event storage.
//! Run manually: cargo test --test stress -- --ignored --nocapture

use strobe::db::{Database, Event, EventType};
use std::time::Instant;
use tempfile::tempdir;

#[test]
#[ignore]
fn stress_test_event_limits() {
    // ... port from stress_test_limits.rs lines 9-131
}

fn test_limit(max_events: usize) {
    // ... port from stress_test_limits.rs lines 30-131
}

fn create_event(session_id: &str, seq: usize) -> Event {
    // ... port from stress_test_limits.rs lines 133-179
}
```

This is essentially a copy of the existing `stress_test_limits.rs` with the function renamed.

**Step 2: Verify**

Run:
```bash
cargo test --test stress -- --list
```
Expected: Shows `stress_test_event_limits` as ignored.

**Checkpoint:** `cargo test --test stress -- --list` shows the benchmark.

**COMMIT 3:** `feat: Add feature-based integration tests (frida_e2e, test_runner, stress)`

---

### Task 8: Delete Old Test Files and Fixture Directories

**Files to delete:**
- `tests/integration.rs`
- `tests/validation.rs`
- `tests/phase1b_stress.rs`
- `tests/phase1c_e2e.rs`
- `tests/phase1d_test.rs`
- `tests/stress_test_phase1d.rs`
- `tests/stress_test_limits.rs`
- `tests/stress_test_phase1b/` (entire directory)
- `tests/stress_test_phase1c/` (entire directory)

**Step 1: Verify new tests cover everything**

Before deleting, run:
```bash
cargo test --lib --test frida_e2e --test test_runner --test stress -- --nocapture
```
Expected: All tests pass (unit + integration).

**Step 2: Delete old files**

```bash
git rm tests/integration.rs tests/validation.rs tests/phase1b_stress.rs \
       tests/phase1c_e2e.rs tests/phase1d_test.rs tests/stress_test_phase1d.rs \
       tests/stress_test_limits.rs
git rm -r tests/stress_test_phase1b tests/stress_test_phase1c
```

**Step 3: Final verification**

```bash
cargo test --lib --test frida_e2e --test test_runner
```
Expected: All pass, no compilation errors.

**Checkpoint:** Old files deleted, all new tests pass. Final directory structure:

```
tests/
├── fixtures/
│   ├── cpp/
│   │   ├── CMakeLists.txt
│   │   ├── src/ (7 files)
│   │   └── tests/test_main.cpp
│   └── rust/
│       ├── Cargo.toml
│       └── src/ (5 files)
├── common/
│   └── mod.rs
├── frida_e2e.rs
├── test_runner.rs
└── stress.rs
```

**COMMIT 4:** `refactor: Remove legacy phase-named test files`

---

## Notes

### Tests that are intentionally NOT migrated

These tests in `integration.rs` and `phase1c_e2e.rs` depend on the OLD fixture binaries (`stress_tester`, `stress_test_phase1c`). They are replaced by equivalent scenarios in `frida_e2e.rs` using the NEW fixtures:

- `test_dwarf_parsing_real_binary` → Replaced by `scenario_cpp_tracing` + `scenario_rust_tracing` (real Frida sessions with DWARF)
- `test_pattern_matching_end_to_end_with_real_dwarf` → Replaced by `scenario_cpp_tracing` (patterns against real binary)
- `test_dwarf_global_variable_parsing` → Replaced by `scenario_watch_variables` (globals via real Frida session)
- `test_dwarf_watch_expression_ptr_member` → Replaced by `scenario_watch_variables` (ptr->member via real session)
- `test_dwarf_phase1c_function_discovery` → Replaced by `scenario_cpp_tracing` (DWARF function matching)
- `test_dwarf_locals_parsing_phase1c_binary` → Replaced by `scenario_crash_null` (locals in crash frame)

### Risk mitigation

1. **Don't delete old files until new tests pass** — Task 8 is last for a reason
2. **Fixture auto-build is cached** — `OnceLock` prevents redundant builds during `cargo test`
3. **Sequential Frida scenarios** — proven pattern from `phase1c_e2e.rs`, avoids GLib race conditions
4. **Catch2 FetchContent** — may be slow on first build (downloads from GitHub). Consider adding a CI cache for `tests/fixtures/cpp/build/_deps/`
