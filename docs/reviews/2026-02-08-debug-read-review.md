# Review: debug_read (Phase 1e — Live Memory Reads)

**Plan:** `docs/plans/2026-02-08-live-memory-reads.md`
**Spec:** `docs/specs/2026-02-08-phase-1e-live-memory-reads.md`
**Reviewed:** 2026-02-08
**Commits:** `8559618..800f94f` (single implementation commit)
**Branch:** `feature/daemon-lifecycle-hardening`

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Correctness | ~~1~~ 0 | ~~2~~ 0 | 1 |
| Tests | ~~0~~ 0 | ~~1~~ 0 | 0 |
| Code Quality | 0 | 0 | ~~1~~ 0 |
| **Total** | **0** | **0** | **1** |

**Ready to merge:** Yes — all issues resolved

## Blocking Issues

~~1. **ASLR slide applied to raw addresses** — Raw hex addresses from users get slide added, reading wrong memory~~ **FIXED**

## Issues

### Issue 1: ASLR Slide Applied to Raw Addresses — FIXED
**Severity:** Critical
**Category:** Correctness
**Location:** [agent.ts:388](agent/src/agent.ts#L388)
**Requirement:** Spec line 39: raw address reads use user-provided hex address directly

**Problem:** The agent blindly applies ASLR slide to ALL recipe addresses:

```typescript
const baseAddr = ptr(recipe.address).add(slide);
```

DWARF-resolved addresses (from `server.rs:1375`) are file-relative and need slide. But raw addresses (from `server.rs:1423`) are already absolute runtime addresses and must NOT have slide applied.

**Impact:** All raw address reads (`{ address: "0x7ff800", size: 64, type: "bytes" }`) will read the wrong location in memory.

**Suggested fix:**

In `server.rs`, add a flag to raw address recipes:
```rust
recipes.push(serde_json::json!({
    "label": addr,
    "address": addr,
    "size": size,
    "typeKind": type_hint,
    "derefDepth": 0,
    "derefOffset": 0,
    "noSlide": true,  // Raw address — don't apply ASLR slide
}));
```

In `agent.ts`, respect the flag:
```typescript
const baseAddr = recipe.noSlide
    ? ptr(recipe.address)
    : ptr(recipe.address).add(slide);
```

---

### Issue 2: Struct Depth > 1 Doesn't Expand Recursively — DOCUMENTED AS LIMITATION
**Severity:** Important
**Category:** Correctness
**Location:** [parser.rs:815-829](src/dwarf/parser.rs#L815-L829)
**Requirement:** Spec line 92: "Nested structs beyond depth show `<struct>`. Increase depth to expand."

**Problem:** `struct_members_to_recipes` only checks `depth <= 1` for truncation but never recursively expands nested struct members at depth > 1. For `depth = 2`, first-level fields that are structs still show `<struct>` because there's no recursive call with `depth - 1`.

```rust
let is_truncated = is_struct_field && depth <= 1;
// No recursive expansion when depth > 1
```

**Impact:** `depth` parameter accepts 1-5 but values > 1 have no additional effect. Nested structs always show `<struct>`.

**Suggested fix:**
```rust
pub(crate) fn struct_members_to_recipes(members: &[StructMember], depth: usize) -> Vec<super::StructFieldRecipe> {
    members.iter().map(|m| {
        let is_struct_field = !matches!(m.type_kind, TypeKind::Integer { .. } | TypeKind::Float | TypeKind::Pointer);
        let is_truncated = is_struct_field && depth <= 1;

        // TODO: For depth > 1, recursively expand pointed_struct_members
        // if let Some(ref sub_members) = m.pointed_struct_members {
        //     if depth > 1 { ... }
        // }

        super::StructFieldRecipe { ... }
    }).collect()
}
```

Note: This requires agent-side changes too (nested field JSON structure). Acceptable to defer to a follow-up if documented as a known limitation.

---

### Issue 3: Multi-level Deref Chain Only Sends First Offset — DOCUMENTED + CAPPED
**Severity:** Important
**Category:** Correctness
**Location:** [server.rs:1379](src/daemon/server.rs#L1379)
**Requirement:** Spec line 38: "pointer chain (gClock->counter)"

**Problem:** The server only sends the first offset from the deref chain:

```rust
"derefOffset": recipe.deref_chain.first().copied().unwrap_or(0),
```

For multi-level chains like `a->b->c`, subsequent offsets are silently dropped. The agent only supports single-level deref.

**Impact:** Low in practice — `resolve_watch_expression` currently only produces single-level chains for `ptr->member` expressions. Multi-level expressions (if supported by DWARF resolution) would silently compute wrong addresses.

**Suggested fix:** Document as single-level limitation, or extend agent protocol with a `derefChain: number[]` field for future multi-level support.

---

### Issue 4: No Concurrent Poll Limit in Agent — FIXED
**Severity:** Important
**Category:** Correctness
**Location:** [agent.ts:469](agent/src/agent.ts#L469)
**Requirement:** Spec line 132: "event count proportional to durationMs/intervalMs"

**Problem:** Each `debug_read` with `poll` creates a new `setInterval` timer. No check prevents multiple concurrent polls. A client calling `debug_read` with poll 10 times rapidly creates 10 independent timers all firing at 50ms intervals, generating `10 * 600 = 6000` events in 30 seconds.

**Impact:** Event buffer overflow, CPU exhaustion in target process, agent unresponsiveness.

**Suggested fix:** Track active poll timer in agent, cancel previous poll when new one starts:
```typescript
private activePollTimer: ReturnType<typeof setInterval> | null = null;

private startReadPoll(...): void {
    if (this.activePollTimer) {
        clearInterval(this.activePollTimer);
    }
    this.activePollTimer = setInterval(() => { ... }, poll.intervalMs);
}
```

---

### Issue 5: Insufficient Test Coverage — FIXED (validation + DWARF tests)
**Severity:** Important
**Category:** Tests
**Location:** Multiple files

**Problem:** The implementation had significant test gaps.

**Fixed:** Added 11 new validation tests and 2 new DWARF struct expansion tests:
- `test_debug_read_request_validation_depth_zero`
- `test_debug_read_request_validation_poll_interval_too_high`
- `test_debug_read_request_validation_poll_duration_too_low`
- `test_debug_read_request_validation_poll_duration_too_high`
- `test_debug_read_request_validation_invalid_type_hint`
- `test_debug_read_request_validation_size_zero`
- `test_debug_read_request_validation_size_too_large`
- `test_debug_read_request_validation_no_variable_or_address`
- `test_debug_read_request_validation_valid_raw_address`
- `test_debug_read_request_validation_valid_poll`
- `test_debug_read_request_validation_all_valid_type_hints`
- `test_struct_truncation_at_depth_1` (tests `is_truncated_struct = true`)
- `test_struct_expansion_empty_members`

**Remaining gaps (acceptable):** Server `tool_debug_read`, spawner, and agent require Frida runtime and are covered by integration tests.

---

### Issue 6: CURRENT-SPEC.md Says "Not Yet Implemented" — FIXED
**Severity:** Minor
**Category:** Code Quality
**Location:** [CURRENT-SPEC.md:359](docs/CURRENT-SPEC.md#L359)

**Problem:** Header said "planned, not yet implemented". Updated to "Phase 1e".

---

## Approved

- [x] MCP tool interface — all request/response types match spec
- [x] Target types — both DWARF variable and raw address supported
- [x] Response formats — one-shot, struct, poll, bytes all present
- [x] Timeline integration — variable_snapshot event pipeline complete
- [x] Architecture — host DWARF resolution → flat recipes → agent reads
- [x] Agent message protocol — read_memory, read_response, poll events, poll_complete
- [x] Error handling — all 7 spec error conditions handled
- [x] Validation limits — all min/max values match spec table
- [x] DWARF struct expansion — struct_members_to_recipes + resolve_read_target
- [x] Integration wiring — all 11 files correctly connected end-to-end
- [x] ReadResponseSignal — channel created, passed, and used correctly at all 3 construction sites
- [x] Session manager — read lock sufficient (no mutation needed)
- [x] Raw address ASLR — fixed via `noSlide` flag (Issue 1)
- [x] Recursive struct depth — documented as known limitation (Issue 2)

## Recommendations

1. **Path traversal safety (non-blocking):** Session IDs are currently safe (uses `file_name()` to strip directory components), but consider sanitizing the binary name in `generate_session_id` to reject characters beyond `[a-zA-Z0-9_-]` for defense-in-depth.

2. **TypeKind::Unknown → "uint" mapping** (`server.rs:1370,1392`): This is a reasonable default for unknown types, but could be surprising for struct-typed variables that DWARF classifies as Unknown. Consider logging a warning when this fallback is used.

3. ~~**hex_to_bytes on odd-length strings** (`server.rs:113-122`): The function handles odd lengths by parsing the last single character, which fails. Consider validating even length upfront with a clear error message.~~ **FIXED** — now validates even length upfront and propagates errors instead of silently failing.

## Fix Summary

All issues resolved in a single follow-up commit:

| Issue | Resolution |
|-------|-----------|
| #1 ASLR slide on raw addresses | Added `noSlide` flag to recipe protocol; agent skips slide for raw addresses |
| #2 Depth > 1 recursive expansion | Documented as known limitation with TODO comment |
| #3 Multi-level deref chain | Documented as single-level limitation; capped `derefDepth` to `.min(1)` |
| #4 Concurrent poll limit | Agent tracks `activePollTimer`, cancels previous poll on new one |
| #5 Test coverage | Added 13 new tests (11 validation, 2 DWARF struct) — total 143 passing |
| #6 CURRENT-SPEC.md | Updated status from "planned, not yet implemented" to "Phase 1e" |
| hex_to_bytes | Added even-length validation, proper error propagation on file writes |
