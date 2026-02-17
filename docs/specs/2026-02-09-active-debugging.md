# Phase 2: Active Debugging

**Date:** 2026-02-09
**Status:** Design

## Goal

Enable the LLM to pause execution at precise points, inspect and modify state, and step through code — capabilities that passive tracing alone cannot provide.

## Supported Languages

Phase 2 targets **C, C++, Rust, and Swift** — languages that produce native binaries with standard DWARF debug info. Go requires goroutine-aware stack unwinding and is deferred to a future phase. Interpreted languages (Python, JS) use the CDP collector (Phase 9).

## Motivation

Tracing + `debug_read` handles most debugging scenarios, but some cases require stopping execution at an exact moment:

- **Complex state inspection**: Reading multiple related variables atomically, when they'd change between separate `debug_read` calls
- **State injection**: Modifying globals, locals, or calling functions to test hypotheses — requires a stopped process
- **Precise causality**: Observing the exact moment a value goes wrong, not just before/after

## Design Principles

- **Separate from tracing**: Breakpoints are their own subsystem. CModule traces stay fast and untouched. Both can coexist on the same function via Frida's listener stacking (separate `Interceptor.attach` calls on the same address).
- **Line-level granularity**: Break at any source line, not just function boundaries. Uses DWARF `.debug_line` table → instruction address mapping + Frida `Interceptor.attach`. Cross-platform: DWARF 2-5 supported via gimli. Address resolution accounts for ASLR via `image_base` (DWARF) + runtime slide (`Process.mainModule.base`).
- **Frida-native pause**: `recv().wait()` blocks the calling native thread while the JS event loop continues processing messages. No ptrace, no GDB protocol. See [Pause Mechanism Research](#pause-mechanism-research) for validation details.
- **Incremental capability**: Ship globals read/write first, then locals, then function calls.
- **Collector-agnostic design**: Condition evaluation is Frida-specific (JavaScript in agent). Future collectors (e.g., CDP for JS, JVM debugger for Java) would implement their own expression evaluators behind the same MCP tool interface.

## Architecture

```
LLM
 │
 ├── debug_breakpoint({ file, line, condition })
 │     → Daemon resolves file:line → address via DWARF line table
 │     → Sends { type: 'setBreakpoint', address, id, condition } to agent
 │     → Agent: Interceptor.attach(address, { onEnter: breakpointHandler })
 │
 ├── debug_logpoint({ file, line, message })
 │     → Same resolution, but agent evaluates template and emits event without blocking
 │
 ├── debug_continue({ action: "step-over" })
 │     → Daemon resolves next line address via DWARF
 │     → Sends { type: 'resume', threadId, oneShot: nextAddress } to agent
 │     → Agent: sets one-shot hook at next address, unblocks paused thread
 │
 └── debug_write({ targets: [{ variable: "sampleRate", value: 48000 }] })
       → Daemon resolves variable location (global: static address, local: DWARF location list at current PC)
       → Agent writes to resolved address/register
```

## MCP Tools

### debug_breakpoint

Set or remove breakpoints. Supports function-level and line-level targeting.

```typescript
debug_breakpoint({
  sessionId: string,
  add?: [{
    // Target: function entry OR source line (exactly one required)
    function?: string,          // Pattern: "audio::processBlock", "MyClass::*"
    file?: string,              // Source file: "src/audio/processor.cpp"
    line?: number,              // Line number: 142

    condition?: string,         // JS expression: "args[0] > 100 && args[1].length == 0"
    hitCount?: number,          // Break on Nth hit (0 = every time, default)
  }],
  remove?: string[],            // Breakpoint IDs to remove
}) → {
  breakpoints: [{
    id: string,
    function?: string,          // Resolved function name (if applicable)
    file?: string,              // Source file
    line?: number,              // Actual line (may differ if requested line has no code)
    address: string,            // Hex instruction address
  }]
}
```

### debug_logpoint

Set or remove logpoints. Evaluate expressions and emit events without pausing.

```typescript
debug_logpoint({
  sessionId: string,
  add?: [{
    // Target: same as breakpoint
    function?: string,
    file?: string,
    line?: number,

    message: string,            // Template: "tempo={args[0]}, rate={args[1].sampleRate}"
    condition?: string,         // Optional: only log when true
  }],
  remove?: string[],
}) → {
  logpoints: [{
    id: string,
    file?: string,
    line?: number,
    address: string,
  }]
}
```

### debug_continue

Resume execution after a breakpoint pause.

```typescript
debug_continue({
  sessionId: string,
  action?: "continue" | "step-over" | "step-into" | "step-out",
  // Default: "continue"
}) → {
  // Returns when the action completes (next pause or process exit)
  status: "paused" | "running" | "exited",
  // If paused again (step hit or another breakpoint):
  breakpointId?: string,
  file?: string,
  line?: number,
  function?: string,
}
```

If a process forks while paused, `debug_continue` resumes only the thread/process specified by `sessionId`. Child processes created during the pause are tracked but not auto-resumed.

### debug_write

Write to variables while paused or running.

```typescript
debug_write({
  sessionId: string,
  targets: [{
    variable?: string,          // "gTempo" (global) or "sampleRate" (local at breakpoint)
    address?: string,           // Raw hex address
    value: number | string | boolean,
    type?: string,              // "f64", "i32", "pointer", etc.
  }]
}) → {
  results: [{
    variable?: string,
    address: string,
    previousValue: any,
    newValue: any,
  }]
}
```

Local variable writes require a paused breakpoint. Globals can be written at any time.

## Agent-Side Pause Mechanism

### Pause mechanism research

The `recv().wait()` pattern is Frida's documented mechanism for blocking receives in agent scripts ([Frida Messages docs](https://frida.re/docs/messages/), [Issue #446](https://github.com/frida/frida/issues/446) — confirmed by oleavr). It works inside `Interceptor.attach` `onEnter` callbacks: `send()` data to the host, then `recv('type', callback).wait()` blocks the calling native thread until a matching message arrives.

**Threading model** ([Issue #756](https://github.com/frida/frida/issues/756)): Frida's JS runtime permits only one native thread to execute JavaScript at a time, coordinated through a lock. `NativeFunction` calls with `'cooperative'` scheduling release this lock before the call and reacquire after. By the same mechanism, `recv().wait()` releases the JS lock while the native thread sleeps on a condition variable, allowing other threads to acquire the lock and execute JS (including their own `onEnter` callbacks and `recv().wait()` calls).

**Implication**: Multiple threads can independently pause via `recv().wait()` without deadlocking — each thread's `wait()` releases the JS lock, so other threads' callbacks continue to fire. This is the same cooperative scheduling model that makes `NativeFunction` calls work across threads.

**Phase 2a prerequisite**: Before building the full breakpoint system, implement a minimal PoC test: two threads hit the same `Interceptor.attach` hook, both call `recv().wait()`, resume one, verify the other stays paused. This validates the multi-thread blocking assumption in Strobe's specific environment.

### How pausing works

Frida's `Interceptor.attach` runs callbacks on the target thread. The agent uses `recv().wait()` to block the thread while keeping the JS event loop alive:

```typescript
onEnter(args) {
  const bp = lookupBreakpoint(this.returnAddress);

  // Evaluate condition
  if (bp.condition && !evaluateCondition(bp.condition, args)) return;

  // Hit count
  bp.hits++;
  if (bp.hitCount > 0 && bp.hits < bp.hitCount) return;

  // Notify daemon
  send({
    type: 'paused',
    threadId: Process.getCurrentThreadId(),
    breakpointId: bp.id,
    funcName: bp.funcName,
    file: bp.file,
    line: bp.line,
  });

  // Block this thread, release JS lock
  // recv() is one-shot: registers a handler for exactly one message of the given type.
  // wait() puts the native thread to sleep on a condition variable, releasing the JS lock.
  // When a 'resume-<threadId>' message arrives, the handler fires and the thread wakes.
  const op = recv('resume-' + Process.getCurrentThreadId(), (msg) => {
    // msg may contain: oneShot address for stepping
    if (msg.payload.oneShot) {
      installOneShotBreakpoint(msg.payload.oneShot);
    }
  });
  op.wait();  // Native thread sleeps. JS lock released. Other threads continue.
}
```

**recv() cleanup**: Each `recv()` call creates a one-shot listener. When the thread hits another breakpoint later, a new `recv()` is registered. No accumulation since each `.wait()` consumes exactly one message. If a thread exits while paused (e.g., process killed), the listener is cleaned up by Frida's script teardown.

### Multi-thread behavior

Each thread blocks independently via its own `recv('resume-<threadId>')`. Other threads continue running. The daemon tracks all paused threads per session:

```
Session.pausedThreads: Map<threadId, {
  breakpointId: string,
  funcName: string,
  file: string,
  line: number,
  timestamp: number,
}>
```

### Condition evaluation

Conditions are JavaScript expressions evaluated in the Frida agent context. For JS-based `Interceptor.attach`, the `args` parameter provides access to all function arguments (unlike CModule hooks which only capture the first 2 as raw u64). The `new Function()` pattern is already proven safe in Strobe's agent — see `cmodule-tracer.ts:600` for existing usage.

```typescript
function evaluateCondition(condition: string, args: any[]): boolean {
  try {
    return Boolean(new Function('args', `return (${condition})`)(args));
  } catch (e) {
    send({ type: 'conditionError', breakpointId, condition, error: e.message });
    return false;  // Don't break on failed conditions
  }
}
```

The same evaluator handles logpoint message templates — `{expr}` placeholders are extracted and evaluated individually.

**Note on Swift**: Swift functions using `@convention(swift)` pass a context pointer differently from C-style `args[0]`. Conditions for Swift functions may need `this.context` register access instead of `args[0]` for the first parameter.

## Stepping Implementation

All step types are implemented as one-shot breakpoints resolved via the DWARF line table.

### step-over

1. Look up current paused address in DWARF line table → get current `(file, line)`
2. Find next line entry in the same function (next `is_statement = true` line with a different line number)
3. Install one-shot `Interceptor.attach` at that address
4. Also install one-shot hook at the function's return address (in case the current line is the last in the function)
5. Resume the paused thread
6. Whichever one-shot fires first triggers a new pause; remove the other

**Return address resolution for step-over/step-out**: When paused at a breakpoint that was set at function entry, `InvocationContext.returnAddress` is available directly. When paused mid-function (line breakpoint), read the return address from the stack frame: `[RBP+8]` on x86_64, `LR` register on ARM64. On ARM64 with pointer authentication (PAC), the return address must be stripped via Frida's `ptr(addr).strip()` before use as a hook target.

### step-into

1. Same as step-over, PLUS:
2. Set one-shot hooks on all function entries that could be called from the current line
3. Resolution: use DWARF call site info if available, or set hooks on all currently-traced functions as a fallback
4. First hook to fire wins — either next line (stepped over) or callee's first line (stepped into)

### step-out

1. Read the return address from the current stack frame (see return address resolution above)
2. Look up the return address in DWARF line table to find the caller's file:line
3. Install one-shot hook at the return address
4. Resume → pauses when the function returns to its caller

### One-shot hook lifecycle

One-shot hooks for stepping must clean up immediately after firing to avoid listener accumulation:

```typescript
function installOneShotBreakpoint(address: NativePointer): InvocationListener {
  const listener = Interceptor.attach(address, {
    onEnter(args) {
      listener.detach();  // Clean up immediately
      oneShotHooks.delete(address.toString());
      // ... trigger pause logic
    }
  });
  oneShotHooks.set(address.toString(), listener);
  return listener;
}
```

When a step completes (one hook fires), all other pending one-shot hooks from the same step operation are detached.

## DWARF Parser Extensions

### Line table parsing

Add `.debug_line` parsing to [src/dwarf/parser.rs](src/dwarf/parser.rs). Per compilation unit:

```rust
struct LineEntry {
    address: u64,
    file: String,       // Relative source path
    line: u32,
    column: u32,
    is_statement: bool, // Valid breakpoint location
}

struct LineTable {
    entries: Vec<LineEntry>,  // Sorted by address
}
```

**Resolution functions:**
- `resolve_line(file, line) → Option<(address, actual_line)>` — Find instruction address for a source line. Snaps to nearest `is_statement` line if exact line has no code.
- `resolve_address(address) → Option<(file, line)>` — Reverse lookup for step calculations.
- `next_line(address) → Option<(address, file, line)>` — Next statement in same function. Used by step-over.

Integrated into the existing parallel CU parsing pipeline (rayon). Line tables are small relative to function/variable info — negligible performance impact. Estimated ~500-800 lines of new Rust code.

**Cross-platform notes:**
- DWARF 2-5 line table format differences are handled transparently by gimli's `LineProgram` API.
- **macOS**: File paths in dSYM bundles are typically absolute.
- **Linux**: File paths in embedded `.debug_info` are often relative. The daemon must normalize paths for matching.
- Only lines with `is_statement = true` should be offered as breakpoint locations — these are guaranteed valid instruction boundaries by the compiler.
- On **ARM64**: instruction addresses are 4-byte aligned. DWARF `is_statement` entries respect this, so `Interceptor.attach` on line addresses should be safe. If an attach fails, report the error with a suggestion to try the nearest function entry.

### Location list parsing (for local variable writes)

Add `DW_AT_location` parsing for local variables within functions. **Parsed lazily at breakpoint pause time** (not during initial DWARF parse) — consistent with the existing lazy philosophy for struct members and crash locals.

```rust
struct LocalVariable {
    name: String,
    type_info: TypeInfo,
    // Location is PC-dependent
    locations: Vec<LocationRange>,
}

struct LocationRange {
    pc_start: u64,
    pc_end: u64,
    location: VariableLocation,
}

enum VariableLocation {
    Register(PlatformRegister),     // Variable is in a CPU register
    StackOffset(i64),               // Variable is at [frame_pointer + offset]
    Address(u64),                   // Variable is at a fixed address
    OptimizedOut,                   // Not available at this PC
}

/// Platform-aware register mapping.
/// gimli::Register::name() provides platform-specific names.
enum PlatformRegister {
    X86_64(X86_64Register),         // RAX, RBX, RBP, RSP, RDI, RSI, ...
    Aarch64(Aarch64Register),       // X0-X30, SP, FP (X29), LR (X30)
}
```

**Resolution at pause time:**
1. Daemon receives "paused at address X" from agent
2. Calls `DwarfParser::resolve_locals_at_pc(function, pc)` (new method, lazy, not cached during initial parse)
3. For each variable, evaluates its `DW_AT_location` for the current PC using gimli's `LocationLists` API
4. Resolves `DW_TAG_lexical_block` nesting to determine which locals are in scope at this PC
5. Returns location type + register/offset to agent for read/write

**Register access in agent**: For register-located variables, the agent uses `InvocationContext` register accessors: `this.context.rax` (x86_64), `this.context.x0` (ARM64). For stack-located variables: `Memory.readDouble(this.context.rbp.add(offset))` or equivalent.

## Daemon Session State Changes

The session manager gains breakpoint and pause tracking:

```rust
struct SessionState {
    // ...existing fields...

    // Breakpoint management
    breakpoints: HashMap<BreakpointId, Breakpoint>,
    logpoints: HashMap<LogpointId, Logpoint>,

    // Pause state
    paused_threads: HashMap<ThreadId, PauseInfo>,
}

struct Breakpoint {
    id: BreakpointId,
    target: BreakpointTarget,  // Function pattern or file:line
    address: u64,
    condition: Option<String>,
    hit_count: u32,
    hits: u32,
}

enum BreakpointTarget {
    Function(String),
    Line { file: String, line: u32 },
}

struct PauseInfo {
    breakpoint_id: BreakpointId,
    func_name: Option<String>,
    file: Option<String>,
    line: Option<u32>,
    paused_at: Instant,
}
```

Follows existing `Arc<RwLock<HashMap<String, ...>>>` pattern from session_manager.rs. Helper methods: `add_breakpoint`, `remove_breakpoint`, `get_paused_threads`, `add_paused_thread`, `remove_paused_thread`.

**Breakpoint removal while paused**: If a breakpoint is removed while a thread is paused on it, the daemon must: (1) send a resume message to the paused thread, (2) wait for the thread to unblock, (3) then detach the Interceptor listener. This prevents dangling blocked threads.

## Logpoint Implementation

Logpoints share hook infrastructure with breakpoints but never block:

```typescript
onEnter(args) {
  const lp = lookupLogpoint(this.returnAddress);

  // Evaluate condition if present
  if (lp.condition && !evaluateCondition(lp.condition, args)) return;

  // Evaluate message template: "tempo={args[0]}" → "tempo=120.5"
  const message = lp.message.replace(/\{([^}]+)\}/g, (_, expr) => {
    try {
      return String(new Function('args', `return (${expr})`)(args));
    } catch (e) {
      return `<error: ${e.message}>`;
    }
  });

  // Emit as event (appears in timeline alongside traces)
  send({ type: 'logpoint', logpointId: lp.id, message, timestamp: Date.now() });
}
```

Logpoint events are stored in the same event timeline as traces and stdout/stderr, queryable via `debug_query`.

## Event Storage

New event types for the database schema (following existing `add_column_if_not_exists` migration pattern in `src/db/schema.rs`):

**New `event_type` values:** `pause`, `logpoint`, `condition_error`

**New columns:**
```sql
add_column_if_not_exists(&conn, "events", "breakpoint_id", "TEXT")?;
add_column_if_not_exists(&conn, "events", "logpoint_message", "TEXT")?;
```

Add `Pause`, `Logpoint`, `ConditionError` variants to the event type used in query filtering.

## Error Handling

New error variants for `src/error.rs` (following existing `thiserror` pattern):

```rust
#[error("NO_CODE_AT_LINE: No executable code at {file}:{line}. Valid lines: {nearest_lines}")]
NoCodeAtLine { file: String, line: u32, nearest_lines: String },

#[error("OPTIMIZED_OUT: Variable '{variable}' is optimized out at this PC. Recompile with -O0.")]
OptimizedOut { variable: String },
```

Local-write-while-not-paused reuses existing `ValidationError`.

| Scenario | Behavior |
|----------|----------|
| Breakpoint on non-existent line | `NoCodeAtLine` error with nearest valid lines: "No code at file.cpp:143. Valid lines: 140, 145" |
| Condition throws at runtime | Emit `conditionError` event, don't pause, continue execution |
| Write to optimized-out local | `OptimizedOut` error: "Variable 'x' is optimized out at this PC. Recompile with -O0." |
| Local write while not paused | `ValidationError`: "Local variable writes require a paused breakpoint." |
| Process exits while paused | Clean up session, report exit status to LLM |
| Remove breakpoint while thread paused on it | Resume thread first, then detach hook |
| Multiple threads hit same breakpoint | Each blocks independently via per-thread `recv().wait()` |
| Interceptor.attach fails on mid-function address | Report error with suggestion to try nearest function entry or use `is_statement` line |
| ARM64 pointer authentication on return address | Strip PAC bits via `ptr(addr).strip()` before using as hook target |

## Breakpoints During Test Runs

When a breakpoint fires during a `debug_test` run:

1. The test runner's polling loop continues (it checks `is_process_alive(pid)` — a paused thread is still alive).
2. The stuck detector (`src/test/stuck_detector.rs`) sees 0% CPU on the paused thread. **The stuck detector must check for active breakpoints before diagnosing deadlock** — if any thread is paused at a breakpoint, suppress stuck warnings for that test.
3. The LLM can use `debug_continue` to resume execution. The test runner waits for the process to complete normally.
4. Logpoints work during test runs without pausing — they just emit events.

## Testing Strategy

### Phase 2a prerequisite: recv().wait() PoC

Before building the full breakpoint system, validate the multi-thread blocking assumption:

1. Build a small test binary with two threads calling the same function in a loop
2. Attach `Interceptor.attach` with `recv().wait()` pause
3. Both threads hit the hook → both call `recv().wait()`
4. Resume one thread → verify it continues
5. Verify the other thread remains paused
6. Resume the second thread → verify it continues

This test validates that `recv().wait()` blocks individual native threads (not the JS event loop) and that multiple threads can be paused independently.

### Test fixtures

A small C test binary with known behavior:
- Function that increments a global counter
- Function with a deliberate off-by-one bug
- Multi-threaded function with shared state
- Nested function calls for step-into/step-out testing

### Integration tests

1. **Breakpoint at function entry**: Launch → set breakpoint → trigger function → verify pause → read counter → continue → verify resumed
2. **Line-level breakpoint**: Set breakpoint at specific line → verify pause at correct address → read local variable
3. **Conditional breakpoint**: Set `args[0] > 5` → call function with args 1..10 → verify only pauses when condition true
4. **Hit count**: Set hitCount=3 → call function 5 times → verify pauses only on 3rd call
5. **Logpoint**: Set logpoint with template → verify event in timeline → verify no pause
6. **Step-over**: Pause at line N → step-over → verify at line N+1
7. **Step-into**: Pause at line with function call → step-into → verify at callee's first line
8. **Step-out**: Pause inside function → step-out → verify at caller's line after call
9. **Write global**: Pause → write global → continue → verify program sees new value
10. **Write local**: Pause at line → write local variable → step-over → verify changed value affects execution
11. **Multi-thread**: Two threads hit same breakpoint → verify both pause independently → continue each separately
12. **Coexistence**: Function with CModule trace + breakpoint → verify trace events still fire AND breakpoint pauses

## Validation Criteria

Find a bug that traces alone couldn't catch:

1. LLM sees suspicious pattern in traces (function called with unexpected frequency)
2. LLM sets conditional breakpoint: `debug_breakpoint({ file: "processor.cpp", line: 87, condition: "args[0] < 0" })`
3. App pauses at exact moment a negative value appears
4. LLM reads local variables: `debug_read({ targets: [{ variable: "sampleRate" }, { variable: "bufferSize" }] })`
5. LLM writes corrected value: `debug_write({ targets: [{ variable: "sampleRate", value: 44100 }] })`
6. LLM continues and verifies the fix: `debug_continue({ action: "continue" })`
7. LLM identifies root cause from the observed state
8. LLM adds logpoint to verify the fix persists: `debug_logpoint({ file: "processor.cpp", line: 87, message: "sampleRate={args[0]}" })`
9. App runs to completion without the bug recurring

## Implementation Phases

### Phase 2a: Core breakpoints + continue
- **Prerequisite**: recv().wait() multi-thread PoC test
- DWARF line table parsing (`.debug_line` via gimli `LineProgram` API)
- `debug_breakpoint` (function + line targeting, conditions, hit counts)
- `debug_continue` (continue only, no stepping yet)
- Agent pause mechanism (`recv().wait()`)
- Daemon pause state tracking
- `debug_write` for globals/statics
- New event types in DB schema (`pause`, `logpoint`, `condition_error`)
- New error variants (`NoCodeAtLine`, `OptimizedOut`)

### Phase 2b: Stepping + logpoints
- Step-over, step-into, step-out
- `debug_logpoint` tool
- One-shot breakpoint infrastructure with proper lifecycle management
- Return address resolution (platform-aware: x86_64 stack vs ARM64 LR + PAC stripping)

### Phase 2c: Local variable writes
- DWARF location list parsing (lazy, at breakpoint pause time)
- `DW_TAG_lexical_block` scope resolution
- `debug_write` for locals at breakpoint
- Platform-aware register mapping (`PlatformRegister` enum)
- Register and stack writes via agent (`InvocationContext` register accessors)

### Future (beyond Phase 2)
- Function calls while paused (`NativeFunction` invocation)
- "Pause all threads" option
- Watchpoints (break when memory address changes — hardware debug registers)
- Time-travel debugging (replay from event history)
- Phase 3 DAP adapter: translate Frida pause mechanism to DAP protocol (`stopped`, `continued`, `stackTrace` events) for VS Code integration

## Architectural Notes

### Hook storage separation

Breakpoints must be stored separately from CModule trace hooks in the agent. The current `CModuleTracer.hooks` map (keyed by address) tracks trace listeners. Breakpoints go in a separate `breakpoints` map. This allows both to coexist on the same address — Frida supports multiple `Interceptor.attach` listeners at the same address.

### Message protocol additions

New daemon→agent message types:
- `setBreakpoint { address, id, condition?, hitCount? }`
- `removeBreakpoint { id }`
- `setLogpoint { address, id, message, condition? }`
- `removeLogpoint { id }`
- `resume-<threadId> { oneShot? }` (per-thread, dynamic type name)

New agent→daemon message types:
- `paused { threadId, breakpointId, funcName, file, line }`
- `logpoint { logpointId, message, timestamp }`
- `conditionError { breakpointId, condition, error }`
- `breakpointSet { id, address }` (confirmation)

These follow the existing JSON message protocol (`post_message_raw` / `send()`). The daemon-side handler in `spawner.rs:291` adds new match arms for incoming types. The `HooksReadySignal` pattern (blocking wait for agent confirmation) is reused for breakpoint set/remove confirmations.
