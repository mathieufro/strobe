# Strobe - Current Specification

> Living document. Updated as the app grows.

## Overview

Strobe is an LLM-native debugging infrastructure. An LLM connects via MCP, launches a target binary with Frida instrumentation, adds/removes trace patterns at runtime, and queries captured events — all without restarting the process.

**Current phase:** 1b (Advanced Runtime Control)

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

Spawn a process with Frida attached. Process stdout/stderr are ALWAYS captured automatically (no tracing needed). Pending patterns (if any) are installed as hooks before the process resumes.

```
Request:
  command: string          # Path to executable (required)
  args?: string[]          # Command line arguments
  cwd?: string             # Working directory
  projectRoot: string      # Root for user code detection (required)
  env?: {[key]: string}    # Environment variables

Response:
  session_id: string               # Human-readable: "myapp-2026-02-05-14h32"
  pid: number
  pending_patterns_applied?: number  # Count of pre-staged patterns (if any)
  next_steps?: string              # Recommended next action (e.g., "Query stderr/stdout first")
```

Session IDs get numeric suffixes on collision (`myapp-2026-02-05-14h32-2`).

The `next_steps` field provides workflow guidance, encouraging the observation loop: check output before adding trace patterns.

### debug_trace

Add or remove trace patterns. **Recommended workflow:** Launch clean → check stderr/stdout → add patterns only if needed.

Works in two modes:

**Pending mode** (no `session_id`): Stages patterns for next launch (advanced usage). Patterns persist until explicitly removed.

**Runtime mode** (with `session_id`): Modifies hooks on running process (recommended). No restart required.

```
Request:
  session_id?: string      # Omit for pending patterns, provide for runtime
  add?: string[]           # Patterns to add
  remove?: string[]        # Patterns to remove

Response:
  mode: string                   # "pending" or "runtime"
  active_patterns: string[]      # Current trace patterns
  hooked_functions: number       # Actual hooks installed (0 if pending or no matches)
  matched_functions?: number     # If different from hooked (e.g., crash during install)
  status?: string                # Contextual guidance based on current state
```

**Status messages** provide actionable guidance:
- When `hooked_functions: 0` on runtime mode, explains possible causes (inline functions, missing symbols, etc.)
- When hooks succeed, provides stability guidance (e.g., "Under 50 hooks - excellent stability")
- In pending mode, reminds about recommended workflow (launch clean first)

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
| `@file:foo.cpp` | All functions defined in files containing `foo.cpp` | Functions from other files |

`*` matches any characters except `::`. `**` matches any characters including `::`.

`@file:` matches by source file path substring — useful when you know which file has the bug but not the function names.

## Agent (Frida-injected TypeScript)

Injected into the target process before resume. Compiled from `agent/src/` to `agent/dist/agent.js`, embedded in the Rust binary via `include_str!`.

### Messages (Rust → Agent)

- `initialize { sessionId }` — set session context
- `hooks { action: "add"|"remove", functions: FunctionTarget[], imageBase?: string }` — update hooks (imageBase sent once for ASLR slide computation)

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

Output is captured at the **Frida Device level**, not inside the agent:

- Spawn uses `FRIDA_STDIO_PIPE` to redirect process stdout/stderr through Frida
- The Device "output" GLib signal delivers data to the daemon's `raw_on_output` callback
- Events are created with `EventType::Stdout` / `EventType::Stderr` and sent to the DB writer
- **Works with ASAN/sanitizer binaries** — no agent-side `write(2)` hook needed
- The agent also has a best-effort `write(2)` hook as a fallback (wrapped in try-catch, silently fails on ASAN binaries)

This is the most important capture mechanism — crash reports, error logs, and ASAN output all flow through stderr and are often sufficient to diagnose issues without any trace patterns.

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
- Prefers `DW_AT_linkage_name` over `DW_AT_name` for fully qualified C++ names
- Handles DWARF v4 (`Addr`) and DWARF v5 (`DebugAddrIndex`) address forms
- Demangles Rust (`rustc-demangle`) and C++ (`cpp_demangle`) symbols
- Extracts image base from `__TEXT` segment (Mach-O) for ASLR slide computation
- DWARF parsers cached per binary path across sessions

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
