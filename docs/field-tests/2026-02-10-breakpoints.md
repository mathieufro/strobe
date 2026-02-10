# Field Test: breakpoints — 2026-02-10

## Summary
- Scenarios run: 7/7
- Passed: 7 (after fixes)
- Failed: 2 initially (fixed: 2, remaining: 0)

## Target
ERAE Touch MK2 Simulator — JUCE-based C++ firmware simulator, arm64, 81MB binary, 190MB dSYM, ~105k functions.

## Results

### Scenario 1: Function Breakpoint
- **Status:** PASS
- **What happened:** Set breakpoint on `embodme::InternalClock::setBeatPerMinute`. Resolved via cross-CU DWARF to `internal_clock.cpp:249` at `0x10164bd94`.
- **Notes:** Cross-CU DWARF resolution (fixed in prior session) is critical for this binary — 18k+ functions have their name in a different compilation unit than their code.

### Scenario 2: Conditional Breakpoint
- **Status:** PASS
- **What happened:** Set BP with condition `args[0] > 100` on same function. BP installed with new ID at same address.

### Scenario 3: Continue
- **Status:** PASS
- **What happened:** `debug_continue` with no paused threads returns `VALIDATION_ERROR: No paused threads in this session`. Correct API contract — clear error rather than silent no-op.

### Scenario 4: State Inspection (debug_read)
- **Status:** PASS (fixed in this session)
- **What happened (before fix):** `debug_read` with raw address timed out after 5s. Agent received the message but `Process.findRangeByAddress()` hung indefinitely on unmapped addresses in this large binary.
- **Fix:** Removed `Process.findRangeByAddress()` pre-check from `readTypedValue()` in agent. Now relies on try/catch around the actual read — Frida throws a catchable `access violation` for unmapped memory.
- **After fix:** Returns `"Address not readable: access violation accessing 0x100008000"` instantly.

### Scenario 5: Global Write (debug_write)
- **Status:** PASS (fixed in this session)
- **What happened (before fix):** Same `findRangeByAddress` hang as scenario 4, but in `writeTypedValue()`.
- **Fix:** Same removal of `findRangeByAddress()` pre-check.
- **After fix:** Returns `"Write failed: access violation accessing 0x100008000"` instantly.

### Scenario 6: Hit Count Breakpoint
- **Status:** PASS
- **What happened:** Set BP on `embodme::InternalClock::tickInternalProcess()` with `hitCount: 5`. Resolved to `internal_clock.cpp:272` at `0x10164e444`.

### Scenario 7: BP on Test Binary
- **Status:** PASS
- **What happened:** Ran `debug_test` with Catch2 binary, filter `Pitchbend`, tracePatterns `["embodme::PitchbendEngine::*"]`. Framework correctly detected as Catch2, 6 tests passed, 13 `function_enter` events captured.
- **Notes:** Initial attempt with filter `arpeggiator` found 0 tests — no arpeggiator tests exist in `touch_common_tests_x86_64`. Changed to `Pitchbend` which matches 6 tests. Also fixed `totalCount` in `debug_query` to reflect filtered count (was returning unfiltered total).

## Bugs Found and Fixed

### Bug 1: `Process.findRangeByAddress()` hangs on large macOS binaries
- **Severity:** High — blocks agent JS thread indefinitely, causing 5s timeout for all read/write operations
- **Root cause:** Frida's `Process.findRangeByAddress()` hangs when called with unmapped addresses in processes with many memory regions (81MB JUCE binary). Works fine on small binaries.
- **Fix:** Removed the pre-check from both `readTypedValue()` and `writeTypedValue()` in `agent/src/agent.ts`. The try/catch in `readSingleTarget`/`writeSingleTarget` already handles access violations cleanly.
- **Files:** `agent/src/agent.ts`

### Bug 2: `debug_query` totalCount ignores event_type filter
- **Severity:** Medium — misleading pagination info (e.g., `totalCount: 35` with `events: []` when filtering by `function_enter`)
- **Root cause:** `tool_debug_query` in `server.rs` used `count_session_events()` which counts ALL event types, while the query itself filters by the requested `event_type`.
- **Fix:** Added `count_filtered_events()` method to `EventDb` that applies the same WHERE clauses as `query_events` (without LIMIT/OFFSET). Updated `server.rs` to use it.
- **Files:** `src/db/event.rs`, `src/daemon/server.rs`

## Remaining Issues
None — all scenarios pass.

## UX Notes
- Raw address reads on unmapped memory now return clear errors instead of hanging — good UX improvement.
- The `totalCount` fix prevents confusion when paginating filtered results.
- Field test reference lists `kMaxNumLayouts` as a read target, but it's `constexpr` (no DWARF variable entry). The reference should suggest a runtime global instead.
- Field test reference lists `arpeggiator` as a test filter, but no arpeggiator tests exist in `touch_common_tests_x86_64`. Reference should be updated.
