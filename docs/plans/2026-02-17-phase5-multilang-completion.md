# Phase 5a: Python Tracing Completion + Cleanup

**Spec:** See `docs/plans/2026-02-17-phase5b-js-ts-support.md` for the JavaScript/TypeScript plan.

**Goal:** Complete Python tracing (readVariable, hook removal, logpoints, breakpoints) and clean up stale docs/TODOs.

**Architecture:** Two independent streams — Python agent fixes (A) and cleanup (C). Stream A requires an agent rebuild at the end. Stream C is pure cleanup with no code changes.

**Tech Stack:** TypeScript (Frida agent), Python `ctypes` + `threading.Event` (breakpoint suspension)

**Commit strategy:** Single commit at end

---

## Workstreams

- **Stream A (Python Completion):** Tasks A1–A4 — all changes in `agent/src/`; no daemon changes needed
- **Stream C (Cleanup):** Task C1 — stale TODO comment + stage deleted docs

Streams A and C are fully independent and can run in parallel.

---

## Stream A — Python Completion

### Task A1: `readVariable` — return actual Python values

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:332-341`

**Problem:** `readVariable(expr)` calls `PyRun_SimpleString` to eval the expression but has no way to get the result back. It returns the placeholder `'<python value>'`.

**Solution:** Create a one-shot `NativeCallback` at call time. Python evaluates the expression and calls the callback synchronously (before `PyRun_SimpleString` returns), passing the JSON-encoded result as a UTF-8 string.

**Step 1: Write the failing test**

Create `tests/fixtures/py_readvar.py`:
```python
import time
x = 42
data = {"key": "hello"}
time.sleep(60)  # stay alive for Frida
```

Integration test: spawn the script, attach, call `debug_read` for expression `"x"`. Currently returns `"<python value>"`. Expected: `42`.

**Step 2: Verify it fails**

Attach to a Python process and call `debug_read`. Observe the placeholder response.

**Step 3: Implement**

Replace `readVariable` at `agent/src/tracers/python-tracer.ts:332`:

```typescript
readVariable(expr: string): any {
  // One-shot NativeCallback — Python calls it synchronously inside PyRun_SimpleString.
  let result = '{"error":"callback not reached"}';
  const cb = new NativeCallback(
    (strPtr: NativePointer) => {
      try { result = strPtr.readUtf8String() ?? '{"error":"null"}'; } catch {}
    },
    'void', ['pointer']
  );
  const cbAddr = (cb as NativePointer).toString();
  // JSON.stringify(expr) produces a correctly-quoted, escaped Python string literal.
  const safeExpr = JSON.stringify(expr);
  this.runPython(`
import json as _j, ctypes as _c
_STROBE_RV = _c.CFUNCTYPE(None, _c.c_char_p)(${cbAddr})
try:
    _rv = _j.dumps(eval(${safeExpr}), default=str)
except Exception as _e:
    _rv = _j.dumps({"error": str(_e)})
_STROBE_RV(_rv.encode('utf-8'))
`);
  return JSON.parse(result);
}
```

**Step 4: Verify it passes**

Repeat the integration test — `debug_read` for `"x"` should return `42`, for `"data"` should return `{"key": "hello"}`.

**Checkpoint:** Python variable inspection returns real values via `debug_read`.

---

### Task A2: Hook removal by file:line

**Files:**
- Modify: `agent/src/agent.ts`

**Problem:** When `remove_trace` is called for interpreted-language hooks, the remove handler at `agent.ts:449-452` is a TODO. Only address-based removal works (native). Python hooks persist forever after removal.

**Step 1: Write the failing test**

Integration test: spawn a Python script with a looping function. Add trace on it; confirm events appear. Call `remove_trace`. Call the function again. Confirm no new events are emitted. Currently new events still appear.

**Step 2: Implement**

Add the reverse-lookup map to the `StrobeAgent` class field declarations (near the existing `addressToFuncId` map):

```typescript
private fileLineToFuncId: Map<string, number> = new Map(); // "file:line" → funcId
```

In the `message.targets` install block (around `agent.ts:414`), after obtaining `funcId`:

```typescript
// After: this.funcIdToName.set(funcId, target.name);
const key = `${target.file}:${target.line}`;
this.fileLineToFuncId.set(key, funcId);
```

Replace the TODO at `agent.ts:449-452`:

```typescript
if (message.targets) {
  for (const target of message.targets) {
    const key = `${target.file}:${target.line}`;
    const funcId = this.fileLineToFuncId.get(key);
    if (funcId !== undefined) {
      this.tracer.removeHook(funcId);
      this.fileLineToFuncId.delete(key);
    }
  }
}
```

**Step 4: Verify it passes**

Repeat the integration test — events stop after `remove_trace`.

**Checkpoint:** Python hook removal works correctly.

---

### Task A3: Python logpoints

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts`

**Problem:** `installLogpoint()` stores data but `syncTraceHooks()` never uses it. The Python `_strobe_trace` function only handles `event='call'` (function entry). Logpoints need `event='line'` (per-line tracing).

**Step 1: Write the failing test**

Integration test: attach to a Python loop (`for i in range(100): x = i * 2`). Install logpoint at the assignment line with message template `"x={x}"`. Expect logpoint output events in the session. Currently nothing appears.

**Step 2: Implement**

Add a `logCallback` NativeCallback in `initialize()`, after the existing `traceCallback` setup:

```typescript
const self2 = this;
this.logCallback = new NativeCallback(
  function(lpIdPtr: NativePointer, lineNum: number, msgPtr: NativePointer) {
    try {
      const lpId = lpIdPtr.readUtf8String() ?? '';
      const msg = msgPtr.readUtf8String() ?? '';
      self2.emitLogpointEvent(lpId, lineNum, msg);
    } catch {}
  },
  'void', ['pointer', 'int', 'pointer']
) as NativePointer;
```

Add `private logCallback: NativePointer | null = null;` to class fields.

Add `emitLogpointEvent()`:
```typescript
private emitLogpointEvent(lpId: string, line: number, msg: string): void {
  const eventId = `${this.sessionId}-pylp-${++this.eventIdCounter}`;
  this.eventBuffer.push({
    id: eventId,
    sessionId: this.sessionId,
    timestampNs: Date.now() * 1_000_000,
    threadId: Process.getCurrentThreadId(),
    eventType: 'stdout',
    text: `[logpoint ${lpId}] ${msg}\n`,
    pid: Process.id,
  });
  if (this.eventBuffer.length >= 50) this.flushEvents();
}
```

Update `syncTraceHooks()` to:
1. Pass the `logCallback` address alongside `traceCallback`
2. Include logpoint entries in the generated Python code
3. Handle `event='line'` in `_strobe_trace` (only when logpoints or breakpoints are active):

```python
# In syncTraceHooks() generated code — add logpoint list:
_strobe_logpoints = [('file_suffix', line_num, 'lp_id', 'msg_template'), ...]

# Extend _strobe_trace to handle 'line' events:
def _strobe_trace(frame, event, arg):
    if event == 'call':
        # ... existing hook matching ...
    elif event == 'line':
        fname = frame.f_code.co_filename
        fline = frame.f_lineno
        for lp_file, lp_line, lp_id, lp_msg in _strobe_logpoints:
            if fname.endswith(lp_file) and fline == lp_line:
                try:
                    msg = lp_msg.format(**{**frame.f_globals, **frame.f_locals})
                except Exception as _e:
                    msg = f'{lp_msg} [fmt error: {_e}]'
                _strobe_log_cb(lp_id.encode(), fline, msg.encode())
                break
    return _strobe_trace
```

Note: `event='line'` fires at every line when `sys.settrace` is active — only add the `elif` branch when logpoints are actually installed.

**Step 4: Verify it passes**

Logpoint output events appear when the target line is reached.

**Checkpoint:** Python logpoints emit output without stopping execution.

---

### Task A4: Python breakpoints — actual suspension

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts`
- Modify: `agent/src/agent.ts`

**Problem:** `installBreakpoint()` stores data but never hooks into the trace function. No suspension occurs.

**Approach:** Two NativeCallbacks + a Python `threading.Event`:
1. `bpHitCallback` — fires when breakpoint is reached; Python then blocks on `threading.Event.wait()`
2. Python-side `_strobe_bp_event.set()` called from JS when the daemon sends a `continue` message

**Step 1: Write the failing test**

Integration test: attach to Python code with a counter loop. Install breakpoint at a specific line. Verify breakpoint-hit is received. Send continue. Verify execution resumes and counter advances.

**Step 2: Implement `bpHitCallback` in `initialize()`**

```typescript
const self3 = this;
this.bpHitCallback = new NativeCallback(
  function(idPtr: NativePointer, lineNum: number) {
    try {
      const id = idPtr.readUtf8String() ?? '';
      self3.agent.emitBreakpointHit(id, lineNum);
    } catch {}
  },
  'void', ['pointer', 'int']
) as NativePointer;
```

Add `private bpHitCallback: NativePointer | null = null;` to class fields.

**Step 3: Add `resumePythonBreakpoint()`**

```typescript
resumePythonBreakpoint(): void {
  this.runPython('_strobe_bp_event.set()');
}
```

**Step 4: Add continue handling in `agent.ts`**

In the message handler for `type === 'continue_breakpoint'` (already exists for native; find it or add it):

```typescript
if ('resumePythonBreakpoint' in this.tracer) {
  (this.tracer as any).resumePythonBreakpoint();
}
```

**Step 5: Update `syncTraceHooks()`**

Include breakpoint data and `threading.Event` suspension:

```python
import threading
_strobe_bp_event = threading.Event()
_strobe_breakpoints = [('file_suffix', line_num, 'bp_id', 'condition_or_empty'), ...]

# In _strobe_trace, inside elif event == 'line':
for bp_file, bp_line, bp_id, bp_cond in _strobe_breakpoints:
    if fname.endswith(bp_file) and fline == bp_line:
        if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
            _strobe_bp_hit_cb(bp_id.encode(), fline)  # notify daemon
            _strobe_bp_event.wait()                     # suspend this thread
            _strobe_bp_event.clear()
        break
```

**Step 6: Verify it passes**

Breakpoint halts Python execution; `continue` resumes it.

**Checkpoint:** Python breakpoints fully work — hit notification, suspension, and resume.

---

## Stream C — Cleanup

### Task C1: Stale TODO + deleted docs

**Files:**
- Modify: `src/test/stuck_detector.rs:60`
- Stage: all `docs/` files shown as unstaged deletions in `git status`

**Step 1: Remove stale TODO comment in `stuck_detector.rs`**

At line 60, remove the stale TODO (the closure-based implementation at line 63 already does what the TODO described):

```rust
// Remove this line:
// TODO Phase 2: Add session_manager: Option<Arc<SessionManager>> to check pause state
```

The field + doc-comment below it stays as-is.

**Step 2: Stage deleted docs**

```bash
git add -u docs/
```

This stages all the `docs/` deletions that appear as ` D` (unstaged deletions) in `git status` — stale planning/spec/review docs from completed phases.

**Step 3: Verify**

```bash
git diff src/test/stuck_detector.rs   # Only the one comment line removed
git status docs/                       # All show as 'D ' (staged)
cargo test stuck_detector              # Still passes
```

**Checkpoint:** No stale TODOs; deleted docs staged; all tests pass.

---

## Final Steps (after all streams)

1. **Rebuild agent and daemon:**
   ```bash
   cd agent && npm run build && cd ..
   touch src/frida_collector/spawner.rs
   cargo build --release
   ```

2. **Run all tests:**
   ```bash
   cargo test
   ```

3. **Single commit:**
   ```bash
   git add src/test/stuck_detector.rs \
           agent/src/tracers/python-tracer.ts \
           agent/src/agent.ts \
           agent/dist/
   git commit -m "feat: complete Python tracing end-to-end (Phase 5a)"
   ```
