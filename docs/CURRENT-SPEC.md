# Strobe - Current Specification

> Living document. Updated as the app grows.

## Overview

Strobe is an LLM-native debugging infrastructure. An LLM connects via MCP, launches a target binary with Frida instrumentation, adds/removes trace patterns at runtime, and queries captured events — all without restarting the process.

**Current phase:** 1a (Tracing Foundation)

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
  └─ SQLite         ← ~/.strobe/strobe.db (WAL mode)
```

### Entry Points

```
strobe daemon   # Start daemon on Unix socket
strobe mcp      # Stdio proxy for MCP clients (auto-starts daemon)
```

### Daemon

- **Socket:** `~/.strobe/strobe.sock`
- **PID file:** `~/.strobe/strobe.pid`
- **Database:** `~/.strobe/strobe.db`
- **Idle timeout:** 30 minutes
- **Protocol:** JSON-RPC 2.0, line-delimited, MCP protocol version `2024-11-05`

## MCP Tools

### debug_launch

Spawn a process with Frida attached. Pending patterns (if any) are installed as hooks before the process resumes. Process stdout/stderr are automatically captured and queryable as events.

```
Request:
  command: string          # Path to executable (required)
  args?: string[]          # Command line arguments
  cwd?: string             # Working directory
  projectRoot: string      # Root for user code detection (required)
  env?: {[key]: string}    # Environment variables

Response:
  session_id: string       # Human-readable: "myapp-2026-02-05-14h32"
  pid: number
```

Session IDs get numeric suffixes on collision (`myapp-2026-02-05-14h32-2`).

### debug_trace

Add or remove trace patterns. Works in two modes:

**Pending mode** (no `session_id`): modifies patterns applied to the next launch. Patterns persist until explicitly removed.

**Live mode** (with `session_id`): modifies hooks on a running process. No restart required.

```
Request:
  session_id?: string      # Omit for pending patterns
  add?: string[]           # Patterns to add
  remove?: string[]        # Patterns to remove

Response:
  active_patterns: string[]
  hooked_functions: number
```

### debug_query

Query the unified execution timeline: function traces AND process stdout/stderr. Returns events in chronological order. Filter by eventType to get only traces or only output.

```
Request:
  session_id: string       # Required
  event_type?: "function_enter" | "function_exit" | "stdout" | "stderr"
  function?:
    equals?: string
    contains?: string
    matches?: string       # Regex
  source_file?:
    equals?: string
    contains?: string
  return_value?:
    equals?: any
    is_null?: boolean
  limit?: number           # Default 50, max 500
  offset?: number          # Default 0
  verbose?: boolean        # Default false

Response:
  events: Event[]
  total_count: number
  has_more: boolean
```

**Summary format** (default):
```json
{ "id", "timestamp_ns", "function", "sourceFile", "line", "duration_ns", "returnType" }
```

**Verbose format** adds: `functionRaw`, `threadId`, `parentEventId`, `arguments`, `returnValue`

### debug_stop

Stop a session and clean up.

```
Request:
  session_id: string

Response:
  success: boolean
  events_collected: number
```

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

`*` matches any characters except `::`. `**` matches any characters including `::`.

## Agent (Frida-injected TypeScript)

Injected into the target process before resume. Compiled from `agent/src/` to `agent/dist/agent.js`, embedded in the Rust binary via `include_str!`.

### Messages (Rust → Agent)

- `initialize { sessionId }` — set session context
- `hooks { action: "add"|"remove", functions: FunctionTarget[] }` — update hooks

### Messages (Agent → Rust)

- `{ type: "agent_loaded" }` — agent ready
- `{ type: "initialized" }` — session context set
- `{ type: "hooks_updated", activeCount }` — hooks changed
- `{ type: "events", events: (TraceEvent | OutputEvent)[] }` — buffered trace and output data
- `{ type: "log", message }` — debug logging

### Hook Behavior

- Uses `Interceptor.attach` on function addresses from DWARF
- `onEnter`: captures thread ID, up to 10 arguments as NativePointers, pushes to per-thread call stack
- `onLeave`: captures return value, computes duration, pops call stack
- Functions too small to hook (e.g. `call_once` thunks) are silently skipped via try-catch

### stdout/stderr Capture

The agent intercepts the `write(2)` syscall to capture process output:

- Hooks `write()` via `Interceptor.attach` on fd 1 (stdout) and fd 2 (stderr)
- Output events are sent through the same event pipeline as trace events
- **Re-entrancy guard:** Prevents infinite recursion when Frida's `send()` itself calls `write()`
- **Per-session limit:** 50MB of captured output per session; emits a truncation indicator when reached
- **Large write handling:** Writes >1MB emit a `[strobe: write of N bytes truncated (>1MB)]` indicator
- Text is read as UTF-8 (with fallback to C string)

### Event Buffering

- Buffer size: 1000 events
- Flush interval: 10ms
- Whichever threshold is hit first triggers a flush

### Serialization

- NativePointer → hex string or null (if `.isNull()`)
- Strings truncated to 1024 chars
- Arrays capped at 100 elements
- Objects capped at 100 keys
- Depth limited to 1 level (nested → `<TypeName>`)

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
| parent_event_id | TEXT | Parent in call stack (nullable) |
| event_type | TEXT | "function_enter", "function_exit", "stdout", "stderr" |
| function_name | TEXT | Demangled name |
| function_name_raw | TEXT | Original mangled name (nullable) |
| source_file | TEXT | From DWARF (nullable) |
| line_number | INTEGER | From DWARF (nullable) |
| arguments | JSON | For enter events (nullable) |
| return_value | JSON | For exit events (nullable) |
| duration_ns | INTEGER | For exit events (nullable) |
| text | TEXT | For stdout/stderr events (nullable) |

Indexes on `(session_id, timestamp_ns)`, `function_name`, `source_file`.

### Write Batching

Database writer task batches up to 100 events, flushing every 10ms.

## DWARF

Uses `gimli` + `object` crates. Supports ELF and Mach-O binaries.

- On macOS, checks for `.dSYM` bundles automatically
- Extracts `DW_TAG_subprogram` entries: name, address range, source file, line number
- Demangles Rust (`rustc-demangle`) and C++ (`cpp_demangle`) symbols
- DWARF parsers cached per binary path across sessions

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

## Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| frida | 0.17 | Dynamic instrumentation (auto-download devkit) |
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
