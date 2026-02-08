# Strobe - Current Specification

> Living document. Updated as the app grows.

## Overview

Strobe is an LLM-native debugging infrastructure. An LLM connects via MCP, launches a target binary with Frida instrumentation, adds/removes trace patterns at runtime, and queries captured events — all without restarting the process.

**Current phase:** 1e (Live Memory Reads — planned)

## Recommended Workflow

Start with minimal observation and escalate only as needed:

1. **Launch with no patterns** — `debug_launch` with no prior `debug_trace`. stdout/stderr are always captured.
2. **Read output first** — `debug_query({ eventType: "stderr" })`. Crash messages, ASAN reports, assertion failures, and error logs are often sufficient.
3. **Add targeted traces** — Only when output doesn't explain the issue. Use `debug_trace({ sessionId, add: [...] })` on the running session.
4. **Narrow or widen** — Adjust patterns based on what you learn. No restart needed.

This incremental approach is faster than guessing patterns upfront, avoids event noise, and mirrors how experienced developers use debuggers.

## Architecture

```
LLM (Claude, etc.)
  │ stdio
  ▼
strobe mcp          ← stdio proxy, auto-starts daemon
  │ unix socket
  ▼
strobe daemon       ← long-running, idle-shuts after 30min
  ├─ MCP server     ← JSON-RPC 2.0 over ~/.strobe/strobe.sock
  ├─ SessionManager ← manages sessions, DWARF cache, pattern state
  ├─ Frida worker   ← dedicated thread (lazily initialized), spawns/instruments processes
  │   └─ Agent.js   ← injected TypeScript, hooks functions via Interceptor
  ├─ TestRunner     ← async test execution with framework adapters
  └─ SQLite         ← ~/.strobe/strobe.db (WAL mode)
```

### Entry Points

```
strobe daemon   # Start daemon on Unix socket
strobe mcp      # Stdio proxy for MCP clients (auto-starts daemon)
strobe install  # Auto-detect coding agent, install MCP config + skills
```

### Daemon

- **Socket:** `~/.strobe/strobe.sock`
- **PID file:** `~/.strobe/strobe.pid`
- **Database:** `~/.strobe/strobe.db`
- **Idle timeout:** 30 minutes
- **Protocol:** JSON-RPC 2.0, line-delimited, MCP protocol version `2024-11-05`

## Configuration

Hierarchical file-based settings with JSON files:

**Hierarchy** (later overrides earlier):
1. Built-in defaults
2. Global: `~/.strobe/settings.json`
3. Project: `<projectRoot>/.strobe/settings.json`

```json
{
  "events.maxPerSession": 500000,
  "test.statusRetryMs": 3000
}
```

| Key | Type | Default | Range | Description |
|-----|------|---------|-------|-------------|
| `events.maxPerSession` | number | 200,000 | 1 - 10,000,000 | Per-session event limit (FIFO buffer) |
| `test.statusRetryMs` | number | 5,000 | 500 - 60,000 | Base polling delay for `debug_test_status` |

**Event limit guidance:**
- 200k: Default — fast queries (<10ms), ~56MB DB
- 500k: Audio/DSP debugging — moderate queries (~28ms), ~140MB DB
- 1M+: Avoid unless necessary — slow queries (>300ms), >280MB DB

Settings are re-read on every tool call (no caching). Invalid values fall back to defaults with a warning.

## MCP Tools

### debug_launch

Spawn a process with Frida attached. Process stdout/stderr are ALWAYS captured automatically (no tracing needed). Pending patterns (if any) are installed as hooks before the process resumes.

```
Request:
  command: string          # Path to executable (required)
  args?: string[]          # Command line arguments
  cwd?: string             # Working directory
  projectRoot: string      # Root for user code detection (required)
  env?: {[key]: string}    # Environment variables

Response:
  sessionId: string                # Human-readable: "myapp-2026-02-05-14h32"
  pid: number
  pendingPatternsApplied?: number  # Count of pre-staged patterns (if any)
  nextSteps?: string               # Recommended next action (e.g., "Query stderr/stdout first")
```

Session IDs get numeric suffixes on collision (`myapp-2026-02-05-14h32-2`).

The `nextSteps` field provides workflow guidance, encouraging the observation loop: check output before adding trace patterns.

### debug_trace

Add or remove trace patterns and watch variables. **Recommended workflow:** Launch clean → check stderr/stdout → add patterns only if needed.

Works in two modes:

**Pending mode** (no `sessionId`): Stages patterns for next launch (advanced usage). Patterns persist until explicitly removed.

**Runtime mode** (with `sessionId`): Modifies hooks on running process (recommended). No restart required.

```
Request:
  sessionId?: string              # Omit for pending patterns, provide for runtime
  add?: string[]                  # Patterns to add
  remove?: string[]               # Patterns to remove
  serializationDepth?: number     # Max depth for recursive argument serialization (default: 3, max: 10)
  projectRoot?: string            # Root directory for settings resolution
  watches?: {
    add?: WatchTarget[]           # Watches to add (max 32 per session)
    remove?: string[]             # Labels of watches to remove
  }

Response:
  mode: string                    # "pending" or "runtime"
  activePatterns: string[]        # Current trace patterns
  hookedFunctions: number         # Actual hooks installed (0 if pending or no matches)
  matchedFunctions?: number       # If different from hooked (e.g., crash during install)
  activeWatches: ActiveWatch[]    # Currently active watches
  warnings: string[]              # Hook installation warnings
  eventLimit: number              # Current per-session event limit (from settings)
  status?: string                 # Contextual guidance based on current state
```

**WatchTarget:**
```
  variable?: string       # Variable name or pointer chain: "gTempo", "gClock->counter"
  address?: string        # Hex address for raw memory: "0x10000a3c0"
  type?: string           # Type hint: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer
  label?: string          # Display label (auto-generated from variable/address if omitted)
  expr?: string           # JavaScript expression: "ptr(0x5678).readU32()"
  on?: string[]           # Function patterns to scope this watch (supports * and **)
```

**Watch scoping (`on` field):**
```
{ variable: "gTempo", on: ["audio::process"] }    // Only during audio::process
{ variable: "gClock", on: ["midi::*"] }            // Any direct child of midi::
{ variable: "gState", on: ["juce::**"] }           // Any descendant of juce::
```

Watch values appear in the `watchValues` field of function_enter/function_exit events (visible with `verbose: true`).

**Limits:** Max 32 watches per session. Expression/variable max 256 chars, max 4 dereference levels (`a->b->c->d`).

**Status messages** provide actionable guidance:
- When `hookedFunctions: 0` on runtime mode, explains possible causes (inline functions, missing symbols, etc.)
- When hooks succeed, provides stability guidance (e.g., "Under 50 hooks - excellent stability")
- In pending mode, reminds about recommended workflow (launch clean first)

### debug_query

Query the unified execution timeline: function traces, process stdout/stderr, crash events, and variable snapshots. Returns events in chronological order.

```
Request:
  sessionId: string              # Required
  eventType?: "function_enter" | "function_exit" | "stdout" | "stderr" | "crash" | "variable_snapshot"
  function?:
    equals?: string
    contains?: string
    matches?: string             # Regex
  sourceFile?:
    equals?: string
    contains?: string
  returnValue?:
    equals?: any
    isNull?: boolean
  threadName?:
    contains?: string
  timeFrom?: number | string     # Absolute ns or relative: "-5s", "-1m", "-500ms"
  timeTo?: number | string       # Absolute ns or relative
  minDurationNs?: number         # Find slow functions
  pid?: number                   # Filter by process ID (multi-process sessions)
  limit?: number                 # Default 50, max 500
  offset?: number                # Default 0
  verbose?: boolean              # Default false

Response:
  events: Event[]
  totalCount: number
  hasMore: boolean
  pids?: number[]                # All PIDs in session (only present when multiple)
```

**Summary format** (default):
```json
{ "id", "timestampNs", "function", "sourceFile", "line", "durationNs", "returnType" }
```

**Verbose format** adds: `functionRaw`, `threadId`, `threadName`, `pid`, `parentEventId`, `arguments`, `returnValue`, `watchValues`, `sampled`

### debug_test

Start a test run asynchronously. Returns a `testRunId` immediately — poll `debug_test_status` for progress and results. Auto-detects framework from `projectRoot`.

**Always use this tool** instead of running test commands via bash. Tests run inside Frida when tracing is requested, allowing patterns to be added at any time without restarting.

```
Request:
  projectRoot: string            # Required. Used for adapter detection
  framework?: string             # Override auto-detection: "cargo", "catch2"
  level?: string                 # "unit", "integration", "e2e". Omit for all.
  test?: string                  # Run single test by name (substring match)
  command?: string               # Test binary path (required for Catch2)
  tracePatterns?: string[]       # Presence triggers Frida path
  watches?: WatchUpdate          # Presence triggers Frida path
  env?: {[key]: string}          # Additional environment variables

Response (immediate):
  testRunId: string              # UUID for polling via debug_test_status
  status: "running"
  framework: string              # Detected adapter: "Cargo", "Catch2", "Generic"
```

**Adapter detection:** Cargo.toml → cargo (confidence 90), binary probe → Catch2 (confidence 85), Generic fallback (confidence 1). Explicit `framework` override as escape hatch.

**Two execution paths** chosen automatically:
- No `tracePatterns`/`watches` → direct subprocess (no overhead)
- `tracePatterns`/`watches` present → Frida path (~1s overhead), response includes `sessionId`

### debug_test_status

Query the status of a running test. **Blocks up to 15 seconds** waiting for completion before returning progress — safe to call immediately after each response.

```
Request:
  testRunId: string

Response (while running):
  testRunId: string
  status: "running"
  sessionId?: string                    # Frida session ID (use with debug_trace/debug_stop)
  progress: {
    elapsedMs: number
    passed: number
    failed: number
    skipped: number
    currentTest?: string                # Name of currently executing test
    currentTestElapsedMs?: number       # How long current test has been running
    currentTestBaselineMs?: number      # Historical avg duration (last 10 runs)
    phase?: "compiling" | "running" | "suites_finished"
    warnings: StuckWarning[]            # Stuck detection warnings
  }

Response (completed):
  testRunId: string
  status: "completed"
  sessionId?: string
  result: {
    framework: string
    summary: { passed, failed, skipped, stuck?, durationMs }
    failures: TestFailure[]
    stuck: StuckTest[]
    details?: string                    # Path to full details file in /tmp/strobe/tests/
    noTests?: boolean
    project?: { language, buildSystem, testFiles }
    hint?: string                       # Guidance when no tests found
  }

Response (failed):
  testRunId: string
  status: "failed"
  error: string
```

**TestFailure:**
```
  name: string
  file?: string
  line?: number
  message: string
  suggestedTraces: string[]
```

**StuckTest:**
```
  name: string
  elapsedMs: number
  diagnosis: string                     # "Deadlock: 0% CPU, identical stacks"
  threads: [{ name, stack: string[] }]  # Thread backtraces captured before kill
  suggestedTraces: string[]
```

**StuckWarning** (in progress):
```
  testName?: string
  idleMs: number
  diagnosis: string                     # "0% CPU, stacks unchanged 6s"
  suggestedTraces: string[]
```

**Stuck detection** runs in parallel with the test subprocess. Multi-signal: output silence + CPU delta sampling (every 2s) + stack comparison (triggered at ~6s). Confirms stuck in ~8s regardless of test level. Captures thread backtraces before killing.

### debug_stop

Stop a session and clean up.

```
Request:
  sessionId: string
  retain?: boolean       # Keep session data for post-mortem analysis (default: false)

Response:
  success: boolean
  eventsCollected: number
```

Retained sessions are accessible via `debug_list_sessions` and can be deleted with `debug_delete_session`.

### debug_list_sessions

List all retained debug sessions.

```
Request:
  (no parameters)

Response:
  sessions: Array<{
    sessionId: string
    binaryPath: string
    pid: number
    startedAt: number
    endedAt: number | null
    status: "running" | "exited" | "stopped"
  }>
```

### debug_delete_session

Delete a retained session and its data.

```
Request:
  sessionId: string

Response:
  success: boolean
```

### debug_read (Phase 1e — planned, not yet implemented)

Read memory from a running process without tracing. Point-in-time snapshots independent of function hooks.

**Full spec:** [specs/2026-02-08-phase-1e-live-memory-reads.md](specs/2026-02-08-phase-1e-live-memory-reads.md)

```
Request:
  sessionId: string
  targets: Array<
    { variable: string } |                              # DWARF-resolved
    { address: string, size: number, type: string }     # Raw address
  >
  depth?: number              # Struct traversal depth (default 1, max 5)
  poll?: {
    intervalMs: number        # Min 50, max 5000
    durationMs: number        # Min 100, max 30000
  }

Response (one-shot):
  results: Array<{
    target: string
    address: string
    type: string
    value: any
    size: number
    fields?: object           # For structs (depth-expanded)
    file?: string             # For bytes type (written to file)
    preview?: string          # Hex preview for bytes
    error?: string            # Per-target errors
  }>

Response (poll):
  polling: true
  variableCount: number
  intervalMs: number
  durationMs: number
  expectedSamples: number
  eventType: "variable_snapshot"
  hint: string
```

Poll samples are stored as `variable_snapshot` events in the timeline, interleaved with function traces. Query with `debug_query({ eventType: "variable_snapshot" })`.

## Pattern Syntax

Glob-style matching on demangled function names:

| Pattern | Matches | Does not match |
|---------|---------|----------------|
| `foo::bar` | `foo::bar` | `foo::baz` |
| `foo::*` | `foo::bar`, `foo::baz` | `foo::bar::qux` |
| `foo::**` | `foo::bar`, `foo::bar::baz` | `other::bar` |
| `*::validate` | `auth::validate`, `form::validate` | `auth::deep::validate` |
| `auth::**::validate` | `auth::validate`, `auth::user::validate` | `form::validate` |
| `@usercode` | All functions with source in `projectRoot` | stdlib, dependencies |
| `@file:foo.cpp` | All functions defined in files containing `foo.cpp` | Functions from other files |

`*` matches any characters except `::`. `**` matches any characters including `::`.

`@file:` matches by source file path substring — useful when you know which file has the bug but not the function names.

## Agent (Frida-injected TypeScript)

Injected into the target process before resume. Compiled from `agent/src/` to `agent/dist/agent.js`, embedded in the Rust binary via `include_str!`.

### CModule Tracing

High-performance native C callbacks compiled via TinyCC at runtime. 10-50x faster than JS hooks.

- Mode encoded in data pointer low bit: `data = (funcId << 1) | is_light`
- C code decodes with bitwise ops, reads watch values from shared memory
- 4 native CModule watches for best performance, remaining watches via JS

### Messages (Rust → Agent)

- `initialize { sessionId }` — set session context
- `hooks { action: "add"|"remove", functions: FunctionTarget[], imageBase?: string, mode?: "full"|"light", serializationDepth?: number }` — update hooks (imageBase sent once for ASLR slide computation)
- `watches { watches: WatchTarget[], exprWatches?: ExprWatch[] }` — update variable watches

### Messages (Agent → Rust)

- `{ type: "agent_loaded" }` — agent script loaded and ready
- `{ type: "initialized", sessionId }` — session context set
- `{ type: "hooks_updated", activeCount }` — hooks changed
- `{ type: "watches_updated", activeCount }` — watches changed
- `{ type: "events", events: (TraceEvent | OutputEvent | CrashEvent)[] }` — buffered event data
- `{ type: "sampling_state_change", funcId, funcName, enabled, sampleRate }` — hot function sampling toggled
- `{ type: "sampling_stats", stats }` — periodic sampling statistics
- `{ type: "log", message }` — debug logging

### Hook Behavior

- Uses `Interceptor.attach` on function addresses from DWARF
- `onEnter`: captures thread ID, up to 10 arguments as NativePointers, pushes to per-thread call stack
- `onLeave`: captures return value, computes duration, pops call stack
- Functions too small to hook (e.g. `call_once` thunks) are silently skipped via try-catch

### stdout/stderr Capture

Output is captured at the **Frida Device level**, not inside the agent:

- Spawn uses `FRIDA_STDIO_PIPE` to redirect process stdout/stderr through Frida
- The Device "output" GLib signal delivers data to the daemon's `raw_on_output` callback
- Events are created with `EventType::Stdout` / `EventType::Stderr` and sent to the DB writer
- **Works with ASAN/sanitizer binaries** — no agent-side `write(2)` hook needed
- The agent also has a best-effort `write(2)` hook as a fallback (wrapped in try-catch, silently fails on ASAN binaries)

This is the most important capture mechanism — crash reports, error logs, and ASAN output all flow through stderr and are often sufficient to diagnose issues without any trace patterns.

### Crash Capture

When the target process crashes, Frida's exception handler captures full context:

- Signal type and faulting address
- CPU registers at crash time
- Stack trace (accurate mode via `Thread.backtrace`)
- Stack frame memory dump (512 bytes below FP, 128 above)
- Memory access details for access violations

Crash events are stored with `eventType: "crash"` and queryable via `debug_query`.

### Event Buffering

- Buffer size: 1000 events
- Flush interval: 10ms
- Whichever threshold is hit first triggers a flush

### Serialization

- NativePointer → hex string or null (if `.isNull()`)
- Strings truncated to 1024 chars
- Arrays capped at 100 elements
- Objects capped at 100 keys
- Depth configurable via `serializationDepth` (default 3, max 10)
- Nested objects beyond depth → `<TypeName>`

### Event Storage Limits

Per-session FIFO buffer (configurable via settings):
- Default: 200,000 events
- Oldest events auto-deleted when limit reached (async cleanup, never blocks tracing)
- Configure via `events.maxPerSession` in settings

## Database

SQLite with WAL mode, `synchronous=NORMAL`.

### sessions

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | Human-readable session ID |
| binary_path | TEXT | Path to executable |
| project_root | TEXT | Project root for user code |
| pid | INTEGER | Process ID |
| started_at | INTEGER | Unix timestamp |
| ended_at | INTEGER | Nullable |
| status | TEXT | "running", "exited", "stopped" |

### events

| Column | Type | Notes |
|--------|------|-------|
| id | TEXT PK | `{sessionId}-{counter}` |
| session_id | TEXT FK | References sessions.id |
| timestamp_ns | INTEGER | Elapsed ns since session start |
| thread_id | INTEGER | OS thread ID |
| thread_name | TEXT | Thread name (nullable) |
| pid | INTEGER | Process ID for multi-process sessions (nullable) |
| parent_event_id | TEXT | Parent in call stack (nullable) |
| event_type | TEXT | "function_enter", "function_exit", "stdout", "stderr", "crash" |
| function_name | TEXT | Demangled name |
| function_name_raw | TEXT | Original mangled name (nullable) |
| source_file | TEXT | From DWARF (nullable) |
| line_number | INTEGER | From DWARF (nullable) |
| arguments | JSON | For enter events (nullable) |
| return_value | JSON | For exit events (nullable) |
| duration_ns | INTEGER | For exit events (nullable) |
| text | TEXT | For stdout/stderr events (nullable) |
| watch_values | JSON | Variable watch values (nullable) |
| sampled | BOOLEAN | True if captured via hot function sampling (nullable) |
| signal | TEXT | For crash events — signal type (nullable) |
| fault_address | TEXT | For crash events — faulting address (nullable) |
| registers | JSON | For crash events — CPU registers (nullable) |
| backtrace | JSON | For crash events — stack trace (nullable) |

Indexes on `(session_id, timestamp_ns)`, `function_name`, `source_file`.

### Write Batching

Database writer task batches up to 100 events, flushing every 10ms.

## DWARF

Uses `gimli` + `object` crates. Supports ELF and Mach-O binaries.

- On macOS, checks for `.dSYM` bundles automatically
- Extracts `DW_TAG_subprogram` entries: name, address range, source file, line number
- Prefers `DW_AT_linkage_name` over `DW_AT_name` for fully qualified C++ names
- Handles DWARF v4 (`Addr`) and DWARF v5 (`DebugAddrIndex`) address forms
- Demangles Rust (`rustc-demangle`) and C++ (`cpp_demangle`) symbols
- Extracts image base from `__TEXT` segment (Mach-O) for ASLR slide computation
- DWARF parsers cached per binary path across sessions
- Parallel CU parsing via rayon, lazy struct member resolution
- Extracts global/static variables with addresses for watch variable resolution

## ASLR Support

macOS applies Address Space Layout Randomization (ASLR) to spawned processes. Function addresses in DWARF are static (pre-ASLR), but the agent needs runtime addresses.

- The daemon extracts the image base from the binary's `__TEXT` segment via the `object` crate
- On first `hooks` message, the agent computes `aslrSlide = Process.mainModule.base - imageBase`
- All subsequent hook addresses are adjusted: `runtimeAddr = staticAddr + aslrSlide`
- The slide is computed once and cached for the session lifetime

## Errors

| Code | Meaning |
|------|---------|
| `NO_DEBUG_SYMBOLS` | Binary has no DWARF info |
| `SIP_BLOCKED` | macOS SIP blocked Frida |
| `SESSION_EXISTS` | Duplicate session for binary |
| `SESSION_NOT_FOUND` | Unknown session ID |
| `PROCESS_EXITED` | Target exited (session still queryable) |
| `FRIDA_ATTACH_FAILED` | Frida instrumentation failed |
| `INVALID_PATTERN` | Malformed trace pattern |
| `WATCH_FAILED` | Watch variable resolution error |
| `TEST_RUN_NOT_FOUND` | Unknown test run ID |
| `VALIDATION_ERROR` | Invalid request parameters |

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| frida | 0.17 | Dynamic instrumentation (auto-download devkit) |
| frida-sys | 0.17 | Raw FFI bindings (bypasses frida-rs Script bugs) |
| gimli | 0.31 | DWARF parsing |
| object | 0.36 | Binary format parsing |
| rusqlite | 0.32 | SQLite |
| tokio | 1.x | Async runtime |
| serde / serde_json | 1.x | Serialization |
| rustc-demangle | 0.1 | Rust symbol demangling |
| cpp_demangle | 0.4 | C++ symbol demangling |
| memmap2 | 0.9 | Memory-mapped file I/O |
| uuid | 1.x | Event IDs |
| chrono | 0.4 | Timestamps |
| tracing | 0.1 | Structured logging |
| rayon | 1.x | Parallel DWARF parsing |

## Setup

### macOS

```bash
sudo DevToolsSecurity -enable
```

Debug builds include `get-task-allow` entitlement by default. For release builds:

```bash
codesign -f -s - --entitlements debug.entitlements /path/to/binary
```

### Linux

No special setup required.

### Target Binaries

Must be compiled with debug symbols (`-g` for C/C++, default for `cargo build`).
