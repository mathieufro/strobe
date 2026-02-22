# Phase 5b: Language Support Deepening

**Date:** 2026-02-22
**Goal:** Fix Python function tracing, add Node.js ESM support, validate JSC Inspector Protocol for Bun, and ship 4 new test adapters (Deno, Go, Mocha, Google Test).

---

## Workstream A: Fix Python Function Tracing

### Root Cause

`sys.settrace()` is **per-thread**. When called from Frida's agent thread via `PyRun_SimpleString`, it installs the trace callback on Frida's ephemeral `PyThreadState`, not the main Python thread. The main thread's `c_tracefunc` is never set.

### Solution: Version-Aware Tracing

**Python 3.12+ (primary):** Use `sys.monitoring` (PEP 669).
- Interpreter-global — affects all threads regardless of calling thread
- 10-20x less overhead than `sys.settrace`
- Fine-grained event selection: `PY_START`, `PY_RETURN`, `CALL`, `LINE`
- Can return `DISABLE` per-location to filter noise

```python
import sys
sys.monitoring.use_tool_id(0, "strobe")
sys.monitoring.set_events(0,
    sys.monitoring.events.PY_START |
    sys.monitoring.events.PY_RETURN
)
sys.monitoring.register_callback(0, sys.monitoring.events.PY_START, _strobe_on_start)
sys.monitoring.register_callback(0, sys.monitoring.events.PY_RETURN, _strobe_on_return)
```

**Python 3.12+ (fallback if sys.monitoring conflicts):** Use `threading.settrace_all_threads()` — iterates all `PyThreadState` objects and sets `c_tracefunc` on each.

**Python 3.11:** Use Frida `NativeFunction` to call `_PyEval_SetTrace` targeting the main thread's `PyThreadState` directly, bypassing `sys.settrace()`:
1. Read `_PyRuntime.gilstate.tstate_current` to find the main thread state
2. Call `_PyEval_SetTrace(main_tstate, trace_func, trace_obj)` via NativeFunction

### Changes

| File | Change |
|------|--------|
| `agent/src/tracers/python-tracer.ts` | Detect CPython version. Use `sys.monitoring` on 3.12+, `_PyEval_SetTrace` NativeFunction on 3.11. |
| `agent/src/tracers/python-tracer.ts` | Refactor `syncTraceHooks()` to generate version-appropriate Python code |
| `src/daemon/session_manager.rs` | No changes (resolver already works) |

### Validation

1. `debug_launch` a Python 3.12 script
2. `debug_trace({ add: ["mymodule.*"] })`
3. `debug_query({ eventType: "function_enter" })` returns matched function calls
4. Repeat with Python 3.11

---

## Workstream B: Node.js ESM Module Support

### Problem

V8Tracer patches `Module.prototype._compile` — CJS only. ESM `import` statements bypass `_compile` entirely. ESM namespace objects have **immutable bindings** — you can't proxy exports post-load.

### Solution: Layered Approach

**Layer 1 (existing):** `Module._compile` hook for CJS. Keep as-is.

**Layer 2 (spawn-time injection):** When Strobe spawns a Node.js process, inject `--import /tmp/strobe-esm-hooks.mjs` into the command args. This registration script uses `module.registerHooks()` (Node 22.15+) or `module.register()` (Node 20.6+) to intercept ESM modules at load time, before static imports resolve.

The hook script:
- Intercepts the `load()` phase for ESM modules
- Injects wrapper code around function exports at source level
- Wrapper code calls a global `__strobe_trace(event, funcName, file, line)` function
- The Frida agent installs `__strobe_trace` on `globalThis` before the hook script runs

**Layer 3 (runtime registration):** When `debug_trace` adds patterns to an already-running session, call `module.registerHooks()` from the V8Tracer to catch future dynamic `import()` calls.

**Layer 4 (fallback for tsx/ts-node CJS mode):** When users run through `tsx`, `ts-node`, or `esbuild-register` in CJS mode, the existing `_compile` hook already works because these tools feed transformed source through the standard CJS compilation path.

### Changes

| File | Change |
|------|--------|
| `src/frida_collector/spawner.rs` | For JS sessions on Node.js, inject `--import` arg pointing to a temp hook script |
| `agent/src/tracers/v8-tracer.ts` | Add `registerEsmHooks()` method using `module.registerHooks()` for runtime ESM tracing |
| `agent/src/tracers/v8-tracer.ts` | Install `globalThis.__strobe_trace` before ESM hooks fire |
| New: temp file generation | Generate the ESM hook registration script (written to `/tmp/strobe-esm-hooks-<session>.mjs`) |

### Version Matrix

| Node.js Version | CJS | ESM (static) | ESM (dynamic import) |
|-----------------|-----|-------------|---------------------|
| < 20.6 | `_compile` hook | Not supported | Not supported |
| 20.6 - 22.14 | `_compile` hook | `--import` + `module.register()` | `module.register()` |
| 22.15+ / 23.5+ | `_compile` hook | `--import` + `module.registerHooks()` | `module.registerHooks()` |

### Validation

1. `debug_launch` a Node.js app using ESM imports
2. `debug_trace({ add: ["Router.*"] })`
3. `debug_query` returns function enter/exit events for ESM-imported functions
4. Verify `tsx` and `ts-node` CJS mode still works

---

## Workstream C: JSC/Bun Inspector Protocol Validation

### Hypothesis

Bun implements the WebKit Inspector Protocol. `Debugger.addSymbolicBreakpoint` with `autoContinue: true` provides function-level tracing by name/regex — version-stable, no JSC struct navigation needed.

### Validation Script

Build a standalone test script that:

1. Spawns `bun --inspect-wait test-app.ts`
2. Connects via WebSocket to the inspector endpoint
3. Sends `Debugger.enable`
4. Listens for `Debugger.scriptParsed` events
5. Sends `Debugger.addSymbolicBreakpoint({ symbol: "handleRequest", options: { autoContinue: true, actions: [{ type: "evaluate", data: "..." }] } })`
6. Resumes execution
7. Measures: (a) Does `autoContinue` truly avoid visible pausing? (b) What's the per-call overhead? (c) Do regex patterns work? (d) Can we extract function args?

### Success Criteria

- `addSymbolicBreakpoint` with `autoContinue: true` fires for matching functions
- Overhead < 5x for traced functions (debugger mode is ~2x baseline)
- Regex patterns match correctly
- We can extract useful data (at minimum: function name, timestamp)

### If Validation Succeeds: Architecture

**Hybrid approach:** Frida for output capture + crash handling. WebSocket Inspector for JS tracing. Two channels to one process.

| Component | Frida | Inspector |
|-----------|-------|-----------|
| stdout/stderr capture | Yes | No |
| Crash handling | Yes | No |
| JS function tracing | No | Yes (symbolic breakpoints) |
| JS breakpoints | No | Yes (line/symbolic) |
| JS variable inspection | No | Yes (`evaluateOnCallFrame`) |

### Changes (validation only)

| File | Change |
|------|--------|
| New: `tests/jsc_inspector_validation.rs` or standalone script | Validation test |
| `src/frida_collector/spawner.rs` | Add `--inspect-wait` to Bun spawn args (behind feature flag) |

### If Validation Fails

Fall back to improving the current JSC tracer: implement source-level instrumentation via `Bun.plugin()` + `onLoad` hook, injected via `--preload` at spawn time.

---

## Workstream D: New Test Adapters

### Deno Test Adapter

| Aspect | Details |
|--------|---------|
| Detection | `deno.json` or `deno.jsonc` in project root (confidence 90) |
| Suite command | `deno test --reporter=junit` |
| Single test | `deno test --filter="<name>"` |
| Output format | JUnit XML (same parser as Bun adapter) |
| Stack capture | SIGUSR1 + V8 inspector (deferred — OS-level first) |

### Go Test Adapter

| Aspect | Details |
|--------|---------|
| Detection | `go.mod` in project root (confidence 90) |
| Suite command | `go test -v -json ./...` |
| Single test | `go test -v -json -run "<name>" ./...` |
| Output format | JSON (one object per line, `Action` field: pass/fail/output) |
| Stack capture | `SIGABRT` → goroutine dump (deferred — OS-level first) |

### Mocha Adapter

| Aspect | Details |
|--------|---------|
| Detection | `.mocharc.*` or `mocha` in package.json (confidence 88) |
| Suite command | `npx mocha --reporter json` |
| Single test | `npx mocha --reporter json --grep "<name>"` |
| Output format | JSON (`stats`, `failures`, `passes` objects) |
| Stack capture | OS-level |

### Google Test Adapter

| Aspect | Details |
|--------|---------|
| Detection | `gtest` or `gmock` in CMakeLists.txt (confidence 85) |
| Suite command | `<binary> --gtest_output=json:/dev/stdout` |
| Single test | `<binary> --gtest_filter="<name>" --gtest_output=json:/dev/stdout` |
| Output format | JSON (`testsuites` array) |
| Stack capture | OS-level (native) |

### Changes

| File | Change |
|------|--------|
| New: `src/test/deno_adapter.rs` | ~200 lines, JUnit XML parsing (reuse Bun's parser) |
| New: `src/test/go_adapter.rs` | ~250 lines, streaming JSON parsing |
| New: `src/test/mocha_adapter.rs` | ~200 lines, JSON parsing |
| New: `src/test/gtest_adapter.rs` | ~250 lines, JSON parsing |
| `src/test/mod.rs` | Register new adapters in the adapter list |

### Validation

For each adapter:
1. Create a minimal test project (2-3 tests, one failing)
2. `debug_test({ projectRoot: "..." })` — adapter auto-detected
3. Structured output with failures, file:line, suggested traces
4. Single test rerun works

---

## Implementation Order

1. **Workstream A (Python fix)** — Highest impact, unblocks real Python tracing. ~1 day.
2. **Workstream D (Test adapters)** — Independent, parallelizable. ~1 day for all 4.
3. **Workstream C (JSC validation)** — Quick validation determines architecture. ~0.5 day.
4. **Workstream B (ESM support)** — Most complex, needs careful Node.js version handling. ~2 days.

Total estimate: ~4-5 days.

---

## What's NOT in Scope

- CDP (Chrome DevTools Protocol) for Node.js — V8Tracer via Frida covers this
- Collector trait abstraction — not needed until we have a non-Frida backend
- Python 3.14 `sys.remote_exec()` support — future when 3.14 ships
- JSC full implementation (if validation fails, that's a separate spec)
- Deno runtime tracer — detection + test adapter only for now
