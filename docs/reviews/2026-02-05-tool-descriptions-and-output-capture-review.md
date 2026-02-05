# Review: Tool Descriptions & Process Output Capture

**Plan:** `docs/plans/2026-02-05-tool-descriptions-and-output-capture.md`
**Reviewed:** 2026-02-05
**Commits:** fae80c7..1c336d6
**Branch:** feature/tool-descriptions-and-output-capture

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Correctness | ~~1~~ 0 | ~~1~~ 0 | 0 |
| Security | 0 | ~~1~~ 0 | ~~1~~ 0 |
| Tests | 0 | ~~3~~ 0 | 0 |
| Code Quality | 0 | ~~1~~ 0 | ~~1~~ 0 |
| **Total** | **0** | **0** | **0** |

**Ready to merge:** Yes (all issues resolved)

## Issues (All Resolved)

### Issue 1: Re-entrancy in write(2) interception
**Severity:** Critical
**Category:** Correctness
**Status:** FIXED

Added `inOutputCapture` boolean guard to `StrobeAgent`. The entire `onEnter` handler is wrapped in `if (self.inOutputCapture) return` with try/finally to ensure the flag is always reset. Prevents infinite recursion when `send()` triggers `write()`.

---

### Issue 2: Empty function_name for output events
**Severity:** Important
**Category:** Correctness
**Status:** FIXED

Added `AND event_type IN ('function_enter', 'function_exit')` to function_equals and function_contains SQL clauses in `query_events`. This ensures function name filters automatically exclude output events (which have empty function_name).

---

### Issue 3: Unbounded text storage / no per-session limits
**Severity:** Important
**Category:** Security
**Status:** FIXED

Added `outputBytesCapture` counter and `maxOutputBytes` (50MB) limit to `StrobeAgent`. When the limit is reached, a final event is emitted with `[strobe: output capture limit reached (50MB), further output truncated]` and all subsequent writes are skipped.

---

### Issue 4: LIKE pattern injection in query builder
**Severity:** Minor
**Category:** Security
**Status:** FIXED

Added `escape_like_pattern()` helper that escapes `\`, `%`, and `_` characters. Applied to both `function_contains` and `source_file_contains` LIKE queries with `ESCAPE '\'` clause.

---

### Issue 5: No tests for parse_event with output events
**Severity:** Important
**Category:** Tests
**Status:** FIXED

Added 6 unit tests in `frida_collector::spawner::tests`:
- `test_parse_event_stdout` - valid stdout parsing
- `test_parse_event_stderr` - valid stderr parsing
- `test_parse_event_stdout_missing_text` - missing text field
- `test_parse_event_stdout_missing_required_fields` - missing id/timestamp/threadId
- `test_parse_event_function_enter` - function event still works
- `test_parse_event_unknown_type` - invalid eventType

---

### Issue 6: No test for mixed event types in same query
**Severity:** Important
**Category:** Tests
**Status:** FIXED

Added `test_mixed_event_types_in_unified_timeline` integration test. Inserts function_enter, stdout, and function_exit events, verifies chronological ordering, and confirms that `function_contains` filter excludes output events.

---

### Issue 7: insert_events_batch not tested with text field
**Severity:** Important
**Category:** Tests
**Status:** FIXED

Added `test_batch_insert_with_output_events` integration test. Batch-inserts a function event and a stdout event, then queries and verifies both are stored correctly with text field preserved.

---

### Issue 8: Type-unsafe buffer mixing in agent
**Severity:** Important
**Category:** Code Quality
**Status:** FIXED

Introduced `type BufferedEvent = TraceEvent | OutputEvent` union type. Changed `eventBuffer` from `TraceEvent[]` to `BufferedEvent[]` and `bufferEvent` to accept `BufferedEvent`. Removed the `event as any` cast.

---

### Issue 9: Silent truncation of >1MB writes
**Severity:** Minor
**Category:** Code Quality
**Status:** FIXED

For writes >1MB, instead of silently returning, the agent now emits a truncation indicator event: `[strobe: write of N bytes truncated (>1MB)]`.

---

## Approved

- [x] Task 1: MCP tool descriptions - implemented correctly
- [x] Task 2: Agent output capture - implemented with re-entrancy guard, output limits, truncation indicators
- [x] Task 3: EventType/Event/parse_event extensions - implemented correctly
- [x] Task 4: Database schema and queries - implemented with LIKE escaping
- [x] Task 5: Query response formatting - implemented correctly
- [x] Task 6: Tests - comprehensive coverage (38 tests total, all passing)

## Recommendations

1. Do a manual end-to-end test with a process that prints to stdout to verify the full pipeline
2. Consider adding a `@stdout` meta-pattern to debug_trace that explicitly opts into output capture (instead of always-on)
3. The `readUtf8String(count)` in the agent may have issues with non-UTF-8 binary output; consider `readByteArray` + manual decode as a follow-up
4. ~~Pre-existing: daemon tests SIGSEGV when run in parallel with other tests due to Frida global state init race~~ **FIXED:** FridaSpawner is now lazily initialized (deferred from `SessionManager::new()` to first use), eliminating the parallel test SIGSEGV
