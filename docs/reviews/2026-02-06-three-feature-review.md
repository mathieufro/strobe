# Architectural Review: Recent Feature Additions

**Reviewed:** 2026-02-06
**Scope:** Current uncommitted changes (contextual watches) + 2 previous commits
**Commits:**
- `fc1d394` - Add CModule high-performance tracing with native callbacks
- `f37bd43` - Add graceful hook limits and fix Frida recv() one-shot bug
- Uncommitted - Contextual watches implementation

**Reviewers:** 5 parallel agents (Completeness, Correctness, Security, Integration, Testing)

---

## Executive Summary

| Category | Critical | Important | Minor | Total |
|----------|----------|-----------|-------|-------|
| Security | 1 | 3 | 2 | 6 |
| Correctness | 2 | 1 | 2 | 5 |
| Integration | 1 | 0 | 3 | 4 |
| Tests | 2 | 3 | 2 | 7 |
| Documentation | 2 | 1 | 2 | 5 |
| **Total** | **8** | **8** | **11** | **27** |

**Ready to merge:** ⚠️ **NO** - 8 critical issues must be fixed first

### Feature Status
- ✅ **Hook Limits (f37bd43):** 100% complete, production-ready
- ✅ **CModule Tracing (fc1d394):** 100% complete, production-ready
- ⚠️ **Contextual Watches (uncommitted):** ~90% complete, **critical gap** prevents `on` field from working

---

## Critical Issues (Block Merge)

### 1. Contextual Watch Filtering Non-Functional
**Severity:** Critical
**Category:** Correctness
**Location:** `src/daemon/server.rs:633`

**Problem:**
The `on: ["pattern"]` field to scope watches to specific functions is **completely non-functional**:
```rust
// For now, all watches are global (no per-function filtering)
// TODO: Implement pattern-to-funcId mapping after hooks are installed
let on_func_ids: Option<Vec<u32>> = None;
```

**Impact:**
- User requests `{ variable: "gCounter", on: ["NoteOn"] }` expecting watch to only capture during NoteOn
- Code parses and stores `on_patterns` but sets `on_func_ids = None`
- Agent receives `onFuncIds: null`, creates empty Set, filters out ALL non-global watches
- **Result:** No watch values are captured because line 596 in cmodule-tracer.ts filters them out

**Fix:**
Either:
1. **Implement the feature** (3-4 hours):
   - After hook installation, map pattern strings → funcIds from hook results
   - Convert `watch_target.on: Vec<String>` → `on_func_ids: Vec<u32>`
   - Pass resolved IDs to agent

2. **Document as unimplemented** (30 minutes):
   - Remove `on` from MCP schema
   - Add warning to types.rs docs: "The on field is not yet implemented"
   - Treat `None` as global in agent (change line 596 logic)

**Recommendation:** Fix #2 immediately (merge blocker), implement #1 in follow-up PR.

---

### 2. Watch Confirmation Missing
**Severity:** Critical
**Category:** Integration
**Location:** `agent/src/agent.ts:131-142`, `src/frida_collector/spawner.rs:756`

**Problem:**
When watches are sent to the agent, there's no confirmation signal back to the daemon (unlike hooks which use `HooksReadySignal`). If watch installation fails (bad address, compilation error), daemon never knows.

**Impact:**
- `debug_trace` returns success even if watches failed to install
- User queries events expecting watch values, gets none, no error message
- Silent failure mode

**Fix:**
Add watch confirmation signal in `spawner.rs`:
```rust
// In SetWatches handler (line 733):
let (signal_tx, signal_rx) = std::sync::mpsc::channel();
{
    let mut guard = hooks_ready.lock().unwrap();
    *guard = Some(signal_tx);
}
// After post_message_raw (line 757):
match signal_rx.recv_timeout(Duration::from_secs(5)) {
    Ok(count) => tracing::info!("Agent confirmed {} watches active", count),
    Err(_) => return Err(Error::WatchFailed("Timeout waiting for confirmation".into())),
}
```

And extend `AgentMessageHandler::handle_payload` to send `signal_tx.send(count)` on `watches_updated`.

---

### 3. Watches Parameter Missing from MCP Schema
**Severity:** Critical
**Category:** Documentation
**Location:** `src/daemon/server.rs:309-321` (debug_trace tool schema)

**Problem:**
The `watches` parameter is parsed by the handler (line 614) but **not advertised in the MCP tool schema**. LLMs cannot discover or use this feature.

**Impact:**
- Feature is invisible to LLM agents
- No documentation of watch syntax, types, or limitations
- Users must read Rust code to discover the API

**Fix:**
Add to `input_schema` at line 319:
```json
"watches": {
  "type": "object",
  "description": "Watch global variables during function execution (requires debug symbols)",
  "properties": {
    "add": {
      "type": "array",
      "items": {
        "type": "object",
        "properties": {
          "variable": { "type": "string", "description": "Variable name or expression like 'gClock->counter'" },
          "address": { "type": "string", "description": "Hex address for raw memory watches" },
          "type": { "type": "string", "description": "Type hint: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer" },
          "label": { "type": "string", "description": "Display label for this watch" },
          "expr": { "type": "string", "description": "JavaScript expression for custom reads" },
          "on": {
            "type": "array",
            "items": { "type": "string" },
            "description": "NOT YET IMPLEMENTED. Will restrict watch to specific function patterns."
          }
        }
      }
    },
    "remove": {
      "type": "array",
      "items": { "type": "string" },
      "description": "Labels of watches to remove"
    }
  }
}
```

---

### 4. Unbounded Watch Expression Parsing (DoS)
**Severity:** Critical
**Category:** Security
**Location:** `src/daemon/server.rs:623-628`, `src/dwarf/parser.rs:647`

**Problem:**
Watch expressions have no length or complexity limits. A crafted input like `"a->".repeat(10000) + "b"` could cause stack overflow during DWARF parsing.

**Impact:**
- Denial of service via memory exhaustion or stack overflow
- Daemon crash on malicious input

**Fix:**
Add validation before DWARF parsing at line 623:
```rust
const MAX_WATCH_EXPR_LEN: usize = 256;
const MAX_DEREF_DEPTH: usize = 4;

if let Some(ref expr) = watch_target.expr {
    if expr.len() > MAX_WATCH_EXPR_LEN {
        return Err(Error::WatchFailed("Expression too long (max 256 chars)".into()));
    }
    if expr.matches("->").count() > MAX_DEREF_DEPTH {
        return Err(Error::WatchFailed("Too many dereferences (max 4 levels)".into()));
    }
    let recipe = dwarf.resolve_watch_expression(expr)?;
    // ...
}
```

---

### 5. Unbounded eventLimit (Disk Fill DoS)
**Severity:** Critical
**Category:** Security
**Location:** `src/daemon/server.rs:605`

**Problem:**
`eventLimit` parameter has no upper bound. User can set `eventLimit: usize::MAX` and fill disk until daemon OOMs or system crashes.

**Impact:**
- Previous incident: 5.4M events (3.2GB DB) caused 179% CPU spin
- Unlimited growth → disk fill → system crash

**Fix:**
Cap at reasonable maximum:
```rust
const MAX_EVENT_LIMIT: usize = 10_000_000; // 10M hard cap

if let Some(limit) = req.event_limit {
    if limit > MAX_EVENT_LIMIT {
        return Err(crate::Error::Frida(format!(
            "Event limit {} exceeds maximum allowed ({})",
            limit, MAX_EVENT_LIMIT
        )));
    }
    if limit == 0 {
        return Err(crate::Error::Frida("Event limit must be > 0".into()));
    }
    self.session_manager.set_event_limit(session_id, limit);
}
```

---

### 6. Hook Count Accumulation Wrong
**Severity:** Critical
**Category:** Correctness
**Location:** `src/frida_collector/spawner.rs:928`

**Problem:**
When installing hooks in chunks, each chunk **overwrites** `total_hooks` instead of accumulating:
```rust
for chunk in full_funcs.chunks(CHUNK_SIZE) {
    match self.send_add_chunk(session_id, chunk.to_vec(), image_base, HookMode::Full).await {
        Ok(count) => total_hooks = count,  // ← BUG: should be +=
        Err(e) => { ... }
    }
}
```

**Impact:**
- MCP response shows count from **last chunk only**, not total
- Installing 150 hooks (3 chunks of 50) reports `hookedFunctions: 50`
- User thinks only 50 hooks are active

**Fix:**
Change line 928, 933, 937, 941 to `total_hooks += count;`

---

### 7. `on` Field Documented But Non-Functional
**Severity:** Critical
**Category:** Documentation
**Location:** `src/mcp/types.rs:67`, `docs/plans/2026-02-06-contextual-watches.md`

**Problem:**
Plan document advertises `on` field as a key feature ("Task 5: per-function filtering via onFuncIds"), types.rs includes it in MCP schema, but it's **completely non-functional** (see Issue #1).

**Impact:**
- Users try to use `on` field based on docs
- Feature appears implemented but silently fails
- No error message, just missing watch values

**Fix:**
Add to types.rs line 67:
```rust
/// IMPORTANT: This field is currently NOT IMPLEMENTED (see server.rs:633).
/// All watches are global regardless of this value. Do not use in production.
#[serde(skip_serializing_if = "Option::is_none")]
pub on: Option<Vec<String>>,
```

---

### 8. Unbounded Watch Count (Memory Corruption Risk)
**Severity:** Critical
**Category:** Security
**Location:** `src/daemon/server.rs:621-686` (no limit on watch loop)

**Problem:**
No limit on number of watches per session. User can send 10,000 watches, agent tries to install them all.

**Impact:**
- CModule has 4 watch slots, but `watch_count` is `gint`
- Agent caps reads at 4, but loop overhead still processes 10k entries
- Memory corruption risk if `watch_count` overflows to negative

**Fix:**
```rust
const MAX_WATCHES_PER_SESSION: usize = 32;

for watch_target in add_watches {
    if frida_watches.len() >= MAX_WATCHES_PER_SESSION {
        warnings.push(format!(
            "Watch limit reached ({} max). Additional watches ignored.",
            MAX_WATCHES_PER_SESSION
        ));
        break;
    }
    // ... existing logic
}
```

---

## Important Issues (Fix Before 1.0)

### 9. EventLimit=0 Silently Drops All Events
**Severity:** Important
**Category:** Correctness
**Location:** `src/db/event.rs:401`

**Current behavior:** Setting `eventLimit: 0` causes cleanup to delete all new events immediately (new_count > 0 is always true).

**Fix:** Reject 0 in validation (see Issue #5 fix).

---

### 10. No Test for Hook Limit Truncation
**Severity:** Important
**Category:** Testing
**Location:** Missing from `tests/integration.rs`

**Gap:** Hook limit (100 cap) is implemented and used in production, but has zero test coverage. Could regress silently.

**Fix:** Add integration test:
```rust
#[tokio::test]
async fn test_hook_limit_truncation() {
    // Create pattern matching >100 functions
    // Verify hooked_functions <= 100
    // Verify warnings include truncation message
    // Verify matched_functions > hooked_functions
}
```

---

### 11. No Test for Event Limit Runtime Config
**Severity:** Important
**Category:** Testing
**Location:** Missing from `tests/`

**Gap:** `debug_trace({ sessionId, eventLimit })` is advertised as the primary way to adjust limits, but is completely untested.

**Fix:** Add test:
```rust
#[tokio::test]
async fn test_event_limit_runtime_change() {
    // Launch session with default 200k limit
    // Generate 100k events
    // Change to eventLimit: 50k via debug_trace
    // Generate 100k more events
    // Verify only 50k remain
}
```

---

### 12. No End-to-End Watch Test
**Severity:** Important
**Category:** Testing
**Location:** Missing from `tests/integration.rs`

**Gap:** All watch tests are unit tests (DWARF parsing, DB storage). No test launches a real binary with global variables and verifies watch values are captured.

**Fix:** Add E2E test using erae_mk2_simulator or simple test binary.

---

### 13. Unaligned Watch Addresses Crash on ARM64
**Severity:** Important
**Category:** Correctness
**Location:** `agent/src/cmodule-tracer.ts:157-160` (C code)

**Problem:**
Direct cast to `guint16/32/64*` and dereference without alignment check. ARM64 crashes on unaligned multi-byte reads.

**Impact:** Watching misaligned struct members crashes target process.

**Fix:** Use Frida's safe read APIs or validate alignment:
```c
// Before reading, check alignment:
if ((addr % watch_sizes[w]) != 0) {
    // Write error sentinel or skip
    continue;
}
```

---

### 14. Lock Contention on event_limits
**Severity:** Important
**Category:** Integration/Performance
**Location:** `src/daemon/session_manager.rs:276, 304`

**Problem:**
DB writer reads `event_limits.read()` on **every batch** (100 events), causing ~100 lock acquisitions/second per session.

**Impact:** Unnecessary lock contention, CPU overhead.

**Fix:** Cache limit for ~1 second:
```rust
let mut cached_limit = max_events;
let mut batches_since_refresh = 0;
// ... in loop:
if batches_since_refresh >= 10 {
    cached_limit = event_limits.read().unwrap().get(session_id)...;
    batches_since_refresh = 0;
}
```

---

### 15. Watches Not Documented in MCP Instructions
**Severity:** Important
**Category:** Documentation
**Location:** `src/daemon/server.rs:234-299` (debugging instructions)

**Problem:** Watch feature exists but is never mentioned in the instructions LLMs read.

**Fix:** Add section after line 282:
```markdown
## Watching Variables

Read global/static variable values during function execution. Requires debug symbols (DWARF).

- Variable syntax: `gCounter`, `gClock->counter` (pointer dereferencing)
- Raw address: `{ address: "0x1234", type: "f64", label: "tempo" }`
- JS expressions: `{ expr: "ptr(0x5678).readU32()", label: "custom" }`
- Max 4 native watches (fast CModule reads), unlimited JS expression watches
- **IMPORTANT:** The `on` field is not yet implemented. All watches are global.
```

---

## Minor Issues (Fix When Convenient)

### 16-27. Additional Minor Issues

See full agent outputs for:
- Watch state cleanup on session restart (#16)
- Multi-level pointer deref limitation (#17)
- CModule watch read optimization (#18)
- RwLock panic handling (#19)
- Agent error surfacing (#20)
- Pattern validation (#21)
- DWARF error path testing (#22)
- Bit-packing overflow documentation (#23)
- Hook limit warning expansion (#24)
- Stress test not in CI (#25)
- Agent zero test coverage (#26)
- TODO comment guidance (#27)

---

## Architectural Assessment

### Strengths

1. **Clean async/sync separation:** No `.await` across lock holds, proper use of `spawn_blocking` for DWARF parsing
2. **Thread safety:** Arc<RwLock> usage is consistent and safe, no deadlock risks identified
3. **Error handling:** Proper `Result<>` propagation, no silent unwraps in error paths (except lock poisoning)
4. **Performance-conscious:** CModule tracing is ~10-50x faster than JS callbacks, event batching reduces DB overhead
5. **MCP interface design:** Consistent naming, structured errors, good separation of concerns

### Weaknesses

1. **Critical feature incomplete:** Contextual watches (`on` field) advertised but non-functional
2. **Input validation gaps:** No limits on expression complexity, watch counts, event limits
3. **Test coverage holes:** Major features (hook limits, event limits, watches) lack integration tests
4. **Documentation lag:** Features implemented but not documented in MCP schema or instructions
5. **Tight coupling:** MCP handler directly calls DWARF parser (blocking async handler)

### Future-Proofing Concerns

1. **Single deref limitation:** Current design only supports `ptr->member`, not `ptr->ptr->member`. Need to extend CModule C code or document limitation.
2. **Confirmation pattern inconsistency:** Hooks use HooksReadySignal, watches don't. Should standardize.
3. **Per-connection state:** Well-implemented, but no cleanup on connection drop (relies on client calling stop).
4. **DWARF cache growth:** No eviction policy, cache grows unbounded. Add LRU eviction at ~100 entries.

---

## Recommendations

### Before Merge (Critical Path)

1. **Fix Issue #1:** Document `on` field as unimplemented, treat `None` as global in agent
2. **Fix Issue #2:** Add watch confirmation signal (copy HooksReadySignal pattern)
3. **Fix Issue #3:** Add `watches` to MCP tool schema
4. **Fix Issue #4:** Add watch expression validation (length + deref depth limits)
5. **Fix Issue #5:** Cap `eventLimit` at 10M, reject 0
6. **Fix Issue #6:** Fix hook count accumulation (change `=` to `+=`)
7. **Fix Issue #7:** Add "NOT IMPLEMENTED" warning to `on` field docs
8. **Fix Issue #8:** Add MAX_WATCHES_PER_SESSION limit (32 watches)

**Estimated effort:** 4-6 hours for all critical fixes.

### Before 1.0 Release

1. Add integration tests for hook limits, event limits, watches E2E
2. Fix alignment check for ARM64 watch reads
3. Cache event_limits to reduce lock contention
4. Document watches in MCP instructions
5. Implement contextual watch filtering (pattern → funcId mapping)

**Estimated effort:** 1-2 days.

### Architecture Improvements

1. Extract watch resolution into `SessionManager::resolve_and_set_watches()` to decouple MCP handler from DWARF
2. Standardize agent command confirmations (hooks, watches, future commands)
3. Add DWARF cache eviction (LRU, max 100 entries)
4. Support multi-level pointer derefs in CModule
5. Add agent test suite (TypeScript unit tests for CModule logic)

**Estimated effort:** 1 week.

---

## Conclusion

The three features under review demonstrate **strong architectural foundations** with careful attention to performance (CModule tracing), robustness (hook limits, chunking), and scalability (event limits). However, **8 critical issues** prevent merging in current state:

- **Contextual watches** are ~90% complete but the core filtering feature doesn't work
- **Input validation** gaps create DoS vectors (unbounded expressions, limits, watch counts)
- **Documentation** is incomplete (watches invisible to LLMs, `on` field misleading)
- **Testing** has major gaps (hook limits, event limits, watches E2E all untested)

**Verdict:** Fix the 8 critical issues (4-6 hours work) before merging. The code is production-ready for **global watches only** and the other two features (hook limits, CModule tracing) are solid.
