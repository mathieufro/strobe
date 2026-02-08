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
| `debug_launch` (general) | User code | Broad observation for unknown bugs |
| `debug_test` (test run) | Minimal/none | Wait for failure, then trace targeted |
| `debug_test` (rerun failed) | Suggested patterns | Stack trace tells us what to trace |

**User code heuristic:** Trace functions whose source file is within the project directory.

| Language | Debug Info | How We Determine Source |
|----------|-----------|------------------------|
| C/C++ | DWARF | `DW_AT_decl_file` |
| Rust | DWARF | Same |
| Go | DWARF | Same |
| TS/JS (future) | Source maps | `sources` array |

**Traced:** `src/main.rs`, `lib/utils.c`
**Not traced:** `/usr/lib/libc.so`, `~/.cargo/registry/...`

The LLM can expand or narrow scope at runtime via `debug_trace`.

### Test Instrumentation: First-Class TDD Support
Test debugging is a core workflow, not an afterthought. The `debug_test` tool:

1. Runs test suite with minimal tracing overhead
2. On failure, returns structured data with **rule-based hints**
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

1. App running with default tracing
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
- File-based patterns: `file:src/render/*.rs`

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
- **Configurable:** Override via `~/.strobe/config.toml`

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
│  debug_launch | debug_trace | debug_query | debug_stop          │
│  debug_test | debug_test_status | debug_read (planned)          │
│  debug_list_sessions | debug_delete_session                     │
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
│  │   - Captures stdout/stderr via write(2) interception       │ │
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
└── test/
    ├── mod.rs                     # Test orchestration, async runs
    ├── adapter.rs                 # TestAdapter trait
    ├── cargo_adapter.rs           # Rust test adapter
    ├── catch2_adapter.rs          # C++ test adapter
    └── stuck_detector.rs          # Deadlock/hang detection
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
- Send events to daemon via Frida messaging

**Note:** stdout/stderr capture happens at the Frida Device level (not in the agent), using `FRIDA_STDIO_PIPE`. This works reliably with ASAN/sanitizer binaries.

```
agent/
├── src/
│   ├── agent.ts             # Main agent — hook installation, output capture, message handling
│   ├── cmodule-tracer.ts    # High-perf native CModule tracing callbacks (10-50x faster)
│   └── rate-tracker.ts      # Hot function detection and auto-sampling
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

Run tests with universal structured output. Auto-detects framework, smart stuck detection, automatic switching between direct subprocess (fast) and Frida (instrumented) paths.

```typescript
debug_test({
  projectRoot: string,          // Required. Used for adapter detection
  framework?: string,           // Override: "cargo", "catch2"
  level?: "unit" | "integration" | "e2e",  // Filter test level
  test?: string,                // Run single test by name
  command?: string,             // Binary path (required for Catch2)
  tracePatterns?: string[],     // Presence triggers Frida path
  watches?: Watch[],            // Presence triggers Frida path
  env?: Record<string, string>,
  timeout?: number              // Hard timeout ms (default per level)
}) → {
  framework: string,
  summary: { passed, failed, skipped, stuck?, duration_ms },
  failures?: TestFailure[],
  stuck?: StuckTest[],          // Deadlocks/infinite loops caught by detector
  sessionId?: string,           // Present on Frida path (for debug_query)
  details: string,              // Path to full details temp file
  // When no tests exist:
  no_tests?: boolean,
  project?: { language, build_system },
  hint?: string
}

interface TestFailure {
  name: string,
  file?: string,
  line?: number,
  message: string,
  suggested_traces: string[]
}

interface StuckTest {
  name: string,
  elapsed_ms: number,
  diagnosis: string,            // "Deadlock: 0% CPU, identical stacks"
  threads: { name: string, stack: string[] }[],
  suggested_traces: string[]
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

Set a conditional breakpoint.

```typescript
debug_breakpoint({
  sessionId: string,
  function: string,          // Function pattern
  condition?: {
    field: string,           // e.g., "args[0].length"
    equals?: any,
    greaterThan?: number,
    lessThan?: number
  },
  hitCount?: number          // Break on Nth hit
}) → {
  breakpointId: string
}
```

### debug_inspect (Phase 2)

Inspect state at a breakpoint or historical event.

```typescript
debug_inspect({
  sessionId: string,
  eventId?: string,          // Inspect at historical event
  expression: string         // e.g., "config.sample_rate"
}) → {
  value: any,
  type: string
}
```

### debug_continue (Phase 2)

Resume execution after breakpoint.

```typescript
debug_continue({
  sessionId: string
}) → { success: boolean }
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
   - Agent reads DWARF, identifies user code functions
   - Hooks installed on user code

3. App runs, function called: process_request(data)

4. Frida hook fires
   - Captures: timestamp, thread, function name, arguments
   - Serializes arguments to JSON
   - Sends message to daemon

5. Daemon receives message
   - Assigns event ID
   - Determines parent event (per-thread stack)
   - Writes to SQLite (batched for performance)

6. LLM calls debug_query({ function: { contains: "process" } })

7. Daemon queries SQLite, returns matching events
```

### Dynamic Trace Adjustment

```
1. App running with user code tracing

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
- **Selective hooking:** Only hook user code by default
- **Streaming events:** Don't buffer large amounts in agent
- **Compiled agent:** TypeScript compiled to optimized JS

---

## Extensibility

The architecture is designed for easy extension. Three clear interfaces:

### Adding a New Collector (Language Support)

Collectors handle runtime instrumentation. Each collector implements the `Collector` trait:

```rust
pub trait Collector: Send + Sync {
    fn attach(&mut self, target: &Target) -> Result<SessionId>;
    fn detach(&mut self, session: SessionId) -> Result<()>;
    fn set_trace_patterns(&mut self, session: SessionId, patterns: Vec<Pattern>) -> Result<()>;
    fn set_breakpoint(&mut self, session: SessionId, bp: Breakpoint) -> Result<BreakpointId>;
    fn resume(&mut self, session: SessionId) -> Result<()>;
    fn poll_events(&mut self, session: SessionId) -> Result<Vec<TraceEvent>>;
}
```

**To add a new language:**
1. Implement `Collector` trait
2. Emit events conforming to `TraceEvent` schema
3. Register with `CollectorManager`

**Current/planned collectors:**

| Collector | Languages | Implementation |
|-----------|-----------|----------------|
| Frida | C/C++/Rust/Go/native | `frida-rust` bindings |
| CDP | JavaScript/TypeScript | WebSocket to Chrome DevTools |
| Python | Python | `sys.settrace` or Frida |

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

| Framework | Language | Detection | Output Format | Stack Capture |
|-----------|----------|-----------|---------------|---------------|
| cargo test | Rust | `Cargo.toml` (confidence 90) | JSON (`--format json`) | OS-level (native) |
| Catch2 | C/C++ | `--list-tests` probe (confidence 85) | XML (`--reporter xml`) | OS-level (native) |
| Generic | Any | Always (confidence 1, fallback) | Regex heuristics | OS-level best-effort |

**Future adapters (community contributions):**

| Framework | Language | Detection | Stack Capture |
|-----------|----------|-----------|---------------|
| pytest | Python | `pytest.ini`, `pyproject.toml` | `py-spy dump --pid` |
| Jest/Vitest | JS/TS | `jest.config.*`, `vitest.config.*` | SIGUSR1 + inspector |
| go test | Go | `go.mod` | SIGABRT → goroutine dump |
| Google Test | C/C++ | `gtest` in CMakeLists | OS-level (native) |

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

## Future: Additional Languages (Phase 9)

### CDP Collector (JavaScript/TypeScript)

Chrome DevTools Protocol for Node.js, browser apps, Electron.

Will share:
- Same MCP interface
- Same SQLite storage
- Same event schema
- Same I/O channel model

Different:
- Uses CDP instead of Frida
- Connects to Node.js --inspect or Chrome remote debugging
- Native async/await tracing instead of function hooks

The LLM uses the same tools regardless of whether target is native or JS.

### Other Languages

- Python (via sys.settrace or Frida)
- Go (enhanced DWARF support, goroutine awareness)
- Java/Kotlin (via ART hooks on Android)
