# Phase 2: Active Debugging

**Date:** 2026-02-09
**Status:** Design

## Goal

Enable the LLM to pause execution at precise points, inspect and modify state, and step through code — capabilities that passive tracing alone cannot provide.

## Motivation

Tracing + `debug_read` handles most debugging scenarios, but some cases require stopping execution at an exact moment:

- **Complex state inspection**: Reading multiple related variables atomically, when they'd change between separate `debug_read` calls
- **State injection**: Modifying globals, locals, or calling functions to test hypotheses — requires a stopped process
- **Precise causality**: Observing the exact moment a value goes wrong, not just before/after

## Design Principles

- **Separate from tracing**: Breakpoints are their own subsystem. CModule traces stay fast and untouched. Both can coexist on the same function.
- **Line-level granularity**: Break at any source line, not just function boundaries. Uses DWARF line table → instruction address mapping + Frida `Interceptor.attach`.
- **Frida-native pause**: `recv().wait()` blocks the target thread while keeping the JS event loop alive for message processing. No ptrace, no GDB protocol.
- **Incremental capability**: Ship globals read/write first, then locals, then function calls.

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

  // Block this thread, release JS event loop
  const op = recv('resume-' + Process.getCurrentThreadId(), (msg) => {
    // msg may contain: oneShot address for stepping
    if (msg.payload.oneShot) {
      installOneShotBreakpoint(msg.payload.oneShot);
    }
  });
  op.wait();  // Thread sleeps. Other threads and JS event loop continue.
}
```

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

Conditions are JavaScript expressions evaluated in the agent context:

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

## Stepping Implementation

All step types are implemented as one-shot breakpoints resolved via the DWARF line table.

### step-over

1. Look up current paused address in DWARF line table → get current `(file, line)`
2. Find next line entry in the same function (next `is_statement = true` line with a different line number)
3. Install one-shot `Interceptor.attach` at that address
4. Also install one-shot hook at the function's return address (in case the current line is the last in the function)
5. Resume the paused thread
6. Whichever one-shot fires first triggers a new pause; remove the other

### step-into

1. Same as step-over, PLUS:
2. Set one-shot hooks on all function entries that could be called from the current line
3. Resolution: use DWARF call site info if available, or set hooks on all currently-traced functions as a fallback
4. First hook to fire wins — either next line (stepped over) or callee's first line (stepped into)

### step-out

1. Read the return address from the current stack frame (via `InvocationContext.returnAddress` or DWARF frame info)
2. Look up the return address in DWARF line table to find the caller's file:line
3. Install one-shot hook at the return address
4. Resume → pauses when the function returns to its caller

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

Integrated into the existing parallel CU parsing pipeline. Line tables are small relative to function/variable info — negligible performance impact.

### Location list parsing (for local variable writes)

Add `DW_AT_location` parsing for local variables within functions:

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
    Register(RegisterId),           // Variable is in a CPU register
    StackOffset(i64),               // Variable is at [RBP + offset]
    Address(u64),                   // Variable is at a fixed address
    OptimizedOut,                   // Not available at this PC
}
```

**Resolution at pause time:**
1. Daemon receives "paused at address X" from agent
2. Looks up local variables in scope at address X (via `DW_TAG_lexical_block` nesting)
3. For each variable, evaluates its location for PC = X
4. Returns location type + register/offset to agent for read/write

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

## Error Handling

| Scenario | Behavior |
|----------|----------|
| Breakpoint on non-existent line | Error with nearest valid lines: "No code at file.cpp:143. Valid lines: 140, 145" |
| Condition throws at runtime | Emit `conditionError` event, don't pause, continue execution |
| Write to optimized-out local | Error: "Variable 'x' is optimized out at this PC. Recompile with -O0." |
| Local write while not paused | Error: "Local variable writes require a paused breakpoint." |
| Process exits while paused | Clean up session, report exit status to LLM |
| Remove breakpoint while thread paused on it | Resume thread first, then detach hook |
| Multiple threads hit same breakpoint | Each blocks independently via per-thread `recv().wait()` |
| Interceptor.attach fails on mid-function address | Fallback: report error with suggestion to try nearest function entry |

## Testing Strategy

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

## Validation Criteria

Find a bug that traces alone couldn't catch:

1. LLM sees suspicious pattern in traces (function called with unexpected frequency)
2. LLM sets conditional breakpoint: `debug_breakpoint({ file: "processor.cpp", line: 87, condition: "args[0] < 0" })`
3. App pauses at exact moment a negative value appears
4. LLM reads local variables: `debug_read({ targets: [{ variable: "sampleRate" }, { variable: "bufferSize" }] })`
5. LLM writes corrected value: `debug_write({ targets: [{ variable: "sampleRate", value: 44100 }] })`
6. LLM continues and verifies the fix: `debug_continue({ action: "continue" })`
7. LLM identifies root cause from the observed state

## Implementation Phases

### Phase 2a: Core breakpoints + continue
- DWARF line table parsing
- `debug_breakpoint` (function + line targeting, conditions, hit counts)
- `debug_continue` (continue only, no stepping yet)
- Agent pause mechanism (`recv().wait()`)
- Daemon pause state tracking
- `debug_write` for globals/statics

### Phase 2b: Stepping + logpoints
- Step-over, step-into, step-out
- `debug_logpoint` tool
- One-shot breakpoint infrastructure

### Phase 2c: Local variable writes
- DWARF location list parsing
- Local variable scope resolution
- `debug_write` for locals at breakpoint
- Register and stack writes via agent

### Future (beyond Phase 2)
- Function calls while paused (`NativeFunction` invocation)
- "Pause all threads" option
- Watchpoints (break when memory address changes — hardware debug registers)
- Time-travel debugging (replay from event history)
