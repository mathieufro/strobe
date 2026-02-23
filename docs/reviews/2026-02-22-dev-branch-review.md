# Dev Branch Review: `dev` vs `main`

**Date**: 2026-02-22
**Scope**: 11 commits, 76 files changed, ~6.3K additions, ~24.6K deletions
**Review method**: 7 parallel code-review agents covering all code areas

## Commits Reviewed

1. `4358e63` feat: add symbolsPath parameter and LLM guidance for missing debug symbols
2. `82b6470` feat: save debug_ui screenshots as PNG files instead of base64
3. `5d4dad7` fix: FIFO eviction preserves stdout/stderr so test output is never truncated
4. `5827e77` feat: per-connection test run isolation for multi-session support
5. `f38fdc8` fix: search project root for .dSYM bundles instead of hardcoding paths
6. `f6ada4b` feat: capture full sanitizer output via log_path redirect
7. `204de1f` feat: add JavaScript/TypeScript support — JsResolver, V8/JSC tracers, Vitest/Jest/Bun adapters
8. `0b96fb5` Merge branch 'feature/phase5a-python-completion' into dev
9. `c1c75ef` fix: resolve 3 Python tracing bugs found during field testing
10. `12244a0` feat: add sanitizer crash detection from stderr output
11. `1e8a26b` feat: complete Python tracing — readVariable, logpoints, breakpoints, hook removal

---

## Critical Findings (Must Fix Before Merge)

### C1. Memory leak: `languages` and `resolvers` maps never cleaned on session stop
**File**: `src/daemon/session_manager.rs:328-336, 360-367`
**Confidence**: 95%

Both `stop_session` and `stop_session_retain` clean up 8 in-memory maps but miss the 2 new ones added in this branch: `languages` and `resolvers`. The `resolvers` map holds `Arc<dyn SymbolResolver>` containing full Python/JS ASTs. In a long-running daemon with many test runs, this is a sustained memory leak.

**Fix**: Add to both stop methods:
```rust
write_lock(&self.languages).remove(id);
write_lock(&self.resolvers).remove(id);
```

### C2. Bun test adapter is non-functional: JUnit output goes to file, not stdout
**File**: `src/test/bun_adapter.rs:28-36`
**Confidence**: 95%

Bun's `--reporter=junit` requires `--reporter-outfile` to specify where XML goes. Without it, stdout receives console output, not XML. The adapter's `parse_output` always receives non-XML content and produces zero results.

**Fix**: Add `--reporter-outfile=/dev/stdout` to the command args, or write to a temp file.

### C3. Bun adapter `single_test_command` passes test name as file filter
**File**: `src/test/bun_adapter.rs:38-48`
**Confidence**: 95%

Bun CLI treats positional args as *file path* filters, not test name filters. Must use `--test-name-pattern` flag to filter by test name.

### C4. JSC tracer emits wrong-hook events for every JS call; exit events missing
**File**: `agent/src/tracers/jsc-tracer.ts:83-102`
**Confidence**: 88%

`tryEmitForJscFunction` emits a single event for whichever hook is first in the Map — with no validation that the intercepted function matches that hook. Every JS call produces events attributed to the wrong function. Additionally, `onLeave` is empty — no exit events are ever emitted.

### C5. V8 tracer `wrapObject` prefix always `''` — qualified name match never fires
**File**: `agent/src/tracers/v8-tracer.ts:142, 154-165`
**Confidence**: 86%

The `prefix` parameter passed to `wrapObject` is always `''`, so `qualifiedName` at the top level is just the key name. A pattern like `Calculator.add` can never match via the `hook.target.name === qualifiedName` path — only the unqualified `targetFuncName === key` fires, causing false-positive matches on any function named `add`.

### C6. Python nested function names double-prefix at depth 2+
**File**: `src/symbols/python_resolver.rs:135-136, 151-152`
**Confidence**: 95%

`new_prefix.push(qualified_name)` pushes the full dotted path instead of just the simple name. For `outer -> inner -> deepest`, the result is `outer.outer.inner.deepest` instead of `outer.inner.deepest`.

**Fix**: `new_prefix.push(f.name.to_string())` instead of `new_prefix.push(qualified_name)`.

### C7. Python `*.egg-info` exclusion is literal match, not glob
**File**: `src/symbols/python_resolver.rs:21`
**Confidence**: 90%

`matches!` compares the literal string `"*.egg-info"` — no glob expansion. Real `.egg-info` directories are never excluded, causing the resolver to walk thousands of library files in site-packages.

**Fix**: Use `name.ends_with(".egg-info") || name.ends_with(".dist-info")`.

### C8. JS source map resolution dead: `dist/` in SKIP_DIRS
**File**: `src/symbols/js_resolver.rs:11, 400`
**Confidence**: 88%

The `SKIP_DIRS` list includes `"dist"`, preventing `WalkDir` from ever indexing source maps in `dist/` — the most common output directory for TypeScript compilation. The test masks this with `if let Some(...)`, passing vacuously when resolution returns `None`.

---

## Important Findings (Should Fix)

### I1. Script pointer leak on `load_script_raw` failure in child spawn
**File**: `src/frida_collector/spawner.rs:1876-1879`
**Confidence**: 95%

When `load_script_raw` fails in `handle_child_spawn`, the `script_ptr` from `create_script_raw` is never unref'd. Same leak exists in the main spawn path (lines 1109-1141).

### I2. `symbolsPath` not validated for path traversal
**File**: `src/daemon/server.rs:991-999`
**Confidence**: 83%

`command` and `project_root` validate against `..` components, but `symbols_path` — a new parameter — does not.

### I3. `stop_session` counts events before flushing writer
**File**: `src/daemon/session_manager.rs:312-313`
**Confidence**: 85%

`count_session_events` runs before the writer task flushes, undercounting by up to the final batch size.

### I4. Watch `no_slide` flag lost on watch removal
**File**: `src/daemon/server.rs:1440-1452`
**Confidence**: 82%

`ActiveWatchState` doesn't store `no_slide`, so address-based watches lose their `no_slide: true` when remaining watches are re-sent to the agent after a removal.

### I5. Python GIL deadlock risk at breakpoints
**File**: `agent/src/tracers/python-tracer.ts:253`
**Confidence**: 92%

Python breakpoint blocks GIL via `_strobe_bp_event.wait()`. Any subsequent `runPython()` call from Frida JS thread (hook adds, breakpoint changes) will call `PyGILState_Ensure()` and deadlock. The 50ms flush timer makes collision near-certain.

### I6. Python `hitCount` stored but never enforced
**File**: `agent/src/tracers/python-tracer.ts:420-421`
**Confidence**: 88%

`PythonBreakpoint` has `hitCount`/`currentHits` fields, but neither is serialized to the Python trace function. Python breakpoints fire every time regardless of `hitCount`.

### I7. Python `writeVariable` allows code injection
**File**: `agent/src/tracers/python-tracer.ts:582`
**Confidence**: 87%

String values are interpolated directly into `PyRun_SimpleString`. A value like `foo"; import os; os.system("cmd")` executes arbitrary Python.

**Fix**: Use `JSON.stringify(value)` for string values.

### I8. FIFO eviction count uses total events, not evictable count
**File**: `src/db/event.rs:596-644`
**Confidence**: 92%

`to_delete` is computed from total event count (including non-evictable stdout/stderr), but DELETE targets only evictable types. When many stdout events exist, the buffer permanently exceeds its limit.

### I9. Child-to-session association uses arbitrary `HashMap` iteration in multi-session
**File**: `src/frida_collector/spawner.rs:1817-1820`
**Confidence**: 90%

`reg.values().next()` picks any active session, potentially attributing a child process to the wrong session.

### I10. Bun adapter detection false-positives on Vitest/Jest projects using Bun as package manager
**File**: `src/test/bun_adapter.rs:9-18`
**Confidence**: 95%

`bun.lockb` presence (Bun as package manager) triggers 95 confidence, beating Vitest's 95. Projects using Vitest+Bun get the wrong adapter.

### I11. Inverted `spawn_with_frida`/`create_session` ordering in tests
**File**: `tests/breakpoint_basic.rs`, `tests/breakpoint_behavioral.rs`, `tests/stepping_basic.rs`, `tests/stepping_behavioral.rs`, `tests/logpoint_and_write.rs`, `tests/phase2a_gaps.rs`, `tests/ui_observation.rs`
**Confidence**: 90%

All behavioral test files call `spawn_with_frida` before `create_session`, creating a race condition where events arrive before the FK exists. The Python tests (correctly) call `create_session` first.

---

## Lower-Priority Findings

| # | Area | Issue | Confidence |
|---|------|-------|------------|
| L1 | JS resolver | `pattern_to_regex` reimplements `PatternMatcher` instead of reusing it | 96% |
| L2 | JS resolver | `//` inside string literals stripped as comments | 80% |
| L3 | JS resolver | Block comment handling doesn't parse code after `*/` on same line | 87% |
| L4 | JS resolver | `constructor` not excluded from method table | 85% |
| L5 | V8 tracer | `writeVariable` via `new Function()` silently swallows all errors | 83% |
| L6 | V8 tracer | `Date.now() * 1_000_000` gives ms-resolution timestamps labeled as ns | 81% |
| L7 | JSC tracer | `Interceptor.attach` on every JS call with no empty-hook fast path | 82% |
| L8 | Python | Single global `_strobe_bp_event` can't handle concurrent thread breakpoints | 83% |
| L9 | Python | `removeAllHooks()` doesn't clear breakpoints/logpoints from state maps | 82% |
| L10 | Python | `NativeFunction` objects re-created on every `readVariable` call | 85% |
| L11 | DB | Column index fragility in `event_from_row` positional access | 83% |
| L12 | Daemon | `symbolsPath` ignored on DWARF re-parse after cache eviction | 90% |
| L13 | Tests | `python_comprehensive.rs` has no assertion — always passes | 85% |
| L14 | Tests | `python_features.rs` breakpoint test doesn't verify pause location | 85% |

---

## Summary

| Severity | Count | Key Areas |
|----------|-------|-----------|
| Critical | 8 | Session leak, Bun adapter broken, JSC tracer wrong, Python resolver |
| Important | 11 | FFI leaks, deadlock risk, injection, FIFO logic, test ordering |
| Lower | 14 | JS parser edge cases, timestamp resolution, test coverage gaps |

**Recommendation**: The critical findings C1 (memory leak), C2-C3 (Bun adapter), C6-C7 (Python resolver), and C8 (source map) are straightforward fixes. C4-C5 (JS tracers) may require more design work. The Bun adapter (C2+C3+I10) is effectively non-functional and should either be fixed or feature-flagged before merge.
