# Phase 1a: Tracing Foundation

**Status:** Spec Complete
**Goal:** Prove the core concept works. LLM can launch a program, add targeted traces, observe execution, and query what happened.

---

## Overview

Phase 1a establishes the foundational tracing infrastructure. By the end of this phase, an LLM can:

1. Launch a binary with Frida attachment
2. Add trace patterns for specific functions
3. Query captured events
4. Stop the session

No code changes to the target binary. No recompilation. Dynamic observation.

---

## Daemon Architecture

### Lifecycle

- **Single global daemon** per user
- **Lazy start**: First MCP call spawns daemon if not running
- **Auto-shutdown**: Terminates after 30 minutes idle
- **Location**: `~/.strobe/`

### Files

| Path | Purpose |
|------|---------|
| `~/.strobe/strobe.sock` | Unix socket for daemon communication |
| `~/.strobe/strobe.db` | SQLite database (WAL mode) |
| `~/.strobe/config.toml` | User configuration (future) |
| `~/.strobe/strobe.pid` | Daemon PID file |

### MCP Transport

```
LLM → stdio → strobe mcp (proxy) → Unix socket → strobe daemon
```

The `strobe mcp` command is a thin stdio proxy that connects to the persistent daemon. This provides maximum MCP client compatibility while allowing the daemon to outlive individual connections.

---

## MCP Tools

### debug_launch

Start a new debug session.

```typescript
debug_launch({
  command: string,              // Path to executable
  args?: string[],              // Command line arguments
  cwd?: string,                 // Working directory (default: directory of command)
  projectRoot: string,          // Root directory for user code detection
  env?: Record<string, string>  // Additional environment variables
}) → {
  sessionId: string,            // Human-readable: "myapp-2026-02-05-14h32"
  pid: number                   // Process ID of launched binary
}
```

**Behavior:**
- Spawns process via Frida
- Parses DWARF debug info to identify functions
- Returns immediately after process starts (does not wait for exit)
- Process stdout/stderr are automatically captured and queryable as events
- Use `debug_trace` to add patterns after launch (or before for pending patterns)

**Session ID Format:**
- Human-readable: `{binary-name}-{YYYY-MM-DD}-{HHhMM}`
- Collision handling: append `-2`, `-3`, etc. if needed
- Example: `myapp-2026-02-05-14h32`, `myapp-2026-02-05-14h32-2`

### debug_trace

Query, add, or remove trace patterns at runtime.

```typescript
debug_trace({
  sessionId: string,
  add?: string[],               // Patterns to start tracing
  remove?: string[]             // Patterns to stop tracing
}) → {
  activePatterns: string[],     // Current trace patterns
  hookedFunctions: number       // Total hooked function count
}
```

**Query mode:** Call with no `add`/`remove` to get current state:
```typescript
debug_trace({ sessionId: "myapp-2026-02-05-14h32" })
→ { activePatterns: ["auth::*", "form::*"], hookedFunctions: 24 }
```

**Pattern Syntax:**
- Glob-style matching
- `*` matches any characters except `::`
- `**` matches any characters including `::`
- Examples:
  - `render::*` — all functions in render module
  - `*::draw` — any function named draw
  - `auth::**::validate` — validate functions anywhere under auth

**Special Patterns:**
- `@usercode` — expands to all functions in projectRoot (equivalent to `traceUserCode: true`)

### debug_query

Query the unified execution timeline (function traces + stdout/stderr).

```typescript
debug_query({
  sessionId: string,
  eventType?: "function_enter" | "function_exit" | "stdout" | "stderr",
  function?: {
    equals?: string,            // Exact match (demangled name)
    contains?: string,          // Substring match
    matches?: string            // Regex match
  },
  sourceFile?: {
    equals?: string,
    contains?: string
  },
  returnValue?: {
    equals?: any,
    isNull?: boolean
  },
  limit?: number,               // Default: 50, max: 500
  offset?: number,              // For pagination
  verbose?: boolean             // Default: false (summary mode)
}) → {
  events: TraceEvent[],
  totalCount: number,
  hasMore: boolean
}
```

**Summary Mode (default):**
Returns compact events for token efficiency.

```typescript
interface TraceEventSummary {
  id: string
  timestamp_ns: number          // Nanoseconds since session start
  function: string              // Demangled name
  sourceFile: string
  line: number
  duration_ns: number
  returnType: string            // "i32", "null", "String", etc.
}
```

**Verbose Mode:**
Returns full event data including arguments and return values.

```typescript
interface TraceEventVerbose {
  id: string
  timestamp_ns: number
  function: string
  functionRaw: string           // Mangled name
  sourceFile: string
  line: number
  duration_ns: number
  threadId: number
  parentEventId: string | null  // For call hierarchy
  arguments: any[]              // Serialized args
  returnValue: any              // Serialized return
}
```

### debug_stop

Stop a debug session.

```typescript
debug_stop({
  sessionId: string
}) → {
  success: boolean,
  eventsCollected: number
}
```

**Behavior:**
- Detaches Frida cleanly
- Deletes session data from database
- If process still running, it continues without tracing

**Session Lifecycle:**
- Session stays alive after process exits (queryable for post-mortem analysis)
- Must call `debug_stop` before relaunching same binary
- `SESSION_EXISTS` error if launching while session active

---

## Event Capture

### What Gets Captured

| Data | Captured | Notes |
|------|----------|-------|
| Function name | Yes | Demangled + raw mangled name |
| Source file | Yes | Absolute path from DWARF |
| Line number | Yes | Via DWARF `DW_AT_decl_line` |
| Arguments | Yes | JSON serialized, depth 1 |
| Return value | Yes | JSON serialized, depth 1 |
| Duration | Yes | Nanosecond precision |
| Timestamp | Yes | Nanoseconds since session start |
| Thread ID | Yes | Numeric thread identifier |
| Parent event | Yes | For call hierarchy reconstruction |
| Process stdout | Yes | Via write(2) interception in agent |
| Process stderr | Yes | Via write(2) interception in agent |

### Serialization Rules (Phase 1a)

Fixed serialization behavior — no runtime configuration.

| Type | Serialization |
|------|---------------|
| Integers | Direct JSON number |
| Floats | Direct JSON number |
| Booleans | Direct JSON boolean |
| Strings | JSON string, truncated at 1KB |
| Arrays | First 100 elements |
| Structs | Depth 1 — top-level fields only |
| Pointers | Hex string: `"0x7fff5fbff8c0"` |
| Enums | Variant name + payload if applicable |

**Depth 1 Example:**
```rust
struct Config {
    name: String,
    settings: Settings,  // Nested struct
}
```
Serialized as:
```json
{
  "name": "myapp",
  "settings": "<Settings at 0x7fff5fbff8c0>"
}
```

---

## Storage

### SQLite Schema

```sql
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    binary_path TEXT NOT NULL,
    project_root TEXT NOT NULL,
    pid INTEGER NOT NULL,
    started_at INTEGER NOT NULL,      -- Unix timestamp
    ended_at INTEGER,                  -- NULL if still running
    status TEXT NOT NULL               -- "running", "exited", "stopped"
);

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    timestamp_ns INTEGER NOT NULL,
    thread_id INTEGER NOT NULL,
    parent_event_id TEXT,
    event_type TEXT NOT NULL,          -- "function_enter", "function_exit", "stdout", "stderr"

    -- Location
    function_name TEXT NOT NULL,
    function_name_raw TEXT,
    source_file TEXT,
    line_number INTEGER,

    -- Payload
    arguments JSON,
    return_value JSON,
    duration_ns INTEGER,
    text TEXT,                         -- For stdout/stderr events

    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

-- Performance indexes
CREATE INDEX idx_session_time ON events(session_id, timestamp_ns);
CREATE INDEX idx_function ON events(function_name);
CREATE INDEX idx_source ON events(source_file);

-- Full-text search
CREATE VIRTUAL TABLE events_fts USING fts5(
    function_name,
    source_file,
    content=events,
    content_rowid=rowid
);
```

### Write Performance

- WAL mode enabled for concurrent read/write
- Batched writes from Frida agent (every 10ms or 1000 events)
- Async insert thread in daemon

---

## Error Handling

| Error Code | Message | LLM Action |
|------------|---------|------------|
| `NO_DEBUG_SYMBOLS` | "Binary has no DWARF debug info. Ask user permission to modify build configuration to compile with debug symbols (e.g., add `-g` flag or use debug build profile)." | Guide user through build config |
| `SIP_BLOCKED` | "macOS System Integrity Protection prevents Frida attachment. Options: (1) Copy binary to /tmp and run from there, (2) Codesign binary with debugging entitlements, (3) Disable SIP for debugging. Ask user which approach they prefer." | Present options to user |
| `SESSION_EXISTS` | "Session already active for this binary. Call debug_stop first before relaunching." | Stop existing session |
| `SESSION_NOT_FOUND` | "No session found with ID '{id}'. Check sessionId spelling." | Verify session ID |
| `PROCESS_EXITED` | "Target process has exited (code: {code}). Session still queryable but cannot add traces." | Query existing events |
| `FRIDA_ATTACH_FAILED` | "Failed to attach Frida: {details}. Check process permissions." | Debug permissions |
| `INVALID_PATTERN` | "Invalid trace pattern '{pattern}': {reason}" | Fix pattern syntax |

---

## Platform Support

| Platform | Status | Notes |
|----------|--------|-------|
| Linux x86_64 | Supported | Primary development platform |
| macOS arm64 | Supported | Requires SIP handling |
| macOS x86_64 | Supported | Requires SIP handling |
| Windows | Future | Phase 10 |

---

## Language Support

| Language | Debug Info | Demangling |
|----------|------------|------------|
| C | DWARF | N/A (no mangling) |
| C++ | DWARF | Itanium ABI |
| Rust | DWARF | Rust symbol format |

**User Code Detection:**

Functions are considered "user code" if their DWARF `DW_AT_decl_file` path is within `projectRoot`.

- **Traced**: `/home/user/myproject/src/main.rs`
- **Not traced**: `/usr/lib/libc.so.6`, `/home/user/.cargo/registry/...`

---

## Validation Criteria

### Scenario: Targeted Tracing Workflow

1. LLM calls `debug_launch({ command: "./myapp", projectRoot: "/home/user/myapp" })`
   - Process starts, no functions traced yet
   - Returns `sessionId: "myapp-2026-02-05-14h32"`

2. User reports: "Bug happens when I click the submit button"

3. LLM calls `debug_trace({ sessionId: "...", add: ["submit::*", "form::validate"] })`
   - Hooks installed instantly
   - Returns `hookedFunctions: 12`

4. User clicks submit button

5. LLM calls `debug_query({ sessionId: "...", function: { contains: "validate" } })`
   - Returns events showing `form::validate` returned `false`

6. LLM calls `debug_query({ ..., verbose: true })` for full arguments

7. LLM identifies root cause, suggests fix

8. LLM calls `debug_stop({ sessionId: "..." })`

**Success Criteria:**
- No code changes to target binary
- No recompilation
- Targeted tracing (not "trace everything")
- LLM found the bug through query-driven investigation

---

## Implementation Notes

### Frida Agent

The Frida agent (TypeScript, compiled to JS) runs inside the target process:

- Receives hook instructions from daemon via Frida messaging
- Installs/removes Interceptor hooks dynamically
- Serializes arguments and return values
- Intercepts `write(2)` to capture stdout/stderr (with re-entrancy guard and 50MB limit)
- Batches events (trace + output) and sends to daemon

### DWARF Parsing

Use `gimli` crate for DWARF parsing:

- Parse on first `debug_trace` call (lazy)
- Cache parsed symbols per binary
- Extract: function names, source files, line numbers
- Support DWARF 4 and 5

### Symbol Demangling

- C++: Use `cpp_demangle` crate
- Rust: Use `rustc-demangle` crate
- Store both mangled (`functionRaw`) and demangled (`function`) names

---

## Out of Scope (Phase 1a)

Explicitly deferred to later phases:

- Hot function sampling (Phase 1b)
- Configurable serialization depth (Phase 1b)
- Multi-threading queries (Phase 1b)
- Storage limits and retention (Phase 1b)
- Crash capture (Phase 1c)
- Fork/exec following (Phase 1c)
- Test instrumentation (Phase 1d)
- Breakpoints (Phase 2)
