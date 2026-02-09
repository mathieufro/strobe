# Review: Phase 2 Active Debugging

**Spec:** `docs/specs/2026-02-09-active-debugging.md`
**Reviewed:** 2026-02-09
**Commits:** `e7fec21..0f77da0` (Phase 2 implementation + subsequent fixes)
**Branch:** main
**Method:** Multi-agent parallel review (5 passes: completeness, correctness, security, integration, test coverage)

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Correctness | 2 | 2 | 1 |
| Security | 0 | 2 | 1 |
| Integration | 0 | 1 | 1 |
| Tests | 0 | 3 | 0 |
| **Total** | **2** | **8** | **3** |

**Ready to merge:** No (2 critical issues must be fixed first)

## Previously-Identified Issues Now Fixed

The following issues from the prior review pass have been **resolved** in the current diff:

- ~~`resolve_address()` binary search returns None~~ — Now handles `Err(idx)` with closest-preceding fallback
- ~~`step-into` duplicate variable declaration~~ — Dead code removed, single clean implementation
- ~~Session stop doesn't clean up Phase 2 state~~ — `breakpoints`, `logpoints`, `paused_threads` cleanup added to both `stop_session()` and `stop_session_retain()`
- ~~`next_line_in_function()` crosses function boundaries~~ — Now uses `func_high_pc` from DWARF function table with `take_while` boundary check
- ~~Agent confirmation messages not handled~~ — `breakpointSet`, `logpointSet`, `breakpointRemoved`, `logpointRemoved` handlers added to spawner
- ~~No resource limits for breakpoints/logpoints~~ — `MAX_BREAKPOINTS_PER_SESSION=50`, `MAX_LOGPOINTS_PER_SESSION=100`, `MAX_LINE_NUMBER=1_000_000` added to types.rs validation
- ~~`remove_breakpoint`/`remove_logpoint` are sync~~ — Now async, send removal message to agent via spawner pipeline

---

## Blocking Issues

1. **`callee_entry_addresses()` returns ALL functions** — step-into hooks every function in the binary (104k+ hooks)
2. **`removeBreakpoint()` in agent uses `send()` to resume paused threads** — this sends to the daemon, not the local recv() handler; threads stay permanently paused

---

## Issues

### Issue 1: `callee_entry_addresses()` returns all functions in the binary
**Severity:** Critical
**Category:** Correctness
**Location:** [parser.rs:1239-1256](src/dwarf/parser.rs#L1239-L1256)
**Requirement:** Spec: "Set one-shot hooks on all function entries that could be called from the current line"

**Problem:** The method returns entry addresses of ALL known functions (minus current), capped at 20 by the caller. For a binary with 104,714 functions, this means step-into picks 20 essentially random functions instead of actual callees from the current line.

```rust
// Current — returns ALL functions:
self.functions
    .iter()
    .filter(|f| { /* exclude current function */ })
    .filter(|f| f.low_pc > 0)
    .map(|f| f.low_pc)
    .collect()  // ← Returns 104k addresses
```

**Impact:** Step-into never actually steps into the right callee. The 20-cap saves it from crashing, but the behavior is essentially "continue" with 20 random one-shot hooks that are unlikely to fire.

**Suggested fix:** Use DWARF call site info (`DW_TAG_call_site` / `DW_AT_call_target`) when available, or analyze instruction bytes at the current address range to find CALL/BL targets. Simpler short-term: intersect with currently-traced function patterns if available. Or just make step-into = step-over until proper callee resolution is implemented, and document this limitation.

---

### Issue 2: Agent `removeBreakpoint()` can't resume paused threads
**Severity:** Critical
**Category:** Correctness
**Location:** [agent.ts:1044-1048](agent/src/agent.ts#L1044-L1048)
**Requirement:** Spec: "Remove breakpoint while thread paused on it: Resume thread first, then detach hook"

**Problem:** When removing a breakpoint while a thread is paused on it, the agent calls:
```typescript
send({ type: `resume-${threadId}`, payload: {} });  // sends to DAEMON
```
But the paused thread is blocked on `recv('resume-${threadId}').wait()` which waits for an incoming message **from the daemon**, not a self-sent message. `send()` goes outbound to the daemon, not inbound to the message queue.

**Impact:** Removing a breakpoint while a thread is paused on it leaves the thread permanently blocked. The listener gets detached but the thread never wakes from `recv().wait()`.

**Suggested fix:** The daemon should handle this — when `remove_breakpoint()` is called in session_manager.rs, check `paused_threads` for any thread paused on that breakpoint and send a resume message through the spawner before removing:

```rust
// In session_manager.rs remove_breakpoint():
let paused = self.get_all_paused_threads(session_id);
for (thread_id, info) in &paused {
    if info.breakpoint_id == breakpoint_id {
        spawner.resume_thread(session_id, *thread_id).await?;
        self.remove_paused_thread(session_id, *thread_id);
    }
}
// Then proceed with agent removal
```

---

### Issue 3: `next_line_in_function()` linear scan is O(N) over all functions
**Severity:** Important
**Category:** Correctness (Performance)
**Location:** [parser.rs:1221-1224](src/dwarf/parser.rs#L1221-L1224)

**Problem:** Finding the function containing an address uses `self.functions.iter().find()` — a linear scan over all functions. For 104k functions, this adds measurable latency to every step-over operation.

```rust
let func_high_pc = self.functions.iter()
    .find(|f| address >= f.low_pc && address < f.high_pc)
    .map(|f| f.high_pc);
```

**Impact:** Step operations are slower than necessary. Not a functional bug but degrades interactive debugging experience.

**Suggested fix:** Build a sorted vector of (low_pc, high_pc) pairs during parse and use binary search. Or add an `address_to_function` index.

---

### Issue 4: `resolve_address()` returns wrong location for inter-function gaps
**Severity:** Important
**Category:** Correctness
**Location:** [parser.rs:1192-1204](src/dwarf/parser.rs#L1192-L1204)

**Problem:** For an address in dead space between functions (e.g., padding between function A ending at 0x4000 and function B starting at 0x6000), `resolve_address(0x5000)` returns the last line entry from function A. This is technically the "closest preceding entry" but semantically wrong — the address isn't in any function.

**Impact:** Low — this case mainly arises in crash reports or unusual stepping scenarios. Functional but may give misleading line numbers in edge cases.

**Suggested fix:** After finding the preceding entry, verify the address falls within a known function's `[low_pc, high_pc)` range.

---

### Issue 5: Step-out silently becomes "continue" when no return address
**Severity:** Important
**Category:** Correctness
**Location:** [session_manager.rs:1202-1214](src/daemon/session_manager.rs#L1202-L1214)

**Problem:** When `pause_info.return_address` is None, step-out sends an empty one-shot list, which effectively becomes "continue" — the thread resumes without any step target.

**Impact:** User expects step-out to pause at caller but thread runs freely. This is confusing UX.

**Suggested fix:** Return an error:
```rust
"step-out" => {
    let ret_addr = pause_info.return_address
        .ok_or_else(|| crate::Error::ValidationError(
            "Cannot step-out: no return address captured (may be in top-level function)".to_string()
        ))?;
    vec![(ret_addr, true)]
}
```

---

### Issue 6: Missing condition/message string length limits
**Severity:** Important
**Category:** Security
**Location:** [types.rs](src/mcp/types.rs) — `BreakpointTarget.condition`, `LogpointTarget.message`

**Problem:** Breakpoint conditions and logpoint messages have no length limits. An enormous string (100MB) could cause memory exhaustion in daemon and agent.

**Impact:** DoS via memory exhaustion, database bloat from condition error events.

**Suggested fix:** Add `MAX_CONDITION_LENGTH = 1024` and `MAX_LOGPOINT_MESSAGE_LENGTH = 2048` validation constants.

---

### Issue 7: Per-session breakpoint count not enforced at add time
**Severity:** Important
**Category:** Security
**Location:** [session_manager.rs:1347-1352](src/daemon/session_manager.rs#L1347-L1352)

**Problem:** `add_breakpoint()` inserts unconditionally without checking session count. While MCP validation limits per-request count (50), multiple requests can accumulate 100+ breakpoints on one session.

**Suggested fix:** Check `session_bps.len() >= MAX_BREAKPOINTS_PER_SESSION` in `add_breakpoint()` and return error.

---

### Issue 8: Logpoint `set_logpoint_async()` calls `spawner.set_breakpoint()`
**Severity:** Minor
**Category:** Integration
**Location:** [session_manager.rs:1315](src/daemon/session_manager.rs#L1315)

**Problem:** The logpoint setup constructs a message with `"type": "setLogpoint"` but sends it via `spawner.set_breakpoint()`. Works correctly (both use `SessionCommand::SetBreakpoint`) but is architecturally confusing.

**Suggested fix:** Either rename to a generic `spawner.send_hook_message()` or add a dedicated `spawner.set_logpoint()` method.

---

### Issue 9: One-shot step hook timeout accumulation
**Severity:** Minor
**Category:** Correctness
**Location:** [agent.ts:908-913](agent/src/agent.ts#L908-L913)

**Problem:** Each step operation creates a 30s `setTimeout` cleanup timer. Rapid stepping before timeouts fire accumulates timers. No cleanup on session disposal.

**Suggested fix:** Track timer IDs, cancel on session cleanup or when cleanup fires.

---

### Issue 10: `resolve_line()` path matching could match wrong file
**Severity:** Minor
**Category:** Security
**Location:** [parser.rs:1130-1134](src/dwarf/parser.rs#L1130-L1134)

**Problem:** Uses `e.file.ends_with(&format!("/{}", file))` which is correct for path components but could still match unintended files if the DWARF data has unusual paths.

**Impact:** Very low — DWARF paths are compiler-generated.

---

### Issue 11: Pause notification channel fails silently
**Severity:** Important
**Category:** Integration
**Location:** [spawner.rs:442](src/frida_collector/spawner.rs#L442)

**Problem:** `let _ = tx.try_send(notification)` silently drops pause notifications if the receiver is dead or channel is full. A paused thread would remain blocked with no daemon-side record.

**Suggested fix:** Log warning on send failure and consider periodic orphan detection.

---

## Test Coverage Issues

### Issue T1: recv().wait() multi-thread PoC test missing
**Severity:** Important
**Category:** Tests
**Requirement:** Spec Phase 2a prerequisite

**Problem:** The spec explicitly requires a proof-of-concept test validating that `recv().wait()` blocks individual threads independently. No such test exists.

---

### Issue T2: Behavioral tests verify execution but not correctness
**Severity:** Important
**Category:** Tests

**Problem:** Stepping tests verify commands succeed (no error) and produce pause events, but don't verify **correct locations**:
- Step-over: doesn't check line advancement (N → N+1)
- Step-into: comment says "same as step-over currently" — doesn't test entering a callee
- Step-out: doesn't verify pause at caller's line after the call

---

### Issue T3: Conditional breakpoint test uses trivial condition
**Severity:** Important
**Category:** Tests
**Location:** [breakpoint_behavioral.rs](tests/breakpoint_behavioral.rs)

**Problem:** Test uses `condition: "true"` which always fires. No test with a selective condition like `args[0] > 5` that validates the evaluator actually filters.

---

## Completeness

All Phase 2a and Phase 2b spec requirements are implemented:

- [x] DWARF line table parsing (lazy, gimli LineProgram API, is_statement filtering)
- [x] `debug_breakpoint` (function + line targeting, conditions, hit counts)
- [x] `debug_continue` (continue, step-over, step-into, step-out)
- [x] Agent pause mechanism (`recv().wait()`, per-thread blocking)
- [x] Daemon pause state tracking (breakpoints, logpoints, paused_threads HashMaps)
- [x] `debug_write` for globals/statics
- [x] `debug_logpoint` with template substitution
- [x] New event types (Pause, Logpoint, ConditionError)
- [x] New error variants (NoCodeAtLine, OptimizedOut)
- [x] ASLR handling (imageBase + noSlide for runtime addresses)
- [x] ARM64 PAC stripping on return addresses
- [x] One-shot hook lifecycle with 30s safety timeout
- [x] Stuck detector breakpoint-aware suppression
- [x] Session cleanup for Phase 2 state
- [x] Message protocol: all daemon↔agent message types
- [ ] Phase 2c: Local variable writes (intentionally deferred)

## Extras (not in spec but implemented)

- Hit count reporting from agent to daemon (breakpoint.hits field)
- Agent-side `breakpointSet`/`breakpointRemoved` confirmation signaling
- Cargo adapter: crash detection from stderr (SIGSEGV, SIGABRT)
- Cargo adapter: `--tests` flag to skip doctests
- Cargo adapter: smart `--test <name>` for integration test binaries
- Test runner: timestamp-based progress polling (replaces broken offset pagination)
- Test runner: final progress drain for late-arriving events
- Test runner: per-test wall-clock durations from DB timestamps

## Recommendations

1. **Fix Issue 1** (callee_entry_addresses) — either implement proper callee resolution or document step-into as step-over until Phase 3
2. **Fix Issue 2** (removeBreakpoint resume) — daemon must send resume before agent detaches
3. **Add string length limits** (Issue 6) — follow existing MAX_WATCH_EXPRESSION_LENGTH pattern
4. **Add per-session count enforcement** (Issue 7) — in add_breakpoint/add_logpoint
5. **Add behavioral assertions** to stepping tests — verify line numbers, not just event existence
6. **Return error on step-out without return address** (Issue 5) — don't silently degrade to continue
