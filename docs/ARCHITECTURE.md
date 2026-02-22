# Architecture

## Design Decisions

Key decisions made during design review. These are locked in unless we discover they don't work in practice.

### Target User
**Developers debugging their own code during development.**

Not reverse engineers, not production debugging, not release builds. The developer has:
- Debug symbols available (DWARF/PDB)
- Dev environment (no code signing restrictions)
- Ability to reproduce bugs

### Core Value Proposition
**Close the observe-modify-rerun loop without touching source code.**

Bug happens → adjust tracing scope dynamically → retrigger → caught. No recompilation, no log statements, no code changes.

### Query API: Structured, Not Natural Language
The LLM constructs explicit structured queries. No natural language parsing.

```typescript
// This:
debug_query({
  eventType: "function_exit",
  function: { contains: "malloc" },
  returnValue: { equals: null }
})

// Not this:
debug_query({ query: "malloc returns null" })  // NO - fragile NL parsing
```

The LLM knows what it's looking for. Structured queries are deterministic and debuggable.

### Context-Aware Tracing Defaults
Tracing scope depends on context. "Trace everything" is wrong for TDD workflows where you already know what failed.

| Context | Default Tracing | Rationale |
|---------|----------------|-----------|
| `debug_launch` (general) | None (stdout/stderr only) | Output-first is usually enough; add traces only if needed |
| `debug_test` (test run) | Minimal/none | Wait for failure, then trace targeted |
| `debug_test` (rerun failed) | Suggested patterns | Stack trace tells us what to trace |

**User code heuristic:** When tracing user code (e.g., `@usercode`), trace functions whose source file is within the project directory.

| Language | Debug Info | How We Determine Source |
|----------|-----------|------------------------|
| C/C++ | DWARF | `DW_AT_decl_file` |
| Rust | DWARF | Same |
| Go | DWARF | Same |
| Python | AST | `rustpython-parser` extracts qualified names from `.py` files |
| JS/TS | Source maps + regex | `JsResolver` parses source, resolves via `sourcemap` crate |

**Traced:** `src/main.rs`, `lib/utils.c`
**Not traced:** `/usr/lib/libc.so`, `~/.cargo/registry/...`

The LLM can expand or narrow scope at runtime via `debug_trace`.

### Test Instrumentation: First-Class TDD Support
Test debugging is a core workflow, not an afterthought. The `debug_test` tool:

1. Starts an async test run with minimal tracing overhead
2. `debug_test_status` returns structured data with **rule-based hints**
3. LLM uses hints to rerun with targeted tracing

**Key insight:** The MCP server is dumb, the calling LLM is smart. We don't need AI in the server - just good structured data.

```typescript
// Server returns structured hints (rule-based, not AI):
{
  failures: [{
    test: "test_auth_flow",
    file: "tests/auth.rs:42",
    error: "assertion failed: expected 200, got 401",
    stackTrace: ["auth::validate_token", "http::client::send"],
    suggestedTrace: ["auth::*", "http::client::*"],  // Extracted from stack
    rerunCommand: "cargo test test_auth_flow"
  }]
}
```

The hints are simple rules:
- Parse stack trace → extract module names → suggest as trace patterns
- Test name → construct rerun command
- Error message patterns → add relevant suggestions

### Dynamic Tracing: No Restart Required
Frida supports adding/removing hooks at runtime. The LLM adjusts observation scope while the app runs:

1. App running with output capture only (no tracing)
2. User reports bug in graphics
3. LLM: `debug_trace({ add: ["render::*"] })`
4. Hooks injected instantly, no restart
5. User reproduces bug
6. LLM queries newly captured events

This is a key differentiator from traditional debuggers.

### Crash Capture: Black Box Recording
When the app crashes (SIGSEGV, SIGABRT, etc.), Frida intercepts the signal before the default handler. We capture:

- Signal type and faulting address
- Full stack trace
- Register state
- Local variables in crashing frame (via DWARF)
- Last N events from trace buffer

This gets stored as a special `crash` event that the LLM can query.

### Serialization: Sensible Defaults, Depth-Limited
Capturing function arguments requires serializing them to JSON. Trade-offs:

- **Primitives:** Direct serialization (int → number, string → string)
- **Pointers:** Captured as hex address by default, optionally dereferenced
- **Structs:** Serialized to configurable depth (default 3, max 10 via `serializationDepth`)
- **Arrays:** First N elements (configurable, default 100)

The LLM can request deeper serialization via `debug_trace({ serializationDepth: 5 })`.

### VS Code Extension: Phase 3, Not Core
The primary interface is MCP for LLMs. VS Code extension is for:
- Distribution (one-click install)
- Human observability (see what the LLM sees)
- Future monetization

Core functionality must work without VS Code.

### Contributor-Friendly Extensibility
The architecture must be clean enough that **anyone can add support for an obscure language, I/O channel, or platform backend** without understanding the whole system.

Three clear extension points:
1. **Collectors** - Add language support by implementing one trait
2. **I/O Channels** - Add app I/O support (MIDI, serial, custom protocols) by implementing `InputChannel`/`OutputChannel` traits
3. **Platform Backends** - Add OS support by implementing `UIObserver`, `UIInput`, `VisionPipeline` traits

All emit to unified schemas. Contributors don't need to touch:
- Storage layer
- Query engine
- MCP interface
- Scenario runner
- VS Code extension

See [Extensibility](#extensibility) section for exact interfaces.

---

## Runtime Decisions

### Multi-Threading Support
All trace events include thread context for debugging concurrent applications:
- **thread_id** - Numeric thread identifier
- **thread_name** - Human-readable name if available
- **timestamp_ns** - Monotonic nanosecond timestamp for ordering

Queries support thread filtering and ordering by timestamp or thread-then-timestamp. Dedicated threading tools (lock tracing, deadlock detection) planned for future phase.

### Serialization & Circular References
When serializing complex data structures:
- **Default depth:** 3 levels (configurable 1-10 via `serializationDepth`)
- **Circular references:** Detected and marked as `<circular ref to X>` (not just truncated)
- **Binary data:** Base64 encoded when relevant (audio buffers, images)
- **Large arrays:** Truncated with count indicator

### Hot Function Handling
When a traced function is called excessively (>100k calls/sec):
- **Auto-detect:** System identifies hot functions automatically
- **Auto-sample:** Reduces capture to configurable percentage (default 1%)
- **Warn LLM:** Response includes sampling indicator so LLM knows data is partial
- **Override:** LLM can disable sampling or narrow trace patterns

### Pattern Matching Syntax
Trace patterns use glob syntax (familiar from .gitignore, shell):
- `*` matches any characters except `::`
- `**` matches any characters including `::`
- Examples: `render::*`, `*::draw`, `auth::**::validate`
- File-based patterns: `@file:src/render/*.rs`

### Daemon Lifecycle
Single global daemon per user:
- **Auto-start:** First MCP call spawns daemon if not running
- **Auto-shutdown:** Terminates after 30 minutes idle
- **Location:** `~/.strobe/` (config, socket, SQLite database)
- **Multi-project:** Multiple projects share daemon, isolated by session ID

### Storage & Retention
SQLite database with automatic cleanup:
- **Default:** Session data kept until `debug_stop()` called
- **Retain option:** `debug_stop({ retain: true })` preserves for later analysis
- **Auto-purge:** Retained sessions deleted after 7 days
- **Hard limit:** 10GB total storage, oldest sessions purged first
- **Configurable:** Override via `~/.strobe/settings.json` (`events.maxPerSession`)

### Process Forking
When target process calls `fork()` or `exec()`:
- **Auto-follow:** Both parent and child traced in same session
- **PID tagging:** Events include process ID for disambiguation
- **Unified view:** LLM sees complete execution across all spawned processes

### MCP Transport
stdio proxy architecture for maximum compatibility:
- **Claude/LLM** spawns `strobe mcp` via standard stdio transport
- **CLI proxy** connects to persistent daemon via Unix socket
- **Daemon** handles all Frida sessions and storage
- **Multiple clients** can share daemon (VS Code, CLI, multiple LLM sessions)
- **Windows:** Named pipes or localhost HTTP fallback

### Symbol Demangling
Full demangling support for all initial languages:
- **C++:** Itanium ABI demangling
- **Rust:** Rust symbol demangling
- **C:** No mangling (pass-through)
- **Go:** Deferred to future phase (runtime complexity)

Raw mangled names preserved in `function_name_raw` for advanced use cases.

### Inlined Functions
Functions inlined by compiler optimizations cannot be hooked (no entry point exists):
- **Detection:** Use DWARF info to identify inlined functions
- **Warning:** Report "function X appears inlined, cannot hook"
- **Future:** Show inlining relationships ("X was inlined into Y")

Note: Debug builds (`-O0`) typically disable inlining, so this is rare in dev workflow.

### Error Handling Philosophy
Errors are opportunities for LLM-guided resolution:
- **No debug symbols:** LLM skill analyzes project, suggests build configuration
- **Frida attach fails (SIP):** LLM guides user through macOS security settings
- **Process crashes:** Return crash info, LLM investigates
- **Daemon unreachable:** Clear error message, LLM can restart

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     MCP Client (LLM)                             │
│  debug_launch | debug_trace | debug_query | debug_session        │
│  debug_test | debug_breakpoint | debug_continue | debug_memory  │
│  debug_ui                                                       │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ MCP (JSON-RPC over stdio)
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                      Core Daemon (Rust)                          │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                      MCP Server                             │ │
│  │   Exposes tools, validates requests, returns results        │ │
│  └────────────────────────────────────────────────────────────┘ │
│                              │                                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                  Execution Index (SQLite)                   │ │
│  │   Events table | Crash records | FTS for function names    │ │
│  └────────────────────────────────────────────────────────────┘ │
│                              │                                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                  Collector Manager                          │ │
│  │   Manages Frida sessions | Routes events | Dynamic hooks    │ │
│  └────────────────────────────────────────────────────────────┘ │
│                              │                                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                   Frida Collector                           │ │
│  │   Spawns/attaches | Injects scripts | Receives messages     │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ Frida IPC
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                      Target Process                              │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                   Frida Agent (JS)                          │ │
│  │   - Hooks functions based on daemon instructions            │ │
│  │   - Serializes arguments/returns                            │ │
│  │   - stdout/stderr capture is at Frida Device level          │ │
│  │     (FRIDA_STDIO_PIPE); agent has write(2) fallback         │ │
│  │   - Sends events back to daemon                             │ │
│  │   - Intercepts crash signals                                │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                   Application Code                          │ │
│  └────────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

---

## Component Details

### Core Daemon (Rust)

Central orchestrator. Single long-running process that:
- Receives MCP requests from LLM
- Manages Frida sessions (spawn, attach, detach)
- Stores events in SQLite
- Handles dynamic trace adjustments
- Captures crash states

**Why Rust:**
- Performance for high event throughput
- Memory safety when dealing with process inspection
- Cross-platform (Linux/macOS, Windows future)
- Good Frida bindings (frida-rust)
- Async runtime (tokio) for concurrent operations

```
src/
├── main.rs                        # Entry point, CLI (daemon/mcp/install)
├── config.rs                      # File-based settings system
├── error.rs                       # Custom error types
├── daemon/
│   ├── server.rs                  # MCP tool dispatch, connection handling
│   └── session_manager.rs         # Session CRUD, DWARF cache, hook/watch state
├── mcp/
│   └── types.rs                   # MCP request/response types, validation
├── db/
│   ├── mod.rs                     # SQLite operations, batched writes
│   └── event.rs                   # Event schema, query builder
├── frida_collector/
│   └── spawner.rs                 # Frida FFI, process spawn, agent injection
├── dwarf/
│   └── parser.rs                  # DWARF parsing, function/variable extraction
├── symbols/
│   ├── mod.rs                     # SymbolResolver trait, Language enum
│   ├── dwarf_resolver.rs          # Native C/C++/Rust resolver (DWARF-based)
│   ├── python_resolver.rs         # Python resolver (AST-based via rustpython-parser)
│   └── js_resolver.rs             # JS/TS resolver (regex + source maps)
├── test/
│   ├── mod.rs                     # Test orchestration, async runs
│   ├── adapter.rs                 # TestAdapter trait
│   ├── cargo_adapter.rs           # Rust test adapter
│   ├── catch2_adapter.rs          # C++ test adapter
│   ├── pytest_adapter.rs          # Python pytest adapter
│   ├── unittest_adapter.rs        # Python unittest adapter
│   ├── vitest_adapter.rs          # JS/TS Vitest adapter
│   ├── jest_adapter.rs            # JS/TS Jest adapter
│   ├── bun_adapter.rs             # JS/TS Bun test adapter
│   ├── deno_adapter.rs            # JS/TS Deno test adapter
│   ├── go_adapter.rs              # Go test adapter
│   ├── gtest_adapter.rs           # C++ Google Test adapter
│   ├── mocha_adapter.rs           # JS/TS Mocha adapter
│   └── stuck_detector.rs          # Deadlock/hang detection
└── ui/
    ├── mod.rs                     # UI observation module
    ├── accessibility.rs           # macOS accessibility tree (AXUIElement)
    ├── accessibility_linux.rs     # Linux accessibility (AT-SPI)
    ├── capture.rs                 # Screenshot capture
    ├── vision.rs                  # Vision-based UI analysis
    ├── tree.rs                    # UI tree formatting
    └── merge.rs                   # Tree + vision merge
```

### Frida Agent (TypeScript)

Runs inside the target process. Compiled to JS, injected by Frida.

Responsibilities:
- Hook functions based on patterns from daemon
- Serialize arguments and return values (configurable depth 1-10)
- High-performance CModule tracing (native C callbacks, 10-50x faster than JS)
- Hot function detection with auto-sampling
- Watch variable reads at function entry/exit
- Intercept crash signals (exception handler captures registers, stack, frame memory)
- Runtime detection: auto-detects V8 (Node.js), JavaScriptCore (Bun), CPython, or native
- Language-specific tracers for each runtime
- Send events to daemon via Frida messaging

**Note:** stdout/stderr capture happens at the Frida Device level (not in the agent), using `FRIDA_STDIO_PIPE`. This works reliably with ASAN/sanitizer binaries.

```
agent/
├── src/
│   ├── agent.ts             # Main agent — runtime detection, message dispatch
│   ├── cmodule-tracer.ts    # High-perf native CModule tracing callbacks (10-50x faster)
│   ├── rate-tracker.ts      # Hot function detection and auto-sampling
│   └── tracers/
│       ├── native-tracer.ts   # C/C++/Rust — Interceptor + CModule hooks
│       ├── v8-tracer.ts       # Node.js — Module._compile + Proxy + ESM hooks
│       ├── jsc-tracer.ts      # Bun — JSObjectCallAsFunction + JSC C API multi-hook
│       └── python-tracer.ts   # Python — sys.monitoring (3.12+) / sys.settrace fallback
├── package.json
└── tsconfig.json
```

### Execution Index (SQLite)

Optimized for:
- High-throughput writes (WAL mode, batching)
- Time-range queries
- Function name search (FTS5)
- Call tree reconstruction

**Schema:**

```sql
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    timestamp_ns INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    thread_name TEXT,
    pid INTEGER,                     -- For multi-process sessions
    parent_event_id TEXT,
    event_type TEXT NOT NULL,        -- function_enter, function_exit, stdout, stderr, crash

    -- Location
    function_name TEXT,
    function_name_raw TEXT,
    source_file TEXT,
    line_number INTEGER,

    -- Payload
    arguments JSON,                  -- For function_enter
    return_value JSON,               -- For function_exit
    duration_ns INTEGER,             -- For function_exit
    text TEXT,                       -- For stdout/stderr events
    watch_values JSON,               -- Variable watch values
    sampled INTEGER,                 -- Hot function sampling flag

    -- Crash fields
    signal TEXT,
    fault_address TEXT,
    registers JSON,
    backtrace JSON
);

CREATE INDEX idx_session_time ON events(session_id, timestamp_ns);
CREATE INDEX idx_function ON events(function_name);
CREATE INDEX idx_source ON events(source_file);
```

---

## MCP Tool Definitions

### debug_launch (Phase 1a)

Start a new debug session. Process stdout/stderr are ALWAYS captured automatically (no tracing needed).

```typescript
debug_launch({
  command: string,              // Path to executable
  args?: string[],              // Command line arguments
  cwd?: string,                 // Working directory
  projectRoot: string,          // Root directory for user code detection
  env?: Record<string, string>  // Environment variables
}) → {
  sessionId: string,            // Human-readable: "myapp-2026-02-05-14h32"
  pid: number,
  pendingPatternsApplied?: number,  // Count of pre-staged patterns (if any)
  nextSteps?: string            // Recommended next action
}
```

**Notes:**
- `projectRoot` is required for user code detection via DWARF
- Session ID is human-readable for easy reference in conversation
- stdout/stderr automatically captured - check output before adding trace patterns
- No default tracing; add patterns with `debug_trace` only when needed
- `nextSteps` provides workflow guidance (e.g., "Query stderr/stdout with debug_query first")

### debug_stop (Phase 1a)

Stop a debug session.

```typescript
debug_stop({
  sessionId: string,
  retain?: boolean           // Keep data for post-mortem (default false)
}) → {
  success: boolean,
  eventsCollected: number
}
```

### debug_test (Phase 1d)

Start a test run asynchronously. Auto-detects framework, uses smart stuck detection, and switches between direct subprocess (fast) and Frida (instrumented) paths based on whether tracing is requested.

```typescript
debug_test({
  projectRoot: string,          // Required. Used for adapter detection
  framework?: string,           // Override: "cargo", "catch2"
  level?: "unit" | "integration" | "e2e",  // Filter test level
  test?: string,                // Run single test by name
  command?: string,             // Binary path (required for Catch2)
  tracePatterns?: string[],     // Presence triggers Frida path
  watches?: Watch[],            // Presence triggers Frida path
  env?: Record<string, string>
}) → {
  testRunId: string,            // Poll via debug_test_status
  status: "running",
  framework: string
}
```

Poll for progress and results:

```typescript
debug_test_status({ testRunId }) → {
  status: "running" | "completed" | "failed",
  sessionId?: string,           // Present on Frida path
  progress?: { elapsedMs, passed, failed, skipped, currentTest?, warnings? },
  result?: {
    framework: string,
    summary: { passed, failed, skipped, stuck?, durationMs },
    failures?: TestFailure[],
    stuck?: StuckTest[],
    details?: string,
    noTests?: boolean,
    project?: { language, buildSystem, testFiles },
    hint?: string
  },
  error?: string
}
```

**Typical TDD workflow:**

```typescript
// 1. Run full suite (direct subprocess, fast)
const result = await debug_test({ projectRoot: "/path/to/project" });
// → Adapter auto-detected, 47 passed, 2 failed

// 2. LLM sees failure, reruns with tracing (Frida path, automatic)
const traced = await debug_test({
  projectRoot: "/path/to/project",
  test: result.failures[0].name,
  tracePatterns: result.failures[0].suggested_traces
});
// → Same failure, but now we have a sessionId

// 3. LLM queries the trace events
const events = await debug_query({
  sessionId: traced.sessionId,
  function: { contains: "validate" },
  verbose: true
});
// → Found the bug
```

### debug_trace (Phase 1a)

Add or remove trace patterns and watch variables on a RUNNING session. No restart required.

**Recommended workflow:** Launch clean (no patterns) → check stderr/stdout → add targeted patterns only if needed.

```typescript
debug_trace({
  sessionId?: string,               // Omit for pending (pre-launch) mode, provide for runtime mode
  add?: string[],                   // Patterns to start tracing
  remove?: string[],                // Patterns to stop tracing
  serializationDepth?: number,      // Recursive argument depth (1-10, default 3)
  projectRoot?: string,             // For settings resolution
  watches?: {
    add?: WatchTarget[],            // Watch variables (max 32 per session)
    remove?: string[]               // Labels to remove
  }
}) → {
  mode: "pending" | "runtime",
  activePatterns: string[],
  hookedFunctions: number,
  matchedFunctions?: number,
  activeWatches: ActiveWatch[],
  warnings: string[],
  eventLimit: number,               // From settings
  status?: string
}
```

**Pattern Syntax:**
- `*` matches any characters except `::`
- `**` matches any characters including `::`
- `@usercode` expands to all functions in projectRoot

**Response Fields:**
- `mode: "pending"` - Patterns staged for next launch (without sessionId)
- `mode: "runtime"` - Patterns applied to running session (with sessionId)
- `status` - Actionable guidance based on current state (e.g., why hookedFunctions is 0, stability recommendations)

### debug_query (Phase 1a)

Query the unified execution timeline (function traces + stdout/stderr + crashes).

```typescript
debug_query({
  sessionId: string,
  eventType?: "function_enter" | "function_exit" | "stdout" | "stderr" | "crash" | "variable_snapshot",
  function?: {
    equals?: string,
    contains?: string,
    matches?: string         // Regex
  },
  sourceFile?: {
    equals?: string,
    contains?: string
  },
  returnValue?: {
    equals?: any,
    isNull?: boolean
  },
  threadName?: {
    contains?: string
  },
  timeFrom?: number | string,    // Absolute ns or "-5s", "-1m", "-500ms"
  timeTo?: number | string,
  minDurationNs?: number,        // Find slow functions
  pid?: number,                  // Filter by process ID
  limit?: number,                // Default 50, max 500
  offset?: number,
  verbose?: boolean              // Default false
}) → {
  events: TraceEvent[],
  totalCount: number,
  hasMore: boolean,
  pids?: number[]                // All PIDs (when multiple)
}
```

### debug_breakpoint (Phase 2)

Set or remove breakpoints. Supports function-level and line-level targeting.
See [active debugging spec](specs/2026-02-09-active-debugging.md) for full design.

```typescript
debug_breakpoint({
  sessionId: string,
  add?: [{
    function?: string,          // Pattern: "audio::processBlock", "MyClass::*"
    file?: string,              // Source file: "src/audio/processor.cpp"
    line?: number,              // Line number: 142
    condition?: string,         // JS expression: "args[0] > 100"
    hitCount?: number,          // Break on Nth hit
  }],
  remove?: string[],            // Breakpoint IDs to remove
}) → {
  breakpoints: [{
    id: string,
    function?: string,
    file?: string,
    line?: number,
    address: string,
  }]
}
```

### debug_logpoint (Phase 2)

Set or remove logpoints. Evaluate expressions without pausing.

```typescript
debug_logpoint({
  sessionId: string,
  add?: [{
    function?: string,
    file?: string,
    line?: number,
    message: string,            // Template: "tempo={args[0]}"
    condition?: string,
  }],
  remove?: string[],
}) → {
  logpoints: [{ id: string, address: string }]
}
```

### debug_continue (Phase 2)

Resume execution after breakpoint pause.

```typescript
debug_continue({
  sessionId: string,
  action?: "continue" | "step-over" | "step-into" | "step-out",
}) → {
  status: "paused" | "running" | "exited",
  breakpointId?: string,
  file?: string,
  line?: number,
  function?: string,
}
```

### debug_write (Phase 2)

Write to variables while paused or running.

```typescript
debug_write({
  sessionId: string,
  targets: [{
    variable?: string,          // "gTempo" (global) or "sampleRate" (local at breakpoint)
    address?: string,
    value: number | string | boolean,
    type?: string,              // "f64", "i32", "pointer", etc.
  }]
}) → {
  results: [{ variable?: string, address: string, previousValue: any, newValue: any }]
}
```

### debug_ui_tree (Phase 4)

Get the current unified UI tree (native accessibility + AI vision).

```typescript
debug_ui_tree({
  sessionId: string,
  verbose?: boolean              // Include bounding boxes (default false)
}) → {
  tree: string                   // Compact text format with stable element IDs
}
```

Computed on-demand (~30-60ms). Merges native accessibility (AXUIElement / AT-SPI2) with AI vision (YOLO + SigLIP for custom widgets). Returns compact format:
```
[window "My App" id=w1]
  [button "Play" id=btn_play enabled]
  [knob "Filter" value≈0.6 id=vk_3 source=vision]
```

### debug_ui_screenshot (Phase 4)

Capture current screenshot.

```typescript
debug_ui_screenshot({
  sessionId: string
}) → {
  screenshot: string             // Base64 PNG
}
```

### debug_ui_action (Phase 5)

Intent-based UI interaction. Motor layer handles widget-specific mechanics (VLM classification for unknown widgets, cached profiles for learned ones).

```typescript
debug_ui_action({
  sessionId: string,
  action: string,                // "click", "set_value", "type", "select", "scroll", "drag", "key"
  id?: string,                   // Target element by stable ID from tree
  value?: number | string,       // For set_value, type
  item?: string,                 // For select
  count?: number,                // For click (double-click = 2)
  direction?: string,            // For scroll
  amount?: number,               // For scroll
  from?: string,                 // For drag (element ID)
  to?: string,                   // For drag (element ID)
  key?: string,                  // For key
  modifiers?: string[]           // For key (e.g., ["cmd", "shift"])
}) → {
  success: boolean,
  element?: object               // Updated element state
}
```

### debug_channel_send (Phase 6)

Send stimulus to a non-UI I/O channel.

```typescript
debug_channel_send({
  sessionId: string,
  channel: string,               // e.g., "midi", "net:8080"
  action: object                 // Channel-specific action
}) → { success: boolean }
```

### debug_channel_query (Phase 6)

Query captured output from any I/O channel.

```typescript
debug_channel_query({
  sessionId: string,
  channel: string,               // e.g., "midi", "audio"
  filter?: object                // Channel-specific filter
}) → {
  events: object[]
}
```

### debug_test_scenario (Phase 6)

Run an autonomous test scenario. Flat action list executed sequentially. On failure, process stays alive and LLM can investigate.

```typescript
debug_test_scenario({
  sessionId: string,
  channels?: string[],           // Required channels (validated before run)
  steps: Step[]                  // Array of do/wait/assert steps
}) → {
  success: boolean,
  completed_steps: number,
  total_steps: number,
  // On failure:
  failed_step?: number,
  step?: object,                 // The failing step
  actual?: string                // What was actually observed
}
```

---

## Data Flow

### Normal Execution Tracing

```
1. LLM calls debug_launch({ command: "./myapp" })

2. Daemon spawns process via Frida
   - Frida agent injected
   - Agent initialized, no trace hooks installed

3. App runs, output captured (stdout/stderr)

4. LLM queries stderr/stdout; if needed, calls debug_trace({ add: ["process::*"] })

5. Hooks installed for matching functions

6. Traced function called: process_request(data)
   - Frida hook fires
   - Captures: timestamp, thread, function name, arguments
   - Serializes arguments to JSON
   - Sends message to daemon

7. Daemon receives message
   - Assigns event ID
   - Determines parent event (per-thread stack)
   - Writes to SQLite (batched for performance)

8. LLM calls debug_query({ function: { contains: "process" } })

9. Daemon queries SQLite, returns matching events
```

### Dynamic Trace Adjustment

```
1. App running with output capture only (no tracing)

2. LLM calls debug_trace({ add: ["tokio::*"] })

3. Daemon sends message to Frida agent

4. Agent installs new hooks for tokio:: functions
   - No process restart
   - Existing hooks unaffected

5. Future tokio:: calls now captured
```

### Crash Capture

```
1. App running, bug triggered

2. SIGSEGV signal raised (null pointer dereference)

3. Frida agent intercepts signal (before default handler)
   - Captures stack trace
   - Captures register state
   - Reads local variables from stack (using DWARF)
   - Gets last N events from buffer

4. Agent sends crash event to daemon

5. Daemon stores crash event in SQLite

6. Default signal handler runs, process terminates

7. LLM queries: debug_query({ eventType: "crash" })
   - Gets full crash context
   - Sees events leading up to crash
   - Can identify root cause
```

---

## Performance Considerations

### Event Throughput

Target: Handle 100k+ events/second without dropping.

Strategies:
- **Batched writes:** Frida agent batches messages (every 10ms or 1000 events)
- **WAL mode:** SQLite WAL for concurrent read/write
- **Async processing:** Daemon uses tokio for non-blocking I/O
- **Bounded buffers:** Ring buffer in agent, oldest events dropped under pressure

### Serialization Overhead

Deep struct serialization is expensive. Mitigations:
- **Depth limit:** Default depth 1, configurable per-pattern
- **Size limit:** Truncate large strings/arrays
- **Lazy serialization:** Only serialize if event matches active patterns

### Memory in Target Process

Frida agent adds memory overhead. Minimized by:
- **Selective hooking:** When tracing, default to user code only
- **Streaming events:** Don't buffer large amounts in agent
- **Compiled agent:** TypeScript compiled to optimized JS

---

## Extensibility

The architecture is designed for easy extension. Three clear interfaces:

### Adding Language Support

Multi-language support is implemented via two extension points:

**1. Symbol Resolver** (Rust-side, `src/symbols/`) — resolves trace patterns to source locations:

```rust
pub trait SymbolResolver: Send + Sync {
    fn resolve_pattern(&self, pattern: &str, root: &Path) -> Result<Vec<ResolvedTarget>>;
    fn resolve_variable(&self, name: &str) -> Option<VariableResolution>;
}
```

**2. Tracer** (Agent-side, `agent/src/tracers/`) — installs hooks in the target runtime:

```typescript
interface Tracer {
    installHook(target: HookTarget, mode: HookMode): void;
    removeHook(target: HookTarget): void;
    syncTraceHooks(): void;
}
```

**To add a new language:**
1. Implement `SymbolResolver` in Rust for pattern resolution
2. Implement a `Tracer` in TypeScript for runtime hook installation
3. Add language detection in `session_manager.rs::detect_language`
4. Wire the resolver in `SessionManager::create_session`

**Current implementations:**

| Language | Resolver | Tracer | Status |
|----------|----------|--------|--------|
| C/C++/Rust | `DwarfResolver` (DWARF) | `NativeTracer` (Interceptor + CModule) | Full |
| Python (3.12+) | `PythonResolver` (AST) | `PythonTracer` (sys.monitoring + settrace fallback) | Full — dual-mode tracing, name+line matching, tool ID retry |
| Python (3.11) | `PythonResolver` (AST) | `PythonTracer` (sys.settrace) | Full — settrace with ±5 line tolerance for decorators |
| JavaScript (Node.js) | `JsResolver` (regex + source maps) | `V8Tracer` (Module._compile + Proxy) | Full — ESM via `module.registerHooks()` source transform |
| JavaScript (Bun) | `JsResolver` (regex + source maps) | `JscTracer` (JSObjectCallAsFunction + JSC C API) | Output capture only — Bun's release binaries strip JSC symbols |
| Go | `DwarfResolver` (DWARF) | `NativeTracer` | Basic (DWARF only, no goroutine awareness) |
| Java/Kotlin | ART hooks (Android) | Future phase |

> **Note:** The docs previously described a `Collector` trait and `CollectorManager` abstraction. In practice, all language support is built on Frida as the single runtime backend, with language-specific `SymbolResolver` + `Tracer` pairs providing the abstraction. A CDP (Chrome DevTools Protocol) collector for standalone JS debugging is a future possibility but not currently implemented.

### Adding an I/O Channel (Phase 6+)

I/O channels handle app input/output beyond function tracing. Each channel implements `InputChannel`, `OutputChannel`, or both:

```rust
pub trait InputChannel: Send + Sync {
    fn name(&self) -> &str;
    fn send(&self, action: ChannelAction) -> Result<ActionResult>;
}

pub trait OutputChannel: Send + Sync {
    fn name(&self) -> &str;
    fn start_capture(&self) -> Result<()>;
    fn stop_capture(&self) -> Result<()>;
    fn query(&self, filter: OutputFilter) -> Result<Vec<ChannelEvent>>;
}
```

**To add a new I/O channel (e.g., serial, custom protocol):**
1. Implement `InputChannel` and/or `OutputChannel` traits
2. Register with channel registry
3. Channel automatically works with `debug_channel_send`, `debug_channel_query`, and `debug_test_scenario`

**Current/planned channels:**

| Channel | Input | Output | Phase |
|---------|-------|--------|-------|
| `stdio` | stdin injection | stdout/stderr (existing) | 1a (wrapped in 6) |
| `trace` | pattern management (existing) | function events (existing) | 1a (wrapped in 6) |
| `ui` | CGEvent / XTest | AX + Vision | 4-5 |
| `midi` | CoreMIDI / ALSA | CoreMIDI / ALSA | 7 |
| `audio` | Virtual device | CoreAudio / JACK | 7 |
| `net` | Socket | Frida intercept | 7 |
| `file` | Filesystem ops | FSEvents / inotify | 7 |

### Adding a New Test Framework Adapter

Test adapters own the full lifecycle: detection, command construction, output parsing, rerun commands, trace suggestions, and language-aware stack capture. Each adapter normalizes its framework's output into a universal structured format.

```rust
pub trait TestAdapter: Send + Sync {
    /// Scan projectRoot for signals. Returns 0-100 confidence. Highest wins.
    fn detect(&self, project_root: &Path) -> u8;

    /// Human-readable name: "cargo", "catch2", "generic"
    fn name(&self) -> &str;

    /// Build command for running tests. `level` filters to unit/integration/e2e.
    fn suite_command(&self, project_root: &Path, level: Option<TestLevel>, env: &HashMap<String, String>) -> TestCommand;

    /// Build command for running a single test by name.
    fn single_test_command(&self, project_root: &Path, test_name: &str) -> TestCommand;

    /// Parse raw stdout + stderr into structured results.
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult;

    /// Given a failure, suggest trace patterns for instrumented rerun.
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;

    /// Capture thread stacks for stuck detection. Language-aware.
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack>;
}
```

**To add a new test framework:**
1. Implement `TestAdapter` trait
2. Add detection logic (`detect()` returns confidence 0-100, highest wins)
3. Implement command construction for suite + single test + test levels
4. Implement output parsing (JSON/XML/text → universal `TestResult`)
5. Implement `capture_stacks` using the best method for the language runtime
6. The adapter is auto-discovered — no registration needed

**Built-in adapters:**

| Framework | Language | Detection | Confidence | Output Format | Stack Capture |
|-----------|----------|-----------|------------|---------------|---------------|
| cargo test | Rust | `Cargo.toml` | 90 | JSON (`--format json`) | OS-level (native) |
| Catch2 | C/C++ | `--list-tests` probe | 85 | XML (`--reporter xml`) | OS-level (native) |
| pytest | Python | `pytest.ini`, `pyproject.toml`, `conftest.py` | 90 | JSON (`--json-report`) | OS-level |
| unittest | Python | `test_*.py` files | 70 | Text (parsed) | OS-level |
| Vitest | JS/TS | `vitest.config.*` or `package.json` | 95 | JSON (`--reporter=json`) | OS-level |
| Jest | JS/TS | `jest.config.*` or `package.json` | 92 | JSON (`--json`) | OS-level |
| Bun test | JS/TS | `bun.lockb` or `package.json` | 85 | JUnit XML (`--reporter=junit`) | OS-level |
| Deno test | JS/TS | `deno.json`/`deno.jsonc` | 90 | JSON | OS-level |
| go test | Go | `go.mod` | 90 | JSON (`-json`) | OS-level |
| Google Test | C++ | `gtest` in CMakeLists.txt | 85 | XML (`--gtest_output=xml`) | OS-level (native) |
| Mocha | JS/TS | `.mocharc.*` or `mocha` in `package.json` | 90 | JSON (`--reporter json`) | OS-level |

### Event Schema (Shared by All Collectors)

All collectors emit the same event types. Key fields:

**Identity & Timing:**
- `id` - Unique event identifier
- `session_id` - Groups events by debug session
- `timestamp_ns` - Monotonic nanoseconds for ordering
- `parent_event_id` - For call tree reconstruction

**Thread & Process:**
- `thread_id` - Numeric thread identifier
- `thread_name` - Human-readable name if available
- `pid` - Process ID (for fork/exec tracking)

**Location:**
- `function_name` - Demangled function name
- `function_name_raw` - Original mangled name
- `source_file` - Source file path
- `source_line` - Line number

**Event Data:**
- `event_type` - One of: function_enter, function_exit, stdout, stderr, crash
- `arguments` - Serialized arguments (depth-limited, cycle-detected)
- `return_value` - Serialized return value
- `duration_ns` - Execution time (exit events only)
- `text` - Output text (stdout/stderr events only)
- `crash_info` - Stack trace, registers, locals (crash events only)

**Sampling Metadata:**
- `sampled` - True if this event was captured via sampling
- `sample_rate` - If sampled, what percentage was captured

This unified schema means the query engine, storage, and MCP interface don't care which collector produced the events.

---

## Security Model

### Process Permissions

- Daemon runs as user, can only debug user-owned processes
- Cannot debug setuid binaries (OS restriction)
- Cannot debug processes owned by other users

### Data Handling

- All data stored locally in SQLite
- No network calls in open source version
- Session data can be deleted via `debug_stop`

---

## Future: I/O Channels & Scenario Runner (Phase 6-7)

UI, MIDI, audio, network, files, stdout/stderr, and function traces are all unified as I/O channels. Each implements `InputChannel` (send stimulus) and/or `OutputChannel` (capture output). The scenario runner (`debug_test_scenario`) executes flat action lists across any combination of channels — enabling fully autonomous runtime testing.

See [specs/2026-02-07-ui-observation-interaction-io-channels.md](specs/2026-02-07-ui-observation-interaction-io-channels.md) for full design.

### Channel Implementations (Phase 7)

| Channel | Input | Output | Platform |
|---------|-------|--------|----------|
| `midi` | CoreMIDI / ALSA virtual port | CoreMIDI / ALSA capture | macOS + Linux |
| `audio` | Virtual device | CoreAudio tap / JACK | macOS + Linux |
| `net` | Socket from daemon | Frida socket intercept | Cross-platform |
| `file` | Filesystem ops | FSEvents / inotify | macOS + Linux |

---

## Future: Additional Languages & Runtimes

### Current Language Status

| Language | Tracing | Test Runner | Status |
|----------|---------|-------------|--------|
| C/C++/Rust | Full (Frida native hooks) | Cargo + Catch2 | Shipped |
| Python | Infrastructure (sys.settrace pending) | Pytest + Unittest | Shipped (output capture), tracing pending |
| JavaScript (Node.js) | Full (V8 tracer) | Jest + Vitest | Shipped |
| JavaScript (Bun) | Partial (JSC tracer) | Bun test | Shipped (single-hook limitation) |

### Planned

- **Deno** — Language detection exists; needs V8-based tracer adaptation + `deno test` adapter
- **Go** — Enhanced DWARF support, goroutine awareness, `go test` adapter
- **Java/Kotlin** — Via ART hooks on Android

### CDP Collector (Future Alternative)

Chrome DevTools Protocol could provide an alternative to Frida-based JS tracing for Node.js, browser apps, and Electron. Would connect via `--inspect` flag. Not currently implemented — the V8Tracer approach via Frida covers the primary use cases.
