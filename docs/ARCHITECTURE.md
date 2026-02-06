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
- **Structs:** Serialized to depth 1 by default (configurable)
- **Arrays:** First N elements (configurable, default 100)

The LLM can request deeper serialization for specific functions via `debug_trace({ depth: 2 })`.

### VS Code Extension: Phase 3, Not Core
The primary interface is MCP for LLMs. VS Code extension is for:
- Distribution (one-click install)
- Human observability (see what the LLM sees)
- Future monetization

Core functionality must work without VS Code.

### Contributor-Friendly Extensibility
The architecture must be clean enough that **anyone can add support for an obscure language or test framework** without understanding the whole system.

Two clear extension points:
1. **Collectors** - Add language support by implementing one trait
2. **Test Adapters** - Add test framework support by implementing one trait

Both emit to unified schemas. Contributors don't need to touch:
- Storage layer
- Query engine
- MCP interface
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
- **Default depth:** 1 level (LLM can request deeper via `debug_trace`)
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
│  debug_launch | debug_query | debug_trace | debug_breakpoint    │
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
core/
├── src/
│   ├── main.rs              # Entry point, CLI
│   ├── daemon.rs            # Main event loop
│   ├── mcp/
│   │   ├── server.rs        # MCP protocol handling
│   │   └── tools.rs         # Tool implementations
│   ├── storage/
│   │   ├── index.rs         # SQLite operations
│   │   └── schema.rs        # Event schema
│   ├── collector/
│   │   ├── manager.rs       # Manages Frida sessions
│   │   ├── frida.rs         # Frida-specific logic
│   │   └── symbols.rs       # DWARF/PDB parsing for user code detection
│   └── crash/
│       └── handler.rs       # Crash capture logic
└── Cargo.toml
```

### Frida Agent (TypeScript)

Runs inside the target process. Compiled to JS, injected by Frida.

Responsibilities:
- Hook functions based on patterns from daemon
- Serialize arguments and return values
- Capture stdout/stderr by intercepting `write(2)` syscall (with re-entrancy guard and 50MB per-session limit)
- Send events to daemon via Frida messaging
- Intercept crash signals (SIGSEGV, SIGABRT)
- Capture crash state before process dies

```
frida-agent/
├── src/
│   ├── index.ts             # Entry point
│   ├── hooks.ts             # Function hooking logic
│   ├── serialize.ts         # Argument serialization
│   ├── crash.ts             # Signal interception
│   └── symbols.ts           # Symbol resolution helpers
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
    session_id TEXT NOT NULL,        -- Groups events by debug session
    timestamp_ns INTEGER NOT NULL,   -- Nanoseconds since session start
    thread_id INTEGER NOT NULL,
    parent_event_id TEXT,            -- For call tree reconstruction
    event_type TEXT NOT NULL,        -- function_enter, function_exit, stdout, stderr, crash

    -- Location
    module TEXT,
    function TEXT,
    source_file TEXT,
    line_number INTEGER,
    address TEXT,

    -- Payload (JSON)
    arguments JSON,                  -- For function_enter
    return_value JSON,               -- For function_exit
    duration_ns INTEGER,             -- For function_exit
    text TEXT,                       -- For stdout/stderr events
    crash_info JSON                  -- For crash events
);

-- Performance indexes
CREATE INDEX idx_session_time ON events(session_id, timestamp_ns);
CREATE INDEX idx_function ON events(function);
CREATE INDEX idx_type ON events(event_type);
CREATE INDEX idx_parent ON events(parent_event_id);

-- Full-text search on function names
CREATE VIRTUAL TABLE events_fts USING fts5(
    function,
    source_file,
    content=events,
    content_rowid=rowid
);
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
  sessionId: string
}) → { success: boolean }
```

### debug_test (Phase 1d)

Run tests with optional tracing. Core feature for TDD workflows.

```typescript
debug_test({
  command: string,             // e.g., "cargo test", "pytest", "npm test"
  test?: string,               // Specific test to run (for rerun)
  suite?: string,              // Specific suite/file
  tracePatterns?: string[],    // Patterns to trace (default: minimal)
  cwd?: string,
  env?: Record<string, string>
}) → {
  passed: number,
  failed: number,
  skipped: number,
  duration_ms: number,
  failures: TestFailure[],
  // If tracePatterns provided:
  sessionId?: string,          // For querying trace events
  events?: TraceEvent[]        // Captured events (if tracing enabled)
}

interface TestFailure {
  test: string,                // Test name
  file: string,                // File path
  line: number,
  error: string,               // Error message
  stackTrace: string[],        // Call stack
  // Rule-based hints (no AI needed):
  suggestedTrace: string[],    // Patterns extracted from stack trace
  rerunCommand: string         // Command to rerun just this test
}
```

**Typical TDD workflow:**

```typescript
// 1. Run full suite (minimal tracing)
const result = await debug_test({ command: "cargo test" });
// → 47 passed, 2 failed

// 2. LLM sees failure, uses hints to rerun with tracing
const traced = await debug_test({
  command: "cargo test",
  test: result.failures[0].test,
  tracePatterns: result.failures[0].suggestedTrace
});
// → Same failure, but now we have trace events

// 3. LLM queries the events
const events = await debug_query({
  sessionId: traced.sessionId,
  function: { contains: "validate" }
});
// → Found the bug
```

### debug_trace (Phase 1a)

Add or remove trace patterns on a RUNNING session. No restart required.

**Recommended workflow:** Launch clean (no patterns) → check stderr/stdout → add targeted patterns only if needed.

```typescript
debug_trace({
  sessionId?: string,        // Omit for pending (pre-launch) mode, provide for runtime mode
  add?: string[],            // Patterns to start tracing
  remove?: string[],         // Patterns to stop tracing
  depth?: number             // Serialization depth (Phase 1b, ignored in 1a)
}) → {
  mode: "pending" | "runtime",   // Indicates pre-launch vs. running session
  activePatterns: string[],      // Current trace patterns
  hookedFunctions: number,       // Actual hooked function count (0 if pending or no matches)
  matchedFunctions?: number,     // If different from hookedFunctions (e.g., crash during install)
  status?: string                // Contextual guidance (e.g., troubleshooting for 0 hooks)
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

Query the unified execution timeline (function traces + stdout/stderr).

```typescript
debug_query({
  sessionId: string,
  eventType?: "function_enter" | "function_exit" | "stdout" | "stderr" | "crash",
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
  duration?: {
    greaterThan?: string,    // e.g., "10ms"
    lessThan?: string
  },
  timeRange?: {
    start?: string,          // e.g., "-5s" or timestamp
    end?: string
  },
  limit?: number,            // Default 100
  offset?: number
}) → {
  events: TraceEvent[],
  totalCount: number
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

### debug_ui_state (Phase 4)

Capture current UI state.

```typescript
debug_ui_state({
  sessionId: string,
  screenshot?: boolean,        // Default true
  accessibilityTree?: boolean  // Default true
}) → {
  screenshot?: string,         // Base64 PNG
  accessibilityTree?: UIElement
}

// UIElement structure
interface UIElement {
  role: string,                // button, textfield, list, etc.
  name?: string,               // Accessible name
  value?: string,              // Current value (for inputs)
  bounds: { x: number, y: number, width: number, height: number },
  actions: string[],           // Available actions: click, type, scroll
  children?: UIElement[]
}
```

### debug_ui_action (Phase 5)

Interact with the UI.

```typescript
debug_ui_action({
  sessionId: string,
  action: "click" | "type" | "scroll" | "drag" | "key",
  target?: {
    name?: string,             // By accessible name
    coordinates?: { x: number, y: number }
  },
  // For type action:
  value?: string,
  // For scroll action:
  direction?: "up" | "down" | "left" | "right",
  amount?: number,             // Pixels
  // For drag action:
  from?: { x: number, y: number },
  to?: { x: number, y: number },
  // For key action:
  key?: string                 // e.g., "Enter", "Ctrl+S"
}) → { success: boolean }
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

The architecture is designed for easy extension. Two clear interfaces:

### Adding a New Collector (Language Support)

Collectors handle runtime instrumentation. Each collector implements the `Collector` trait:

```rust
pub trait Collector: Send + Sync {
    /// Attach to a running process or spawn a new one
    fn attach(&mut self, target: &Target) -> Result<SessionId>;

    /// Detach and cleanup
    fn detach(&mut self, session: SessionId) -> Result<()>;

    /// Update tracing patterns at runtime
    fn set_trace_patterns(&mut self, session: SessionId, patterns: Vec<Pattern>) -> Result<()>;

    /// Set a breakpoint (Phase 2+)
    fn set_breakpoint(&mut self, session: SessionId, bp: Breakpoint) -> Result<BreakpointId>;

    /// Resume after breakpoint
    fn resume(&mut self, session: SessionId) -> Result<()>;

    /// Receive events (called by collector manager)
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

### Adding a New Test Framework Adapter

Test adapters parse framework output into our unified `TestFailure` schema:

```rust
pub trait TestAdapter: Send + Sync {
    /// Check if this adapter handles the given project
    fn detect(&self, project_path: &Path) -> Option<DetectedFramework>;

    /// Get the command to run tests
    fn run_command(&self, config: &TestConfig) -> String;

    /// Get the command to run a single test
    fn rerun_command(&self, test_name: &str) -> String;

    /// Parse test output into unified schema
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult;

    /// Extract trace hints from failure
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;
}

pub struct TestResult {
    pub passed: u32,
    pub failed: u32,
    pub skipped: u32,
    pub duration_ms: u64,
    pub failures: Vec<TestFailure>,
}

pub struct TestFailure {
    pub test: String,
    pub file: String,
    pub line: u32,
    pub error: String,
    pub stack_trace: Vec<String>,
    pub suggested_trace: Vec<String>,  // From suggest_traces()
    pub rerun_command: String,          // From rerun_command()
}
```

**To add a new test framework:**
1. Implement `TestAdapter` trait
2. Add detection logic (look for config files, etc.)
3. Implement output parsing (JSON/XML/text)
4. Register with `TestAdapterRegistry`

**Current/planned adapters:**

| Framework | Language | Detection | Output Format |
|-----------|----------|-----------|---------------|
| cargo test | Rust | `Cargo.toml` | JSON (`--format json`) |
| Google Test | C/C++ | `gtest` in CMakeLists | XML |
| Catch2 | C/C++ | `catch.hpp` includes | XML/JSON |
| CTest | C/C++ | `CMakeLists.txt` | JSON |
| pytest | Python | `pytest.ini`, `pyproject.toml` | JSON (plugin) |
| Jest | JS/TS | `jest.config.*` | JSON |
| go test | Go | `go.mod` | JSON (`-json`) |

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
- `event_type` - One of: function_enter, function_exit, stdout, stderr, crash, log
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

## Future: Smart Test Integration (Phase 6)

Language-specific test framework adapters that normalize output into our unified `TestFailure` schema.

### Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Test Framework Adapters                       │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐             │
│  │  Rust       │  │  C/C++      │  │  Python     │  ...        │
│  │  cargo test │  │  gtest/catch│  │  pytest     │             │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘             │
│         │                │                │                      │
│         └────────────────┼────────────────┘                      │
│                          ▼                                       │
│              ┌───────────────────────┐                          │
│              │   Unified TestFailure │                          │
│              │   Schema + Hints      │                          │
│              └───────────────────────┘                          │
└─────────────────────────────────────────────────────────────────┘
```

### Supported Frameworks

| Language | Framework | Output Format | Notes |
|----------|-----------|---------------|-------|
| Rust | cargo test | `--format json` | Native JSON support |
| C/C++ | Google Test | `--gtest_output=xml` | XML parsing required |
| C/C++ | Catch2 | `-r xml` or `-r json` | JSON in v3+ |
| C/C++ | CTest | `--output-json` | CMake integration |
| Python | pytest | `--json-report` | Plugin required |
| JS/TS | Jest | `--json` | Native JSON support |
| Go | go test | `-json` | Native JSON support |

### Setup Skill

```typescript
debug_setup_tests({
  language?: string,     // Auto-detect from project files
  framework?: string     // Preferred framework (optional)
}) → {
  detected: { language: string, framework: string },
  steps: string[],       // Setup instructions
  configChanges: Record<string, string>,
  runCommand: string,
  rerunTemplate: string  // e.g., "cargo test {name}"
}
```

The setup skill examines project files (Cargo.toml, CMakeLists.txt, package.json, etc.) and provides configuration guidance.

---

## Future: CDP Collector (Phase 7+)

For JavaScript/TypeScript debugging via Chrome DevTools Protocol.

Will share:
- Same MCP interface
- Same SQLite storage
- Same event schema

Different:
- Uses CDP instead of Frida
- Connects to Node.js --inspect or Chrome remote debugging
- Native async/await tracing instead of function hooks

The LLM uses the same tools regardless of whether target is native or JS.
