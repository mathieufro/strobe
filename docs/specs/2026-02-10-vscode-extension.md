# Phase 3: VS Code Extension for Strobe

> Cross-language dynamic debugger — every Strobe feature at a click's reach, with live visual feedback.

## Overview

A VS Code extension that surfaces all Strobe capabilities through native VS Code UI patterns:
- **Right-click to trace** any function (no recompile)
- **Breakpoints, stepping, variables** via Debug Adapter Protocol
- **Live test results** (red/green) in native Test Explorer with stuck detection
- **Inline decorations** showing call counts, timing, return values on traced functions
- **"Strobe" Output Channel** streaming stdout/stderr/traces/logpoints in real-time
- **Session-centric sidebar** showing active patterns, watches, breakpoints

Works with C, C++, Rust, Swift, Go, Python, Node.js — any language Frida can instrument. Multi-language from day one via `LanguageProfile` abstraction (see below).

## Architecture

```
┌─────────────────────────────────────────────────────┐
│                  VS Code Extension                   │
│                                                      │
│  ┌───────────┐  ┌───────────┐  ┌────────────────┐  │
│  │  Strobe   │  │    DAP    │  │ Test Explorer  │  │
│  │  Sidebar  │  │  Adapter  │  │   Provider     │  │
│  │ (TreeView)│  │           │  │                │  │
│  └─────┬─────┘  └─────┬─────┘  └───────┬────────┘  │
│        │               │                │            │
│  ┌─────┴───────────────┴────────────────┴─────────┐ │
│  │            StrobeClient (TypeScript)            │ │
│  │    JSON-RPC 2.0 over Unix socket                │ │
│  │    Polling engine for live updates              │ │
│  └──────────────────────┬─────────────────────────┘ │
│                         │                            │
│  ┌──────────────────────┼────────────────────────┐  │
│  │  Inline Decorations  │  Context Menus         │  │
│  │  CodeLens Provider   │  Output Channel        │  │
│  └──────────────────────┴────────────────────────┘  │
└─────────────────────────┼───────────────────────────┘
                          │ ~/.strobe/strobe.sock
                  ┌───────┴────────┐
                  │ Strobe Daemon  │
                  │ (auto-started) │
                  └────────────────┘
```

### Communication Model

The extension talks directly to the Strobe daemon over its Unix socket using JSON-RPC 2.0. This is the same protocol the MCP proxy uses. The extension replaces the MCP layer entirely for human users.

**Hybrid approach**: DAP for breakpoints/stepping/variables (reuses VS Code's built-in debug UI), direct daemon calls for Strobe-unique features (dynamic tracing, watches, test runner, queries).

**Daemon lifecycle**: Invisible to user. Extension auto-starts daemon on first use, shows green dot in status bar. Daemon's 30-minute idle timeout handles cleanup.

**Error resilience**: All daemon calls use optimistic try/catch. `SESSION_NOT_FOUND` is treated as graceful session end (not an error). No request queue — the daemon handles its own concurrency. If the daemon crashes, the extension shows "Reconnecting..." and retries.

## Multi-Language Architecture

Strobe uses Frida as the **injection mechanism** for all languages, but the tracing strategy differs by runtime:

- **Compiled languages** (C, C++, Rust, Swift, Go): Frida hooks native functions directly. DWARF provides symbols, variables, source mapping.
- **Interpreted languages** (Python, Node/TS): Frida injects into the process, then talks to the **runtime's own debugging API** — `sys.monitoring` for Python, V8 Inspector Protocol for Node/TS. Same UX, different plumbing.

The extension abstracts this via a `LanguageProfile` system.

### LanguageProfile Interface

```typescript
interface LanguageProfile {
  id: string;                          // "rust", "cpp", "python", "go", "node"
  displayName: string;
  filePatterns: string[];              // ["*.rs"], ["*.cpp", "*.cc", "*.h"], ["*.py"]

  // Launch
  launchConfig(program: string, args: string[]): LaunchOptions;
  // e.g. Python: { command: "python3", args: [program, ...args] }
  // e.g. Rust/C++: { command: program, args }
  // e.g. Go: may need env: { GODEBUG: "..." }

  // Instrumentation strategy
  instrumentationMode: "native" | "runtime";
  // "native": Frida hooks native functions, DWARF for symbols (C/C++/Rust/Swift/Go)
  // "runtime": Frida injects runtime-level hooks via the language's debug API (Python/Node/TS)

  // Symbols
  symbolSource: "dwarf" | "runtime";   // where function names + variables come from
  patternHint: string;                 // "module::function" for Rust/C++, "module.function" for Python

  // Runtime bridge (only for instrumentationMode: "runtime")
  runtimeBridge?: RuntimeBridge;

  // Test discovery (optional — not all languages have tests)
  testDiscoverer?: TestDiscoverer;
}

interface RuntimeBridge {
  // Agent-side script that sets up runtime-level tracing
  // Injected by Frida, communicates back via send()/recv()
  agentScript: string;                 // path to TS/JS agent for this runtime
  // Maps runtime trace events to Strobe's unified event format
  mapEvent(runtimeEvent: any): StrobeEvent;
}
```

### Built-in Profiles

| Profile | Mode | Launch | Symbols | Test Discovery | Notes |
|---|---|---|---|---|---|
| `rust` | native | Direct binary | DWARF (needs dsymutil on macOS) | `cargo test -- --list` | Full support |
| `cpp` | native | Direct binary | DWARF | Catch2 `--list-tests` / GoogleTest `--gtest_list_tests` | Full support |
| `swift` | native | Direct binary | DWARF | XCTest discovery | Full support |
| `go` | native | Direct binary | DWARF (Go emits by default) | `go test -list .` | Full support. Goroutine scheduler may affect stepping. |
| `python` | runtime | `python3 script.py` | `sys.monitoring` (3.12+) / `sys.settrace` | pytest `--collect-only` | Full support via CPython debug API |
| `node` | runtime | `node script.js` | V8 Inspector Protocol (CDP) | Jest `--listTests` / Vitest `--list` | Full support via V8 debug API |
| `ts` | runtime | `tsx script.ts` / `ts-node` | V8 Inspector + source maps | Jest/Vitest | Inherits Node profile + source map resolution |

### Runtime Bridges: How Interpreted Languages Work

For `instrumentationMode: "runtime"`, Frida is the **injection vector** but not the tracing mechanism. Instead, a runtime-specific agent bridges the gap:

**Python Bridge:**
1. Frida spawns `python3 script.py` and injects the Python agent
2. Agent calls `PyRun_SimpleString()` via Frida NativeFunction to inject a `sys.monitoring` hook
3. Python runtime reports function enter/exit with `frame.f_code.co_filename`, `frame.f_lineno`, `frame.f_locals`
4. Agent maps these to Strobe's `function_enter`/`function_exit` events with full local variables
5. Patterns like `mymodule.myfunction` are resolved against Python's module/function names

**Node/TS Bridge:**
1. Frida spawns `node --inspect=0 script.js` (or injects and enables inspector via V8 C++ API)
2. Agent connects to V8 Inspector Protocol (Chrome DevTools Protocol)
3. CDP provides: `Debugger.paused`, `Runtime.callFunctionOn`, `Debugger.setBreakpoint`, scope variables
4. Agent maps CDP events to Strobe's unified event format
5. For TypeScript: source maps resolve `.ts` file/line from `.js` execution context

**What this means for users:**
- Same right-click "Trace This Function" UX for Python/TS as for C++/Rust
- Same Output Channel format, same inline decorations
- Same Test Explorer integration (pytest, Jest, Vitest)
- Variables work: Python gets `frame.f_locals`, Node gets V8 scope variables
- Breakpoints work: mapped to `sys.settrace` breakpoints or CDP breakpoints
- The only difference is under the hood — and users don't need to know

### Key Design Decisions

**All languages are first-class.** Every profile provides function tracing, variable inspection, breakpoints, and test discovery. The instrumentation mechanism differs (native hooks vs runtime API) but the UX is identical.

**Go is native, not interpreted.** Go compiles to machine code with DWARF symbols. Frida hooks Go functions the same way it hooks C/Rust. The goroutine scheduler is a quirk (cooperative scheduling can delay hook execution), not a fundamental limitation.

**Profile detection is automatic.** On workspace open, scan for `Cargo.toml`, `CMakeLists.txt`, `Package.swift`, `go.mod`, `pyproject.toml`, `package.json`, `tsconfig.json`. Multiple profiles can be active simultaneously (e.g., a Rust project with Python bindings).

**Profiles are extensible.** Third-party extensions can contribute profiles via a `strobe.languageProfiles` contribution point. The built-in profiles (compiled) ship with M1, runtime bridges ship with M5.

### Impact on Existing Components

- **launch.json**: Gains optional `"languageProfile": "python"` field. When omitted, auto-detected from `program` extension or workspace files.
- **Context menus**: "Trace This Function" uses the active profile's `patternHint` to format the pattern correctly (`module::func` for native, `module.func` for Python, etc.).
- **Test Explorer**: `TestDiscoverer` implementations are registered per profile. Only profiles with a `testDiscoverer` appear in the Test Explorer.
- **Variable scopes**: Native profiles show Arguments + Globals + best-effort Locals via DWARF. Runtime profiles show full locals from the runtime's frame introspection (often *better* than DWARF).
- **Agent**: The Frida agent gains a plugin system — `RuntimeBridge` implementations are loaded per profile. The base agent handles native hooks; bridges handle runtime-level hooks.

## Daemon API Prerequisites

The following daemon changes are required before/during extension development:

### P1: `debug_session` with `action: "status"` (lightweight heartbeat)

New `status` action on the consolidated `debug_session` tool. Returns session state without querying the event timeline.

```rust
// Request
pub struct DebugSessionStatusRequest {
    pub session_id: String,
}

// Response
pub struct DebugSessionStatusResponse {
    pub status: String,               // "running" | "paused" | "exited"
    pub pid: u32,
    pub event_count: u64,
    pub hooked_functions: u32,
    pub trace_patterns: Vec<String>,
    pub breakpoints: Vec<BreakpointInfo>,
    pub logpoints: Vec<LogpointInfo>,
    pub watches: Vec<ActiveWatch>,
    pub paused_threads: Vec<PausedThreadInfo>,  // threadId, breakpointId, file, line, function
}
```

**Rationale**: Extension needs a single poll endpoint for sidebar + DAP state sync. This is the `status` action on the consolidated `debug_session` tool (which also handles stop, list, delete). Avoids querying breakpoints, logpoints, and patterns with separate calls.

### P2: Extend `eventType` filter in `debug_query`

Add missing values to the query filter enum exposed in the tool schema:
- `pause` — breakpoint hit events
- `logpoint` — logpoint message events
- `variable_snapshot` — polling read snapshots
- `condition_error` — JS condition eval failures

The `EventTypeFilter` enum in `types.rs` already has these — they just need to appear in the `debug_query` tool schema in `server.rs`.

### P3: Cursor-based query pagination

Add `afterEventId` field to `debug_query` for reliable incremental polling:

```rust
pub struct DebugQueryRequest {
    // existing fields...
    /// Cursor: return only events with rowid > after_event_id
    pub after_event_id: Option<i64>,
}

pub struct DebugQueryResponse {
    pub events: Vec<serde_json::Value>,
    pub total_count: u64,
    pub has_more: bool,
    /// Highest event rowid in this response (use as next cursor)
    pub last_event_id: Option<i64>,
    /// True if FIFO eviction happened since the cursor position
    pub events_dropped: bool,
}
```

**Rationale**: Timestamp-based `timeFrom` can miss events during FIFO rollover. Cursor-based pagination guarantees no silent data loss. Extension shows warning when `events_dropped` is true.

### P4: Backtrace capture at breakpoint pause

When the agent pauses at a breakpoint, capture `Thread.backtrace()` and include it in the pause message to the daemon. Store on `PauseInfo`:

```rust
pub struct PauseInfo {
    pub breakpoint_id: String,
    pub func_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub paused_at: Instant,
    pub return_address: Option<u64>,
    // NEW:
    pub backtrace: Vec<BacktraceFrame>,  // from Frida Thread.backtrace()
    pub locals: Vec<LocalVariable>,       // best-effort from Interceptor args + DWARF
}

pub struct BacktraceFrame {
    pub address: u64,
    pub module_name: Option<String>,
    pub function_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
}
```

**Rationale**: DAP's `stackTrace` request needs multiple frames. Crash events already capture backtraces — same pattern. Also capture best-effort locals (function args from Interceptor are always available, DWARF globals via `debug_memory`).

### P5: Implement proper step-into (callee resolution)

Currently `step-into` behaves identically to `step-over`. To make the DAP "Step Into" button useful, implement callee resolution in the DWARF parser:

- Parse `DW_TAG_call_site` / `DW_AT_call_target` attributes
- For indirect calls (vtable, function pointers): fall back to step-over behavior
- For direct calls: resolve callee address, set one-shot breakpoint at callee entry

**Scope**: This is significant DWARF work. Can be implemented incrementally — direct calls first, indirect calls deferred.

## Components

### 1. StrobeClient

TypeScript library wrapping all MCP tools. Shared by every extension component.

```typescript
class StrobeClient extends EventEmitter {
  // Connection lifecycle
  connect(): Promise<void>          // Auto-starts daemon if not running
  disconnect(): void
  get isConnected(): boolean

  // Maps to 8 consolidated MCP tools:
  // debug_launch
  launch(opts: LaunchOptions): Promise<Session>

  // debug_session (action: status | stop | list | delete)
  sessionStatus(sessionId: string): Promise<SessionStatusResponse>
  stop(sessionId: string, retain?: boolean): Promise<StopResponse>
  listSessions(): Promise<Session[]>
  deleteSession(sessionId: string): Promise<void>

  // debug_trace
  trace(req: TraceRequest): Promise<TraceResponse>

  // debug_query
  query(req: QueryRequest): Promise<QueryResponse>

  // debug_breakpoint (unified breakpoints + logpoints)
  setBreakpoints(req: BreakpointRequest): Promise<BreakpointResponse>

  // debug_continue
  continue(sessionId: string, action?: StepAction): Promise<ContinueResponse>

  // debug_memory (action: read | write)
  readMemory(req: ReadRequest): Promise<ReadResponse>
  writeMemory(req: WriteRequest): Promise<WriteResponse>

  // debug_test (action: run | status)
  runTest(req: TestRequest): Promise<TestStartResponse>
  testStatus(testRunId: string): Promise<TestStatusResponse>

  // Polling engine
  startPolling(sessionId: string, intervalMs?: number): void
  stopPolling(): void

  // Events (from polling)
  on('events', (events: StrobeEvent[]) => void)    // New trace/output events
  on('pause', (info: PauseInfo) => void)            // Breakpoint hit (from session_status)
  on('testProgress', (p: TestProgress) => void)     // Test status update
  on('sessionEnd', (id: string) => void)            // Process exited
  on('eventsDropped', () => void)                   // FIFO rollover warning
}
```

The client exposes friendly method names but maps to only 8 MCP tools over the wire. Each method knows which `action` discriminator to send.

**Polling strategy** (two-tier):
- **Fast path**: `debug_session({ action: "status" })` every 200ms — detects pause/exit, updates sidebar state
- **Event path**: `debug_query(afterEventId=cursor)` every 500ms — feeds Output Channel + decorations
- **Test runs**: `debug_test({ action: "status" })` every 1s (server blocks up to 15s)
- **No session**: No polling

### 2. DAP Adapter

Implements VS Code's Debug Adapter Protocol, translating to Strobe API calls.

**launch.json configuration:**
```jsonc
{
  "type": "strobe",
  "request": "launch",
  "name": "Debug with Strobe",
  "program": "${workspaceFolder}/build/myapp",
  "args": ["--flag"],
  "cwd": "${workspaceFolder}",
  // Strobe-specific options:
  "languageProfile": "cpp",              // Optional, auto-detected if omitted
  "tracePatterns": ["myapp::*"],         // Pre-set trace patterns
  "watches": [{ "variable": "gTempo" }]  // Pre-set watches
}
```

**Two ways to use Strobe:**
- **Quick Trace (zero-config)**: Right-click any function → "Trace with Strobe" → instantly see calls. Uses workspace defaults from `.strobe/settings.json` (if present). No launch.json needed.
- **Full Debug Session (launch.json)**: F5 to launch with pre-configured patterns, watches, breakpoints. Ideal for complex workflows. These modes are NOT mutually exclusive — launch with F5, then right-click to add more patterns to the same session.

**DAP → Strobe mapping:**

| DAP Request | Strobe Call |
|---|---|
| `launch` | `debug_launch` + `debug_trace` (if tracePatterns) |
| `setBreakpoints` | `debug_breakpoint({ add: [...] })` (entries without `message` → breakpoints) |
| `continue` | `debug_continue({ action: "continue" })` |
| `next` | `debug_continue({ action: "step-over" })` |
| `stepIn` | `debug_continue({ action: "step-into" })` — requires P5 |
| `stepOut` | `debug_continue({ action: "step-out" })` |
| `stackTrace` | Read from `debug_session({ action: "status" })` → `paused_threads[].backtrace` |
| `scopes` | Two scopes: "Arguments" (from Interceptor) + "Globals" (from `debug_memory`) |
| `variables` | `debug_memory({ action: "read" })` for globals/watches; args from pause info |
| `evaluate` | `debug_memory({ action: "read", targets: [{ variable: expr }] })` |
| `disconnect` | `debug_session({ action: "stop" })` |

**Breakpoint hit flow:**
1. Fast-path poll (`debug_session({ action: "status" })`) detects `status: "paused"` + `paused_threads`
2. DAP adapter fires `stopped` event to VS Code
3. VS Code requests `stackTrace` → served from `paused_threads[].backtrace` (captured at pause time, P4)
4. VS Code requests `scopes` → "Arguments" scope (from pause info) + "Globals" scope
5. VS Code requests `variables` for "Globals" → `debug_memory({ action: "read" })` for DWARF globals/statics
6. VS Code requests `variables` for "Arguments" → served from pause info args (Interceptor capture)

**Variable inspection model:**
- **Arguments scope**: Function arguments captured by Frida's Interceptor at breakpoint entry. Always available, zero latency.
- **Globals scope**: DWARF-resolved global/static variables via `debug_memory({ action: "read" })`. Available for any variable the DWARF parser can resolve.
- **Watch scope**: User-defined watch expressions evaluated via `debug_memory({ action: "read" })`.
- **Locals scope**: Best-effort. Available locals from the pause info's `locals` field (P4). Some variables may be optimized out or unavailable.

**Known limitations:**
- `step-into` requires daemon P5 (callee resolution). Until then, ships as implemented — may behave as step-over for some calls.
- `step-out` may return `ValidationError` when no return address is available. DAP adapter handles this gracefully by showing a notification.

### 3. Sidebar (TreeView)

Session-centric TreeView. Activates when a session starts. State refreshed from `debug_session({ action: "status" })` poll.

```
STROBE
├── 📊 Session: myapp-2026-02-10-14h30
│   ├── PID: 12345 | Events: 1,247
│   ├── ⚡ Trace Patterns
│   │   ├── myapp::audio::*  (23 hooks)
│   │   ├── myapp::midi::parse  (1 hook)
│   │   └── + Add pattern...
│   ├── 👁 Watches
│   │   ├── gTempo = 120.5
│   │   ├── gClock->counter = 48291
│   │   └── + Add watch...
│   ├── 🔴 Breakpoints
│   │   ├── main.cpp:42 (hits: 3)
│   │   └── audio::process [if tempo > 120]
│   └── 📝 Logpoints
│       └── audio::render "frame={args[0]}"
└── (no active session)
    Launch or attach to begin
```

**Interactions:**
- Click pattern → jump to Output Channel filtered to that pattern
- Click watch → show latest value, click to edit
- Click breakpoint → jump to file:line in editor
- Inline buttons: remove (x), edit (pencil), enable/disable (eye)
- "Add pattern" opens quick input with pattern syntax hints

### 4. Test Explorer Provider

Registers with VS Code's native Testing API (beaker icon sidebar).

**Test discovery (extension-side):** Uses a `TestDiscoverer` interface with per-framework implementations. Runs discovery commands directly (no daemon needed):

```typescript
interface TestDiscoverer {
  detect(workspaceFolder: string): Promise<number>;  // confidence 0-100
  listTests(workspaceFolder: string): Promise<DiscoveredTest[]>;
}

// Implementations:
// - CargoDiscoverer: runs `cargo test -- --list`, parses output
// - Catch2Discoverer: runs binary `--list-tests`, parses output
// - GenericDiscoverer: regex scan for test patterns in source files
```

Extensible for future frameworks (Jest, pytest, Go test, etc.) by adding new `TestDiscoverer` implementations.

**Live execution view:**
```
🧪 TEST EXPLORER
├── ✅ test_audio_processing (0.3s)
├── ❌ test_midi_parsing (0.1s)
│   └── assertion failed: expected NoteOn, got NoteOff
│       at src/midi/parser.rs:42
│       💡 Suggested: trace midi::parse_event
├── ⏭ test_network_sync (skipped)
├── ⏳ test_deadlock_detection (running... 8.2s)
│   └── ⚠️ STUCK: 0% CPU, stacks unchanged — likely deadlock
│       Thread 1: mutex::lock → sync::wait
│       Thread 2: mutex::lock → sync::wait
└── 42 passed, 1 failed, 1 skipped (2.1s)
```

**Polling**: During test execution, polls `debug_test({ action: "status" })` every 1s:
- Updates pass/fail counts as they arrive
- Shows all `running_tests` with individual elapsed times and baselines
- Surfaces stuck detection warnings immediately
- Displays failure messages with file:line links

**Actions on failed tests:**
- "Debug with Strobe" — re-runs with Frida + suggested trace patterns pre-loaded
- "Show suggested traces" — reveals the patterns that would help diagnose this failure
- Click file:line in failure → opens editor at that location

**CodeLens above test functions:**
```
Run Test | Debug Test | Run with Trace
#[test]
fn test_audio_processing() {
```

### 5. Output Channel

VS Code native Output Channel named "Strobe". Appears alongside Terminal, Debug Console.

```
[14:30:01.234] stdout: Starting audio engine...
[14:30:01.456] → audio::init(sampleRate=48000) → 0  [2.3ms]
[14:30:01.789] stderr: Warning: sample rate mismatch
[14:30:02.001] ⏸ PAUSED at main.cpp:42 (breakpoint bp-1)
[14:30:02.100] 📊 gTempo = 120.5, gClock->counter = 48291
[14:30:02.345] 📝 logpoint: frame=1024, tempo=120.5
[14:30:02.500] ← audio::process(frame=1024) → void  [0.8ms]
```

**Formatting:**
- `→` for function_enter (with args)
- `←` for function_exit (with return value + duration)
- `⏸` for breakpoint pauses
- `📝` for logpoint messages
- `📊` for watch value snapshots
- stdout/stderr prefixed and colorized where possible

**Links:** File paths and function names are clickable, opening the corresponding source location.

### 6. Inline Editor Decorations

**After traced function signatures:**
```cpp
void process(int frame) {  // ⚡ 1,247 calls | avg 0.3ms | last → 0
```

Shown as faded `decorationType` text. **Debounced**: decorations re-render at most once per second, not every poll cycle. Extension tracks dirty functions (those with new events since last render) and batches all decoration updates into a single `setDecorations()` call per editor.

**On breakpoint lines (when hit):**
```cpp
int tempo = getTempo();  // ⏸ PAUSED | tempo = 120
```

**CodeLens on traced functions:**
```
⚡ 1,247 calls | avg 0.3ms | View in Timeline
void process(int frame) {
```

### 7. Context Menus

Right-click on a function name in the editor:

```
Strobe ▸
  ├── Trace This Function
  ├── Set Breakpoint
  ├── Add Logpoint...
  ├── Watch Return Value
  └── Profile Duration
```

**Function identification:** Uses VS Code's `executeDocumentSymbolProvider` command (talks to rust-analyzer, clangd, or whatever language server is active) to identify the function at cursor position. Falls back to regex heuristics when no language server is available.

**Smart behavior:**
- If no session active: "Trace This Function" prompts for binary path, launches, and traces
- If session active: Adds pattern to active session instantly
- "Add Logpoint" opens inline input for message template (e.g., `"tempo={args[0]}"`)
- "Watch Return Value" creates a watch that captures return value on every call
- "Profile Duration" traces with minimal overhead, shows timing stats in decoration

### 8. Status Bar

Left side of status bar:
```
$(circle-filled) Strobe: myapp (PID 12345) | 1,247 events | 23 hooks
```

- Green circle when connected + session active
- Yellow circle when connected, no session
- Red circle when daemon unreachable
- Click to open command palette with Strobe commands

## File Structure

```
strobe-vscode/
├── package.json                    # Extension manifest, contributions
├── tsconfig.json
├── webpack.config.js
├── src/
│   ├── extension.ts                # activate/deactivate, register all providers
│   ├── client/
│   │   ├── strobe-client.ts        # StrobeClient class (JSON-RPC over Unix socket)
│   │   ├── polling-engine.ts       # Two-tier polling (session_status + query)
│   │   └── types.ts                # TypeScript types mirroring MCP types.rs
│   ├── dap/
│   │   ├── debug-adapter.ts        # DAP implementation
│   │   ├── debug-session.ts        # Per-session state management
│   │   └── launch-config.ts        # launch.json schema + validation
│   ├── sidebar/
│   │   ├── sidebar-provider.ts     # TreeDataProvider for session tree
│   │   ├── tree-items.ts           # TreeItem subclasses (pattern, watch, breakpoint)
│   │   └── commands.ts             # Sidebar action handlers
│   ├── testing/
│   │   ├── test-controller.ts      # VS Code TestController implementation
│   │   ├── test-discovery.ts       # TestDiscoverer interface + implementations
│   │   └── test-run.ts             # Async test execution + progress polling
│   ├── editor/
│   │   ├── decorations.ts          # Inline decorations (debounced, dirty-tracked)
│   │   ├── codelens-provider.ts    # CodeLens on traced/test functions
│   │   ├── context-menu.ts         # "Strobe" submenu actions
│   │   └── function-identifier.ts  # LSP + regex function detection at cursor
│   ├── output/
│   │   └── output-channel.ts       # Strobe Output Channel formatting
│   ├── profiles/
│   │   ├── language-profile.ts     # LanguageProfile + RuntimeBridge interfaces
│   │   ├── native-profiles.ts      # Rust, C++, Swift, Go profiles
│   │   ├── python-bridge.ts        # Python RuntimeBridge (sys.monitoring) [M5]
│   │   └── node-bridge.ts          # Node/TS RuntimeBridge (V8 Inspector) [M5]
│   └── utils/
│       ├── daemon-manager.ts       # Auto-start/stop daemon
│       └── status-bar.ts           # Status bar item management
├── media/                          # Icons, images
│   └── strobe-icon.svg
└── test/
    ├── client.test.ts
    ├── dap.test.ts
    └── testing.test.ts
```

## Milestones

### M0: Daemon Prerequisites (1 week)

**Daemon-side changes required before extension work:**
- P1: `debug_session` tool — consolidates `stop` + `list_sessions` + `delete_session` + new `session_status` action
- P2: Extend `eventType` filter (add `pause`, `logpoint`, `variable_snapshot`, `condition_error`)
- P3: Cursor-based query pagination (`afterEventId` + `events_dropped`)
- P6: Consolidate `breakpoint` + `logpoint` into unified `debug_breakpoint`
- P7: Consolidate `read` + `write` into `debug_memory`
- P8: Consolidate `test` + `test_status` into `debug_test` with `action` field

These changes reduce the MCP surface from 13 → 8 tools while enabling reliable extension polling. See **MCP Tool Consolidation** section for full schemas.

### M1: Core Extension — Right-Click Trace + Output (2-3 weeks)

**Ships:**
- Extension scaffold (TypeScript, webpack, VS Code extension API)
- Platform-specific VSIX packaging with bundled `strobe` binary (see **Packaging & Distribution**)
- `StrobeClient` with daemon auto-start, two-tier polling engine, cursor-based pagination
- Built-in `LanguageProfile` implementations: Rust, C++, Swift (see **Multi-Language Architecture**)
- "Strobe" Output Channel showing live stdout/stderr/trace events
- Right-click context menu: "Strobe > Trace This Function" (LSP + regex identification)
- Command palette: "Strobe: Launch", "Strobe: Stop", "Strobe: Add Trace Pattern"
- Status bar item with connection/session status
- Basic sidebar showing active session, patterns, event count

**Demo moment:** Open a C++/Rust project, right-click a function, "Trace with Strobe" → instantly see every call with args and return values flowing in the Output panel. No config, no recompile.

**Key risks:**
- Unix socket communication from VS Code extension (node `net` module handles this)
- Daemon auto-start reliability (use `child_process.spawn` with bundled binary path)
- Platform-specific binary bundling in CI pipeline

### M2: Test Runner Integration (1-2 weeks)

**Ships:**
- Test Explorer provider with extensible `TestDiscoverer` interface
- `CargoDiscoverer` (runs `cargo test -- --list` directly, no daemon needed)
- Live red/green test status with `debug_test({ action: "status" })` polling + `running_tests` display
- Stuck detection warnings surfaced as test messages
- Failure details with file:line links and suggested traces
- "Debug with Strobe" action on failed tests
- CodeLens on test functions (Run | Debug | Trace)

**Demo moment:** Click play on test suite, watch tests go green one by one. A failure shows the exact assertion line and "suggested traces" — click to instantly trace the failing path.

**Key risks:**
- Mapping Strobe's test output to VS Code's TestItem API
- Catch2 discoverer (binary `--list-tests`) needs the test binary to already be built

### M3: Full DAP Debugging (2-3 weeks)

**Daemon prerequisites (can overlap with M2):**
- P4: Backtrace capture at breakpoint pause (agent + daemon)
- P5: Proper step-into with callee resolution (DWARF parser)

**Ships:**
- Complete DAP adapter (breakpoints, stepping, variable scopes)
- launch.json configuration with IntelliSense schema
- Breakpoint gutter integration (click-to-set, conditional via right-click)
- Debug toolbar (play/pause/step-over/step-into/step-out)
- Variable scopes: Arguments (from Interceptor), Globals (from `debug_memory`), best-effort Locals
- Watch expressions via `debug_memory`
- Inline decorations on traced functions (debounced, call count, avg duration, last return value)
- Sidebar enrichment: watches tree, breakpoints tree, logpoints tree (from `debug_session({ action: "status" })`)
- Logpoint support via context menu

**Demo moment:** Set a breakpoint in a C++ function, launch with Strobe, hit the breakpoint — full VS Code debug experience with variables and stepping. But trace patterns are also running, showing execution flow in the Output panel alongside. Two debugger paradigms unified.

**Key risks:**
- Polling latency for breakpoint hits (200ms poll on `session_status` means up to 200ms delay)
- Step-into callee resolution (P5) is significant DWARF work — may ship partially

### M4: Polish & Power Features (1-2 weeks)

**Ships:**
- Memory inspector panel (read/write arbitrary addresses via `debug_memory`)
- Live watch variable viewer with polling values
- Contextual watch scoping UI ("watch gTempo only during audio::process")
- Session management: list retained sessions, reopen for post-mortem analysis (via `debug_session({ action: "list" })`)
- Settings UI integration (event limits, serialization depth, poll intervals)
- Keyboard shortcuts for common actions
- Theme-aware styling for all decorations
- Go `LanguageProfile` (native mode — same as Rust/C++, just needs goroutine-awareness)
- Marketplace icon, README, and changelog

**Demo moment:** A polished, professional tool that feels native to VS Code. Power users can inspect memory, set contextual watches, manage multiple sessions. Go developers get the same experience as Rust/C++.

### M5: Runtime Bridges — Python, Node, TypeScript (3-4 weeks)

**Daemon-side:**
- P9: Agent plugin system — `RuntimeBridge` interface for loading per-language bridge scripts
- P10: Event normalization — runtime bridge events mapped to Strobe's unified event schema

**Ships:**
- Python bridge: `sys.monitoring` (3.12+) / `sys.settrace` injection via `PyRun_SimpleString()`
  - Python-level function enter/exit with `frame.f_locals`
  - Source file + line from `frame.f_code`
  - Pattern matching on Python module.function names
  - Breakpoints via `sys.settrace` conditional traps
- Node/TS bridge: V8 Inspector Protocol (CDP) via `--inspect` or runtime injection
  - JS/TS-level function tracing via CDP `Debugger` domain
  - Full scope variables from V8
  - Source map resolution for TypeScript (`.ts` file/line from `.js` execution)
  - Breakpoints via CDP `Debugger.setBreakpoint`
- pytest `TestDiscoverer` + Jest/Vitest `TestDiscoverer`
- Extension-side source map support for TypeScript inline decorations

**Demo moment:** Right-click a Python function, "Trace with Strobe" → see every call with full local variables. Set a breakpoint in a `.ts` file, hit it, inspect V8 scope variables. Same UX as C++ debugging. The language doesn't matter — Strobe handles it.

**Key risks:**
- `sys.monitoring` is Python 3.12+ only — `sys.settrace` fallback has significant overhead
- V8 Inspector Protocol stability across Node versions
- Source map resolution edge cases (bundlers, transpilers)
- Performance of runtime-level tracing vs native hooks (runtime bridges add interpreter overhead)

## MCP Tool Consolidation

The current daemon exposes 13 MCP tools. Adding a separate `debug_session_status` would make 14. Instead, we consolidate down to 8 tools — reducing LLM cognitive load (fewer tools to evaluate per turn) while giving the extension a cleaner API surface.

### Consolidation Plan: 14 → 8 Tools

| New Tool | Absorbs | Discriminator |
|---|---|---|
| `debug_launch` | *(unchanged)* | — |
| `debug_session` | `stop` + `list_sessions` + `delete_session` + new `session_status` | `action: "status" \| "stop" \| "list" \| "delete"` |
| `debug_trace` | *(unchanged)* | — |
| `debug_query` | *(unchanged, gains `afterEventId`)* | — |
| `debug_breakpoint` | `breakpoint` + `logpoint` | entries with `message` field → logpoint; without → breakpoint |
| `debug_continue` | *(unchanged)* | — |
| `debug_memory` | `read` + `write` | `action: "read" \| "write"` |
| `debug_test` | `test` + `test_status` | `action: "run" \| "status"` |

### `debug_session` (new unified tool)

```rust
pub struct DebugSessionRequest {
    pub action: SessionAction,          // "status" | "stop" | "list" | "delete"
    pub session_id: Option<String>,     // required for status/stop/delete, ignored for list
    pub retain: Option<bool>,           // only for stop
}

pub enum SessionAction { Status, Stop, List, Delete }

// Response is an enum:
pub enum DebugSessionResponse {
    Status(SessionStatusResponse),      // session state, patterns, breakpoints, watches, paused_threads
    Stop(StopResponse),
    List(Vec<SessionSummary>),
    Delete { success: bool },
}
```

### `debug_breakpoint` absorbs logpoints

```rust
pub struct DebugBreakpointRequest {
    pub session_id: String,
    pub add: Vec<BreakpointEntry>,
    pub remove: Vec<String>,            // IDs (both breakpoint and logpoint IDs)
}

pub struct BreakpointEntry {
    pub function: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub condition: Option<String>,
    pub hit_count: Option<u32>,
    pub message: Option<String>,        // present → logpoint, absent → breakpoint
}
```

### `debug_memory` (merges read + write)

```rust
pub struct DebugMemoryRequest {
    pub session_id: String,
    pub action: MemoryAction,           // "read" | "write"
    pub targets: Vec<MemoryTarget>,
    pub depth: Option<u8>,              // read only
    pub poll: Option<PollConfig>,       // read only
}
```

### `debug_test` absorbs status

```rust
pub struct DebugTestRequest {
    pub action: TestAction,             // "run" | "status"
    // For "run":
    pub project_root: Option<String>,
    pub test: Option<String>,
    pub trace_patterns: Option<Vec<String>>,
    // For "status":
    pub test_run_id: Option<String>,
}
```

### Migration Strategy

This is a **breaking change** for existing MCP clients (Claude's system prompt). Migration:
1. Implement new consolidated tools alongside old ones (both work during transition)
2. Update MCP system prompt to reference new tool names
3. Deprecate old tools (log warning when called)
4. Remove old tools after one release cycle

The extension is built against the new API from day one.

## Packaging & Distribution

### One-Click Install

The extension must be fully self-contained — users install from the VS Code Marketplace and everything works. No manual binary installation.

### Platform Binaries

The `strobe` binary (daemon + MCP proxy) must be bundled per platform:

| Platform | Binary | Frida Devkit | Total |
|---|---|---|---|
| macOS arm64 | `strobe-darwin-arm64` | `frida-core-devkit-darwin-arm64` | ~50MB |
| macOS x64 | `strobe-darwin-x64` | `frida-core-devkit-darwin-x64` | ~50MB |
| Linux x64 | `strobe-linux-x64` | `frida-core-devkit-linux-x64` | ~50MB |
| Linux arm64 | `strobe-linux-arm64` | `frida-core-devkit-linux-arm64` | ~50MB |

### Distribution Strategy: Platform-Specific VSIX

Follow the rust-analyzer model — publish **platform-specific extensions** to the Marketplace:

```json
// package.json
{
  "name": "strobe",
  "engines": { "vscode": "^1.85.0" },
  "contributes": { ... },
  "extensionPack": [],
  // Platform targeting (VS Code 1.85+)
  "target": "darwin-arm64"  // one per platform variant
}
```

This means 4 VSIX files published per release, each ~50MB. Users only download their platform's variant. VS Code handles this automatically.

### Binary Location & Lifecycle

```typescript
// daemon-manager.ts
class DaemonManager {
  private binaryPath: string;  // extension/bin/strobe-{platform}

  async ensureDaemon(): Promise<void> {
    // 1. Check if daemon already running (try connect to socket)
    // 2. If not, spawn: child_process.spawn(this.binaryPath, ["daemon"])
    // 3. Wait for socket to appear (poll with backoff, max 5s)
    // 4. Connect
  }
}
```

The binary lives at `<extensionPath>/bin/strobe`. On extension activate, `DaemonManager.ensureDaemon()` is called. The daemon's 30-minute idle timeout handles cleanup automatically.

### Build Pipeline

```
CI Matrix:
  ├── macOS arm64:  cargo build --release → bundle into strobe-darwin-arm64.vsix
  ├── macOS x64:    cargo build --release → bundle into strobe-darwin-x64.vsix
  ├── Linux x64:    cargo build --release → bundle into strobe-linux-x64.vsix
  └── Linux arm64:  cross build --release → bundle into strobe-linux-arm64.vsix

Each job:
  1. cd agent && npm install && npm run build
  2. cargo build --release
  3. cp target/release/strobe extension/bin/strobe
  4. cd extension && vsce package --target <platform>
  5. vsce publish --target <platform>
```

### Version Synchronization

The extension version and daemon version must match. On extension update:
1. Extension activates with new binary
2. If old daemon is running, send shutdown signal via socket
3. New daemon starts on next use

The daemon's response to `initialize` includes its version. Extension compares and force-restarts if mismatched.

## Daemon Protocol Notes

The extension communicates with the daemon the same way the MCP proxy does:

1. **Connect** to `~/.strobe/strobe.sock` via Node.js `net.Socket`
2. **Handshake**: Send MCP `initialize` request, receive capabilities (required — daemon enforces this)
3. **Send** `notifications/initialized`
4. **Call tools** via `tools/call` method with tool name + arguments
5. **Parse** JSON-RPC responses with `result` or `error` fields

**Error handling**: Map daemon's `ErrorCode` enum to user-friendly messages:
- `NO_DEBUG_SYMBOLS` → "Binary lacks debug symbols. Recompile with `-g` flag."
- `SIP_BLOCKED` → "macOS SIP is blocking Frida. See Strobe docs for workaround."
- `SESSION_EXISTS` → "A session is already running for this binary."
- `PROCESS_EXITED` → "The process has exited." (auto-cleanup session state)
- `SESSION_NOT_FOUND` → Treated as graceful session end. Clear sidebar, stop polling.

**Resilience**: All daemon calls wrapped in try/catch. `SESSION_NOT_FOUND` on any call triggers automatic cleanup. If daemon connection drops, extension shows "Reconnecting..." and retries with exponential backoff.

## Future Considerations

### Server-Push Events (Post-M4)
The current polling model adds up to 200ms latency. A future daemon enhancement could add JSON-RPC notifications (server → client) for:
- Breakpoint hit events (instant pause UX)
- New trace events (real-time streaming)
- Process exit notifications

This would be a daemon-side change (add notification support to the Unix socket protocol) that the extension can adopt without restructuring.

### WebSocket Transport (Post-M4)
For remote debugging scenarios, the daemon could expose a WebSocket endpoint alongside the Unix socket. The `StrobeClient` would gain a `WebSocketTransport` alongside the existing `UnixSocketTransport`.

### Multi-Session Support
Current design is session-centric (one active session). Future: sidebar tabs for multiple concurrent sessions, useful for microservice debugging.
