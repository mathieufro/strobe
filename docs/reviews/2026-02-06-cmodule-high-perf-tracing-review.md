# Review: CModule High-Performance Tracing

**Plan:** `docs/plans/2026-02-06-cmodule-high-perf-tracing.md`
**Reviewed:** 2026-02-06
**Commits:** `fc1d394` (Add CModule high-performance tracing with native callbacks)
**Branch:** `feature/cmodule-high-perf-tracing`

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Correctness | 1 | 2 | 1 |
| Integration | 1 | 1 | 1 |
| Security | 0 | 1 | 0 |
| Tests | 0 | 2 | 0 |
| **Total** | **2** | **6** | **2** |

**Ready to merge:** No — 2 critical issues need fixing first

## Blocking Issues

1. **Ring buffer partial-read race** — JS drain can read partially-written entries from concurrent C threads
2. **Empty sessionId on early drain** — drain timer starts in constructor, before `setSessionId()` is called; events with `sessionId: ''` will fail DB insertion

## Issues

### Issue 1: Ring buffer write-read race condition
**Severity:** Critical
**Category:** Correctness
**Location:** `agent/src/cmodule-tracer.ts:100-114` (C code), `354-458` (JS drain)
**Confidence:** 95

**Problem:** The C `write_entry()` atomically increments `write_idx` to claim a slot, then writes 48 bytes of entry data non-atomically. The JS drain reads `writeIdx`, then iterates all entries up to that index. If a C thread has claimed slot N but hasn't finished writing, the drain reads garbage fields (func_id, thread_id, etc.).

**Impact:** Corrupted events with wrong function names, thread IDs, or parent relationships. Probability increases with hook count and call frequency.

**Suggested fix:** Add a per-entry "write complete" flag. The simplest approach: repurpose `_pad` as a write-complete marker — C code writes it last as `0xFFFF`, JS drain skips entries where `_pad != 0xFFFF`:
```c
// In write_entry(), write _pad LAST as a completion marker:
e->arg0 = a0; e->arg1 = a1; e->retval = rv;
__atomic_store_n(&e->_pad, 0xFFFF, __ATOMIC_RELEASE);
```
```typescript
// In drain(), check completion:
const pad = entryPtr.add(46).readU16();
if (pad !== 0xFFFF) continue; // entry not yet fully written
```

---

### Issue 2: Empty sessionId on early drain ticks
**Severity:** Critical
**Category:** Integration
**Location:** `agent/src/cmodule-tracer.ts:251` (timer start), `266-267` (setSessionId)
**Confidence:** 100

**Problem:** The drain timer starts in the CModuleTracer constructor (line 251). `setSessionId()` is called later when the `initialize` message arrives from the daemon. If any hooks fire between agent load and `initialize`, drained events will have `sessionId: ''`, producing event IDs like `-1`, `-2`. These will fail or corrupt the database.

**Impact:** Unlikely with current flow (hooks aren't installed until after initialize), but a latent bug if the order changes.

**Suggested fix:** Guard drain to bail early if sessionId is not yet set:
```typescript
private drain(): void {
  if (!this.sessionId) return; // Not yet initialized
  // ...
}
```

---

### Issue 3: U32 wraparound in drain overflow detection
**Severity:** Important
**Category:** Correctness
**Location:** `agent/src/cmodule-tracer.ts:361`
**Confidence:** 85

**Problem:** `let count = writeIdx - readIdx` — when `writeIdx` wraps past `2^32` and is less than `readIdx`, this produces a negative number in JavaScript. The check `count > RING_CAPACITY` fails (negative < positive), and the loop iterates with a huge count.

**Impact:** After ~4.3 billion ring buffer writes (e.g., ~12 hours at 100K events/sec), the drain corrupts. Long-running sessions under heavy tracing are at risk.

**Suggested fix:**
```typescript
let count = (writeIdx - readIdx) >>> 0; // Force unsigned 32-bit
```

---

### Issue 4: funcId overflow at 2^30
**Severity:** Important
**Category:** Correctness
**Location:** `agent/src/cmodule-tracer.ts:286`
**Confidence:** 90

**Problem:** `ptr((funcId << 1) | isLight)` — JavaScript `<<` operates on signed 32-bit. When `funcId >= 2^30`, the shift overflows to negative, producing invalid data pointers. The C code will decode wrong func_ids.

**Impact:** Extremely unlikely (would need >1 billion hook installs), but represents undefined behavior.

**Suggested fix:** No immediate fix needed. Add a guard if paranoia warranted:
```typescript
if (funcId >= (1 << 30)) { /* error: too many hooks */ return false; }
```

---

### Issue 5: `sampled` field not persisted to database
**Severity:** Important
**Category:** Integration
**Location:** `agent/src/cmodule-tracer.ts:38,428,455` vs `src/db/event.rs:37-52`
**Confidence:** 80

**Problem:** The new TraceEvent includes `sampled?: boolean`, and events set it when adaptive sampling is active. However, the Rust `Event` struct and database schema have no `sampled` column. The field is silently dropped during event parsing.

**Impact:** Users cannot distinguish sampled events from full-fidelity events in query results. Loss of observability metadata.

**Suggested fix:** Add `sampled: Option<bool>` to Event struct and `sampled INTEGER` to events table schema. Low priority — can be done in follow-up.

---

### Issue 6: Signed integer overflow in C ring buffer `write_idx`
**Severity:** Important
**Category:** Security (memory safety)
**Location:** `agent/src/cmodule-tracer.ts:100` (C code)
**Confidence:** 95

**Problem:** `write_idx` is declared as `volatile gint` (signed 32-bit). `g_atomic_int_add` returns `gint`. When `write_idx` overflows past `INT_MAX` (~2.1 billion), C signed integer overflow is **undefined behavior**. `slot = pos % RING_CAPACITY` with negative `pos` produces negative slot → out-of-bounds write.

**Impact:** After ~2.1 billion writes (~6 hours at 100K events/sec), undefined behavior occurs. In practice, most compilers wrap, but it's UB per C standard.

**Suggested fix:** Cast to unsigned before modulo:
```c
guint32 slot = ((guint32)pos) % RING_CAPACITY;
```

---

### Issue 7: Missing `durationNs` on function_exit events
**Severity:** Minor
**Category:** Integration
**Location:** `agent/src/cmodule-tracer.ts:442-456`
**Confidence:** 90

**Problem:** The old agent computed `durationNs` for exit events by tracking enter timestamps. The CModule tracer doesn't compute duration — exit events lack `durationNs`. The database schema and MCP query results expose `duration_ns`, so this is a regression.

**Impact:** Consumers expecting `durationNs` (e.g. debug_query verbose mode) will see null values. Low severity since the data can be reconstructed from enter/exit timestamps.

---

### Issue 8: HookMode serialization not tested
**Severity:** Important
**Category:** Tests
**Location:** `src/frida_collector/hooks.rs:4-9`
**Confidence:** 90

**Problem:** The `#[serde(rename_all = "lowercase")]` attribute on `HookMode` is the contract between Rust and the TypeScript agent. No test verifies it serializes to `"full"`/`"light"`. A serde misconfiguration would silently break the protocol.

**Suggested fix:**
```rust
#[test]
fn test_hookmode_serde() {
    assert_eq!(serde_json::to_string(&HookMode::Full).unwrap(), "\"full\"");
    assert_eq!(serde_json::to_string(&HookMode::Light).unwrap(), "\"light\"");
}
```

---

### Issue 9: Mode-splitting logic in add_patterns untested
**Severity:** Important
**Category:** Tests
**Location:** `src/frida_collector/spawner.rs:851-889`
**Confidence:** 95

**Problem:** The core feature logic — splitting patterns into full/light batches via `classify_with_count` — has no unit test. The only verification was manual testing.

**Suggested fix:** Extract the grouping logic into a testable helper and add tests for: all-Full, all-Light, mixed, and empty-match edge cases.

---

### Issue 10: `overflow_count` header field allocated but never used
**Severity:** Minor
**Category:** Correctness
**Location:** `agent/src/cmodule-tracer.ts:216` (alloc), C code (never writes)
**Confidence:** 90

**Problem:** The ring buffer header reserves `overflow_count` at offset 8, but neither the C code increments it nor the JS drain reads it. The plan specified it should be incremented on overflow for observability.

**Impact:** No way to detect if events were lost. Cosmetic issue — drain's skip-ahead logic still works.

---

## Approved

- [x] CModule C source with ring buffer and mode encoding — superior single-CModule design
- [x] Ring buffer layout (header + 16384 × 48-byte entries) — matches spec
- [x] CModuleTracer class with all public API methods
- [x] HookInstaller thin facade delegation
- [x] agent.ts simplification (removed old JS hot path)
- [x] Rust HookMode enum with classify_pattern + classify_with_count
- [x] Rust spawner: mode field, pattern splitting, per-mode batches
- [x] 8 comprehensive pattern classification tests
- [x] mach_timebase_info conversion (Apple Silicon + Intel)
- [x] Adaptive sampling with hysteresis (2 up / 5 down cycles)
- [x] Per-thread depth-based parent tracking
- [ ] Ring buffer thread safety — blocked by Issue 1
- [ ] Event format compatibility — blocked by Issues 5, 7

## Recommendations

**Positive deviations from plan:**
- Single CModule with data-pointer mode encoding is cleaner than the planned two-CModule approach
- Comprehensive Rust test suite (26+ tests) exceeds plan requirements
- Platform-aware tick-to-nanosecond conversion handles both Apple Silicon and Intel

**Non-blocking suggestions:**
- Consider adding CModule compilation fallback to JS callbacks (plan mentioned as nice-to-have)
- Remove dead `serializer.ts` if still present (no longer imported)
- Add `overflow_count` increment to C code for observability
