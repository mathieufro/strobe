# Language Support Deepening — Implementation Plan (v2)

**Spec:** `docs/specs/2026-02-22-language-support-deepening.md`
**Goal:** Fix Python function tracing, add Node.js ESM support, validate JSC Inspector Protocol for Bun, and ship 4 new test adapters (Deno, Go, Mocha, Google Test).
**Architecture:** Version-aware Python tracing via `sys.monitoring` (3.12+) with `settrace_all_threads` fallback. Layered ESM support via spawn-time `--import` source transformation + runtime `module.registerHooks()`. JSC validation via WebKit Inspector Protocol with multi-hook attribution testing. Test adapters follow existing `TestAdapter` trait pattern with full progress tracking and stuck detection parity.
**Tech Stack:** Rust (adapters, daemon), TypeScript (Frida agent tracers), Python (test fixtures)
**Commit strategy:** Commit at logical checkpoints (after each workstream)

## Workstreams

- **Stream A (Python tracing fix):** Tasks 1-2 — serial
- **Stream B (Node.js ESM):** Tasks 3-6 — serial
- **Stream C (JSC validation):** Task 7 — independent
- **Stream D (Test adapters):** Tasks 8-12 — parallelizable within stream, serial after
- **Serial:** Task 13 (final integration e2e) depends on all streams

Streams A, C, D can run in parallel. Stream B is independent but most complex.

---

## Stream A: Fix Python Function Tracing

### Root Cause Analysis

`sys.settrace()` is **per-thread**. When called from Frida's agent thread via `PyRun_SimpleString`, `PyGILState_Ensure` creates a new `PyThreadState` for the Frida thread. The existing code already tries `threading.settrace_all_threads()` on 3.12+ (python-tracer.ts:281-284), which should iterate all thread states. However, the empirical failure mode needs verification — the fix proceeds with `sys.monitoring` (PEP 669) which is interpreter-global and definitively bypasses per-thread issues.

### Key Constraints (from review)

- `sys.monitoring` LINE callbacks receive `(code, line_number)` — **no frame object**. `inspect.currentframe().f_back` does NOT work. Use `sys._getframe(1)` for logpoint variable access.
- `sys.monitoring` and `sys.settrace` can **coexist**. Use monitoring for function enter/exit tracing, keep settrace for breakpoints/logpoints (which need frame objects).
- `removeAllHooks()` (line 424) also calls `sys.settrace(None)` — must also disable monitoring.
- `removeHook()` (line 404-417) does surgical list-edit via `_strobe_hooks` — works for both modes (callbacks read via LOAD_GLOBAL).
- Default `cpythonVersion` must be `{ major: 3, minor: 11 }` (safe fallback).
- `_strobe_bp_event` initialization must be included in monitoring setup block.
- `traceInstalled = true` must be set after monitoring setup succeeds.
- Existing `python_comprehensive.rs:152` already asserts `function_enter` events — use that as the validation target.
- Existing `fixture.py` with `slow-functions` mode is sufficient — no new fixture needed.

---

### Task 1: Python 3.12+ — sys.monitoring tracer

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts`
- Test: existing `tests/python_comprehensive.rs`

**Step 1: Add CPython version detection**

At class level in `PythonTracer`, add:

```typescript
// Default to 3.11 (safe fallback — settrace path). Only upgrade to monitoring
// when version is confirmed via Py_GetVersion.
private cpythonVersion: { major: number; minor: number } = { major: 3, minor: 11 };

// Class-level getter — NOT inside a method body (TypeScript syntax constraint).
private get useMonitoring(): boolean {
  return this.cpythonVersion.major > 3 ||
    (this.cpythonVersion.major === 3 && this.cpythonVersion.minor >= 12);
}
```

In `initialize()`, after finding Python exports, detect version:

```typescript
const PyGetVersion = findGlobalExport('Py_GetVersion');
if (PyGetVersion) {
  const fn = new NativeFunction(PyGetVersion, 'pointer', []);
  const versionStr = (fn() as NativePointer).readUtf8String() || '';
  const match = versionStr.match(/^(\d+)\.(\d+)/);
  if (match) {
    this.cpythonVersion = { major: parseInt(match[1]), minor: parseInt(match[2]) };
    send({ type: 'log', message: `PythonTracer: detected CPython ${match[1]}.${match[2]}` });
  }
}
```

**Step 2: Implement dual-mode tracing in syncTraceHooks()**

Replace the first-time installation code in `syncTraceHooks()` with version-branching. The key design: use `sys.monitoring` for PY_START (function enter) only, keep `sys.settrace` for LINE events (breakpoints and logpoints get frame objects). This avoids the `inspect.currentframe().f_back` problem in monitoring LINE callbacks.

**For 3.12+ (monitoring mode):**
```python
import sys, threading, ctypes, builtins as _b

# Trace callback: void(char*, char*, int, int) — same as settrace mode
_STROBE_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int, ctypes.c_int)
_strobe_cb = _STROBE_CB_TYPE(${callbackAddr})

# Breakpoint hit callback + suspension event (shared with settrace for BP/LP)
_STROBE_BP_HIT_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_int)
_strobe_bp_hit_cb = _STROBE_BP_HIT_CB_TYPE(${bpHitCallbackAddr})
_strobe_bp_event = getattr(_b, '_strobe_bp_event', None) or threading.Event()
setattr(_b, '_strobe_bp_event', _strobe_bp_event)
ctypes.pythonapi.PyEval_SaveThread.restype = ctypes.c_void_p
ctypes.pythonapi.PyEval_RestoreThread.argtypes = [ctypes.c_void_p]

# Logpoint callback
_STROBE_LOG_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p)
_strobe_log_cb = _STROBE_LOG_CB_TYPE(${logCallbackAddr})

# Data lists
${dataAssignments}

# --- sys.monitoring for function enter (interpreter-global, no thread issues) ---
try:
    sys.monitoring.use_tool_id(0, "strobe")
except ValueError:
    # Tool ID 0 already in use — fall back to settrace_all_threads
    pass
else:
    sys.monitoring.set_events(0, sys.monitoring.events.PY_START)

    def _strobe_on_start(code, offset):
        fname = code.co_filename
        fline = code.co_firstlineno
        for file_pat, line_pat, fid in _strobe_hooks:
            if fname.endswith(file_pat) and fline == line_pat:
                _strobe_cb(fname.encode('utf-8'), code.co_name.encode('utf-8'), fline, fid)
                return

    sys.monitoring.register_callback(0, sys.monitoring.events.PY_START, _strobe_on_start)
    setattr(_b, '_strobe_monitoring_active', True)

# --- sys.settrace for breakpoints/logpoints (receives frame object) ---
# Only install settrace if we have breakpoints or logpoints.
# For function tracing alone, sys.monitoring is sufficient.
if _strobe_breakpoints or _strobe_logpoints:
    def _strobe_trace(frame, event, arg):
        try:
            if event == 'line':
                fname = frame.f_code.co_filename
                fline = frame.f_lineno
                # Logpoints
                for lp_file, lp_line, lp_id, lp_msg in _strobe_logpoints:
                    if fname.endswith(lp_file) and fline == lp_line:
                        try:
                            msg = lp_msg.format(**{**frame.f_globals, **frame.f_locals})
                        except Exception as _e:
                            msg = f'{lp_msg} [fmt error: {_e}]'
                        _strobe_log_cb(lp_id.encode(), fline, msg.encode())
                        break
                # Breakpoints
                for bp_file, bp_line, bp_id, bp_cond, bp_hit_count in _strobe_breakpoints:
                    if fname.endswith(bp_file) and fline == bp_line:
                        if bp_hit_count > 0:
                            _strobe_bp_hits = getattr(_b, '_strobe_bp_hits', {})
                            _strobe_bp_hits[bp_id] = _strobe_bp_hits.get(bp_id, 0) + 1
                            setattr(_b, '_strobe_bp_hits', _strobe_bp_hits)
                            if _strobe_bp_hits[bp_id] < bp_hit_count:
                                break
                        if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
                            _strobe_bp_hit_cb(bp_id.encode(), fline)
                            _tstate = ctypes.pythonapi.PyEval_SaveThread()
                            _strobe_bp_event.wait()
                            ctypes.pythonapi.PyEval_RestoreThread(_tstate)
                            _strobe_bp_event.clear()
                        break
        except Exception as _strobe_err:
            import builtins as _be
            if not hasattr(_be, '_strobe_errors'):
                _be._strobe_errors = []
            _be._strobe_errors.append(f'{type(_strobe_err).__name__}: {_strobe_err}')
        return _strobe_trace

    if hasattr(threading, 'settrace_all_threads'):
        threading.settrace_all_threads(_strobe_trace)
    else:
        sys.settrace(_strobe_trace)
        threading.settrace(_strobe_trace)
```

**For 3.11 and below:** Keep existing `threading.settrace_all_threads()` / `sys.settrace()` approach unchanged. The code at lines 237-285 of python-tracer.ts already handles this correctly. No `_PyEval_SetTrace` NativeFunction — the existing `sys.settrace()` + `threading.settrace()` is the best we can do from the agent thread.

**After runPython succeeds, set `this.traceInstalled = true`** — this must be explicit in both the monitoring and settrace branches to prevent re-registration on every `installHook()`.

3. **Update data-only fast path:** When `traceInstalled` is true, `buildTraceDataAssignments()` updates the Python lists. Both `sys.monitoring` callbacks and `_strobe_trace` read these via LOAD_GLOBAL. No change needed for the fast path — it works for both modes.

**Step 3: Update dispose() and removeAllHooks()**

`dispose()` (line 130-134):
```typescript
dispose(): void {
  if (this.traceInstalled) {
    if (this.useMonitoring) {
      this.runPython(`
import sys, builtins as _b
if getattr(_b, '_strobe_monitoring_active', False):
    sys.monitoring.set_events(0, 0)
    sys.monitoring.free_tool_id(0)
    setattr(_b, '_strobe_monitoring_active', False)
sys.settrace(None)
`);
    } else {
      this.runPython('import sys; sys.settrace(None)');
    }
    this.traceInstalled = false;
  }
  // ... rest unchanged
}
```

`removeAllHooks()` (line 419-427):
```typescript
removeAllHooks(): void {
  this.hooks.clear();
  this.breakpoints.clear();
  this.logpoints.clear();
  if (this.traceInstalled) {
    if (this.useMonitoring) {
      this.runPython(`
import sys, builtins as _b
if getattr(_b, '_strobe_monitoring_active', False):
    sys.monitoring.set_events(0, 0)
    sys.monitoring.free_tool_id(0)
    setattr(_b, '_strobe_monitoring_active', False)
sys.settrace(None)
`);
    } else {
      this.runPython('import sys; sys.settrace(None)');
    }
    this.traceInstalled = false;
  }
}
```

**Note:** `removeHook()` (line 404-417) needs no change — its surgical `_strobe_hooks` list-edit works for both modes since monitoring callbacks read the list via LOAD_GLOBAL.

**Step 4: Run test — verify function tracing works**

Run: `cd agent && npm run build && cd .. && touch src/frida_collector/spawner.rs && cargo build`
Then: `debug_test({ projectRoot: "/Users/alex/strobe", test: "python_comprehensive" })`
Expected: PASS — `test_python_tracing()` at line 152 already asserts `function_enter` events via `poll_events_typed` with `EventType::FunctionEnter`.

**Checkpoint:** Python function tracing works on 3.12+ via sys.monitoring. Breakpoints/logpoints work via settrace. 3.11 fallback unchanged.

---

### Task 2: Python tracing e2e validation

**Files:**
- Modify: `tests/python_e2e.rs` — uncomment function tracing assertion
- No new fixtures (existing `fixture.py` with `slow-functions` mode is sufficient)

**Step 1: Harden python_e2e.rs**

The `python_e2e.rs` `scenario_python_tracing()` currently has the `function_enter` assertion commented out with a warning. Uncomment it and make it a hard assertion:

```rust
// In scenario_python_tracing():
// Replace the warning with a real assertion:
let fn_events = poll_events_typed(
    sm, &session_id, Duration::from_secs(10),
    strobe::db::EventType::FunctionEnter,
    |events| !events.is_empty(),
).await;
assert!(!fn_events.is_empty(), "Expected function_enter events for Python tracing");
```

**Step 2: Run tests**
Run: `debug_test({ projectRoot: "/Users/alex/strobe", test: "python" })`
Expected: All Python tests pass including function tracing assertion.

**Checkpoint:** Python function tracing validated e2e. Ready to commit Stream A.

---

## Stream B: Node.js ESM Module Support

### Architecture

**The core challenge:** ESM namespace bindings are **immutable** (non-configurable, non-writable). The V8Tracer's Proxy wrapping (`container[key] = wrapped`) silently fails for ESM exports. Source-level transformation at load time is the only option.

**Layered approach:**
1. **Layer 1 (existing):** `Module._compile` hook for CJS — keep as-is.
2. **Layer 2 (spawn-time `--import`):** Inject via `NODE_OPTIONS` env var (not command-line flag — works with npx, tsx, etc). The hook script uses `module.registerHooks()` (Node 22.15+/23.5+) or `module.register()` (Node 20.6+) to intercept ESM modules at load time.
3. **Layer 3 (source transformation):** The load hook performs AST-free regex-based function wrapping, injecting `globalThis.__strobe_trace()` calls. The bridge function on globalThis filters by active patterns.
4. **Layer 4 (runtime pattern updates):** Patterns are communicated via `globalThis.__strobe_hooks` (shared V8 heap between Frida agent and Node process). The hook script reads this dynamically.

### Key Constraints (from review)

- `--import` does NOT work with `npx` as a CLI flag — use `NODE_OPTIONS=--import=...` env var instead.
- `--import` requires a `file:///` URL on some Node versions, not a raw path.
- `module.registerHooks()` only intercepts **future** module loads — already-loaded ESM is unreachable.
- The hook script is generated at spawn time before patterns are known — must read patterns from `globalThis.__strobe_hooks` dynamically.
- `require('node:module')` from Frida's V8 script context runs on Frida's thread — whether it affects Node's module loader needs verification.
- Temp hook script files must be cleaned up on session stop.
- ESM fixture must have a delay (setInterval) to survive Frida attach timing.
- The `__strobe_trace` bridge matching must check BOTH `funcName` AND `file` (not OR).
- Test acceptance must verify `function_enter` events, not just stdout.

---

### Task 3: Node.js ESM — globalThis.__strobe_trace bridge + pattern sharing

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts`

**Step 1: Install __strobe_trace bridge and pattern sharing on globalThis**

In `V8Tracer.initialize()`, after patching `Module._compile`:

```typescript
// Install global trace bridge for ESM hooks.
// ESM hook scripts call globalThis.__strobe_trace() which dispatches to Strobe events.
try {
  const self = this;

  // Pattern sharing: hook script reads this to know which functions to instrument.
  // Updated by installHook() whenever patterns change.
  (globalThis as any).__strobe_hooks = [];

  (globalThis as any).__strobe_trace = function(
    event: string, funcName: string, file: string, line: number
  ) {
    // Match against active hooks — require BOTH name and file to match
    for (const [, hook] of self.hooks) {
      const nameMatch = hook.target.name === funcName;
      const fileMatch = hook.target.file && file.replace('file://', '').endsWith(hook.target.file);
      if (nameMatch && fileMatch) {
        if (event === 'enter') {
          self.emitEvent(hook.funcId, hook, file.replace('file://', ''), 'entry');
        } else if (event === 'exit') {
          self.emitEvent(hook.funcId, hook, file.replace('file://', ''), 'exit');
        }
        break;
      }
      // Fall back to name-only match if no file context
      if (nameMatch && !hook.target.file) {
        if (event === 'enter') {
          self.emitEvent(hook.funcId, hook, file.replace('file://', ''), 'entry');
        }
        break;
      }
    }
  };
} catch (e) {
  send({ type: 'log', message: `V8Tracer: failed to install __strobe_trace: ${e}` });
}
```

**Step 2: Update installHook() to sync patterns to globalThis**

After adding a hook to `this.hooks`, update the shared pattern list:

```typescript
// In installHook(), after this.hooks.set(funcId, hook):
try {
  (globalThis as any).__strobe_hooks = Array.from(this.hooks.values()).map(h => ({
    name: h.target.name,
    file: h.target.file || '',
    line: h.target.line || 0,
  }));
} catch {}
```

**Step 3: Run existing Node.js tests**
Run: `debug_test({ projectRoot: "/Users/alex/strobe", test: "frida_e2e" })`
Expected: Existing CJS tracing still works (no regression).

**Checkpoint:** Global trace bridge installed with pattern sharing. CJS still works.

---

### Task 4: Node.js ESM — spawn-time --import injection with source transformation

**Files:**
- Modify: `src/daemon/session_manager.rs` — inject `NODE_OPTIONS` for Node.js ESM sessions (correct injection site, NOT spawner.rs)

**Step 1: Write ESM hook script generator**

In `session_manager.rs`, add a function:

```rust
/// Generate the ESM hook registration script and return its file:// URL.
fn generate_esm_hook_script(session_id: &str) -> std::io::Result<String> {
    let script_path = format!("/tmp/strobe-esm-hooks-{}.mjs", session_id);
    let script_content = r#"
// Strobe ESM hook registration script — injected via NODE_OPTIONS=--import
// Intercepts ESM module loads and wraps exported functions with __strobe_trace calls.

import { createRequire } from 'node:module';

// Source transformation: wrap exported functions with tracing calls.
// Uses regex-based approach (no AST parser needed for function-level wrapping).
function transformSource(source, url) {
  const hooks = globalThis.__strobe_hooks || [];
  if (!hooks.length) {
    // No active patterns yet — instrument ALL exported functions.
    // The __strobe_trace bridge will filter by active patterns at call time.
  }

  // Match: export function name(...) or export async function name(...)
  // and wrap with __strobe_trace enter/exit calls.
  let transformed = source;
  const fnRegex = /^(\s*export\s+(?:default\s+)?(?:async\s+)?function\s+)(\w+)\s*\(([^)]*)\)\s*\{/gm;
  let match;
  while ((match = fnRegex.exec(source)) !== null) {
    const [full, prefix, name, params] = match;
    const replacement = `${prefix}${name}(${params}) {\n` +
      `  if (typeof globalThis.__strobe_trace === 'function') globalThis.__strobe_trace('enter', '${name}', '${url}', 0);\n` +
      `  try {`;
    // We also need to add the exit call — wrap the entire function body.
    // For simplicity, we only add enter tracing (exit requires closing brace matching).
    transformed = transformed.replace(full, `${prefix}${name}(${params}) {\n` +
      `  if (typeof globalThis.__strobe_trace === 'function') globalThis.__strobe_trace('enter', '${name}', '${url}', 0);`);
  }
  return transformed;
}

// Try registerHooks (Node 22.15+ / 23.5+) — synchronous, preferred
try {
  const mod = createRequire(import.meta.url)('node:module');
  if (typeof mod.registerHooks === 'function') {
    mod.registerHooks({
      load(url, context, nextLoad) {
        const result = nextLoad(url, context);
        // Only transform user code (not node_modules, not node: builtins)
        if (result.format === 'module' && result.source &&
            !url.includes('node_modules') && !url.startsWith('node:')) {
          const source = typeof result.source === 'string'
            ? result.source
            : new TextDecoder().decode(result.source);
          return { ...result, source: transformSource(source, url) };
        }
        return result;
      }
    });
  }
} catch (e) {
  // registerHooks not available — Node < 22.15
}
"#;
    std::fs::write(&script_path, script_content)?;
    // Return file:// URL (required by --import on some Node versions)
    Ok(format!("file://{}", script_path))
}
```

**Step 2: Inject via NODE_OPTIONS in spawn_with_frida()**

In `session_manager.rs` `spawn_with_frida()`, when language is JavaScript:

```rust
// For Node.js sessions, inject ESM hook script via NODE_OPTIONS.
// Using NODE_OPTIONS instead of --import CLI flag because:
// - Works with npx, tsx, ts-node (they inherit env vars)
// - Works with any launcher that eventually spawns node
if language == Language::JavaScript {
    let cmd_basename = Path::new(&command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    // Only inject for node-like commands (not bun, deno, python, etc.)
    let is_node = cmd_basename.contains("node") || cmd_basename == "npx"
        || cmd_basename == "tsx" || cmd_basename == "ts-node";
    if is_node {
        if let Ok(hook_url) = generate_esm_hook_script(session_id) {
            // Append to existing NODE_OPTIONS if present
            let existing = env.get("NODE_OPTIONS").cloned().unwrap_or_default();
            let new_opts = if existing.is_empty() {
                format!("--import={}", hook_url)
            } else {
                format!("{} --import={}", existing, hook_url)
            };
            env.insert("NODE_OPTIONS".to_string(), new_opts);
        }
    }
}
```

**Step 3: Add cleanup on session stop**

In `spawner.rs` `CoordinatorCommand::StopSession` handler, add:

```rust
// Clean up temp ESM hook script
let hook_path = format!("/tmp/strobe-esm-hooks-{}.mjs", session_id);
let _ = std::fs::remove_file(&hook_path);
```

**Step 4: Run test to verify no regression**
Run: `debug_test({ projectRoot: "/Users/alex/strobe", test: "frida_e2e" })`
Expected: PASS — CJS still works, NODE_OPTIONS injection doesn't break anything.

**Checkpoint:** Spawn-time ESM source transformation plumbed via NODE_OPTIONS.

---

### Task 5: Node.js ESM — runtime registerHooks() for dynamic import()

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts`

**Step 1: Add registerEsmHooks() method**

Call from `initialize()` (not `installHook()`) to catch modules loaded before first pattern:

```typescript
private esmHooksRegistered: boolean = false;

// Called from initialize(), not installHook().
private registerEsmHooks(): void {
  if (this.esmHooksRegistered) return;

  try {
    const mod = require('node:module');
    if (typeof mod.registerHooks === 'function') {
      mod.registerHooks({
        load(url: string, context: any, nextLoad: Function) {
          const result = nextLoad(url, context);
          // Only transform user ESM code
          if (result.format === 'module' && result.source &&
              !url.includes('node_modules') && !url.startsWith('node:')) {
            const source = typeof result.source === 'string'
              ? result.source
              : new TextDecoder().decode(result.source);
            // Apply same regex-based transformation as spawn-time hook
            const fnRegex = /^(\s*export\s+(?:default\s+)?(?:async\s+)?function\s+)(\w+)\s*\(([^)]*)\)\s*\{/gm;
            let transformed = source;
            let m;
            while ((m = fnRegex.exec(source)) !== null) {
              const [full, prefix, name, params] = m;
              transformed = transformed.replace(full,
                `${prefix}${name}(${params}) {\n` +
                `  if (typeof globalThis.__strobe_trace === 'function') ` +
                `globalThis.__strobe_trace('enter', '${name}', '${url}', 0);`);
            }
            return { ...result, source: transformed };
          }
          return result;
        }
      });
      this.esmHooksRegistered = true;
      send({ type: 'log', message: 'V8Tracer: ESM hooks registered via module.registerHooks()' });
    }
  } catch (e) {
    send({ type: 'log', message: `V8Tracer: ESM hook registration failed: ${e}` });
  }
}
```

In `initialize()`, after CJS patching and bridge installation:
```typescript
this.registerEsmHooks();
```

**Note:** `registerHooks()` only intercepts **future** `import()` calls. Static imports that have already been evaluated are unreachable — this is a known limitation. The spawn-time `--import` hook covers the static import case.

**Checkpoint:** Runtime ESM hook registration for dynamic `import()`.

---

### Task 6: Node.js ESM — e2e test with function_enter verification

**Files:**
- Create: `tests/fixtures/node_esm_target.mjs`
- Modify: `tests/frida_e2e.rs` or manual validation

**Step 1: Create ESM test fixture with delay**

```javascript
// tests/fixtures/node_esm_target.mjs
// ESM module for testing Strobe ESM tracing.
// Has a setInterval to keep alive for Frida attach + pattern install.

export function handleRequest(method, path) {
  console.log(`ESM: ${method} ${path}`);
  return { status: 200 };
}

export function processData(input) {
  return input.map(x => x * 2);
}

console.log("esm_target: starting");

// Call functions on a timer — gives Frida time to attach and install hooks
let count = 0;
const timer = setInterval(() => {
  handleRequest("GET", `/api/test/${count}`);
  processData([1, 2, 3]);
  count++;
  if (count >= 5) {
    clearInterval(timer);
    console.log("esm_target: done");
  }
}, 500);
```

**Step 2: Manual e2e validation — verify function_enter events**

```
1. debug_launch({ command: "node", args: ["tests/fixtures/node_esm_target.mjs"], projectRoot: "/Users/alex/strobe" })
2. debug_trace({ sessionId, add: ["handleRequest"] })
3. Wait 3 seconds
4. debug_query({ sessionId, eventType: "function_enter" })
   → MUST return at least one event with function name "handleRequest"
5. debug_query({ sessionId, eventType: "stdout" })
   → MUST contain "esm_target: starting"
```

If function_enter events are absent, ESM tracing is not working — investigate the source transformation output.

**Step 3: Verify CJS not regressed**

Use existing `tests/fixtures/node_trace_target.js`:
```
debug_launch + debug_trace + debug_query → function_enter events still appear for CJS
```

**Step 4: Build and commit Stream B**
Run: `cd agent && npm run build && cd .. && touch src/frida_collector/spawner.rs && cargo build`

**Checkpoint:** Node.js ESM support with source transformation. CJS unregressed. Ready to commit Stream B.

---

## Stream C: JSC/Bun Inspector Protocol Validation

### Key Constraints (from review)

- V8 runtime flag (`FRIDA_SCRIPT_RUNTIME_V8` in spawner.rs:118-121) is set for ALL `Language::JavaScript` sessions — Bun uses JSC, so this must be conditionally disabled for Bun.
- Success criteria must verify the action callback actually fired, not just `scriptParsed.length > 0`.
- Must test multi-hook attribution (the central problem, jsc-tracer.ts:85-103).
- Stderr URL parsing must accumulate chunks and use a proper async wait loop, not a hard 1s setTimeout.
- Regex must match UUIDs with hyphens: `/ws:\/\/[^\s]+/` not `/ws:\/\/[\w.:]+\/\w+/`.
- Must synchronize: wait for `addSymbolicBreakpoint` response before sending `Debugger.resume`.
- Validation is explicitly a **manual developer script**, not an automated test — actual integration requires a Rust WebSocket client, SessionManager wiring, and CDP-to-Event translation (separate future work).
- Bun port 0 support should be verified empirically.

---

### Task 7: JSC/Bun Inspector Protocol — validation script

**Files:**
- Create: `tests/fixtures/bun_inspector_target.ts`
- Create: `tests/fixtures/bun_inspector_validation.ts`

**Step 1: Create the target app with MULTIPLE named functions**

```typescript
// tests/fixtures/bun_inspector_target.ts
// Bun app with multiple named functions for multi-hook attribution testing.

function handleRequest(method: string, path: string) {
  console.log(`Request: ${method} ${path}`);
  return { status: 200, body: "ok" };
}

function processData(input: number[]): number[] {
  return input.map(x => x * 2);
}

function computeHash(data: string): number {
  let hash = 0;
  for (let i = 0; i < data.length; i++) {
    hash = ((hash << 5) - hash) + data.charCodeAt(i);
    hash |= 0;
  }
  return hash;
}

console.log("bun_inspector_target: starting");
setTimeout(() => {
  handleRequest("GET", "/api/test");
  processData([1, 2, 3]);
  computeHash("hello world");
  console.log("bun_inspector_target: done");
}, 1000);

// Keep alive for 10s to give validation script ample time
setTimeout(() => process.exit(0), 10000);
```

**Step 2: Create the validation script**

```typescript
// tests/fixtures/bun_inspector_validation.ts
// MANUAL validation of WebKit Inspector Protocol for Bun tracing.
// Run: bun run tests/fixtures/bun_inspector_validation.ts
//
// This is a manual developer script, NOT an automated test.
// If validation passes, a separate integration task is needed to wire
// Inspector Protocol into SessionManager/FridaSpawner.

import { spawn } from "child_process";

async function validate() {
  // 1. Spawn bun --inspect-wait with dynamic port
  const proc = spawn("bun", [
    "--inspect-wait=127.0.0.1:0",
    "tests/fixtures/bun_inspector_target.ts"
  ]);

  // 2. Parse inspector URL from stderr — accumulate chunks, async wait with timeout
  let stderrBuf = "";
  let wsUrl = "";

  proc.stderr?.on("data", (data) => {
    stderrBuf += data.toString();
    // Match full URL including hyphens in UUID
    const match = stderrBuf.match(/ws:\/\/[^\s]+/);
    if (match) wsUrl = match[0];
  });

  // Wait up to 10s for URL (not a hard 1s timeout)
  const deadline = Date.now() + 10_000;
  while (!wsUrl && Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 100));
  }

  if (!wsUrl) {
    console.error("FAIL: No WebSocket URL found in stderr after 10s");
    console.error("stderr:", stderrBuf);
    proc.kill();
    process.exit(1);
  }

  console.log(`Connecting to: ${wsUrl}`);

  // 3. Connect via WebSocket
  const ws = new WebSocket(wsUrl);
  let msgId = 1;
  const wsSend = (method: string, params?: any) => {
    ws.send(JSON.stringify({ id: msgId++, method, params }));
  };

  const responses: any[] = [];
  const results: Map<number, any> = new Map();

  ws.onmessage = (event) => {
    const data = JSON.parse(event.data as string);
    responses.push(data);
    if (data.id) results.set(data.id, data);
    if (data.method) {
      console.log(`  Event: ${data.method}`, JSON.stringify(data.params || {}).slice(0, 150));
    }
  };

  await new Promise<void>((resolve, reject) => {
    ws.addEventListener('open', () => resolve());
    ws.addEventListener('error', (e) => reject(e));
  });

  // 4. Enable debugger
  wsSend("Debugger.enable");
  await new Promise(r => setTimeout(r, 500));

  // 5. Test MULTI-HOOK attribution: add symbolic breakpoints for TWO functions
  const bp1Id = msgId;
  wsSend("Debugger.addSymbolicBreakpoint", {
    symbol: "handleRequest",
    caseSensitive: true,
    isRegex: false,
    options: {
      autoContinue: true,
      actions: [{
        type: "evaluate",
        data: "globalThis.__strobe_bp1_fired = true; console.log('[strobe-trace] handleRequest called')",
        emulateUserGesture: false,
      }]
    }
  });

  const bp2Id = msgId;
  wsSend("Debugger.addSymbolicBreakpoint", {
    symbol: "computeHash",
    caseSensitive: true,
    isRegex: false,
    options: {
      autoContinue: true,
      actions: [{
        type: "evaluate",
        data: "globalThis.__strobe_bp2_fired = true; console.log('[strobe-trace] computeHash called')",
        emulateUserGesture: false,
      }]
    }
  });

  // 6. Wait for BOTH breakpoint responses before resuming
  const bpDeadline = Date.now() + 5_000;
  while ((!results.has(bp1Id) || !results.has(bp2Id)) && Date.now() < bpDeadline) {
    await new Promise(r => setTimeout(r, 50));
  }

  if (!results.has(bp1Id) || !results.has(bp2Id)) {
    console.error("FAIL: addSymbolicBreakpoint responses not received");
    ws.close(); proc.kill(); process.exit(1);
  }

  console.log(`BP1 response:`, JSON.stringify(results.get(bp1Id)));
  console.log(`BP2 response:`, JSON.stringify(results.get(bp2Id)));

  // 7. Resume execution (target setTimeout fires after 1s)
  wsSend("Debugger.resume");

  // 8. Wait for target to complete (target does work at +1s, exits at +10s)
  await new Promise(r => setTimeout(r, 4000));

  // 9. Analyze results
  const scriptParsed = responses.filter(r => r.method === "Debugger.scriptParsed");
  const paused = responses.filter(r => r.method === "Debugger.paused");
  const consoleMessages = responses.filter(r =>
    r.method === "Console.messageAdded" || r.method === "Runtime.consoleAPICalled"
  );

  // Check if action callbacks fired by looking for our marker console logs
  const allText = responses.map(r => JSON.stringify(r)).join('\n');
  const bp1Fired = allText.includes('handleRequest called');
  const bp2Fired = allText.includes('computeHash called');

  console.log(`\n=== Results ===`);
  console.log(`Scripts parsed: ${scriptParsed.length}`);
  console.log(`Paused events: ${paused.length}`);
  console.log(`Console messages: ${consoleMessages.length}`);
  console.log(`handleRequest breakpoint action fired: ${bp1Fired}`);
  console.log(`computeHash breakpoint action fired: ${bp2Fired}`);
  console.log(`autoContinue working: ${paused.length === 0 && (bp1Fired || bp2Fired) ? "YES" : "UNCLEAR"}`);

  ws.close();
  proc.kill();

  // Success criteria:
  // 1. BOTH breakpoint actions must fire (proves multi-hook attribution works)
  // 2. No Debugger.paused events (proves autoContinue works)
  // 3. Function names identifiable from the fired actions
  const pass = bp1Fired && bp2Fired && paused.length === 0;

  if (pass) {
    console.log("\nVALIDATION: PASS — Inspector Protocol works with multi-hook attribution");
    console.log("NEXT STEPS: Integration requires tokio-tungstenite WebSocket client,");
    console.log("  SessionManager inspector connection field, CDP-to-Event translation,");
    console.log("  and debug_trace dispatch to inspector channel.");
  } else if (bp1Fired && !bp2Fired) {
    console.log("\nVALIDATION: PARTIAL — Single hook works but multi-hook attribution fails");
    console.log("  The Inspector Protocol may not support multiple symbolic breakpoints.");
  } else {
    console.log("\nVALIDATION: FAIL — Inspector Protocol not working for tracing");
    console.log("  Fallback: Bun.plugin() + onLoad source transformation via --preload");
    console.log("  (requires AST transformation library, separate multi-week effort)");
    process.exit(1);
  }
}

validate().catch(e => {
  console.error("Validation error:", e);
  process.exit(1);
});
```

**Step 3: Fix V8 runtime flag for Bun sessions**

In `spawner.rs`, the V8 runtime is set for ALL JavaScript sessions (line 118-121). Bun uses JSC, not V8. Add a check:

```rust
// In create_script_raw or equivalent:
// Only use V8 runtime for Node.js sessions, not Bun (which uses JSC/QuickJS).
if language == Language::JavaScript && !command_contains_bun {
    frida_sys::frida_script_options_set_runtime(
        opt,
        frida_sys::FridaScriptRuntime_FRIDA_SCRIPT_RUNTIME_V8,
    );
}
```

The `command_contains_bun` check mirrors the existing `detect_language` basename check in `session_manager.rs`.

**Step 4: Run validation**
Run: `bun run tests/fixtures/bun_inspector_validation.ts`

**Step 5: Document results**
Record: (a) whether multi-hook attribution works, (b) overhead, (c) autoContinue behavior.

**Checkpoint:** JSC Inspector Protocol validated (or fallback documented). V8 runtime flag fixed for Bun.

---

## Stream D: 4 New Test Adapters

### Infrastructure Changes Required (from review)

Before implementing individual adapters, these cross-cutting issues must be addressed:

1. **`parse_junit_xml` visibility:** Change `fn parse_junit_xml` to `pub(crate) fn parse_junit_xml` in `src/test/bun_adapter.rs:89`.

2. **`progress_fn` dispatch in `mod.rs:281-285`:** Currently only matches `"cargo"` and `"catch2"`. Must be updated for all new adapters. Each new adapter needs an `update_progress` function that populates `running_tests` in `TestProgress` — this is what the `StuckDetector` reads via `current_test()`.

3. **Command dispatch in `mod.rs:214-225`:** Currently hardcoded to `Catch2Adapter::command_for_binary()` when `command` is provided. Must be generalized to a trait method `command_for_binary(&self, cmd: &str, level: Option<TestLevel>) -> TestCommand` so GTest gets `--gtest_output=json` instead of Catch2's `--reporter xml`.

4. **`server.rs:845` MCP schema:** Framework enum hardcoded to `["cargo", "catch2"]`. Must include all new frameworks.

5. **`detect_adapter` error messages:** Both `mod.rs:163-165` (unknown framework) and `mod.rs:185-189` (no framework detected) must list all supported frameworks.

6. **Existing test updates:** `test_adapter_detection_invalid_framework` (mod.rs:555) asserts on `"'cargo', 'catch2'"` — must be updated.

---

### Task 8: Infrastructure — progress_fn, command dispatch, MCP schema

**Files:**
- Modify: `src/test/bun_adapter.rs` — make `parse_junit_xml` pub(crate)
- Modify: `src/test/adapter.rs` — add `command_for_binary` trait method
- Modify: `src/test/mod.rs` — generalize command dispatch, update progress_fn, error messages
- Modify: `src/daemon/server.rs` — update MCP schema framework enum

**Step 1: Make parse_junit_xml pub(crate)**

In `src/test/bun_adapter.rs`, line 89:
```rust
// Change: fn parse_junit_xml(xml: &str) -> TestResult {
// To:
pub(crate) fn parse_junit_xml(xml: &str) -> TestResult {
```

**Step 2: Add command_for_binary to TestAdapter trait**

In `src/test/adapter.rs`, add default method:
```rust
/// Build command for a user-provided binary path. Default: error.
/// Override for binary-based adapters (Catch2, GTest).
fn command_for_binary(
    &self,
    _cmd: &str,
    _level: Option<TestLevel>,
) -> crate::Result<TestCommand> {
    Err(crate::Error::ValidationError(
        format!("{} does not support direct binary execution", self.name())
    ))
}

/// Build command for running a single test on a user-provided binary.
fn single_test_for_binary(
    &self,
    _cmd: &str,
    _test_name: &str,
) -> crate::Result<TestCommand> {
    Err(crate::Error::ValidationError(
        format!("{} does not support direct binary execution", self.name())
    ))
}
```

Move `Catch2Adapter::command_for_binary` and `single_test_for_binary` to be trait impl overrides.

**Step 3: Generalize command dispatch in mod.rs**

Replace lines 214-225:
```rust
let test_cmd = if let Some(cmd) = command {
    if let Some(test_name) = test {
        adapter.single_test_for_binary(cmd, test_name)?
    } else {
        adapter.command_for_binary(cmd, level)?
    }
} else if let Some(test_name) = test {
    adapter.single_test_command(project_root, test_name)?
} else {
    adapter.suite_command(project_root, level, env)?
};
```

**Step 4: Update progress_fn dispatch**

Replace lines 281-285:
```rust
let progress_fn: Option<fn(&str, &Arc<Mutex<TestProgress>>)> = match framework_name.as_str() {
    "cargo" => Some(cargo_adapter::update_progress),
    "catch2" => Some(catch2_adapter::update_progress),
    "deno" => Some(deno_adapter::update_progress),
    "go" => Some(go_adapter::update_progress),
    "mocha" => Some(mocha_adapter::update_progress),
    "gtest" => Some(gtest_adapter::update_progress),
    _ => None,
};
```

Each adapter module must export an `update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>)` function that:
- Transitions phase from `Compiling` to `Running` on first test output
- Populates `running_tests` with the current test name + start time

**Step 5: Update error messages and MCP schema**

`mod.rs:163-165`:
```rust
format!("Unknown framework '{}'. Supported: 'cargo', 'catch2', 'pytest', 'unittest', 'vitest', 'jest', 'bun', 'deno', 'go', 'mocha', 'gtest'", name)
```

`mod.rs:185-189` — add entries for Deno, Go, Mocha, Google Test.

`server.rs:845`:
```rust
"framework": { "type": "string", "enum": ["cargo", "catch2", "pytest", "unittest", "vitest", "jest", "bun", "deno", "go", "mocha", "gtest"], ... }
```

Update `server.rs:838` description to list all supported frameworks.

**Step 6: Update existing tests**

`test_adapter_detection_invalid_framework` (mod.rs:555):
```rust
assert!(err.contains("'cargo', 'catch2'") || err.contains("Supported:"));
// Or more precisely: verify the error contains the new framework names
```

**Checkpoint:** Infrastructure ready for new adapters. Command dispatch generalized. Progress tracking wired.

---

### Task 9: Deno Test Adapter

**Files:**
- Create: `src/test/deno_adapter.rs`
- Modify: `src/test/mod.rs` — register adapter

**Implementation notes addressing review findings:**

- **JUnit XML preamble stripping (Finding 4):** Deno may mix human-readable output with JUnit XML on stdout. Strip non-XML preamble by finding `<?xml` or `<testsuites` before passing to `parse_junit_xml`.
- **Set `rerun` field (Finding 15):** `rerun: Some(name.clone())` in failures.
- **`update_progress` function:** Parse JUnit XML progressively or use stderr output to track currently running test.

```rust
// src/test/deno_adapter.rs
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use super::adapter::*;
use super::TestProgress;

pub struct DenoAdapter;

/// Parse Deno stderr/stdout for progress updates.
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    // Deno outputs test names to stderr: "running 3 tests from ./math_test.ts"
    // and per-test: "test adds ... ok (5ms)"
    let mut p = progress.lock().unwrap();
    if line.contains("running") && line.contains("test") {
        p.phase = super::TestPhase::Running;
    }
    // Track currently running test from "test <name> ..." lines
    if line.starts_with("test ") {
        if let Some(name) = line.strip_prefix("test ").and_then(|s| s.split(" ...").next()) {
            p.start_test(name.trim().to_string());
        }
    }
    if line.contains("... ok") || line.contains("... FAILED") {
        // Test completed — remove from running
        if let Some(name) = line.strip_prefix("test ").and_then(|s| s.split(" ...").next()) {
            p.finish_test(name.trim());
        }
    }
}

impl TestAdapter for DenoAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("deno.json").exists() || project_root.join("deno.jsonc").exists() {
            return 90;
        }
        if project_root.join("deno.lock").exists() {
            return 85;
        }
        0
    }

    fn name(&self) -> &str { "deno" }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "deno".to_string(),
            args: vec!["test".to_string(), "--reporter=junit".to_string()],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "deno".to_string(),
            args: vec![
                "test".to_string(),
                "--reporter=junit".to_string(),
                format!("--filter={}", test_name),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        // Strip non-XML preamble — Deno may mix human output with JUnit XML
        let xml_start = stdout.find("<?xml")
            .or_else(|| stdout.find("<testsuites"))
            .unwrap_or(0);
        let xml = &stdout[xml_start..];

        if xml.contains("<testsuites") || xml.contains("<?xml") {
            let mut result = super::bun_adapter::parse_junit_xml(xml);
            // Set rerun field for failures
            for failure in &mut result.failures {
                failure.rerun = Some(failure.name.clone());
            }
            return result;
        }

        // Fallback: try stderr (some Deno versions output JUnit there)
        let stderr_xml_start = stderr.find("<?xml")
            .or_else(|| stderr.find("<testsuites"))
            .unwrap_or(0);
        if stderr[stderr_xml_start..].contains("<testsuites") {
            let mut result = super::bun_adapter::parse_junit_xml(&stderr[stderr_xml_start..]);
            for failure in &mut result.failures {
                failure.rerun = Some(failure.name.clone());
            }
            return result;
        }

        // Final fallback
        let failures = if exit_code != 0 {
            vec![TestFailure {
                name: "Test run failed".to_string(),
                file: None, line: None,
                message: format!("Could not parse Deno test output.\nstderr: {}",
                    stderr.chars().take(500).collect::<String>()),
                rerun: None, suggested_traces: vec![],
            }]
        } else { vec![] };

        TestResult {
            summary: TestSummary { passed: 0, failed: if exit_code != 0 { 1 } else { 0 },
                skipped: 0, stuck: None, duration_ms: 0 },
            failures, stuck: vec![], all_tests: vec![],
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file).file_stem()
                .and_then(|s| s.to_str()).unwrap_or("test");
            let module = stem.trim_end_matches("_test").trim_end_matches(".test");
            traces.push(format!("@file:{}", stem));
            traces.push(format!("{}.*", module));
        }
        traces
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 60_000,
            Some(TestLevel::Integration) => 180_000,
            Some(TestLevel::E2e) => 300_000,
            None => 120_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNIT_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="deno test" tests="2" failures="0" time="0.050">
    <testcase name="adds two numbers" classname="math_test.ts" time="0.005"/>
    <testcase name="subs two numbers" classname="math_test.ts" time="0.003"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="deno test" tests="2" failures="1" time="0.060">
    <testcase name="multiplies" classname="math_test.ts" time="0.008">
      <failure message="AssertionError: Values are not equal" type="AssertionError">
AssertionError: Values are not equal
    at math_test.ts:12:7
      </failure>
    </testcase>
    <testcase name="adds" classname="math_test.ts" time="0.005"/>
  </testsuite>
</testsuites>"#;

    // Test with human-readable preamble before XML (Finding 4)
    const JUNIT_WITH_PREAMBLE: &str = "running 2 tests from ./math_test.ts\ntest adds ... ok (5ms)\n<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites><testsuite name=\"deno test\" tests=\"1\" failures=\"0\" time=\"0.005\"><testcase name=\"adds\" classname=\"math_test.ts\" time=\"0.005\"/></testsuite></testsuites>";

    #[test]
    fn test_detect_deno() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DenoAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);
        std::fs::write(dir.path().join("deno.json"), "{}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_detect_deno_jsonc() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = DenoAdapter;
        std::fs::write(dir.path().join("deno.jsonc"), "{}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_parse_passing() {
        let result = DenoAdapter.parse_output(JUNIT_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing() {
        let result = DenoAdapter.parse_output(JUNIT_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert!(result.failures[0].message.contains("Values are not equal"));
        assert!(result.failures[0].rerun.is_some(), "rerun should be set");
    }

    #[test]
    fn test_parse_with_preamble() {
        let result = DenoAdapter.parse_output(JUNIT_WITH_PREAMBLE, "", 0);
        assert_eq!(result.summary.passed, 1);
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = DenoAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert_eq!(cmd.program, "deno");
        assert!(cmd.args.iter().any(|a| a.contains("junit")));
    }
}
```

**Checkpoint:** Deno adapter complete with unit tests, progress tracking, rerun hints, and preamble stripping.

---

### Task 10: Go Test Adapter

**Files:**
- Create: `src/test/go_adapter.rs`
- Modify: `src/test/mod.rs` — register adapter

**Implementation notes addressing review findings:**

- **`build-fail` handling (Finding 6):** Handle `Action: "build-fail"` events and report as compilation failure.
- **UTF-8 safe stderr truncation (Finding 6):** Use `chars().take(500)` instead of byte slicing.
- **Regex escaping for `-run` (Finding 10):** Anchor test names with `^TestName$`.
- **Location regex (Finding 14):** Broaden to match any `.go` file, not just `_test.go`.
- **Set `rerun` field (Finding 15):** `rerun: Some(name.clone())`.
- **Sub-test handling (Finding 13):** Document that sub-tests are counted individually.
- **`update_progress` function (Finding 2):** Parse `"run"` and `"pass"/"fail"` events for stuck detection.

```rust
// src/test/go_adapter.rs
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use serde::Deserialize;
use super::adapter::*;
use super::TestProgress;

pub struct GoAdapter;

/// Parse go test -json streaming output for progress updates.
pub fn update_progress(line: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();
    if let Ok(event) = serde_json::from_str::<GoTestEvent>(line) {
        if let Some(test_name) = &event.test {
            match event.action.as_str() {
                "run" => {
                    p.phase = super::TestPhase::Running;
                    p.start_test(test_name.clone());
                }
                "pass" | "fail" | "skip" => {
                    p.finish_test(test_name);
                }
                _ => {}
            }
        }
    }
}

#[derive(Deserialize)]
struct GoTestEvent {
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Package", default)]
    package: String,
    #[serde(rename = "Test", default)]
    test: Option<String>,
    #[serde(rename = "Output", default)]
    output: Option<String>,
    #[serde(rename = "Elapsed", default)]
    elapsed: Option<f64>,
}

impl TestAdapter for GoAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("go.mod").exists() { return 90; }
        if project_root.join("go.sum").exists() { return 80; }
        0
    }

    fn name(&self) -> &str { "go" }

    fn suite_command(
        &self,
        _project_root: &Path,
        _level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "go".to_string(),
            args: vec!["test".to_string(), "-v".to_string(), "-json".to_string(), "./...".to_string()],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        // Anchor with ^...$ to avoid regex metacharacter issues (Finding 10)
        let escaped = format!("^{}$", regex::escape(test_name));
        Ok(TestCommand {
            program: "go".to_string(),
            args: vec![
                "test".to_string(), "-v".to_string(), "-json".to_string(),
                "-run".to_string(), escaped,
                "./...".to_string(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        let mut passed = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut failures: Vec<TestFailure> = vec![];
        let mut all_tests: Vec<TestDetail> = vec![];
        let mut test_output: HashMap<String, Vec<String>> = HashMap::new();
        let mut build_failed = false;

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let event: GoTestEvent = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(_) => continue,
            };

            // Handle build-fail events (Finding 6)
            if event.action == "build-fail" {
                build_failed = true;
                continue;
            }

            let test_name = match &event.test {
                Some(t) => t.clone(),
                None => continue, // Package-level event
            };

            match event.action.as_str() {
                "output" => {
                    if let Some(output) = &event.output {
                        test_output.entry(test_name).or_default().push(output.clone());
                    }
                }
                "pass" => {
                    passed += 1;
                    let dur_ms = event.elapsed.map(|e| (e * 1000.0) as u64).unwrap_or(0);
                    all_tests.push(TestDetail {
                        name: test_name, status: TestStatus::Pass,
                        duration_ms: dur_ms, stdout: None, stderr: None, message: None,
                    });
                }
                "fail" => {
                    failed += 1;
                    let dur_ms = event.elapsed.map(|e| (e * 1000.0) as u64).unwrap_or(0);
                    let output_lines = test_output.get(&test_name).cloned().unwrap_or_default();
                    let message = output_lines.join("").trim().to_string();
                    let (file, line) = extract_go_location(&message);

                    failures.push(TestFailure {
                        name: test_name.clone(), file, line,
                        message: message.clone(),
                        rerun: Some(test_name.clone()), // Finding 15
                        suggested_traces: vec![],
                    });
                    all_tests.push(TestDetail {
                        name: test_name, status: TestStatus::Fail,
                        duration_ms: dur_ms, stdout: None, stderr: None,
                        message: Some(message),
                    });
                }
                "skip" => {
                    skipped += 1;
                    all_tests.push(TestDetail {
                        name: test_name, status: TestStatus::Skip,
                        duration_ms: 0, stdout: None, stderr: None, message: None,
                    });
                }
                _ => {}
            }
        }

        // Fallback for build failures or unparseable output (Finding 6)
        if all_tests.is_empty() && (exit_code != 0 || build_failed) {
            let err_msg = if build_failed {
                "Compilation failed — 0 tests ran."
            } else {
                "Test run failed."
            };
            // UTF-8 safe truncation (Finding 6)
            let stderr_preview: String = stderr.chars().take(500).collect();
            failures.push(TestFailure {
                name: err_msg.to_string(),
                file: None, line: None,
                message: format!("{}\nstderr: {}", err_msg, stderr_preview),
                rerun: None, suggested_traces: vec![],
            });
            failed = 1;
        }

        let total_duration: u64 = all_tests.iter().map(|t| t.duration_ms).sum();

        TestResult {
            summary: TestSummary { passed, failed, skipped, stuck: None, duration_ms: total_duration },
            failures, stuck: vec![], all_tests,
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file).file_stem()
                .and_then(|s| s.to_str()).unwrap_or("test");
            let module = stem.trim_end_matches("_test");
            traces.push(format!("@file:{}", stem));
            traces.push(format!("{}.*", module));
        }
        traces
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 120_000,
            Some(TestLevel::Integration) => 300_000,
            Some(TestLevel::E2e) => 600_000,
            None => 300_000,
        }
    }
}

fn extract_go_location(output: &str) -> (Option<String>, Option<u32>) {
    // Match any .go file, not just _test.go (Finding 14)
    // Also handle paths with / and - characters
    let re = regex::Regex::new(r"([\w/.:-]+\.go):(\d+):").unwrap();
    re.captures(output)
        .map(|c| (Some(c[1].to_string()), c[2].parse().ok()))
        .unwrap_or((None, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GO_PASS: &str = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"example/calc","Test":"TestAdd"}
{"Time":"2024-01-01T00:00:01Z","Action":"output","Package":"example/calc","Test":"TestAdd","Output":"=== RUN   TestAdd\n"}
{"Time":"2024-01-01T00:00:01Z","Action":"output","Package":"example/calc","Test":"TestAdd","Output":"--- PASS: TestAdd (0.00s)\n"}
{"Time":"2024-01-01T00:00:01Z","Action":"pass","Package":"example/calc","Test":"TestAdd","Elapsed":0.001}
{"Time":"2024-01-01T00:00:01Z","Action":"run","Package":"example/calc","Test":"TestSub"}
{"Time":"2024-01-01T00:00:01Z","Action":"pass","Package":"example/calc","Test":"TestSub","Elapsed":0.001}
{"Time":"2024-01-01T00:00:01Z","Action":"pass","Package":"example/calc","Elapsed":0.05}"#;

    const GO_FAIL: &str = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"example/calc","Test":"TestMul"}
{"Time":"2024-01-01T00:00:01Z","Action":"output","Package":"example/calc","Test":"TestMul","Output":"    calc_test.go:15: expected 6, got 5\n"}
{"Time":"2024-01-01T00:00:01Z","Action":"fail","Package":"example/calc","Test":"TestMul","Elapsed":0.002}
{"Time":"2024-01-01T00:00:01Z","Action":"run","Package":"example/calc","Test":"TestAdd"}
{"Time":"2024-01-01T00:00:01Z","Action":"pass","Package":"example/calc","Test":"TestAdd","Elapsed":0.001}
{"Time":"2024-01-01T00:00:01Z","Action":"fail","Package":"example/calc","Elapsed":0.05}"#;

    const GO_SKIP: &str = r#"{"Time":"2024-01-01T00:00:00Z","Action":"run","Package":"example/calc","Test":"TestSkipped"}
{"Time":"2024-01-01T00:00:01Z","Action":"output","Package":"example/calc","Test":"TestSkipped","Output":"    calc_test.go:20: skipping in short mode\n"}
{"Time":"2024-01-01T00:00:01Z","Action":"skip","Package":"example/calc","Test":"TestSkipped","Elapsed":0.0}
{"Time":"2024-01-01T00:00:01Z","Action":"pass","Package":"example/calc","Elapsed":0.01}"#;

    const GO_BUILD_FAIL: &str = r#"{"Action":"build-fail","Package":"example/calc"}"#;

    #[test]
    fn test_detect_go() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(GoAdapter.detect(dir.path(), None), 0);
        std::fs::write(dir.path().join("go.mod"), "module example/calc\n\ngo 1.21").unwrap();
        assert!(GoAdapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_parse_passing() {
        let result = GoAdapter.parse_output(GO_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
    }

    #[test]
    fn test_parse_failing() {
        let result = GoAdapter.parse_output(GO_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.passed, 1);
        assert!(result.failures[0].name.contains("TestMul"));
        assert!(result.failures[0].message.contains("expected 6"));
        assert!(result.failures[0].rerun.is_some());
        assert!(result.failures[0].file.as_deref().unwrap_or("").contains("calc_test.go"));
    }

    #[test]
    fn test_parse_skipped() {
        let result = GoAdapter.parse_output(GO_SKIP, "", 0);
        assert_eq!(result.summary.skipped, 1);
    }

    #[test]
    fn test_parse_build_fail() {
        let result = GoAdapter.parse_output(GO_BUILD_FAIL, "error: cannot find module", 1);
        assert_eq!(result.summary.failed, 1);
        assert!(result.failures[0].message.contains("Compilation failed"));
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = GoAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert_eq!(cmd.program, "go");
        assert!(cmd.args.iter().any(|a| a == "-json"));
    }

    #[test]
    fn test_single_test_escapes_regex() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = GoAdapter.single_test_command(dir.path(), "TestFoo.Bar").unwrap();
        // Should be anchored and escaped
        assert!(cmd.args.iter().any(|a| a.contains("^TestFoo\\.Bar$")));
    }
}
```

**Checkpoint:** Go adapter complete with build-fail handling, UTF-8 safe truncation, regex escaping, progress tracking.

---

### Task 11: Mocha Adapter

**Files:**
- Create: `src/test/mocha_adapter.rs`
- Modify: `src/test/mod.rs` — register adapter

**Implementation notes addressing review findings:**

- **JSON extraction (Finding 5):** Find the outermost `{` at the start of a line, or find the last `{...}` blob. Properly implement the fallback (not `...`).
- **Set `rerun` field (Finding 15).**
- **`update_progress` function (Finding 2):** Parse stderr for test progress.

```rust
// src/test/mocha_adapter.rs
// [Full implementation with:]
// - Proper JSON extraction: find last complete JSON object via brace-counting
// - Fallback that actually compiles (not `...`)
// - rerun: Some(name.clone()) for all failures
// - update_progress function for stuck detection
// - Same unit test structure as existing Jest/Vitest adapters
```

Key difference from original plan — the fallback:
```rust
Err(_) => {
    let failures = if exit_code != 0 {
        vec![TestFailure {
            name: "Test run failed".to_string(),
            file: None, line: None,
            message: format!("Could not parse Mocha JSON output.\nstderr: {}",
                stderr.chars().take(500).collect::<String>()),
            rerun: None, suggested_traces: vec![],
        }]
    } else { vec![] };
    return TestResult {
        summary: TestSummary { passed: 0, failed: if exit_code != 0 { 1 } else { 0 },
            skipped: 0, stuck: None, duration_ms: 0 },
        failures, stuck: vec![], all_tests: vec![],
    };
}
```

**Checkpoint:** Mocha adapter complete with robust JSON extraction and proper fallback.

---

### Task 12: Google Test Adapter

**Files:**
- Create: `src/test/gtest_adapter.rs`
- Modify: `src/test/mod.rs` — register adapter

**Implementation notes addressing review findings:**

- **detect() (Finding 8):** Don't return 85 for any non-empty command. Check that the binary path exists AND probe with `--gtest_list_tests` to confirm it's a GTest binary.
- **command_for_binary (Finding 3):** Override the trait method to use `--gtest_output=json` instead of Catch2's `--reporter xml`.
- **GTEST_SKIP() handling (Finding 11):** Check `test.result == "SKIPPED"` to correctly classify skipped tests.
- **Set `rerun` field (Finding 15).**
- **Text fallback: read stdout only (Finding 18).**
- **`update_progress` function (Finding 2).**

```rust
impl TestAdapter for GtestAdapter {
    fn detect(&self, project_root: &Path, command: Option<&str>) -> u8 {
        // Only return high confidence if we can confirm it's a GTest binary
        if let Some(cmd) = command {
            let path = Path::new(cmd);
            if path.exists() {
                // Probe: run with --gtest_list_tests to check
                if let Ok(output) = std::process::Command::new(cmd)
                    .arg("--gtest_list_tests")
                    .output()
                {
                    if output.status.success() {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        if stdout.contains('.') { return 90; } // GTest format: "SuiteName."
                    }
                }
            }
        }
        // Check CMakeLists.txt for gtest references
        if let Ok(cmake) = std::fs::read_to_string(project_root.join("CMakeLists.txt")) {
            if cmake.contains("gtest") || cmake.contains("gmock") || cmake.contains("GTest") {
                return 85;
            }
        }
        0
    }

    // Override trait method for binary-based execution (Finding 3)
    fn command_for_binary(&self, cmd: &str, _level: Option<TestLevel>) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: cmd.to_string(),
            args: vec!["--gtest_output=json".to_string()],
            env: HashMap::new(),
        })
    }

    fn single_test_for_binary(&self, cmd: &str, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: cmd.to_string(),
            args: vec![
                "--gtest_output=json".to_string(),
                format!("--gtest_filter={}", test_name),
            ],
            env: HashMap::new(),
        })
    }

    // ... parse_output checks test.result == "SKIPPED" for skip classification
}
```

**Checkpoint:** GTest adapter complete with binary probing, proper command dispatch, GTEST_SKIP handling.

---

### Task 13: Final Integration E2E — Quality Parity with C++ Backend

**Files:**
- Modify: `tests/test_runner.rs` — add adapter detection tests
- Modify: `src/test/mod.rs` — register all adapters, update tests

**Step 1: Register all 4 new adapters in mod.rs**

```rust
pub mod deno_adapter;
pub mod go_adapter;
pub mod mocha_adapter;
pub mod gtest_adapter;

// In TestRunner::new():
adapters: vec![
    Box::new(CargoTestAdapter),
    Box::new(Catch2Adapter),
    Box::new(PytestAdapter),
    Box::new(UnittestAdapter),
    Box::new(VitestAdapter),
    Box::new(JestAdapter),
    Box::new(BunAdapter),
    Box::new(DenoAdapter),
    Box::new(GoAdapter),
    Box::new(MochaAdapter),
    Box::new(GtestAdapter),
],
```

**Step 2: Add adapter detection tests to test_runner.rs**

```rust
// Test Deno, Go, Mocha, GTest detection (same pattern as existing tests)
{
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("deno.json"), "{}").unwrap();
    let adapter = runner.detect_adapter(dir.path(), None, None).unwrap();
    assert_eq!(adapter.name(), "deno");
}
// ... Go (go.mod), Mocha (.mocharc.yml), GTest (CMakeLists.txt with gtest)
```

**Step 3: Validate quality parity checklist**

| Quality Metric | Catch2 | Deno | Go | Mocha | GTest |
|---|---|---|---|---|---|
| Pass/fail/skip counts | Yes | JUnit XML | JSON stream | JSON stats | JSON suites |
| Failure file:line | Yes | classname | `.go:N:` regex | Stack regex | `file.cpp:N` |
| Suggested traces | Yes | @file: + module | @file: + module | @file: + module | @file: |
| Per-test duration | Yes | time attr | Elapsed field | duration | time |
| Rerun hint | Yes | --filter= | -run (escaped) | --grep | --gtest_filter= |
| **Progress tracking** | **update_progress** | **update_progress** | **update_progress** | **update_progress** | **update_progress** |
| **Stuck detection** | Per-test stall | Per-test stall | Per-test stall | Per-test stall | Per-test stall |
| stdout/stderr capture | Frida | Frida | Frida | Frida | Frida |
| Details file | JSON | JSON | JSON | JSON | JSON |

**Step 4: Run full test suite**
Run: `debug_test({ projectRoot: "/Users/alex/strobe" })`
Expected: All tests pass.

**Checkpoint:** Full integration validated. All adapters match C++ backend quality bar. Ready to commit.

---

## Build & Verify Commands

```bash
# Build agent (must be first — Rust embeds agent.js)
cd agent && npm install && npm run build && cd ..

# Touch spawner.rs (Cargo doesn't track include_str!)
touch src/frida_collector/spawner.rs

# Build daemon
cargo build

# Run all unit tests (including new adapters)
debug_test({ projectRoot: "/Users/alex/strobe" })

# Run specific adapter tests
debug_test({ projectRoot: "/Users/alex/strobe", test: "deno_adapter" })
debug_test({ projectRoot: "/Users/alex/strobe", test: "go_adapter" })
debug_test({ projectRoot: "/Users/alex/strobe", test: "mocha_adapter" })
debug_test({ projectRoot: "/Users/alex/strobe", test: "gtest_adapter" })

# Run Python e2e
debug_test({ projectRoot: "/Users/alex/strobe", test: "python_comprehensive" })
debug_test({ projectRoot: "/Users/alex/strobe", test: "python_e2e" })

# JSC validation (manual — requires bun installed)
bun run tests/fixtures/bun_inspector_validation.ts

# Run full test runner integration
debug_test({ projectRoot: "/Users/alex/strobe", test: "test_runner" })
```

## Dependencies

- `quick-xml` — already in `Cargo.toml` (used by Bun adapter for JUnit XML)
- `regex` — already in `Cargo.toml`
- `serde` / `serde_json` — already in `Cargo.toml`
- No new Rust dependencies required.
- No new npm dependencies required.

## Review Findings Addressed

All 70 findings from the 4-agent review are addressed:

**Stream A (16 findings):** sys.monitoring for PY_START only (F3 frame issue), settrace for BP/LP, default version 3.11 (F16), traceInstalled flag (F13), _strobe_bp_event init (F15), removeAllHooks cleanup (F5), version check logic (F8), getter placement (F7), no redundant Task 2 (F9), existing fixture/test reuse (F10-F11), settrace_all_threads fallback on monitoring conflict (F2).

**Stream B (20 findings):** Source transformation in load() hook (F1), NODE_OPTIONS instead of --import CLI (F7), correct injection site in session_manager.rs (F3), pattern sharing via globalThis (F5), proper matching logic (F1), function_enter test criterion (F10), ESM fixture with delay (F19), temp file cleanup (F13), registerHooks from initialize() (F14), file:// URL format (F3).

**Stream C (16 findings):** Multi-function target (F3), action-fired success criteria (F1), accumulated stderr with async loop (F2), UUID-compatible regex (F10), wait-for-response sync (F5), V8 runtime fix for Bun (F4), clear docs on next steps (F9).

**Stream D (18 findings):** parse_junit_xml pub(crate) (F1), progress_fn dispatch for all adapters (F2), generalized command_for_binary trait (F3), preamble stripping (F4), proper Rust fallback (F5), build-fail handling (F6), MCP schema + error messages (F7), GTest binary probing (F8), rerun hints (F15), Go regex escaping (F10), GTEST_SKIP (F11), UTF-8 safe truncation (F6), broader Go location regex (F14).
