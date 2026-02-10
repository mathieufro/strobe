# M3: Full DAP Debugging — Implementation Plan

**Spec:** `docs/specs/2026-02-10-vscode-extension.md` (M3 section, lines 648-669)
**Goal:** Add complete Debug Adapter Protocol support to the VS Code extension — breakpoints via gutter, stepping toolbar, variable inspection, stack traces, inline decorations on traced functions, and logpoint context menus.
**Architecture:** An inline DAP adapter (runs in-process via `@vscode/debugadapter`) translates VS Code debug requests into StrobeClient calls. Prerequisite P4 adds backtrace + argument capture to the agent's pause message so `stackTrace`/`scopes`/`variables` DAP requests can be served from `paused_threads` data. Inline decorations aggregate per-function stats from the polling engine and render debounced `TextEditorDecorationType`s. P5 (step-into callee resolution) is deferred — existing step-into ships as-is.
**Tech Stack:** `@vscode/debugadapter`, `@vscode/debugprotocol`, VS Code Debug API (`vscode.debug.*`), existing `StrobeClient` + `PollingEngine`
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (P4: Daemon prerequisite):** Tasks 1, 2 — agent backtrace capture + daemon propagation
- **Stream B (DAP adapter):** Tasks 3, 4, 5 — package.json contributions, adapter core, variable scopes
- **Stream C (Decorations + Menus):** Tasks 6, 7 — inline decorations, context menu enrichment
- **Serial:** Task 8 (wire everything into extension.ts — depends on A, B, C)

Streams B and C can start in parallel with Stream A. Task 5 (variable scopes / stack trace) depends on P4 completing. Task 4 (adapter core) can proceed with stub data while P4 lands.

---

### Task 1: Agent — Backtrace + Arguments Capture on Pause

**Files:**
- Modify: `agent/src/agent.ts` (breakpoint onEnter handler, ~lines 840-860)

**Step 1: Add backtrace + args to the pause message**

In the `setBreakpoint()` method's `onEnter` handler, capture a backtrace using the same pattern as `buildCrashEvent()` (line 408), and serialize the first N arguments. The `this.context` from `InvocationContext` provides the CPU context needed for `Thread.backtrace()`.

This is inside a non-arrow `onEnter(args) { ... }` handler (already correct — Interceptor needs `this` as InvocationContext), so `this.context` is available.

```typescript
// In setBreakpoint(), inside onEnter(args) handler, BEFORE the send() call:

// Capture backtrace (same pattern as buildCrashEvent)
let backtrace: BacktraceFrame[] = [];
try {
  const frames = Thread.backtrace(this.context, Backtracer.ACCURATE);
  backtrace = frames.map((addr: NativePointer) => {
    const sym = DebugSymbol.fromAddress(addr);
    return {
      address: addr.toString(),
      moduleName: sym.moduleName,
      name: sym.name,
      fileName: sym.fileName,
      lineNumber: sym.lineNumber,
    };
  });
} catch (_) {
  // Backtrace may fail in some contexts
}

// Capture first 8 arguments (best-effort)
const capturedArgs: Array<{ index: number; value: string }> = [];
for (let i = 0; i < 8; i++) {
  try {
    capturedArgs.push({ index: i, value: args[i].toString() });
  } catch {
    break;
  }
}

send({
  type: 'paused',
  threadId,
  breakpointId: bp.id,
  hits: bp.hits,
  funcName: bp.funcName,
  file: bp.file,
  line: bp.line,
  returnAddress: returnAddr ? returnAddr.strip().toString() : null,
  backtrace,           // NEW
  arguments: capturedArgs, // NEW
});
```

**Important:** `this.context` is the `InvocationContext`'s CPU context (CpuContext). The existing handler already uses non-arrow function syntax and accesses `this.returnAddress`, so `this.context` is also available. Do NOT use an arrow function here.

**Step 2: Rebuild agent**

```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
```

**Checkpoint:** Agent now sends backtrace frames and argument values in every pause message. Existing daemon ignores unknown fields, so this is backwards-compatible.

---

### Task 2: Daemon — Backtrace Propagation Through Pause Pipeline

**Files:**
- Modify: `agent/src/agent.ts` — already done in Task 1
- Modify: `src/frida_collector/spawner.rs:284-295` (PauseNotification struct) and `~394-455` (pause handler)
- Modify: `src/daemon/session_manager.rs:1728-1737` (PauseInfo struct), `~526-538` (receiver task), `~1566-1575` (status builder)
- Modify: `src/mcp/types.rs:1221-1230` (PausedThreadInfo struct)
- Modify: `strobe-vscode/src/client/types.ts:57-63` (PausedThreadInfo TS type)

**Step 1: Add backtrace types to Rust MCP types**

In `src/mcp/types.rs`, add a `BacktraceFrame` struct and extend `PausedThreadInfo`:

```rust
// After PausedThreadInfo definition (~line 1230)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BacktraceFrame {
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapturedArg {
    pub index: u32,
    pub value: String,
}
```

Extend `PausedThreadInfo` with two new optional fields:

```rust
pub struct PausedThreadInfo {
    pub thread_id: u64,
    pub breakpoint_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    // NEW:
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub backtrace: Vec<BacktraceFrame>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub arguments: Vec<CapturedArg>,
}
```

**Step 2: Extend PauseNotification in spawner.rs**

In `src/frida_collector/spawner.rs`, add fields to `PauseNotification` (~line 284):

```rust
pub struct PauseNotification {
    pub session_id: String,
    pub thread_id: u64,
    pub breakpoint_id: String,
    pub hits: u32,
    pub func_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub return_address: Option<u64>,
    pub address: Option<u64>,
    // NEW:
    pub backtrace: Vec<crate::mcp::BacktraceFrame>,
    pub arguments: Vec<crate::mcp::CapturedArg>,
}
```

**Step 3: Parse backtrace from agent message in spawner.rs**

In the `"paused"` handler (~line 394), parse the new fields from the JSON payload:

```rust
// After existing field parsing (~line 407):
let backtrace: Vec<crate::mcp::BacktraceFrame> = payload
    .get("backtrace")
    .and_then(|v| v.as_array())
    .map(|arr| {
        arr.iter().filter_map(|frame| {
            Some(crate::mcp::BacktraceFrame {
                address: frame.get("address")?.as_str()?.to_string(),
                module_name: frame.get("moduleName").and_then(|v| v.as_str()).map(|s| s.to_string()),
                function_name: frame.get("name").and_then(|v| v.as_str()).map(|s| s.to_string()),
                file: frame.get("fileName").and_then(|v| v.as_str()).map(|s| s.to_string()),
                line: frame.get("lineNumber").and_then(|v| v.as_u64()).map(|n| n as u32),
            })
        }).collect()
    })
    .unwrap_or_default();

let arguments: Vec<crate::mcp::CapturedArg> = payload
    .get("arguments")
    .and_then(|v| v.as_array())
    .map(|arr| {
        arr.iter().filter_map(|arg| {
            Some(crate::mcp::CapturedArg {
                index: arg.get("index")?.as_u64()? as u32,
                value: arg.get("value")?.as_str()?.to_string(),
            })
        }).collect()
    })
    .unwrap_or_default();

// Include in PauseNotification:
let notification = PauseNotification {
    // ... existing fields ...
    backtrace,
    arguments,
};
```

**Step 4: Extend PauseInfo in session_manager.rs**

In `src/daemon/session_manager.rs`, add fields to `PauseInfo` (~line 1728):

```rust
pub struct PauseInfo {
    pub breakpoint_id: String,
    pub func_name: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub paused_at: Instant,
    pub return_address: Option<u64>,
    pub address: Option<u64>,
    // NEW:
    pub backtrace: Vec<crate::mcp::BacktraceFrame>,
    pub arguments: Vec<crate::mcp::CapturedArg>,
}
```

Update the receiver task (~line 526) that converts `PauseNotification` → `PauseInfo`:

```rust
let info = PauseInfo {
    breakpoint_id: notification.breakpoint_id.clone(),
    func_name: notification.func_name,
    file: notification.file,
    line: notification.line,
    paused_at: Instant::now(),
    return_address: notification.return_address,
    address: notification.address,
    backtrace: notification.backtrace,
    arguments: notification.arguments,
};
```

Update the status builder (~line 1567) that converts `PauseInfo` → `PausedThreadInfo`:

```rust
.map(|(tid, info)| crate::mcp::PausedThreadInfo {
    thread_id: tid,
    breakpoint_id: info.breakpoint_id,
    function: info.func_name,
    file: info.file,
    line: info.line,
    backtrace: info.backtrace,
    arguments: info.arguments,
})
```

**Step 5: Fix all PauseInfo construction sites**

Search for all places that construct `PauseInfo` (tests, step hooks, etc.) and add the new fields with defaults:

```rust
backtrace: Vec::new(),
arguments: Vec::new(),
```

Run: `cargo build 2>&1 | grep "missing field"` to find all sites.

**Step 6: Update TypeScript types**

In `strobe-vscode/src/client/types.ts`, extend `PausedThreadInfo`:

```typescript
export interface BacktraceFrame {
  address: string;
  moduleName?: string;
  functionName?: string;
  file?: string;
  line?: number;
}

export interface CapturedArg {
  index: number;
  value: string;
}

export interface PausedThreadInfo {
  threadId: number;
  breakpointId: string;
  function?: string;
  file?: string;
  line?: number;
  backtrace?: BacktraceFrame[];
  arguments?: CapturedArg[];
}
```

**Step 7: Verify build**

```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
cargo build
cd strobe-vscode && npm run build && cd ..
```

Run existing tests to verify no regressions:

```bash
cargo test
```

**Checkpoint:** `debug_session({ action: "status" })` now includes `backtrace` and `arguments` arrays in each `pausedThreads` entry. DAP adapter can serve `stackTrace`, `scopes`, and `variables` requests from this data.

---

### Task 3: DAP Adapter — Package.json + Launch Config Schema

**Files:**
- Modify: `strobe-vscode/package.json` (add debugger contribution)
- Create: `strobe-vscode/src/dap/launch-config.ts`

**Step 1: Add debugger contribution to package.json**

Add to `contributes` section:

```json
"debuggers": [
  {
    "type": "strobe",
    "label": "Strobe",
    "languages": ["rust", "c", "cpp", "swift", "go"],
    "configurationAttributes": {
      "launch": {
        "required": ["program"],
        "properties": {
          "program": {
            "type": "string",
            "description": "Path to the executable to debug",
            "default": "${workspaceFolder}/target/debug/${workspaceFolderBasename}"
          },
          "args": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Command-line arguments",
            "default": []
          },
          "cwd": {
            "type": "string",
            "description": "Working directory",
            "default": "${workspaceFolder}"
          },
          "env": {
            "type": "object",
            "additionalProperties": { "type": "string" },
            "description": "Additional environment variables",
            "default": {}
          },
          "tracePatterns": {
            "type": "array",
            "items": { "type": "string" },
            "description": "Trace patterns to apply at launch (e.g., [\"myapp::*\"])",
            "default": []
          },
          "watches": {
            "type": "array",
            "items": {
              "type": "object",
              "properties": {
                "variable": { "type": "string" },
                "label": { "type": "string" }
              }
            },
            "description": "Watch variables to monitor",
            "default": []
          },
          "stopOnEntry": {
            "type": "boolean",
            "description": "Pause on first instruction",
            "default": false
          }
        }
      }
    },
    "configurationSnippets": [
      {
        "label": "Strobe: Launch",
        "description": "Launch and debug with Strobe",
        "body": {
          "type": "strobe",
          "request": "launch",
          "name": "Debug with Strobe",
          "program": "^\"\\${workspaceFolder}/target/debug/\\${workspaceFolderBasename}\"",
          "args": [],
          "cwd": "^\"\\${workspaceFolder}\""
        }
      }
    ]
  }
]
```

Also add `@vscode/debugadapter` and `@vscode/debugprotocol` as dependencies:

```json
"dependencies": {
  "@vscode/debugadapter": "^1.68.0",
  "@vscode/debugprotocol": "^1.68.0"
}
```

**Step 2: Create launch config types**

Create `strobe-vscode/src/dap/launch-config.ts`:

```typescript
import { DebugProtocol } from '@vscode/debugprotocol';

export interface StrobeLaunchConfig extends DebugProtocol.LaunchRequestArguments {
  program: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  tracePatterns?: string[];
  watches?: Array<{ variable?: string; label?: string }>;
  stopOnEntry?: boolean;
}

export function validateLaunchConfig(config: StrobeLaunchConfig): string | undefined {
  if (!config.program) {
    return 'Missing required field: program';
  }
  return undefined;
}
```

**Step 3: Verify build**

```bash
cd strobe-vscode && npm install && npm run build
```

**Checkpoint:** VS Code recognizes "strobe" as a debug type. `launch.json` has IntelliSense for all Strobe-specific fields. The "Add Configuration" button shows the "Strobe: Launch" snippet.

---

### Task 4: DAP Adapter — Core Session Lifecycle

**Files:**
- Create: `strobe-vscode/src/dap/debug-adapter.ts`
- Modify: `strobe-vscode/src/extension.ts` (register adapter factory)

**Step 1: Create the debug adapter**

Create `strobe-vscode/src/dap/debug-adapter.ts`. This extends `@vscode/debugadapter`'s `DebugSession` and uses an inline implementation (runs in the extension host process, shares the existing `StrobeClient`).

```typescript
import {
  DebugSession,
  InitializedEvent,
  TerminatedEvent,
  StoppedEvent,
  ContinuedEvent,
  OutputEvent,
  Thread,
  StackFrame,
  Scope,
  Source,
  Variable,
  Breakpoint,
} from '@vscode/debugadapter';
import { DebugProtocol } from '@vscode/debugprotocol';
import { StrobeLaunchConfig, validateLaunchConfig } from './launch-config';
import { StrobeClient } from '../client/strobe-client';
import { DaemonManager } from '../utils/daemon-manager';
import {
  PausedThreadInfo,
  BacktraceFrame,
  CapturedArg,
  SessionStatusResponse,
} from '../client/types';

const POLL_INTERVAL_MS = 200;
const MAIN_THREAD_ID = 1; // DAP thread IDs are adapter-assigned

export class StrobeDebugAdapter extends DebugSession {
  private client: StrobeClient | undefined;
  private daemonManager: DaemonManager;
  private sessionId: string | undefined;
  private pollTimer: ReturnType<typeof setInterval> | undefined;

  // variablesReference tracking (reset on each stop)
  private nextVarRef = 1;
  private varRefMap = new Map<number, VarRefData>();

  // Cached pause state (refreshed on each stop)
  private pausedThreads: PausedThreadInfo[] = [];
  private lastStatus: SessionStatusResponse | undefined;

  // Track breakpoints per source file
  private sourceBreakpoints = new Map<string, DebugProtocol.SourceBreakpoint[]>();

  // Thread tracking: maps Frida thread IDs to DAP thread IDs
  private threadMap = new Map<number, number>(); // fridaThreadId → dapThreadId
  private reverseThreadMap = new Map<number, number>(); // dapThreadId → fridaThreadId
  private nextDapThreadId = 1;

  constructor(daemonManager: DaemonManager) {
    super();
    this.daemonManager = daemonManager;
    this.setDebuggerLinesStartAt1(true);
    this.setDebuggerColumnsStartAt1(true);
  }

  // ---- Lifecycle ----

  protected initializeRequest(
    response: DebugProtocol.InitializeResponse,
    args: DebugProtocol.InitializeRequestArguments,
  ): void {
    response.body = {
      supportsConfigurationDoneRequest: true,
      supportsFunctionBreakpoints: true,
      supportsConditionalBreakpoints: true,
      supportsHitConditionalBreakpoints: true,
      supportsLogPoints: true,
      supportsEvaluateForHovers: true,
      supportsSteppingGranularity: false,
      supportsTerminateRequest: true,
    };
    this.sendResponse(response);
    this.sendEvent(new InitializedEvent());
  }

  protected async launchRequest(
    response: DebugProtocol.LaunchResponse,
    args: DebugProtocol.LaunchRequestArguments,
  ): Promise<void> {
    const config = args as StrobeLaunchConfig;
    const error = validateLaunchConfig(config);
    if (error) {
      response.success = false;
      response.message = error;
      this.sendResponse(response);
      return;
    }

    try {
      this.client = await this.daemonManager.ensureClient();

      // Launch the target process
      const result = await this.client.launch({
        command: config.program,
        args: config.args,
        cwd: config.cwd,
        projectRoot: config.cwd || require('path').dirname(config.program),
        env: config.env,
      });
      this.sessionId = result.sessionId;

      // Apply trace patterns if specified
      if (config.tracePatterns && config.tracePatterns.length > 0) {
        await this.client.trace({
          sessionId: this.sessionId,
          add: config.tracePatterns,
        });
      }

      // Start polling for breakpoint hits / session end
      this.startPolling();

      this.sendResponse(response);
    } catch (e: any) {
      response.success = false;
      response.message = e.message || String(e);
      this.sendResponse(response);
    }
  }

  protected configurationDoneRequest(
    response: DebugProtocol.ConfigurationDoneResponse,
    args: DebugProtocol.ConfigurationDoneArguments,
  ): void {
    // Breakpoints are already set by this point.
    // Process is already running (Strobe launches eagerly).
    this.sendResponse(response);
  }

  protected async terminateRequest(
    response: DebugProtocol.TerminateResponse,
    args: DebugProtocol.TerminateArguments,
  ): Promise<void> {
    await this.stopSession();
    this.sendResponse(response);
  }

  protected async disconnectRequest(
    response: DebugProtocol.DisconnectResponse,
    args: DebugProtocol.DisconnectArguments,
  ): Promise<void> {
    await this.stopSession();
    this.sendResponse(response);
  }

  // ---- Breakpoints ----

  protected async setBreakPointsRequest(
    response: DebugProtocol.SetBreakpointsResponse,
    args: DebugProtocol.SetBreakpointsArguments,
  ): Promise<void> {
    const sourcePath = args.source.path || '';
    const requested = args.breakpoints || [];

    // Store for reference
    this.sourceBreakpoints.set(sourcePath, requested);

    if (!this.client || !this.sessionId) {
      // Session not started yet — return unverified breakpoints.
      // They'll be set during configurationDone or on first poll.
      response.body = {
        breakpoints: requested.map((bp) => ({
          verified: false,
          line: bp.line,
          message: 'Session not started',
        })),
      };
      this.sendResponse(response);
      return;
    }

    try {
      // Remove existing breakpoints for this file, then add new ones.
      // Strobe uses function-based breakpoints; file:line maps to debug_breakpoint.
      const targets = requested.map((bp) => ({
        file: sourcePath,
        line: bp.line,
        condition: bp.condition,
        hitCount: bp.hitCondition ? parseInt(bp.hitCondition, 10) : undefined,
        message: bp.logMessage, // logMessage present → logpoint
      }));

      const result = await this.client.setBreakpoints({
        sessionId: this.sessionId,
        add: targets,
      });

      // Map Strobe breakpoint responses back to DAP breakpoints
      const dapBreakpoints: DebugProtocol.Breakpoint[] = [];
      const allBps = [...result.breakpoints, ...result.logpoints];
      for (let i = 0; i < requested.length; i++) {
        const strobeBp = allBps[i];
        if (strobeBp) {
          dapBreakpoints.push({
            verified: true,
            line: strobeBp.line || requested[i].line,
            source: args.source,
          });
        } else {
          dapBreakpoints.push({
            verified: false,
            line: requested[i].line,
            message: 'Could not resolve breakpoint location',
          });
        }
      }

      response.body = { breakpoints: dapBreakpoints };
    } catch (e: any) {
      response.body = {
        breakpoints: requested.map((bp) => ({
          verified: false,
          line: bp.line,
          message: e.message || 'Failed to set breakpoint',
        })),
      };
    }

    this.sendResponse(response);
  }

  protected async setFunctionBreakPointsRequest(
    response: DebugProtocol.SetFunctionBreakpointsResponse,
    args: DebugProtocol.SetFunctionBreakpointsArguments,
  ): Promise<void> {
    if (!this.client || !this.sessionId) {
      response.body = {
        breakpoints: args.breakpoints.map(() => ({ verified: false })),
      };
      this.sendResponse(response);
      return;
    }

    try {
      const targets = args.breakpoints.map((bp) => ({
        function: bp.name,
        condition: bp.condition,
        hitCount: bp.hitCondition ? parseInt(bp.hitCondition, 10) : undefined,
      }));

      const result = await this.client.setBreakpoints({
        sessionId: this.sessionId,
        add: targets,
      });

      response.body = {
        breakpoints: result.breakpoints.map((bp) => ({
          verified: true,
          source: bp.file ? new Source(require('path').basename(bp.file), bp.file) : undefined,
          line: bp.line,
        })),
      };
    } catch (e: any) {
      response.body = {
        breakpoints: args.breakpoints.map(() => ({
          verified: false,
          message: e.message || 'Failed to set breakpoint',
        })),
      };
    }

    this.sendResponse(response);
  }

  // ---- Execution Control ----

  protected async continueRequest(
    response: DebugProtocol.ContinueResponse,
    args: DebugProtocol.ContinueArguments,
  ): Promise<void> {
    await this.doStep('continue');
    response.body = { allThreadsContinued: true };
    this.sendResponse(response);
  }

  protected async nextRequest(
    response: DebugProtocol.NextResponse,
    args: DebugProtocol.NextArguments,
  ): Promise<void> {
    await this.doStep('step-over');
    this.sendResponse(response);
  }

  protected async stepInRequest(
    response: DebugProtocol.StepInResponse,
    args: DebugProtocol.StepInArguments,
  ): Promise<void> {
    await this.doStep('step-into');
    this.sendResponse(response);
  }

  protected async stepOutRequest(
    response: DebugProtocol.StepOutResponse,
    args: DebugProtocol.StepOutArguments,
  ): Promise<void> {
    await this.doStep('step-out');
    this.sendResponse(response);
  }

  private async doStep(action: 'continue' | 'step-over' | 'step-into' | 'step-out'): Promise<void> {
    if (!this.client || !this.sessionId) return;
    try {
      await this.client.continue(this.sessionId, action);
      this.sendEvent(new ContinuedEvent(MAIN_THREAD_ID, true));
    } catch (e: any) {
      this.sendEvent(new OutputEvent(`Step failed: ${e.message}\n`, 'console'));
    }
  }

  // ---- Threads / Stack / Scopes / Variables ----

  protected threadsRequest(response: DebugProtocol.ThreadsResponse): void {
    if (this.pausedThreads.length === 0) {
      // Not paused — report single main thread
      response.body = { threads: [new Thread(MAIN_THREAD_ID, 'main')] };
    } else {
      response.body = {
        threads: this.pausedThreads.map((pt) => {
          const dapId = this.getDapThreadId(pt.threadId);
          return new Thread(dapId, `Thread ${pt.threadId}`);
        }),
      };
    }
    this.sendResponse(response);
  }

  protected stackTraceRequest(
    response: DebugProtocol.StackTraceResponse,
    args: DebugProtocol.StackTraceArguments,
  ): void {
    const fridaThreadId = this.reverseThreadMap.get(args.threadId);
    const paused = this.pausedThreads.find((pt) =>
      fridaThreadId !== undefined ? pt.threadId === fridaThreadId : false,
    );

    if (!paused) {
      response.body = { stackFrames: [], totalFrames: 0 };
      this.sendResponse(response);
      return;
    }

    const frames: DebugProtocol.StackFrame[] = [];

    // Frame 0: the breakpoint location itself
    frames.push(new StackFrame(
      this.allocFrameId(args.threadId, 0),
      paused.function || '<unknown>',
      paused.file ? new Source(require('path').basename(paused.file), paused.file) : undefined,
      paused.line || 0,
      0,
    ));

    // Remaining frames from backtrace
    const bt = paused.backtrace || [];
    for (let i = 0; i < bt.length; i++) {
      const frame = bt[i];
      frames.push(new StackFrame(
        this.allocFrameId(args.threadId, i + 1),
        frame.functionName || frame.moduleName || `0x${frame.address}`,
        frame.file ? new Source(require('path').basename(frame.file), frame.file) : undefined,
        frame.line || 0,
        0,
      ));
    }

    // Apply pagination
    const start = args.startFrame || 0;
    const levels = args.levels || frames.length;
    const paged = frames.slice(start, start + levels);

    response.body = { stackFrames: paged, totalFrames: frames.length };
    this.sendResponse(response);
  }

  protected scopesRequest(
    response: DebugProtocol.ScopesResponse,
    args: DebugProtocol.ScopesArguments,
  ): void {
    const scopes: DebugProtocol.Scope[] = [];

    // Find which thread/frame this belongs to
    const frameInfo = this.getFrameInfo(args.frameId);

    if (frameInfo && frameInfo.frameIndex === 0) {
      // Arguments scope — from Interceptor capture (always available, frame 0 only)
      const argsRef = this.allocVarRef({ type: 'arguments', frameId: args.frameId });
      scopes.push(new Scope('Arguments', argsRef, false));
    }

    // Globals scope — from debug_memory (expensive, lazy-loaded)
    const globalsRef = this.allocVarRef({ type: 'globals', frameId: args.frameId });
    scopes.push(new Scope('Globals', globalsRef, true));

    response.body = { scopes };
    this.sendResponse(response);
  }

  protected async variablesRequest(
    response: DebugProtocol.VariablesResponse,
    args: DebugProtocol.VariablesArguments,
  ): Promise<void> {
    const refData = this.varRefMap.get(args.variablesReference);
    if (!refData) {
      response.body = { variables: [] };
      this.sendResponse(response);
      return;
    }

    const variables: DebugProtocol.Variable[] = [];

    if (refData.type === 'arguments') {
      // Serve from cached pause info
      const paused = this.pausedThreads[0]; // TODO: match by frameId
      const capturedArgs = paused?.arguments || [];
      for (const arg of capturedArgs) {
        variables.push({
          name: `arg${arg.index}`,
          value: arg.value,
          variablesReference: 0,
        });
      }
    } else if (refData.type === 'globals') {
      // Fetch from debug_memory if watches are configured
      if (this.client && this.sessionId && this.lastStatus) {
        const watches = this.lastStatus.watches || [];
        for (const w of watches) {
          try {
            const result = await this.client.readMemory({
              sessionId: this.sessionId,
              targets: [{ variable: w.label }],
            });
            // Parse the result — debug_memory returns JSON with read values
            const text = typeof result === 'string' ? result : JSON.stringify(result);
            variables.push({
              name: w.label,
              value: text,
              type: w.typeName,
              variablesReference: 0,
            });
          } catch {
            variables.push({
              name: w.label,
              value: '<unavailable>',
              variablesReference: 0,
            });
          }
        }
      }
    } else if (refData.type === 'struct') {
      // Nested struct expansion
      const children = refData.children || [];
      for (const child of children) {
        variables.push({
          name: child.name,
          value: child.value,
          type: child.type,
          variablesReference: child.childRef || 0,
        });
      }
    }

    response.body = { variables };
    this.sendResponse(response);
  }

  protected async evaluateRequest(
    response: DebugProtocol.EvaluateResponse,
    args: DebugProtocol.EvaluateArguments,
  ): Promise<void> {
    if (!this.client || !this.sessionId) {
      response.success = false;
      response.message = 'No active session';
      this.sendResponse(response);
      return;
    }

    try {
      const result = await this.client.readMemory({
        sessionId: this.sessionId,
        targets: [{ variable: args.expression }],
      });
      const text = typeof result === 'string' ? result : JSON.stringify(result);
      response.body = {
        result: text,
        variablesReference: 0,
      };
    } catch (e: any) {
      response.body = {
        result: `<error: ${e.message}>`,
        variablesReference: 0,
      };
    }

    this.sendResponse(response);
  }

  // ---- Polling ----

  private startPolling(): void {
    this.pollTimer = setInterval(() => this.pollStatus(), POLL_INTERVAL_MS);
  }

  private async pollStatus(): Promise<void> {
    if (!this.client || !this.sessionId) return;
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.lastStatus = status;

      if (status.status === 'paused' && status.pausedThreads.length > 0) {
        // Check if this is a new pause (not already reported)
        const newPause = this.pausedThreads.length === 0;
        this.pausedThreads = status.pausedThreads;

        if (newPause) {
          this.resetVarRefs();
          // Build thread map
          for (const pt of this.pausedThreads) {
            this.getDapThreadId(pt.threadId);
          }
          const firstPaused = this.pausedThreads[0];
          const dapThreadId = this.getDapThreadId(firstPaused.threadId);
          this.sendEvent(new StoppedEvent('breakpoint', dapThreadId));
        }
      } else if (status.status === 'running') {
        if (this.pausedThreads.length > 0) {
          // Was paused, now running — clear state
          this.pausedThreads = [];
          this.resetVarRefs();
        }
      } else if (status.status === 'exited') {
        this.stopPolling();
        this.sendEvent(new TerminatedEvent());
      }
    } catch {
      // SESSION_NOT_FOUND → session ended
      this.stopPolling();
      this.sendEvent(new TerminatedEvent());
    }
  }

  // ---- Helpers ----

  private getDapThreadId(fridaThreadId: number): number {
    let dapId = this.threadMap.get(fridaThreadId);
    if (dapId === undefined) {
      dapId = this.nextDapThreadId++;
      this.threadMap.set(fridaThreadId, dapId);
      this.reverseThreadMap.set(dapId, fridaThreadId);
    }
    return dapId;
  }

  private allocFrameId(threadId: number, frameIndex: number): number {
    // Encode threadId + frameIndex into a single integer
    // Use top 16 bits for threadId, bottom 16 for frame index
    return (threadId << 16) | (frameIndex & 0xffff);
  }

  private getFrameInfo(frameId: number): { threadId: number; frameIndex: number } | undefined {
    const threadId = (frameId >> 16) & 0xffff;
    const frameIndex = frameId & 0xffff;
    return { threadId, frameIndex };
  }

  private allocVarRef(data: VarRefData): number {
    const ref = this.nextVarRef++;
    this.varRefMap.set(ref, data);
    return ref;
  }

  private resetVarRefs(): void {
    this.nextVarRef = 1;
    this.varRefMap.clear();
  }

  private stopPolling(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  private async stopSession(): Promise<void> {
    this.stopPolling();
    if (this.client && this.sessionId) {
      try {
        await this.client.stop(this.sessionId);
      } catch {
        // Session may already be gone
      }
      this.sessionId = undefined;
    }
  }
}

interface VarRefData {
  type: 'arguments' | 'globals' | 'struct';
  frameId?: number;
  children?: Array<{ name: string; value: string; type?: string; childRef?: number }>;
}
```

**Step 2: Verify build**

```bash
cd strobe-vscode && npm install && npm run build
```

**Checkpoint:** A complete DAP adapter that handles launch, breakpoints, stepping, threads, stack traces, scopes (Arguments + Globals), variables, evaluate, and disconnect. Polls session status at 200ms to detect breakpoint hits and process exit.

---

### Task 5: DAP Adapter — Registration + Extension Integration

**Files:**
- Modify: `strobe-vscode/src/extension.ts`

**Step 1: Register the debug adapter factory**

In `extension.ts`, import the adapter and register an inline factory:

```typescript
import { StrobeDebugAdapter } from './dap/debug-adapter';
import * as vscode from 'vscode';

// In activate():
context.subscriptions.push(
  vscode.debug.registerDebugAdapterDescriptorFactory('strobe', {
    createDebugAdapterDescriptor(session: vscode.DebugSession) {
      return new vscode.DebugAdapterInlineImplementation(
        new StrobeDebugAdapter(daemonManager)
      );
    },
  })
);
```

**Step 2: Share session state between DAP and existing UI**

The DAP adapter creates its own session via `client.launch()`. To share with the existing sidebar/output channel/status bar, listen for `vscode.debug.onDidStartDebugSession` and `onDidTerminateDebugSession`:

```typescript
// In activate():
context.subscriptions.push(
  vscode.debug.onDidStartDebugSession(async (session) => {
    if (session.type === 'strobe') {
      // The DAP adapter already launched the session.
      // Poll for the sessionId and wire up the existing UI.
      // Note: session.configuration contains the launch config.
      // The adapter stores sessionId internally — we can retrieve it
      // via a custom DAP request or by polling session list.
      try {
        const client = await daemonManager.ensureClient();
        const sessions = await client.listSessions();
        if (sessions.length > 0) {
          const latest = sessions[sessions.length - 1];
          // The latest session is likely the one just launched
          if (latest.sessionId && latest.sessionId !== activeSessionId) {
            activeSessionId = latest.sessionId;
            startSession(latest.sessionId);
          }
        }
      } catch {
        // Best-effort
      }
    }
  })
);

context.subscriptions.push(
  vscode.debug.onDidTerminateDebugSession((session) => {
    if (session.type === 'strobe') {
      endSession();
    }
  })
);
```

**Step 3: Verify build**

```bash
cd strobe-vscode && npm run build
```

**Checkpoint:** F5 with a Strobe launch config launches the process via DAP, breakpoints can be set via the gutter, stepping works via the debug toolbar, stack traces and variables appear in the Debug sidebar. The existing Strobe sidebar and output channel also update because we sync the session ID.

---

### Task 6: Inline Decorations on Traced Functions

**Files:**
- Create: `strobe-vscode/src/editor/decorations.ts`
- Modify: `strobe-vscode/src/extension.ts` (wire decoration manager to polling engine)

**Step 1: Create the decoration manager**

Create `strobe-vscode/src/editor/decorations.ts`:

```typescript
import * as vscode from 'vscode';
import { StrobeEvent } from '../client/types';

const DEBOUNCE_MS = 1000;

interface FunctionStats {
  callCount: number;
  totalDurationNs: number;
  lastReturnValue?: string;
  file?: string;
  line?: number;
}

export class DecorationManager implements vscode.Disposable {
  private stats = new Map<string, FunctionStats>(); // key: "file:line" or function name
  private decorationType: vscode.TextEditorDecorationType;
  private dirty = false;
  private debounceTimer: ReturnType<typeof setTimeout> | undefined;
  private disposables: vscode.Disposable[] = [];

  constructor() {
    this.decorationType = vscode.window.createTextEditorDecorationType({
      after: {
        color: new vscode.ThemeColor('editorCodeLens.foreground'),
        margin: '0 0 0 2em',
        fontStyle: 'italic',
      },
    });

    // Re-render when active editor changes
    this.disposables.push(
      vscode.window.onDidChangeActiveTextEditor(() => this.render()),
    );
  }

  /** Feed new events from the polling engine */
  onEvents(events: StrobeEvent[]): void {
    for (const event of events) {
      if (event.eventType === 'function_exit' && event.function) {
        const key = event.sourceFile && event.line
          ? `${event.sourceFile}:${event.line}`
          : event.function;

        const existing = this.stats.get(key) || {
          callCount: 0,
          totalDurationNs: 0,
          file: event.sourceFile,
          line: event.line,
        };
        existing.callCount++;
        if (event.duration_ns) {
          existing.totalDurationNs += event.duration_ns;
        }
        if (event.returnValue !== undefined) {
          existing.lastReturnValue = formatValue(event.returnValue);
        }
        this.stats.set(key, existing);
        this.dirty = true;
      }
    }

    if (this.dirty) {
      this.scheduleRender();
    }
  }

  private scheduleRender(): void {
    if (this.debounceTimer) return; // Already scheduled
    this.debounceTimer = setTimeout(() => {
      this.debounceTimer = undefined;
      this.dirty = false;
      this.render();
    }, DEBOUNCE_MS);
  }

  private render(): void {
    const editor = vscode.window.activeTextEditor;
    if (!editor) return;

    const filePath = editor.document.uri.fsPath;
    const decorations: vscode.DecorationOptions[] = [];

    for (const [key, stat] of this.stats) {
      // Match by file path
      if (!stat.file || !filePath.endsWith(stat.file.replace(/^.*\//, ''))) continue;
      if (!stat.line) continue;

      const line = stat.line - 1; // VS Code is 0-indexed
      if (line < 0 || line >= editor.document.lineCount) continue;

      const parts: string[] = [];
      parts.push(`${stat.callCount} call${stat.callCount !== 1 ? 's' : ''}`);
      if (stat.callCount > 0 && stat.totalDurationNs > 0) {
        const avgNs = stat.totalDurationNs / stat.callCount;
        parts.push(`avg ${formatDuration(avgNs)}`);
      }
      if (stat.lastReturnValue !== undefined) {
        const rv = stat.lastReturnValue.length > 30
          ? stat.lastReturnValue.slice(0, 30) + '...'
          : stat.lastReturnValue;
        parts.push(`last -> ${rv}`);
      }

      decorations.push({
        range: new vscode.Range(line, 0, line, 0),
        renderOptions: {
          after: { contentText: `  // ${parts.join(' | ')}` },
        },
      });
    }

    editor.setDecorations(this.decorationType, decorations);
  }

  clear(): void {
    this.stats.clear();
    this.dirty = false;
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = undefined;
    }
    // Clear decorations from all visible editors
    for (const editor of vscode.window.visibleTextEditors) {
      editor.setDecorations(this.decorationType, []);
    }
  }

  dispose(): void {
    this.clear();
    this.decorationType.dispose();
    for (const d of this.disposables) d.dispose();
  }
}

function formatDuration(ns: number): string {
  if (ns < 1_000) return `${ns.toFixed(0)}ns`;
  if (ns < 1_000_000) return `${(ns / 1_000).toFixed(1)}us`;
  if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(1)}ms`;
  return `${(ns / 1_000_000_000).toFixed(2)}s`;
}

function formatValue(v: unknown): string {
  if (v === null || v === undefined) return 'null';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}
```

**Step 2: Wire into extension.ts**

```typescript
import { DecorationManager } from './editor/decorations';

// In activate():
const decorationManager = new DecorationManager();
context.subscriptions.push(decorationManager);

// In startSession(), after creating the polling engine:
pollingEngine.on('events', (events) => {
  // ... existing output channel handling ...
  decorationManager.onEvents(events);
});

// In endSession():
decorationManager.clear();
```

**Step 3: Verify build**

```bash
cd strobe-vscode && npm run build
```

**Checkpoint:** While a trace session is running, function definition lines in the editor show faded inline annotations like `// 1,247 calls | avg 0.3ms | last -> 0`. Annotations update at most once per second. They clear when the session ends.

---

### Task 7: Context Menu Enrichment — Breakpoints + Logpoints

**Files:**
- Modify: `strobe-vscode/package.json` (add menu items)
- Modify: `strobe-vscode/src/editor/context-menu.ts` (add handlers)
- Modify: `strobe-vscode/src/extension.ts` (register new commands)

**Step 1: Add menu items to package.json**

Add to `strobe.submenu` array:

```json
{
  "command": "strobe.setBreakpoint",
  "group": "2_debug"
},
{
  "command": "strobe.addLogpoint",
  "group": "2_debug"
}
```

Add to `commands` array:

```json
{
  "command": "strobe.setBreakpoint",
  "title": "Strobe: Set Breakpoint"
},
{
  "command": "strobe.addLogpoint",
  "title": "Strobe: Add Logpoint..."
}
```

**Step 2: Add handlers in context-menu.ts**

Add two new exported functions to `strobe-vscode/src/editor/context-menu.ts`:

```typescript
export async function setBreakpointAtCursor(
  client: StrobeClient,
  sessionId: string | undefined,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;

  const filePath = editor.document.uri.fsPath;
  const line = editor.selection.active.line + 1; // 1-indexed

  if (!sessionId) {
    vscode.window.showWarningMessage('No active Strobe session. Launch a program first.');
    return;
  }

  try {
    await client.setBreakpoints({
      sessionId,
      add: [{ file: filePath, line }],
    });
    vscode.window.showInformationMessage(`Breakpoint set at ${require('path').basename(filePath)}:${line}`);
  } catch (e: any) {
    vscode.window.showErrorMessage(`Failed to set breakpoint: ${e.message}`);
  }
}

export async function addLogpointAtCursor(
  client: StrobeClient,
  sessionId: string | undefined,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;

  const filePath = editor.document.uri.fsPath;
  const line = editor.selection.active.line + 1;

  if (!sessionId) {
    vscode.window.showWarningMessage('No active Strobe session. Launch a program first.');
    return;
  }

  const message = await vscode.window.showInputBox({
    prompt: 'Logpoint message (use {args[0]}, {args[1]}, {threadId} for values)',
    placeHolder: 'value={args[0]}, thread={threadId}',
  });
  if (!message) return;

  try {
    await client.setBreakpoints({
      sessionId,
      add: [{ file: filePath, line, message }],
    });
    vscode.window.showInformationMessage(`Logpoint added at ${require('path').basename(filePath)}:${line}`);
  } catch (e: any) {
    vscode.window.showErrorMessage(`Failed to add logpoint: ${e.message}`);
  }
}
```

**Step 3: Register commands in extension.ts**

```typescript
import { setBreakpointAtCursor, addLogpointAtCursor } from './editor/context-menu';

// In activate():
context.subscriptions.push(
  vscode.commands.registerCommand('strobe.setBreakpoint', async () => {
    const client = await daemonManager.ensureClient();
    await setBreakpointAtCursor(client, activeSessionId);
  }),
  vscode.commands.registerCommand('strobe.addLogpoint', async () => {
    const client = await daemonManager.ensureClient();
    await addLogpointAtCursor(client, activeSessionId);
  }),
);
```

**Step 4: Verify build**

```bash
cd strobe-vscode && npm run build
```

**Checkpoint:** Right-click context menu now shows "Strobe > Trace This Function", "Strobe > Set Breakpoint", "Strobe > Add Logpoint...". The breakpoint command sets a breakpoint at the cursor line. The logpoint command prompts for a message template and sets a logpoint.

---

### Task 8: Final Integration + Build Verification

**Files:**
- Modify: `strobe-vscode/src/extension.ts` (final wiring)
- All files from Tasks 1-7

**Step 1: Create dap/index.ts barrel export**

Create `strobe-vscode/src/dap/index.ts`:

```typescript
export { StrobeDebugAdapter } from './debug-adapter';
export { StrobeLaunchConfig, validateLaunchConfig } from './launch-config';
```

**Step 2: Ensure webpack bundles new dependencies**

The `@vscode/debugadapter` and `@vscode/debugprotocol` packages must be bundled by webpack (they're runtime dependencies, not external like `vscode`). Verify `webpack.config.js` does NOT list them as externals. The current config only externalizes `vscode`, so this should work by default.

**Step 3: Full rebuild and verify**

```bash
# Agent rebuild
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs

# Daemon rebuild
export PATH="$HOME/.cargo/bin:$PATH"
cargo build

# Run daemon tests (existing + new backtrace tests)
cargo test

# Extension rebuild
cd strobe-vscode && npm install && npm run build
```

**Step 4: Manual verification checklist**

1. **launch.json IntelliSense**: Create a launch.json, type "strobe" — should show config snippet with `program`, `args`, `cwd`, `tracePatterns`, `watches` fields.
2. **F5 Launch**: With a compiled binary, F5 should launch the program via Strobe.
3. **Gutter breakpoints**: Click in the gutter to set a breakpoint. The program should pause when the breakpoint is hit.
4. **Debug toolbar**: Step Over, Step Into, Step Out, Continue buttons should work.
5. **Stack trace**: When paused, the Call Stack panel should show the current function and backtrace frames.
6. **Variables**: The Variables panel should show "Arguments" scope with captured arg values, and "Globals" scope with watch values.
7. **Watch expressions**: Adding a watch expression should evaluate via `debug_memory`.
8. **Inline decorations**: While tracing, traced function lines should show call count and avg duration.
9. **Context menu**: Right-click should show "Set Breakpoint" and "Add Logpoint..." options.
10. **Session sync**: The Strobe sidebar, status bar, and output channel should all reflect the DAP session state.

**Checkpoint:** All M3 features working end-to-end. Ready for commit.

---

## Notes

**P5 (step-into callee resolution) is deferred.** The existing `step-into` implementation works — it just behaves identically to `step-over` in some cases. This is documented in the spec as acceptable for M3: "Step-into callee resolution (P5) is significant DWARF work — may ship partially."

**Polling latency.** Breakpoint hits are detected via 200ms session status polling. This means up to 200ms delay between the process pausing and VS Code showing the stopped state. The spec acknowledges this as a known limitation. Server-push events (post-M4) would eliminate this.

**Breakpoint model mismatch.** VS Code's DAP sends `setBreakpoints` per source file (replacing all breakpoints for that file). Strobe's `debug_breakpoint` uses incremental add/remove. The adapter handles this by removing old breakpoints and adding new ones on each `setBreakpoints` call. A more sophisticated diff approach could be added later if needed, but the simple approach works correctly.

**`StrobeClient.readMemory` return type.** The current `strobe-client.ts` may need a type update to match the debug_memory response format. The adapter's `variablesRequest` and `evaluateRequest` handlers should parse the JSON response appropriately. Adjust based on actual daemon response shape during integration.

---

## Review Findings

**Reviewed:** 2026-02-10
**Commits:** `main..feature/mcp-consolidation`

### Issues

#### Issue 1: Breakpoints are never removed (DAP set = replace)
**Severity:** Critical
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:144-211`
**Requirement:** DAP `setBreakpoints` replaces all breakpoints for a source file
**Problem:** The adapter only calls `client.setBreakpoints({ add: targets })` — it never sends `remove` for breakpoints that were previously set but are now absent. Breakpoints accumulate and can never be cleared via the VS Code UI.
**Suggested fix:** Track Strobe breakpoint IDs returned per source file. On each `setBreakPointsRequest`, diff against stored IDs, remove old ones, add new ones:
```typescript
// Before adding:
const oldIds = this.trackedBreakpointIds.get(sourcePath) || [];
if (oldIds.length > 0) {
  await this.client.setBreakpoints({ sessionId, remove: oldIds });
}
// After adding, store new IDs from result
```

#### Issue 2: Breakpoint/logpoint response index mapping is wrong
**Severity:** Critical
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:179-196`
**Problem:** `const allBps = [...result.breakpoints, ...result.logpoints]` concatenates two arrays and maps by index to `requested`. If request mixes breakpoints and logpoints (e.g., `[bp, logpoint, bp]`), the response arrays are `breakpoints: [bp1, bp3]` + `logpoints: [lp2]`, but the code maps `allBps[0]→requested[0]`, `allBps[1]→requested[1]`, `allBps[2]→requested[2]` — wrong.
**Suggested fix:** Partition requested items by `logMessage` presence, map each response array back to the correct request indices separately.

#### Issue 3: Variables scope always reads from first paused thread
**Severity:** Critical
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:400-409`
**Problem:** `const paused = this.pausedThreads[0]` ignores which thread the user selected. Multi-thread breakpoint scenarios show wrong thread's arguments.
**Suggested fix:** Use `refData.frameId` → `getFrameInfo()` → lookup correct `PausedThreadInfo` by Frida thread ID via `reverseThreadMap`.

#### Issue 4: Overlapping async poll calls can corrupt state
**Severity:** Important
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:485-521`
**Problem:** `setInterval` at 200ms fires `pollStatus()` (async). If a poll takes >200ms, multiple are in-flight simultaneously, potentially sending duplicate `StoppedEvent` or racing on `this.pausedThreads`.
**Suggested fix:** Add `private isPolling = false` guard, or switch to chained `setTimeout`.

#### Issue 5: Only first thread's StoppedEvent is emitted
**Severity:** Important
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:499-507`
**Problem:** Only `StoppedEvent` for `pausedThreads[0]` is sent. Other simultaneously paused threads are invisible in the debug UI.
**Suggested fix:** Send `new StoppedEvent('breakpoint', dapThreadId, true)` with `allThreadsStopped: true`.

#### Issue 6: `watches` and `stopOnEntry` launch config silently ignored
**Severity:** Important
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:78-117`, `strobe-vscode/src/dap/launch-config.ts:9-10`
**Problem:** Both fields are declared in the config type and exposed in `package.json` IntelliSense, but `launchRequest` never processes them. Users get no error or warning.
**Suggested fix:** Either implement them (watches via `client.trace()`, stopOnEntry via a deferred resume) or remove from the type + schema until implemented.

#### Issue 7: `readMemory` response incorrectly handled
**Severity:** Important
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:415-419, 463-467`
**Problem:** The adapter treats `readMemory` response as a potentially-raw string (`typeof result === 'string' ? result : JSON.stringify(result)`). The daemon returns `{ results: [{ target, value, fields, error }] }`. The Variables panel will show the full JSON blob instead of the variable value.
**Suggested fix:** Define `ReadMemoryResponse` type in `types.ts`. Extract `result.results[0].value` for display.

#### Issue 8: Decoration file matching is fragile
**Severity:** Important
**Location:** `strobe-vscode/src/editor/decorations.ts:82`
**Problem:** `filePath.endsWith(stat.file.replace(/^.*\//, ''))` does basename-suffix matching. `my-parser.rs` matches any file ending in `parser.rs`.
**Suggested fix:** Use `path.basename()` exact comparison, or resolve full paths.

#### Issue 9: Dual polling when DAP session active
**Severity:** Important
**Location:** `strobe-vscode/src/extension.ts:143-164` + `strobe-vscode/src/dap/debug-adapter.ts:485`
**Problem:** `onDidStartDebugSession` calls `startSession()` which creates a `PollingEngine`, while the DAP adapter has its own `setInterval` poll. Both query `sessionStatus` at ~200ms → doubled RPC traffic.
**Suggested fix:** Skip `startSession()` for DAP sessions, or have the DAP adapter emit events that extension.ts can consume.

#### Issue 10: Spec gap — no "Locals" variable scope
**Severity:** Important
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:368-385`
**Requirement:** Spec says "Variable scopes: Arguments, Globals, best-effort Locals"
**Problem:** Only "Arguments" and "Globals" scopes implemented. No "Locals" scope exists.
**Suggested fix:** Add a placeholder "Locals" scope that shows "Not available for this frame" or attempts DWARF-sourced local variable names when available.

#### Issue 11: `allocFrameId` bit-packing uses arithmetic shift
**Severity:** Minor
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:535-543`
**Problem:** `(frameId >> 16) & 0xffff` uses sign-extending `>>`. If threadId >= 32768, produces wrong results. `getFrameInfo` never returns `undefined` despite the return type.
**Suggested fix:** Use `>>>` (unsigned shift). Consider Map-based approach instead.

#### Issue 12: Dead ternary in ContinuedEvent
**Severity:** Minor
**Location:** `strobe-vscode/src/dap/debug-adapter.ts:298`
**Problem:** `new ContinuedEvent(this.nextDapThreadId > 1 ? 1 : 1, true)` — always 1.
**Suggested fix:** Simplify to `new ContinuedEvent(1, true)`.

#### Issue 13: Duplicate `formatDuration` utility
**Severity:** Minor
**Location:** `strobe-vscode/src/editor/decorations.ts:131` + `strobe-vscode/src/output/output-channel.ts:88`
**Suggested fix:** Extract to shared `utils/format.ts`.

#### Issue 14: Zero TypeScript tests
**Severity:** Critical
**Location:** `strobe-vscode/` (entire directory)
**Problem:** No test files exist for any M3 code. The DAP adapter (580 lines) with complex state management has zero automated coverage.
**Suggested fix:** Priority tests: `StrobeDebugAdapter` protocol compliance (init, launch, stackTrace, variables), `setBreakPointsRequest` breakpoint/logpoint index mapping, `pollStatus` state transitions.

#### Issue 15: Backtrace parsing untested on Rust side
**Severity:** Important
**Location:** `src/frida_collector/spawner.rs:411-438`
**Problem:** The `paused` message handler's backtrace/arguments JSON parsing has no unit test. The existing test (`test_handler_paused_creates_event_and_notification`) does not include backtrace data.
**Suggested fix:** Add a test with a `paused` payload containing populated `backtrace` and `arguments` arrays.

### Approved
- [x] P4: Agent backtrace + arguments capture
- [x] P4: Daemon propagation (spawner → session_manager → MCP response → TS types)
- [x] DAP adapter: launch, breakpoints, stepping, stack traces, scopes, variables, evaluate
- [x] launch.json config with IntelliSense schema
- [x] Breakpoint gutter integration (native DAP)
- [x] Debug toolbar (native DAP)
- [x] Arguments scope (from Interceptor)
- [x] Globals scope (from debug_memory)
- [x] Watch expressions via evaluateRequest
- [x] Inline decorations (debounced, call count, avg duration, last return value)
- [x] Sidebar enrichment: watches, breakpoints, logpoints trees
- [x] Logpoint support via context menu
- [x] "Set Breakpoint" context menu
- [ ] Breakpoint removal — Issue 1
- [ ] Breakpoint/logpoint response mapping — Issue 2
- [ ] Multi-thread variable scopes — Issue 3
- [ ] Locals scope — Issue 10
- [ ] readMemory response parsing — Issue 7
- [ ] Test coverage — Issue 14

### Summary
- Critical: 3 (breakpoint removal, response mapping, no TS tests)
- Important: 8 (poll overlap, single-thread stop, dead config, readMemory, file matching, dual polling, Locals scope, backtrace test)
- Minor: 3 (frame ID shift, dead ternary, duplicate util)
- Ready to merge: **No** — fix Issues 1, 2, 3 first (functional breakage in every debug session)
