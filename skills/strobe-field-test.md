---
name: strobe-field-test
description: Dogfood Strobe against a real C++ project (ERAE Touch MK2 Simulator) to validate features work in practice. Run after implementing/reviewing a Strobe feature.
---

# Strobe Field Test

Test Strobe against **ERAE Touch MK2 Simulator** — a real C++ firmware simulator for a MIDI controller. This catches issues unit tests miss: ergonomics, performance, edge cases in real binaries.

## Target Project

- **Path:** `/Users/mathieu/erae_touch_mk2_fw`
- **Binary:** `build/bin/erae_mk2_simulator.app/Contents/MacOS/erae_mk2_simulator`
- **Tests:** Catch2 (`tests/` directory, 6+ test files)
- **Build:** `cd build && make -j$(nproc)` (CMake + Conan, C++23)

## Core Rule

**Don't just report bugs — FIX THEM.** When Strobe breaks or behaves wrong during testing, debug it, patch it, add a test, commit with `fix: <description>`, then continue the scenario to verify the fix.

## Workflow

1. **Study the ERAE codebase** — understand the architecture before testing
2. **Execute the scenario** using Strobe's MCP tools (`debug_launch`, `debug_trace`, `debug_query`, `debug_test`, `debug_read`, `debug_stop`)
3. **Push boundaries** — try edge cases, combine tools, stress-test
4. **Fix what breaks** — you have full access to the Strobe repo
5. **Document everything** — write findings to `docs/field-tests/YYYY-MM-DD-{scenario-name}.md`

## Example Scenarios

### After Phase 1e (Live Memory Reads)
Launch the ERAE simulator. Use `debug_read` to poll global state variables while the app runs. Find MIDI clock state, tempo, active layout. Check if values change when you trace specific functions.

### After Phase 2a (Breakpoints & Stepping)
Set breakpoints on MIDI processing functions. Trigger events by running test_midi_sender_triggers. Step through event routing. Inspect local variables at breakpoints. Test C++ name mangling handling.

### TDD Workflow
Run `debug_test` on the ERAE tests. Look at failures. Use `suggested_traces` to rerun with instrumentation. Query trace events to understand what happened. Test the full workflow loop.

### Crash Investigation
Launch the simulator with ASAN. Stress-test layout switching code. Use Strobe to capture crash context. Evaluate what information was available and how useful it was.

## Output

Each field test produces:
- **`docs/field-tests/YYYY-MM-DD-{name}.md`** — detailed findings (what was tried, what worked, what broke, how it was fixed)
- **Commits with bug fixes** — the main deliverable
- **Remaining issues** — filed as TODO comments or added to docs

## What to Document

For each tool call:
- What you tried (exact parameters)
- What worked as expected
- What broke and **how you fixed it** (with commit hash)
- Remaining issues you couldn't fix (with diagnosis)
- UX suggestions for Strobe improvements
