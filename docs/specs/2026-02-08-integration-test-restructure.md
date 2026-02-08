# Integration Test Restructure

**Date:** 2026-02-08
**Status:** Draft
**Goal:** Replace the phase-named, duplicated integration test files with a clean, future-proof test infrastructure where every test exercises real Strobe-managed Frida sessions against well-designed fixture programs.

---

## Problem

The current `tests/` directory is a mess accumulated over phase-based development:

| File | Lines | Issues |
|------|-------|--------|
| `integration.rs` | 910 | Mixes unit-level DB/serialization tests with DWARF integration tests. Many tests duplicate in-file `#[cfg(test)]` modules. |
| `validation.rs` | 328 | Pure validation tests — belongs in `src/mcp/types.rs` unit tests. |
| `phase1b_stress.rs` | 345 | Duplicates validation.rs tests. Contains manual test docs as a "test". |
| `phase1c_e2e.rs` | 1443 | Good Frida E2E tests but phase-named, uses ad-hoc C binary. |
| `phase1d_test.rs` | 147 | Adapter tests that belong in unit tests or test_runner integration. |
| `stress_test_phase1d.rs` | 1150 | Hardcoded `/Users/alex/erae_touch_mk2_fw/...` paths. Mixes adapter parsing stress with real execution. |
| `stress_test_limits.rs` | 192 | Good perf benchmark, just needs cleanup. |

**Key issues:**
1. Phase-based naming (`phase1b`, `phase1c`, `phase1d`) — meaningless, unsearchable
2. Massive duplication (serialization depth tests appear in 3 files)
3. Unit tests masquerading as integration tests (DB roundtrips, type serialization)
4. Two separate test target programs with different build systems (Cargo vs Makefile)
5. Hardcoded absolute paths to external projects
6. Fragile binary discovery (each file re-implements it)
7. No automatic fixture building — manual steps required
8. Many Strobe features untested at the integration level

---

## Design Principles

1. **Every integration test goes through a real Frida session.** If it doesn't need Frida, it's a unit test and belongs in `#[cfg(test)]` inside the source file.
2. **Two fixture programs (C++ and Rust)** covering realistic scenarios. Both languages are Strobe's primary targets.
3. **Fixtures build themselves automatically** on first use. `cargo test` just works.
4. **Future-proof fixture programs** — add new modes/functions to existing programs rather than creating new ones.
5. **Feature-based file naming** — `frida_e2e.rs`, `test_runner.rs`, not `phase2a_thing.rs`.

---

## Test Fixture Programs

### C++ Fixture: `tests/fixtures/cpp/`

CMake project producing **two binaries**:

#### `strobe_test_target` — CLI binary with modes

```
strobe_test_target <mode>
  hello           Print to stdout/stderr, clean exit
  crash-null      NULL dereference (SIGSEGV) with interesting locals
  crash-abort     abort() (SIGABRT)
  crash-stack     Stack overflow via deep recursion
  fork-workers    Fork N child processes doing work
  fork-exec       Fork + exec another program
  slow-functions  Functions with varied durations (0ms, 5ms, 50ms, 500ms)
  threads         Multi-threaded with named threads and globals
  globals         Read/write globals and sleep (for watch testing)
```

**Source structure:**
```
tests/fixtures/cpp/
├── CMakeLists.txt
├── src/
│   ├── main.cpp              # CLI dispatcher
│   ├── audio.h / audio.cpp   # namespace audio { process_buffer(), apply_effect(), generate_sine() }
│   ├── midi.h / midi.cpp     # namespace midi { note_on(), control_change(), generate_sequence() }
│   ├── crash.h / crash.cpp   # namespace crash { null_deref(), abort_signal(), stack_overflow() }
│   ├── timing.h / timing.cpp # namespace timing { fast(), medium(), slow(), very_slow() }
│   └── globals.h             # Global variables: g_counter, g_tempo, g_sample_rate, g_point (struct*)
└── tests/
    └── test_main.cpp          # Catch2 test suite
```

**Global variables** (for watch expression testing):
```cpp
// globals.h
extern uint32_t g_counter;        // Simple integer
extern double g_tempo;             // Float
extern int64_t g_sample_rate;      // Signed integer
extern Point* g_point_ptr;         // Struct pointer (for gPointPtr->x watch expressions)

struct Point {
    int32_t x, y;
    double value;
};
```

**Namespaced functions** give realistic demangled names for pattern matching:
- `audio::process_buffer` → C++ mangled: `_ZN5audio14process_bufferE...`
- Pattern `audio::*` matches direct children
- Pattern `audio::**` matches all descendants
- `@file:audio.cpp` matches by source file

#### `strobe_test_suite` — Catch2 test binary

~10 normal tests + 1 stuck test, using the same functions from the target:

```cpp
TEST_CASE("Audio buffer processing", "[unit][audio]") {
    AudioBuffer buf = audio::generate_sine(440.0f);
    float rms = audio::process_buffer(&buf);
    REQUIRE(rms > 0.0f);
}

TEST_CASE("MIDI note on", "[unit][midi]") {
    REQUIRE(midi::note_on(60, 100) == true);
}

TEST_CASE("Timing fast function", "[integration][timing]") {
    timing::fast();  // Should complete quickly
    REQUIRE(true);
}

// Intentionally failing test (for adapter validation)
TEST_CASE("Intentional failure", "[unit][expected-fail]") {
    REQUIRE(1 == 2);
}

// Intentionally stuck test (for stuck detector validation)
TEST_CASE("Stuck test - infinite loop", "[stuck]") {
    volatile bool done = false;
    while (!done) { }  // Infinite loop — stuck detector should flag this
}
```

Tags enable filtering: `[unit]`, `[integration]`, `[stuck]`, `[expected-fail]`.

#### CMakeLists.txt

```cmake
cmake_minimum_required(VERSION 3.14)
project(strobe_test_fixtures CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_BUILD_TYPE Debug)  # Always debug for DWARF symbols

# Fetch Catch2
include(FetchContent)
FetchContent_Declare(Catch2 GIT_REPOSITORY https://github.com/catchorg/Catch2.git GIT_TAG v3.5.0)
FetchContent_MakeAvailable(Catch2)

# Shared library of fixture functions
add_library(fixture_lib STATIC
    src/audio.cpp src/midi.cpp src/crash.cpp src/timing.cpp)
target_include_directories(fixture_lib PUBLIC src/)

# CLI target binary
add_executable(strobe_test_target src/main.cpp)
target_link_libraries(strobe_test_target PRIVATE fixture_lib)

# Catch2 test suite
add_executable(strobe_test_suite tests/test_main.cpp)
target_link_libraries(strobe_test_suite PRIVATE fixture_lib Catch2::Catch2WithMain)

# CTest integration
include(CTest)
include(Catch)
catch_discover_tests(strobe_test_suite)
```

### Rust Fixture: `tests/fixtures/rust/`

Separate Cargo project (not a workspace member).

```
tests/fixtures/rust/
├── Cargo.toml
├── src/
│   ├── main.rs       # CLI: basic, threads, globals modes
│   ├── lib.rs         # Re-exports modules, #[cfg(test)] with pass/fail/skip tests
│   ├── audio.rs       # pub fn process_buffer(), generate_sine(), apply_effect()
│   ├── midi.rs        # pub fn note_on(), control_change(), generate_sequence()
│   └── engine.rs      # pub fn update_state(), print_stats()
```

**Library tests** (for `debug_test` Cargo adapter validation):
```rust
// src/lib.rs
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
    #[ignore]  // Shows up as "skipped" in test report
    fn test_ignored_for_now() {
        todo!("not implemented yet");
    }

    #[test]
    fn test_intentional_failure() {
        assert_eq!(1, 2, "intentional failure for adapter testing");
    }
}
```

**Binary modes:**
- `basic` — calls all namespaced functions, prints to stdout, exits
- `threads` — spawns named threads (audio-0, audio-1, midi-processor, automation) doing work with globals
- `globals` — initializes atomics, sleeps (for watch variable testing on running process)

**Globals** (atomics for watch testing):
```rust
pub static G_SAMPLE_RATE: AtomicU64 = AtomicU64::new(44100);
pub static G_TEMPO: AtomicU64 = AtomicU64::new(120_000);
pub static G_BUFFER_COUNT: AtomicU64 = AtomicU64::new(0);
pub static G_NOTE_COUNT: AtomicU64 = AtomicU64::new(0);
```

---

## Integration Test Files

### `tests/common/mod.rs` — Shared infrastructure

```rust
use std::path::PathBuf;
use std::sync::OnceLock;

/// Auto-build and return the C++ CLI target binary path.
/// Builds on first call, caches via OnceLock. Rebuilds if sources changed.
pub fn cpp_target() -> PathBuf { ... }

/// Auto-build and return the C++ Catch2 test suite binary path.
pub fn cpp_test_suite() -> PathBuf { ... }

/// Auto-build and return the Rust fixture binary path (with dsymutil on macOS).
pub fn rust_target() -> PathBuf { ... }

/// Return the Rust fixture project root (for debug_test Cargo adapter).
pub fn rust_fixture_project() -> PathBuf { ... }

/// Create a SessionManager with a temp database.
pub fn create_session_manager() -> (strobe::daemon::SessionManager, tempfile::TempDir) { ... }

/// Shared SessionManager for Frida tests (OnceLock singleton).
/// Multiple FridaSpawner instances race on GLib state — one shared SM avoids this.
pub fn shared_session_manager() -> Arc<strobe::daemon::SessionManager> { ... }

/// Poll DB until predicate returns true or timeout.
pub async fn poll_events(
    sm: &SessionManager, session_id: &str, timeout: Duration,
    predicate: impl Fn(&[Event]) -> bool,
) -> Vec<Event> { ... }

/// Poll with event type filter.
pub async fn poll_events_typed(
    sm: &SessionManager, session_id: &str, timeout: Duration,
    event_type: EventType, predicate: impl Fn(&[Event]) -> bool,
) -> Vec<Event> { ... }

/// Check if sources are newer than binary (for rebuild detection).
fn needs_rebuild(src_dir: &Path, binary: &Path) -> bool { ... }

/// Build C++ fixtures via cmake.
fn build_cpp_fixtures(fixture_dir: &Path) { ... }

/// Build Rust fixture via cargo + dsymutil.
fn build_rust_fixture(fixture_dir: &Path) { ... }
```

### `tests/frida_e2e.rs` — Frida integration scenarios (~500 lines)

Single `#[tokio::test(flavor = "multi_thread")]` orchestrator running all scenarios sequentially through `shared_session_manager()`:

**Scenario 1: Output capture**
- Launch C++ target `hello` mode via `spawn_with_frida()`
- Poll DB for stdout/stderr events
- Verify stdout contains expected output, stderr if any
- Verify PID on output events matches spawned PID

**Scenario 2: Function tracing — C++ namespaces**
- Launch C++ target `slow-functions` mode
- Add trace patterns: `timing::fast`, `timing::slow`, `timing::very_slow`
- Call `update_frida_patterns()` on running session
- Verify hook count > 0
- Poll for `FunctionEnter` + `FunctionExit` events
- Verify function names match, duration_ns present on exits
- Verify `timing::slow` has duration >= 40ms

**Scenario 3: Function tracing — Rust namespaces**
- Launch Rust target `basic` mode
- Add patterns: `strobe_test_fixture::audio::**`, `strobe_test_fixture::midi::**`
- Verify hooks installed on Rust demangled names
- Poll for function events
- Verify Rust module paths in function names

**Scenario 4: Crash capture (SIGSEGV)**
- Launch C++ target `crash-null` mode
- Poll for Crash event
- Verify: signal contains "access-violation" or "SEGV"
- Verify: fault_address is "0x0"
- Verify: registers present (pc, sp, fp on ARM64)
- Verify: backtrace has frames, one contains "null_deref"
- Verify: stdout captured before crash

**Scenario 5: Crash capture (SIGABRT)**
- Launch C++ target `crash-abort` mode
- Poll for Crash event or stdout-before-crash
- Verify signal or graceful degradation (abort not always catchable on macOS)

**Scenario 6: Fork workers — multi-process**
- Launch C++ target `fork-workers` mode
- Poll stdout for child process output
- Verify `get_all_pids()` has multiple PIDs
- Verify PID field on events

**Scenario 7: Fork exec**
- Launch C++ target `fork-exec` mode
- Verify parent output captured

**Scenario 8: Duration query filter (end-to-end)**
- Uses events from Scenario 2 (or re-launches slow-functions)
- Query with `min_duration_ns >= 40_000_000`
- Verify only slow/very_slow functions returned
- Verify fast_function excluded

**Scenario 9: Time range query filter**
- Query with `timestamp_from_ns` / `timestamp_to_ns` on captured events
- Verify fewer results than unfiltered

**Scenario 10: Pattern add/remove on running session**
- Launch C++ target `slow-functions` mode (runs for a while)
- Add patterns for `timing::*`
- Verify hook count
- Remove `timing::fast` pattern
- Verify hook count decreased

**Scenario 11: Watch variables**
- Launch C++ target `globals` mode (sits and updates globals)
- Add trace pattern for a function that runs periodically
- Add watch: `{ variable: "g_counter" }`
- Add watch: `{ variable: "g_point_ptr->x", on: ["audio::*"] }` (contextual)
- Poll for events with watch_values
- Verify watch_values JSON contains expected keys

**Scenario 12: Multi-threaded tracing**
- Launch C++ target `threads` mode (spawns named threads)
- Add patterns for `audio::*`
- Poll for events
- Verify multiple distinct thread_name values in events

### `tests/test_runner.rs` — Test runner integration (~400 lines)

Uses `TestRunner::run()` which internally spawns Frida sessions.

**Test 1: Cargo test execution — Rust fixture**
- Run `TestRunner::run()` on Rust fixture project with `framework: "cargo"`
- Verify: framework == "cargo"
- Verify: summary has expected pass/fail/skip counts (fixture has known test results)
- Verify: session_id is Some (always Frida)
- Verify: all_tests details match summary counts
- Verify: failures have file/line/message

**Test 2: Cargo single test filtering**
- Run with `test: Some("test_audio_process")`
- Verify: exactly 1 test passed, 0 failed

**Test 3: Catch2 test execution — C++ fixture**
- Run `TestRunner::run()` on C++ Catch2 test suite binary
- Verify: framework == "catch2"
- Verify: summary has expected counts (10 pass, 1 fail for expected-fail, 0 skip)
- Verify: failure details have file/line from Catch2 XML
- Verify: all_tests list is populated

**Test 4: Catch2 single test filtering**
- Run with `test: Some("MIDI note on")`
- Verify: 1 test passed

**Test 5: Catch2 stuck test detection**
- Run with `test: Some("Stuck test")` (the intentionally stuck test)
- Set a short hard_timeout (e.g., 5 seconds)
- Poll `TestProgress` for warnings
- Verify: warning appears with diagnosis containing "deadlock" or "infinite_loop" or "timeout"
- Stop the session via `stop_session()`

**Test 6: Adapter detection**
- Verify CargoTestAdapter detects Rust fixture project with confidence 90
- Verify Catch2Adapter detects C++ test binary with confidence 85
- Verify GenericAdapter always returns confidence 1

**Test 7: Details file writing**
- Run a test, write details via `output::write_details()`
- Read the file, parse JSON
- Verify structure: framework, summary, failures, tests, rawStdout, rawStderr

### `tests/stress.rs` — Performance benchmarks (~200 lines)

All tests `#[ignore]` — run manually with `cargo test --test stress -- --ignored --nocapture`.

**Benchmark 1: Event limit stress**
- Insert events at various limits (10k, 50k, 100k, 200k, 500k, 1M)
- Measure: fill time, query time, cleanup overhead, sustained throughput
- Report: events/sec, bytes/event, DB size

---

## Build Automation

### Fixture auto-build in `tests/common/mod.rs`

```rust
fn build_cpp_fixtures(fixture_dir: &Path) {
    let build_dir = fixture_dir.join("build");

    // cmake configure
    let status = Command::new("cmake")
        .args(["-B", "build", "-DCMAKE_BUILD_TYPE=Debug"])
        .current_dir(fixture_dir)
        .status()
        .expect("cmake not found. Install with: xcode-select --install");
    assert!(status.success(), "cmake configure failed");

    // cmake build
    let status = Command::new("cmake")
        .args(["--build", "build", "--parallel"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "cmake build failed");
}

fn build_rust_fixture(fixture_dir: &Path) {
    let status = Command::new("cargo")
        .args(["build"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "Rust fixture build failed");

    // dsymutil on macOS
    if cfg!(target_os = "macos") {
        let binary = fixture_dir.join("target/debug/strobe_test_fixture");
        Command::new("dsymutil")
            .arg(&binary)
            .status()
            .expect("dsymutil failed");
    }
}
```

**Rebuild detection:** Compare newest source file mtime against binary mtime. Only rebuild if sources are newer.

**OnceLock caching:** Each fixture path is cached via `OnceLock<PathBuf>` so the build runs at most once per `cargo test` invocation.

---

## What Gets Deleted

| Old file | Disposition |
|----------|-------------|
| `tests/integration.rs` | **Delete.** Unit tests move to source files. DWARF/pattern tests covered by `frida_e2e.rs` Scenario 2-3. |
| `tests/validation.rs` | **Delete.** Already covered by `src/mcp/types.rs` `#[cfg(test)]`. |
| `tests/phase1b_stress.rs` | **Delete.** Duplicated content. |
| `tests/phase1c_e2e.rs` | **Delete.** Replaced by `frida_e2e.rs` with C++ fixture. |
| `tests/phase1d_test.rs` | **Delete.** Replaced by `test_runner.rs`. |
| `tests/stress_test_phase1d.rs` | **Delete.** Adapter parsing tests move to unit tests. Real execution moves to `test_runner.rs`. |
| `tests/stress_test_limits.rs` | **Delete.** Replaced by `stress.rs`. |
| `tests/stress_test_phase1b/` | **Delete.** Replaced by `tests/fixtures/rust/`. |
| `tests/stress_test_phase1c/` | **Delete.** Replaced by `tests/fixtures/cpp/`. |

## What Gets Created

```
tests/
├── fixtures/
│   ├── cpp/                        # C++ fixture (CMake + Catch2)
│   │   ├── CMakeLists.txt
│   │   ├── src/
│   │   │   ├── main.cpp
│   │   │   ├── audio.h / audio.cpp
│   │   │   ├── midi.h / midi.cpp
│   │   │   ├── crash.h / crash.cpp
│   │   │   ├── timing.h / timing.cpp
│   │   │   └── globals.h
│   │   └── tests/
│   │       └── test_main.cpp       # Catch2 test suite (~10 tests + 1 stuck)
│   │
│   └── rust/                       # Rust fixture (separate Cargo project)
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── lib.rs              # Modules + #[test]s (pass, fail, skip)
│           ├── audio.rs
│           ├── midi.rs
│           └── engine.rs
│
├── common/
│   └── mod.rs                      # Auto-build, fixture discovery, shared helpers
│
├── frida_e2e.rs                    # 12 Frida scenarios: tracing, crashes, fork, watches, queries
├── test_runner.rs                  # 7 TestRunner scenarios: cargo, catch2, stuck detection
└── stress.rs                       # Performance benchmarks (#[ignore])
```

## Unit Tests That Remain In-File

These tests stay as `#[cfg(test)]` in their source files (not in `tests/`):

- `src/db/mod.rs` — Event CRUD, query filters, batch insert, session lifecycle
- `src/mcp/types.rs` — Serialization roundtrips, validation (watches, depth, boundaries)
- `src/dwarf/parser.rs` — Pattern matching, variable resolution, demangling
- `src/config.rs` — Settings parsing, resolution, defaults
- `src/test/cargo_adapter.rs` — Cargo output parsing (JSON edge cases)
- `src/test/catch2_adapter.rs` — Catch2 XML parsing
- `src/test/generic_adapter.rs` — Heuristic parsing
- `src/test/stuck_detector.rs` — CPU sampling, fast exit
- `src/test/output.rs` — Details file writing
- `src/frida_collector/mod.rs` — HookManager, pattern expansion
- `src/error.rs` — Error type formatting

---

## Migration Notes

- Any unit-level tests currently in `tests/*.rs` that have no equivalent `#[cfg(test)]` in the source files must be moved there first (e.g., pattern matching tests, DB roundtrip tests)
- The Frida E2E orchestrator pattern (one `#[tokio::test]`, sequential scenarios) is proven to work reliably — keep it
- All hardcoded external paths (`/Users/alex/erae_touch_mk2_fw/...`) are eliminated
- `OnceLock<Arc<SessionManager>>` pattern for shared Frida state is kept (prevents GLib races)
