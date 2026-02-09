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

Works with C, C++, Rust, Swift — any language Frida can instrument.

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

## Components

### 1. StrobeClient

TypeScript library wrapping all 14 MCP tools. Shared by every extension component.

```typescript
class StrobeClient extends EventEmitter {
  // Connection lifecycle
  connect(): Promise<void>          // Auto-starts daemon if not running
  disconnect(): void
  get isConnected(): boolean

  // 1:1 MCP tool wrappers
  launch(opts: LaunchOptions): Promise<Session>
  stop(sessionId: string, retain?: boolean): Promise<StopResponse>
  trace(req: TraceRequest): Promise<TraceResponse>
  query(req: QueryRequest): Promise<QueryResponse>
  setBreakpoints(req: BreakpointRequest): Promise<BreakpointResponse>
  continue(sessionId: string, action?: StepAction): Promise<ContinueResponse>
  read(req: ReadRequest): Promise<ReadResponse>
  write(req: WriteRequest): Promise<WriteResponse>
  setLogpoints(req: LogpointRequest): Promise<LogpointResponse>
  runTest(req: TestRequest): Promise<TestStartResponse>
  testStatus(testRunId: string): Promise<TestStatusResponse>
  listSessions(): Promise<Session[]>
  deleteSession(sessionId: string): Promise<void>

  // Polling engine
  startPolling(sessionId: string, intervalMs?: number): void
  stopPolling(): void

  // Events (from polling)
  on('events', (events: StrobeEvent[]) => void)    // New trace/output events
  on('pause', (info: PauseInfo) => void)            // Breakpoint hit
  on('testProgress', (p: TestProgress) => void)     // Test status update
  on('sessionEnd', (id: string) => void)            // Process exited
}
```

**Polling strategy** (since daemon is request/response only):
- Active session: `debug_query(timeFrom=lastSeen)` every 200ms
- Test run: `debug_test_status` every 1s (server blocks up to 15s)
- Paused at breakpoint: Single query for pause event, then stop until resume
- No session: No polling

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
  "tracePatterns": ["myapp::*"],        // Pre-set trace patterns
  "watches": [{ "variable": "gTempo" }] // Pre-set watches
}
```

**DAP → Strobe mapping:**

| DAP Request | Strobe Call |
|---|---|
| `launch` | `debug_launch` + `debug_trace` (if tracePatterns) |
| `setBreakpoints` | `debug_breakpoint({ add/remove })` |
| `continue` | `debug_continue({ action: "continue" })` |
| `next` | `debug_continue({ action: "step-over" })` |
| `stepIn` | `debug_continue({ action: "step-into" })` |
| `stepOut` | `debug_continue({ action: "step-out" })` |
| `stackTrace` | `debug_query({ eventType: "pause" })` |
| `scopes` / `variables` | `debug_read({ targets: [...] })` |
| `evaluate` | `debug_read({ targets: [{ variable: expr }] })` |
| `disconnect` | `debug_stop` |

**Breakpoint hit flow:**
1. Polling engine detects `pause` event in `debug_query`
2. DAP adapter fires `stopped` event to VS Code
3. VS Code requests `stackTrace` → we return function/file/line from pause info
4. VS Code requests `variables` → we call `debug_read` for locals/globals

### 3. Sidebar (TreeView)

Session-centric TreeView. Activates when a session starts.

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

**Test discovery**: Watches workspace for `Cargo.toml`, `CMakeLists.txt`, Catch2 binaries. Enumerates tests by running framework-specific list commands.

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

**Polling**: During test execution, polls `debug_test_status` every 1s:
- Updates pass/fail counts as they arrive
- Shows currently-running test name + elapsed time
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

Shown as faded `decorationType` text. Updated every poll cycle.

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
│   │   ├── polling-engine.ts       # Event polling with configurable intervals
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
│   │   ├── test-discovery.ts       # Framework detection + test enumeration
│   │   └── test-run.ts             # Async test execution + progress polling
│   ├── editor/
│   │   ├── decorations.ts          # Inline decorations (call count, timing)
│   │   ├── codelens-provider.ts    # CodeLens on traced/test functions
│   │   └── context-menu.ts         # "Strobe" submenu actions
│   ├── output/
│   │   └── output-channel.ts       # Strobe Output Channel formatting
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

### M1: Core Extension — Right-Click Trace + Output (2-3 weeks)

**Ships:**
- Extension scaffold (TypeScript, webpack, VS Code extension API)
- `StrobeClient` with daemon auto-start and polling engine
- "Strobe" Output Channel showing live stdout/stderr/trace events
- Right-click context menu: "Strobe > Trace This Function"
- Command palette: "Strobe: Launch", "Strobe: Stop", "Strobe: Add Trace Pattern"
- Status bar item with connection/session status
- Basic sidebar showing active session, patterns, event count

**Demo moment:** Open a C++/Rust project, right-click a function, "Trace with Strobe" → instantly see every call with args and return values flowing in the Output panel. No config, no recompile.

**Key risks:**
- Unix socket communication from VS Code extension (node `net` module handles this)
- Daemon auto-start reliability (use `child_process.spawn` with `strobe daemon` command)

### M2: Test Runner Integration (1-2 weeks)

**Ships:**
- Test Explorer provider (discovery + live execution)
- Red/green test status with `debug_test_status` polling
- Stuck detection warnings surfaced as test messages
- Failure details with file:line links and suggested traces
- "Debug with Strobe" action on failed tests
- CodeLens on test functions (Run | Debug | Trace)

**Demo moment:** Click play on test suite, watch tests go green one by one. A failure shows the exact assertion line and "suggested traces" — click to instantly trace the failing path.

**Key risks:**
- Test discovery for different frameworks (start with Cargo, add Catch2 later)
- Mapping Strobe's test output to VS Code's TestItem API

### M3: Full DAP Debugging (2-3 weeks)

**Ships:**
- Complete DAP adapter (breakpoints, stepping, variables, watches)
- launch.json configuration with IntelliSense schema
- Breakpoint gutter integration (click-to-set, conditional via right-click)
- Debug toolbar (play/pause/step-over/step-into/step-out)
- Variable inspection in Debug sidebar via `debug_read`
- Watch expressions
- Inline decorations on traced functions (call count, avg duration, last return value)
- Sidebar enrichment: watches tree, breakpoints tree, logpoints tree
- Logpoint support via context menu

**Demo moment:** Set a breakpoint in a C++ function, launch with Strobe, hit the breakpoint — full VS Code debug experience with variables and stepping. But trace patterns are also running, showing execution flow in the Output panel alongside. Two debugger paradigms unified.

**Key risks:**
- Polling latency for breakpoint hits (200ms poll means up to 200ms delay before VS Code shows pause state)
- Variable resolution: DWARF variables need `debug_read`, which may not cover all local variables yet

### M4: Polish & Power Features (1-2 weeks)

**Ships:**
- Memory inspector panel (read/write arbitrary addresses)
- Live watch variable viewer with polling values
- Contextual watch scoping UI ("watch gTempo only during audio::process")
- Session management: list retained sessions, reopen for post-mortem analysis
- Settings UI integration (event limits, serialization depth, poll intervals)
- Keyboard shortcuts for common actions
- Theme-aware styling for all decorations
- Extension marketplace packaging and icon

**Demo moment:** A polished, professional tool that feels native to VS Code. Power users can inspect memory, set contextual watches, manage multiple sessions.

## Daemon Protocol Notes

The extension communicates with the daemon the same way the MCP proxy does:

1. **Connect** to `~/.strobe/strobe.sock` via Node.js `net.Socket`
2. **Handshake**: Send MCP `initialize` request, receive capabilities
3. **Send** `notifications/initialized`
4. **Call tools** via `tools/call` method with tool name + arguments
5. **Parse** JSON-RPC responses with `result` or `error` fields

**Error handling**: Map daemon's `ErrorCode` enum to user-friendly messages:
- `NO_DEBUG_SYMBOLS` → "Binary lacks debug symbols. Recompile with `-g` flag."
- `SIP_BLOCKED` → "macOS SIP is blocking Frida. See Strobe docs for workaround."
- `SESSION_EXISTS` → "A session is already running for this binary."
- `PROCESS_EXITED` → "The process has exited." (auto-cleanup session state)

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
