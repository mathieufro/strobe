---
name: strobe-field-test
description: Dogfood Strobe against a real C++ project (ERAE Touch MK2 Simulator) to validate features work in practice. Run after implementing/reviewing a Strobe feature.
---

# Strobe Field Test

Validate Strobe features against **ERAE Touch MK2 Simulator** — a real C++ firmware simulator for a MIDI controller (JUCE-based, 81MB binary, 141MB dSYM, ~105k functions). This catches issues unit tests miss: ergonomics, performance, edge cases in real-world binaries.

## Input

This skill takes a **feature/phase name** as argument. Examples:

- `tracing` — Phase 1a core tracing
- `test-runner` — Phase 1d test instrumentation
- `live-reads` — Phase 1e memory reads
- `breakpoints` — Phase 2a breakpoints + continue
- `stepping` — Phase 2b stepping + logpoints
- `watches` — Contextual watch filtering
- `full` — Run all scenarios for a comprehensive check

Map the argument to the relevant scenario group below. If no argument given, ask which phase to test.

## Prerequisites

Verify ALL of these before starting scenarios. If any fail, stop and tell the user.

```
[ ] Strobe builds:     cargo build --release  (from /Users/alex/strobe)
[ ] Agent built:       ls /Users/alex/strobe/agent/dist/agent.js
[ ] Simulator exists:  ls /Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator
[ ] Simulator dSYM:    ls /Users/alex/erae_touch_mk2_fw/build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator.dSYM
[ ] Test binaries:     ls /Users/alex/erae_touch_mk2_fw/build/erae_data/tests/touch_common_tests_x86_64
```

## ERAE Reference

### Paths

| What | Path |
|------|------|
| Project root | `/Users/alex/erae_touch_mk2_fw` |
| Simulator binary | `build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator` |
| Simulator dSYM | `build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator.dSYM` |
| Test binary (touch_common) | `build/erae_data/tests/touch_common_tests_x86_64` |
| Test binary (erae_data) | `build/erae_data/tests/erae_data_tests_x86_64` |
| Test binary (mfes) | `build/erae_data/tests/mfes_tests_x86_64` |
| Build command | `cd build && make -j$(sysctl -n hw.ncpu)` |

### Architecture

- **Namespace:** Everything is under `embodme::`
- **Main conductor:** `embodme::EraeMK2` — orchestrates all subsystems, runs in a dedicated thread via `MainMcuSimulator::run()`
- **Singletons:** `FingerDetector::instance()`, `InternalClock::instance()`, `EventRouter::instance()`, `GlobalSettingsManager::instance()`, `AnimationHandler::Instance()`
- **Event system:** `EventRouter` dispatches `LayoutEvent`, `ProjectEvent`, `MidiEvent`, `ClockEvent`

### Key Functions (trace/breakpoint targets)

**Clock & Timing:**
- `embodme::InternalClock::setBeatPerMinute` — tempo changes
- `embodme::InternalClock::tickInternalProcess` — internal clock tick
- `embodme::InternalClock::tickFromMidi` — MIDI clock tick
- `embodme::InternalClock::start` / `stop` / `startStop` — transport

**Touch Processing:**
- `embodme::FingerDetector::process` — main touch pipeline
- `embodme::FingerDetector::propagateEvents` — dispatches touch to layouts
- `embodme::FingerDetector::updateFingersState` — finger tracking state machine
- `embodme::FingerDetector::applyAdaptiveThreshold` — sensitivity processing

**Layout & Elements:**
- `embodme::LayoutManager::instantiateProject` — loads a project
- `embodme::LayoutManager::instantiateLayout` — switches active layout
- `embodme::LayoutInstance::handleTouch` — touch event dispatch to elements
- `embodme::LayoutInstance::tickElementsMs` — element time updates

**MIDI:**
- `embodme::MidiManager::handleMidiInChannelVoice` — incoming MIDI notes/CC
- `embodme::MidiManager::handleMidiInSystemRealTime` — MIDI clock/transport
- `embodme::MidiManager::processInputs` — MIDI input polling
- `embodme::MidiManager::syncBuffers` — buffer synchronization

**CV/Gate:**
- `embodme::CVGateEngine::noteOn` / `noteOff` / `noteContinuous`
- `embodme::CVGateEngine::tick` — CV output update

**Looper:**
- `embodme::LooperManager::process` — looper playback
- `embodme::LooperManager::clockTick` — sync to clock
- `embodme::LooperManager::allNotesOff` — panic

**Arpeggiator:**
- `embodme::ArpeggiatorEngine::*` — arpeggio generation

**Project/Settings:**
- `embodme::ProjectManagerInterface::loadProjectAsync`
- `embodme::GlobalSettingsManager::writeFlash`
- `embodme::GlobalSettingsManager::dispatchSettings`

### Key Member Variables (watch/read targets)

**Clock state (on `InternalClock` singleton):**
- `mClockPos` (uint32_t) — current clock position
- `mPpqnTempo` (float) — tempo in PPQN
- `mIsRunning` (bool) — transport running
- `mJustStarted` (bool) — sync flag

**Touch state (on `FingerDetector` singleton):**
- `mFingers` (vector) — active finger positions
- `mCurrentFrame` (uint32_t) — FSR frame counter
- `mLastID` (uint32_t) — last assigned finger ID
- `mCurrentGlobalSensitivity` — sensitivity enum
- `mPropagationState` (atomic) — layout switching state machine

**Layout state:**
- `EraeMK2::mCalibrationDone` (bool) — hardware calibration

**Constants:**
- `kMaxNumLayouts` = 8
- `kMaxNumFootswitches` = 2
- `kNumCVOutputMax` = 24

### Test Framework

- **Framework:** Catch2 3.8.1
- **Test suites:** touch_common (animation, arpeggiator, pitchbend, CV clock, color, style morph), erae_data (serialization, MIDI conversion, scales), mfes (MIDI framework events)
- **Notable test file:** `test_arpeggiator.cpp` (38KB, extensive — good stress test)

## Scenario Map

### `tracing` — Phase 1a Core Tracing

**Goal:** Verify launch, output capture, pattern matching, and event queries on a large real binary.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Launch & output | `debug_launch` the simulator (no patterns). Query stderr, then stdout. | Session created, output events captured, no errors |
| 2 | Namespace tracing | `debug_trace({ add: ["embodme::InternalClock::*"] })` on running session | hookedFunctions > 0, function_enter events appear |
| 3 | Deep glob | `debug_trace({ add: ["embodme::FingerDetector::**"] })` | Matches nested methods, not just direct children |
| 4 | Source file pattern | `debug_trace({ add: ["@file:internal_clock."] })` | Hooks functions from that source file |
| 5 | Query filters | Query with `function: { contains: "Clock" }`, then with `sourceFile`, then `verbose: true` | All filter modes return correct results |
| 6 | Pattern removal | Remove patterns, verify hookedFunctions drops | Clean removal, no orphan hooks |
| 7 | Stop & cleanup | `debug_stop` | Session ends cleanly, no daemon errors |

### `test-runner` — Phase 1d Test Instrumentation

**Goal:** Verify Catch2 adapter detection, structured output, and stuck detection on real test binaries.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Catch2 detection | `debug_test({ projectRoot: "...", command: "<touch_common binary>" })` | Framework detected as "Catch2", tests run |
| 2 | Structured results | Poll `debug_test_status` until complete | Structured summary with pass/fail counts, no raw output parsing needed |
| 3 | Single test | `debug_test({ command: "...", test: "arpeggiator" })` | Runs only arpeggiator tests, not full suite |
| 4 | Instrumented rerun | `debug_test({ command: "...", test: "arpeggiator", tracePatterns: ["embodme::ArpeggiatorEngine::*"] })` | Frida path, sessionId returned, trace events queryable |
| 5 | Multiple binaries | Run erae_data_tests, then mfes_tests | Both detected as Catch2, both produce structured output |

### `live-reads` — Phase 1e Live Memory Reads

**Goal:** Verify DWARF variable resolution and polling on the running simulator.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | One-shot read | Launch simulator, `debug_read({ targets: [{ variable: "embodme::kMaxNumLayouts" }] })` | Returns value 8 (or correct constant) |
| 2 | Struct traversal | `debug_read({ targets: [{ variable: "<a global struct>" }], depth: 2 })` | Fields expanded, types correct |
| 3 | Poll mode | `debug_read({ targets: [...], poll: { intervalMs: 200, durationMs: 2000 } })` | Polling starts, variable_snapshot events appear in timeline |
| 4 | Bad variable | `debug_read({ targets: [{ variable: "nonexistent_var" }] })` | Per-target error, no crash, other targets still work |
| 5 | Multiple targets | Read 3+ variables in one call | All resolve independently, partial failures don't block |

### `breakpoints` — Phase 2a Breakpoints + Continue

**Goal:** Verify breakpoints pause execution, state is inspectable, and continue resumes correctly.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Function breakpoint | Set BP on `embodme::InternalClock::setBeatPerMinute`, trigger tempo change | Process pauses, breakpoint hit reported |
| 2 | Conditional BP | Set BP with condition `args[0] > 100` on same function | Only triggers for tempos > 100 BPM |
| 3 | Continue | Resume after breakpoint pause | Process continues normally, no hang |
| 4 | State inspection | While paused, `debug_read` to inspect variables | Variables readable at breakpoint |
| 5 | Global write | While paused, `debug_write` to modify a global | Value changes, continues with new value |
| 6 | Hit count | Set BP with hitCount: 5 on a frequently-called function | Triggers only on 5th hit |
| 7 | BP on test binary | Set BP in Catch2 tests via `debug_test` with tracePatterns | Breakpoint works in test context |

### `stepping` — Phase 2b Stepping + Logpoints

**Goal:** Verify step-over/into/out work correctly, and logpoints produce timeline events.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Step over | Break on `LayoutInstance::handleTouch`, step-over | Advances to next line in same function |
| 2 | Step into | At a line with a function call, step-into | Enters the called function |
| 3 | Step out | Inside a nested call, step-out | Returns to caller |
| 4 | Logpoint | Set logpoint on `InternalClock::tickInternalProcess` with message template `"tick pos={args[0]}"` | Events appear in timeline without pausing execution |
| 5 | Logpoint + trace | Combine logpoint with function traces | Both event types interleaved correctly in timeline |

### `watches` — Contextual Watch Filtering

**Goal:** Verify scoped watches only fire during specified functions.

| # | Scenario | Steps | Pass criteria |
|---|----------|-------|---------------|
| 1 | Global watch | `debug_trace({ watches: { add: [{ variable: "embodme::kMaxNumLayouts", label: "maxLayouts" }] } })` | Watch values appear on all traced function events |
| 2 | Scoped watch | `{ variable: "...", on: ["embodme::InternalClock::*"] }` + trace some clock functions | Watch values appear ONLY on clock function events |
| 3 | Wildcard scope | `{ variable: "...", on: ["embodme::FingerDetector::**"] }` | Matches nested functions too |
| 4 | Expression watch | `{ expr: "Process.mainModule.base", label: "baseAddr" }` | Expression evaluates, value appears |
| 5 | Watch removal | Remove a watch by label, verify it stops appearing | Clean removal |

### `full` — Comprehensive Check

Run ALL scenario groups above, in order: tracing → test-runner → live-reads → breakpoints → stepping → watches. Stop on first scenario group that has a failure, fix it, then continue.

## Workflow

1. **Verify prerequisites** — check every item in the prerequisites list
2. **Build Strobe** — `cargo build --release` in `/Users/alex/strobe`
3. **Execute scenarios** — run each scenario in the mapped group sequentially
4. **Log findings inline** — for each scenario, note: pass/fail, any unexpected behavior, exact error messages
5. **Complete ALL scenarios first** — do NOT stop to fix bugs mid-scenario-group (just document and continue)
6. **Fix phase** — after all scenarios are done, go back and fix failures:
   - Debug the issue in Strobe's codebase
   - Write the fix
   - Add/update a test if the bug is non-trivial
   - Commit with `fix: <description>`
   - Max 3 fix cycles per field test; document remaining issues if you can't fix them
7. **Re-verify** — re-run failed scenarios to confirm fixes
8. **Write report** — output findings to `docs/field-tests/YYYY-MM-DD-{phase-name}.md`

## Core Rule

**Fix what breaks, but finish observing first.** Complete all scenarios to understand the full picture before diving into fixes. This prevents rabbit-holing on one issue while missing others.

## Report Format

```markdown
# Field Test: {phase-name} — {date}

## Summary
- Scenarios run: X/Y
- Passed: X
- Failed: X (fixed: X, remaining: X)

## Results

### Scenario 1: {name}
- **Status:** PASS / FAIL (fixed in {commit}) / FAIL (open)
- **What happened:** ...
- **Fix:** ... (if applicable)

## Remaining Issues
- {description} — {diagnosis, what you tried}

## UX Notes
- {any ergonomics observations, confusing output, missing guidance}
```

## Success Criteria

The field test passes when:
- All scenarios in the mapped group executed (even if some initially failed)
- No crashes or hangs in Strobe daemon
- All fixable bugs committed with `fix:` prefix
- Remaining issues documented with diagnosis
- Report written to `docs/field-tests/`
