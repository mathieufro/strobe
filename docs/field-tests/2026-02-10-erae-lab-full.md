# Field Test: Erae Lab (Full) — 2026-02-10

## Target
**Erae Lab** — JUCE 8.0.10 C++20 app for programming Erae Touch MIDI controllers.
- 80+ source files, Catch2 3.8.1 tests, universal binary (arm64+x86_64)
- 152MB dSYM, ~105k functions

## Summary
- Scenarios run: 7/7
- Passed: 5 (clean)
- Passed with caveats: 2 (logpoints, memory reads)
- Failed: 0
- No crashes, no hangs, no daemon errors

## Results

### Scenario 1: Catch2 Unit Tests
- **Status:** PASS
- **What happened:** `debug_test` auto-detected Catch2 framework. 51 tests passed in <1s. Structured result with pass/fail/skipped counts returned correctly.

### Scenario 2: Catch2 E2E Tests
- **Status:** PASS
- **What happened:** 41 e2e tests passed. Progress polling showed incremental results during execution (8 → 26 → 41). `elapsedMs` tracking worked throughout.

### Scenario 3: Launch App + Trace Functions
- **Status:** PASS
- **What happened:**
  - **Fat binary issue:** Universal binary's dSYM caused `Unsupported file format` error from DWARF parser. Workaround: extract arm64 slice with `lipo -thin arm64` then regenerate dSYM. See "Remaining Issues" below.
  - After workaround: `ProjectManager::*` + `LayoutManager::*` matched 114 functions (100 hooked, hit per-call limit). Pattern removal worked correctly (182 → 61 hooks).
  - `MidiSysexInterface::*` captured full timer callback chain: `timerCallback → scanForBridgePorts → detectEraeDevicePorts → handleStatusRequestTimeout → fetchEraeTouchStatus → sendToOutputs → managePortConnections → retrySubmoduleVersionFetch`.
  - Source file paths, line numbers, parent-child event relationships, and durations all correct.
  - `@file:MainComponent` pattern worked for source-file matching.

### Scenario 4: Breakpoints
- **Status:** PASS
- **What happened:**
  - Function breakpoint on `MidiSysexInterface::fetchEraeTouchStatus()` resolved to line 797 of MidiSysexInterface.cpp. Process paused on next timer tick, `debug_continue` resumed cleanly.
  - Hit count breakpoint (`hitCount: 3`) on `sendToOutputs` — process paused on exactly the 3rd call as expected.
  - Breakpoint removal via `debug_breakpoint({ remove: [...] })` cleaned up correctly.

### Scenario 5: Logpoints
- **Status:** PASS (with caveat)
- **What happened:**
  - Two logpoints installed successfully on `MidiSysexInterface::timerCallback` and `PadLayout::timerCallback` with message templates.
  - Installation returned correct addresses, source files, line numbers.
  - Logpoint removal worked correctly.
  - **Caveat:** Logpoint events not visible in `debug_query` results. The `logpoint` eventType is not a valid query filter at the MCP level (`unknown variant 'logpoint'`). The Rust server's `EventType` enum for query filtering doesn't include logpoint/pause/condition_error event types.
  - **Additional finding:** Pause events from breakpoints have wall-clock timestamps (~1.77e18 ns) while trace events use process-relative timestamps (~5.02e10 ns). This creates confusing sort order in the unified timeline.

### Scenario 6: Live Memory Reads
- **Status:** PASS (with caveat)
- **What happened:**
  - Error handling works correctly: non-existent variables return per-target errors without crashing. Multiple targets in one call each fail independently.
  - **Caveat:** All tested globals (`kMaxNumLayouts`, `kMaxNumFootswitches`, `kNumCVOutputMax`) are `constexpr` constants inlined by the compiler — no DWARF storage entries. This is a real-world limitation: most C++ "constants" won't be readable.
  - Raw address read (`0x100000000`) correctly reported access violation (ASLR-shifted base).

### Scenario 7: Single Test + Instrumented Rerun
- **Status:** PASS
- **What happened:**
  - Ran single test `"Add element via toolbar drag"` with `tracePatterns: ["@file:test_e2e_element_lifecycle"]`.
  - Test passed, Frida session created, trace events captured.
  - `CATCH2_INTERNAL_TEST_0()` called 11 times (Catch2's SECTION re-entry pattern).
  - stderr captured `PadLayout::addElementToLayout new element: GridElement` confirming the test exercised element creation.
  - Session collected 170 events total (traces + stdout/stderr).

## Remaining Issues

### 1. Fat/Universal Binary Support (High)
DWARF parser fails on macOS universal (fat) binaries with `Unsupported file format`. The `object` crate's `File::parse()` doesn't handle fat Mach-O headers. Workaround exists (`lipo -thin arm64`) but is friction for users.

**Fix approach:** Use `object::macho::FatHeader` to detect fat binaries, extract the correct architecture slice based on the running process, then parse that slice's DWARF.

### 2. Missing EventType Query Filters (Medium)
The MCP `debug_query` `eventType` filter only accepts: `function_enter`, `function_exit`, `stdout`, `stderr`, `crash`, `variable_snapshot`. Missing: `logpoint`, `pause`, `condition_error`. These events exist in the DB but can't be filtered for specifically.

### 3. Timestamp Domain Mismatch (Low)
Pause events use wall-clock nanoseconds (~1.77e18), trace events use process-relative nanoseconds (~5.02e10). This causes confusing interleaving in the unified timeline when both are present.

## UX Notes

- Catch2 auto-detection is excellent — zero configuration needed.
- `debug_test` test name filter uses substring match, not Catch2 tag syntax. Users might try `[e2e][element-lifecycle]` and get "no tests found". Consider documenting this or supporting tag syntax.
- The 100-function-per-`debug_trace`-call limit produces clear warnings. The workaround (narrower patterns) is obvious.
- dSYM generation adds significant upfront setup time (~30s for a 152MB binary). Consider detecting missing dSYM and suggesting `dsymutil` in error messages.
- Session IDs auto-generated from binary name + timestamp are clean and readable.
