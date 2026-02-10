# M1: Core VS Code Extension — Right-Click Trace + Output

**Spec:** `docs/specs/2026-02-10-vscode-extension.md` (M1 section)
**Goal:** Ship a VS Code extension that lets users right-click any function, trace it with Strobe, and see live output — zero config, no recompile.
**Architecture:** TypeScript VS Code extension communicating directly with the Strobe daemon over `~/.strobe/strobe.sock` using JSON-RPC 2.0. The extension replicates the proxy's daemon lifecycle management (auto-start, reconnect) but talks to the socket directly rather than going through `strobe mcp` stdio.
**Tech Stack:** TypeScript, VS Code Extension API, webpack, Node.js `net` module (Unix socket), JSON-RPC 2.0
**Commit strategy:** Single commit at end

## Workstreams

Three independent streams that converge in Task 7:

- **Stream A (Foundation):** Tasks 1, 2, 3 — extension scaffold, StrobeClient, daemon manager
- **Stream B (UI Components):** Tasks 4, 5, 6 — Output Channel, context menus, status bar + sidebar
- **Serial:** Task 7 (integration wiring — depends on A and B), Task 8 (verification)

---

### Task 1: Extension Scaffold

**Files:**
- Create: `strobe-vscode/package.json`
- Create: `strobe-vscode/tsconfig.json`
- Create: `strobe-vscode/webpack.config.js`
- Create: `strobe-vscode/.vscodeignore`
- Create: `strobe-vscode/src/extension.ts`

**Step 1: Initialize npm package**

Run: `mkdir -p strobe-vscode && cd strobe-vscode && npm init -y`

**Step 2: Install dependencies**

Run:
```bash
cd strobe-vscode && npm install --save-dev \
  @types/vscode@^1.85.0 \
  @types/node@^20 \
  typescript@^5.4 \
  webpack@^5 \
  webpack-cli@^5 \
  ts-loader@^9 \
  @vscode/vsce@^3
```

**Step 3: Create `package.json` extension manifest**

```json
{
  "name": "strobe",
  "displayName": "Strobe",
  "description": "Dynamic instrumentation debugger — trace any function at runtime",
  "version": "0.1.0",
  "publisher": "strobe",
  "license": "MIT",
  "engines": { "vscode": "^1.85.0" },
  "categories": ["Debuggers", "Testing"],
  "activationEvents": [],
  "main": "./dist/extension.js",
  "contributes": {
    "commands": [
      {
        "command": "strobe.launch",
        "title": "Strobe: Launch Program"
      },
      {
        "command": "strobe.stop",
        "title": "Strobe: Stop Session"
      },
      {
        "command": "strobe.addTracePattern",
        "title": "Strobe: Add Trace Pattern"
      },
      {
        "command": "strobe.traceFunction",
        "title": "Strobe: Trace This Function"
      }
    ],
    "menus": {
      "editor/context": [
        {
          "submenu": "strobe.submenu",
          "group": "strobe"
        }
      ],
      "strobe.submenu": [
        {
          "command": "strobe.traceFunction",
          "group": "1_trace"
        }
      ]
    },
    "submenus": [
      {
        "id": "strobe.submenu",
        "label": "Strobe"
      }
    ],
    "viewsContainers": {
      "activitybar": [
        {
          "id": "strobe",
          "title": "Strobe",
          "icon": "media/strobe-icon.svg"
        }
      ]
    },
    "views": {
      "strobe": [
        {
          "id": "strobe.session",
          "name": "Session"
        }
      ]
    }
  },
  "scripts": {
    "vscode:prepublish": "npm run build",
    "build": "webpack --mode production",
    "watch": "webpack --mode development --watch",
    "package": "vsce package"
  }
}
```

**Step 4: Create `tsconfig.json`**

```json
{
  "compilerOptions": {
    "module": "commonjs",
    "target": "ES2022",
    "lib": ["ES2022"],
    "outDir": "dist",
    "rootDir": "src",
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "sourceMap": true,
    "declaration": false,
    "resolveJsonModule": true
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist"]
}
```

**Step 5: Create `webpack.config.js`**

```js
'use strict';
const path = require('path');

/** @type {import('webpack').Configuration} */
module.exports = {
  target: 'node',
  mode: 'none',
  entry: './src/extension.ts',
  output: {
    path: path.resolve(__dirname, 'dist'),
    filename: 'extension.js',
    libraryTarget: 'commonjs2'
  },
  externals: {
    vscode: 'commonjs vscode'
  },
  resolve: {
    extensions: ['.ts', '.js']
  },
  module: {
    rules: [{ test: /\.ts$/, exclude: /node_modules/, use: 'ts-loader' }]
  },
  devtool: 'nosources-source-map'
};
```

**Step 6: Create `.vscodeignore`**

```
src/**
node_modules/**
.vscode/**
tsconfig.json
webpack.config.js
```

**Step 7: Create stub `src/extension.ts`**

```typescript
import * as vscode from 'vscode';

export function activate(context: vscode.ExtensionContext): void {
  const outputChannel = vscode.window.createOutputChannel('Strobe');
  outputChannel.appendLine('Strobe extension activated');
}

export function deactivate(): void {}
```

**Step 8: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: Produces `dist/extension.js` with no errors.

**Checkpoint:** Extension scaffold builds. Can be loaded in VS Code Extension Development Host.

---

### Task 2: StrobeClient — JSON-RPC over Unix Socket

**Files:**
- Create: `strobe-vscode/src/client/types.ts`
- Create: `strobe-vscode/src/client/strobe-client.ts`
- Create: `strobe-vscode/src/client/polling-engine.ts`
- Test: `strobe-vscode/src/test/client.test.ts`

This is the core communication layer. The extension talks to the daemon the same way the Rust proxy (`src/mcp/proxy.rs`) does — JSON-RPC 2.0 over `~/.strobe/strobe.sock`, line-delimited.

**Step 1: Write `src/client/types.ts`**

TypeScript types mirroring the Rust `src/mcp/types.rs` structs. All field names use camelCase (matching the Rust `#[serde(rename_all = "camelCase")]`).

```typescript
// ---- JSON-RPC protocol ----

export interface JsonRpcRequest {
  jsonrpc: '2.0';
  id: string | number;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: '2.0';
  id: string | number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

// ---- MCP protocol ----

export interface McpToolCallResponse {
  content: Array<{ type: 'text'; text: string }>;
  isError?: boolean;
}

// ---- debug_launch ----

export interface LaunchOptions {
  command: string;
  args?: string[];
  cwd?: string;
  projectRoot: string;
  env?: Record<string, string>;
}

export interface LaunchResponse {
  sessionId: string;
  pid: number;
  pendingPatternsApplied?: number;
  nextSteps?: string;
}

// ---- debug_session ----

export type SessionAction = 'status' | 'stop' | 'list' | 'delete';

export interface SessionStatusResponse {
  status: 'running' | 'paused' | 'exited';
  pid: number;
  eventCount: number;
  hookedFunctions: number;
  tracePatterns: string[];
  breakpoints: BreakpointInfo[];
  logpoints: LogpointInfo[];
  watches: ActiveWatch[];
  pausedThreads: PausedThreadInfo[];
}

export interface PausedThreadInfo {
  threadId: number;
  breakpointId: string;
  function?: string;
  file?: string;
  line?: number;
}

export interface BreakpointInfo {
  id: string;
  function?: string;
  file?: string;
  line?: number;
  address: string;
}

export interface LogpointInfo {
  id: string;
  message: string;
  function?: string;
  file?: string;
  line?: number;
  address: string;
}

export interface ActiveWatch {
  label: string;
  address: string;
  size: number;
  typeName?: string;
  onPatterns?: string[];
}

// ---- debug_trace ----

export interface TraceRequest {
  sessionId?: string;
  add?: string[];
  remove?: string[];
  serializationDepth?: number;
  projectRoot?: string;
  watches?: {
    add?: WatchTarget[];
    remove?: string[];
  };
}

export interface WatchTarget {
  variable?: string;
  address?: string;
  type?: string;
  label?: string;
  expr?: string;
  on?: string[];
}

export interface TraceResponse {
  mode: 'pending' | 'runtime';
  activePatterns: string[];
  hookedFunctions: number;
  matchedFunctions?: number;
  activeWatches: ActiveWatch[];
  warnings: string[];
  eventLimit: number;
  status?: string;
}

// ---- debug_query ----

export interface QueryRequest {
  sessionId: string;
  eventType?: string;
  function?: { equals?: string; contains?: string; matches?: string };
  sourceFile?: { equals?: string; contains?: string };
  returnValue?: { equals?: unknown; isNull?: boolean };
  threadName?: { contains?: string };
  timeFrom?: number | string;
  timeTo?: number | string;
  minDurationNs?: number;
  pid?: number;
  limit?: number;
  offset?: number;
  verbose?: boolean;
  afterEventId?: number;
}

export interface QueryResponse {
  events: StrobeEvent[];
  totalCount: number;
  hasMore: boolean;
  pids?: number[];
  lastEventId?: number;
  eventsDropped?: boolean;
}

export interface StrobeEvent {
  id: string;
  timestampNs: number;
  eventType?: string;
  function?: string;
  sourceFile?: string;
  line?: number;
  durationNs?: number;
  returnType?: string;
  // verbose fields
  threadId?: number;
  threadName?: string;
  pid?: number;
  arguments?: unknown;
  returnValue?: unknown;
  watchValues?: Record<string, unknown>;
  text?: string;  // for stdout/stderr
}

// ---- debug_breakpoint ----

export interface BreakpointRequest {
  sessionId: string;
  add?: BreakpointTarget[];
  remove?: string[];
}

export interface BreakpointTarget {
  function?: string;
  file?: string;
  line?: number;
  condition?: string;
  hitCount?: number;
  message?: string;  // present = logpoint
}

export interface BreakpointResponse {
  breakpoints: BreakpointInfo[];
  logpoints: LogpointInfo[];
}

// ---- debug_continue ----

export type StepAction = 'continue' | 'step-over' | 'step-into' | 'step-out';

export interface ContinueResponse {
  status: string;
  breakpointId?: string;
  function?: string;
  file?: string;
  line?: number;
}

// ---- debug_memory ----

export interface MemoryReadRequest {
  sessionId: string;
  action?: 'read';
  targets: Array<{
    variable?: string;
    address?: string;
    size?: number;
    type?: string;
  }>;
  depth?: number;
  poll?: { intervalMs: number; durationMs: number };
}

// ---- debug_test ----

export interface TestRunRequest {
  action?: 'run';
  projectRoot: string;
  framework?: string;
  level?: string;
  test?: string;
  command?: string;
  tracePatterns?: string[];
  env?: Record<string, string>;
}

export interface TestStartResponse {
  testRunId: string;
  status: 'running';
  framework: string;
}

export interface TestStatusResponse {
  testRunId: string;
  status: 'running' | 'completed' | 'failed';
  sessionId?: string;
  progress?: {
    elapsedMs: number;
    passed: number;
    failed: number;
    skipped: number;
    currentTest?: string;
    currentTestElapsedMs?: number;
    phase?: string;
    warnings?: Array<{ testName?: string; idleMs: number; diagnosis: string }>;
  };
  result?: {
    framework: string;
    summary: { passed: number; failed: number; skipped: number; durationMs: number };
    failures: Array<{ name: string; file?: string; line?: number; message: string; suggestedTraces: string[] }>;
    stuck: unknown[];
  };
  error?: string;
}

// ---- Error codes ----

export const StrobeErrorCodes = {
  NO_DEBUG_SYMBOLS: 'NO_DEBUG_SYMBOLS',
  SIP_BLOCKED: 'SIP_BLOCKED',
  SESSION_EXISTS: 'SESSION_EXISTS',
  SESSION_NOT_FOUND: 'SESSION_NOT_FOUND',
  PROCESS_EXITED: 'PROCESS_EXITED',
  FRIDA_ATTACH_FAILED: 'FRIDA_ATTACH_FAILED',
  INVALID_PATTERN: 'INVALID_PATTERN',
  VALIDATION_ERROR: 'VALIDATION_ERROR',
} as const;
```

**Step 2: Write `src/client/strobe-client.ts`**

```typescript
import * as net from 'net';
import * as path from 'path';
import * as os from 'os';
import { EventEmitter } from 'events';
import {
  JsonRpcRequest, JsonRpcResponse, McpToolCallResponse,
  LaunchOptions, LaunchResponse,
  SessionStatusResponse,
  TraceRequest, TraceResponse,
  QueryRequest, QueryResponse,
  BreakpointRequest, BreakpointResponse,
  StepAction, ContinueResponse,
  TestRunRequest, TestStartResponse, TestStatusResponse,
} from './types';

const SOCKET_PATH = path.join(os.homedir(), '.strobe', 'strobe.sock');
const PROTOCOL_VERSION = '2024-11-05';

export class StrobeClient extends EventEmitter {
  private socket: net.Socket | null = null;
  private buffer = '';
  private requestId = 0;
  private pending = new Map<string | number, {
    resolve: (value: unknown) => void;
    reject: (err: Error) => void;
  }>();
  private _connected = false;

  get isConnected(): boolean { return this._connected; }

  async connect(): Promise<void> {
    if (this._connected) return;

    this.socket = net.createConnection(SOCKET_PATH);

    await new Promise<void>((resolve, reject) => {
      this.socket!.once('connect', resolve);
      this.socket!.once('error', reject);
    });

    this.socket.on('data', (data) => this.onData(data));
    this.socket.on('close', () => this.onClose());
    this.socket.on('error', (err) => this.emit('error', err));

    this._connected = true;

    // MCP handshake: initialize
    await this.sendRequest('initialize', {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: {},
      clientInfo: { name: 'strobe-vscode', version: '0.1.0' },
    });

    // Send initialized notification (no response expected)
    this.sendNotification('notifications/initialized', {});
  }

  disconnect(): void {
    if (this.socket) {
      this.socket.destroy();
      this.socket = null;
    }
    this._connected = false;
    // Reject all pending requests
    for (const [, pending] of this.pending) {
      pending.reject(new Error('Disconnected'));
    }
    this.pending.clear();
  }

  // ---- Tool methods (map to 8 consolidated MCP tools) ----

  async launch(opts: LaunchOptions): Promise<LaunchResponse> {
    return this.callTool('debug_launch', opts) as Promise<LaunchResponse>;
  }

  async sessionStatus(sessionId: string): Promise<SessionStatusResponse> {
    return this.callTool('debug_session', { action: 'status', sessionId }) as Promise<SessionStatusResponse>;
  }

  async stop(sessionId: string, retain = false): Promise<unknown> {
    return this.callTool('debug_session', { action: 'stop', sessionId, retain });
  }

  async listSessions(): Promise<unknown> {
    return this.callTool('debug_session', { action: 'list' });
  }

  async deleteSession(sessionId: string): Promise<unknown> {
    return this.callTool('debug_session', { action: 'delete', sessionId });
  }

  async trace(req: TraceRequest): Promise<TraceResponse> {
    return this.callTool('debug_trace', req) as Promise<TraceResponse>;
  }

  async query(req: QueryRequest): Promise<QueryResponse> {
    return this.callTool('debug_query', req) as Promise<QueryResponse>;
  }

  async setBreakpoints(req: BreakpointRequest): Promise<BreakpointResponse> {
    return this.callTool('debug_breakpoint', req) as Promise<BreakpointResponse>;
  }

  async continue(sessionId: string, action?: StepAction): Promise<ContinueResponse> {
    return this.callTool('debug_continue', { sessionId, action }) as Promise<ContinueResponse>;
  }

  async readMemory(req: { sessionId: string; targets: Array<{ variable?: string; address?: string; size?: number; type?: string }>; depth?: number }): Promise<unknown> {
    return this.callTool('debug_memory', { ...req, action: 'read' });
  }

  async runTest(req: TestRunRequest): Promise<TestStartResponse> {
    return this.callTool('debug_test', { ...req, action: 'run' }) as Promise<TestStartResponse>;
  }

  async testStatus(testRunId: string): Promise<TestStatusResponse> {
    return this.callTool('debug_test', { action: 'status', testRunId }) as Promise<TestStatusResponse>;
  }

  // ---- Protocol layer ----

  private async callTool(name: string, args: unknown): Promise<unknown> {
    const response = await this.sendRequest('tools/call', { name, arguments: args });
    const toolResp = response as McpToolCallResponse;

    if (toolResp.isError) {
      const text = toolResp.content?.[0]?.text ?? 'Unknown error';
      throw new StrobeError(text);
    }

    // Tool responses wrap the actual JSON in a text content block
    const text = toolResp.content?.[0]?.text;
    if (!text) return {};
    return JSON.parse(text);
  }

  private sendRequest(method: string, params: unknown): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = ++this.requestId;
      this.pending.set(id, { resolve, reject });

      const msg: JsonRpcRequest = {
        jsonrpc: '2.0',
        id,
        method,
        params,
      };

      this.socket!.write(JSON.stringify(msg) + '\n');
    });
  }

  private sendNotification(method: string, params: unknown): void {
    const msg = { jsonrpc: '2.0', method, params };
    this.socket!.write(JSON.stringify(msg) + '\n');
  }

  private onData(data: Buffer): void {
    this.buffer += data.toString();
    let newlineIdx: number;
    while ((newlineIdx = this.buffer.indexOf('\n')) !== -1) {
      const line = this.buffer.slice(0, newlineIdx);
      this.buffer = this.buffer.slice(newlineIdx + 1);
      if (line.trim()) {
        this.handleMessage(line);
      }
    }
  }

  private handleMessage(line: string): void {
    try {
      const msg = JSON.parse(line) as JsonRpcResponse;
      const pending = this.pending.get(msg.id);
      if (pending) {
        this.pending.delete(msg.id);
        if (msg.error) {
          pending.reject(new StrobeError(
            `${msg.error.message}`,
            msg.error.data as string | undefined,
          ));
        } else {
          pending.resolve(msg.result);
        }
      }
    } catch {
      // Ignore malformed messages
    }
  }

  private onClose(): void {
    this._connected = false;
    this.emit('disconnected');
  }
}

export class StrobeError extends Error {
  public readonly code?: string;
  constructor(message: string, code?: string) {
    super(message);
    this.name = 'StrobeError';
    this.code = code;
  }
}
```

**Step 3: Write `src/client/polling-engine.ts`**

Two-tier polling as specified in the spec:
- **Fast path:** `debug_session({ action: "status" })` every 200ms — detects pause/exit
- **Event path:** `debug_query({ afterEventId })` every 500ms — feeds Output Channel

```typescript
import { EventEmitter } from 'events';
import { StrobeClient } from './strobe-client';
import { StrobeEvent, SessionStatusResponse } from './types';

export interface PollingEvents {
  events: (events: StrobeEvent[]) => void;
  status: (status: SessionStatusResponse) => void;
  sessionEnd: (sessionId: string) => void;
  eventsDropped: () => void;
  error: (err: Error) => void;
}

export class PollingEngine extends EventEmitter {
  private statusTimer: ReturnType<typeof setInterval> | null = null;
  private eventTimer: ReturnType<typeof setInterval> | null = null;
  private cursor: number | undefined;
  private lastStatus: string | undefined;
  private polling = false;

  constructor(
    private client: StrobeClient,
    private sessionId: string,
    private statusIntervalMs = 200,
    private eventIntervalMs = 500,
  ) {
    super();
  }

  start(): void {
    if (this.polling) return;
    this.polling = true;

    // Fast path: session status
    this.statusTimer = setInterval(() => this.pollStatus(), this.statusIntervalMs);

    // Event path: incremental query
    this.eventTimer = setInterval(() => this.pollEvents(), this.eventIntervalMs);
  }

  stop(): void {
    this.polling = false;
    if (this.statusTimer) { clearInterval(this.statusTimer); this.statusTimer = null; }
    if (this.eventTimer) { clearInterval(this.eventTimer); this.eventTimer = null; }
  }

  private async pollStatus(): Promise<void> {
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.emit('status', status);

      // Detect session end
      if (status.status === 'exited' && this.lastStatus !== 'exited') {
        this.emit('sessionEnd', this.sessionId);
      }
      this.lastStatus = status.status;
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes('SESSION_NOT_FOUND')) {
        this.emit('sessionEnd', this.sessionId);
        this.stop();
      } else {
        this.emit('error', err instanceof Error ? err : new Error(msg));
      }
    }
  }

  private async pollEvents(): Promise<void> {
    try {
      const resp = await this.client.query({
        sessionId: this.sessionId,
        afterEventId: this.cursor,
        limit: 200,
        verbose: true,
      });

      if (resp.events.length > 0) {
        this.emit('events', resp.events);
      }

      if (resp.lastEventId !== undefined) {
        this.cursor = resp.lastEventId;
      }

      if (resp.eventsDropped) {
        this.emit('eventsDropped');
      }
    } catch (err: unknown) {
      // Suppress SESSION_NOT_FOUND (handled by status poll)
      const msg = err instanceof Error ? err.message : String(err);
      if (!msg.includes('SESSION_NOT_FOUND')) {
        this.emit('error', err instanceof Error ? err : new Error(msg));
      }
    }
  }
}
```

**Step 4: Write test**

Create `strobe-vscode/src/test/client.test.ts`:

```typescript
import * as assert from 'assert';
// Unit tests for type parsing — no live daemon needed

import { StrobeError } from '../client/strobe-client';

describe('StrobeClient', () => {
  it('StrobeError has code', () => {
    const err = new StrobeError('test message', 'SESSION_NOT_FOUND');
    assert.strictEqual(err.code, 'SESSION_NOT_FOUND');
    assert.strictEqual(err.message, 'test message');
  });
});
```

**Step 5: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS — no compilation errors.

**Checkpoint:** StrobeClient can connect to daemon, send MCP handshake, call all 8 tools, and parse responses. PollingEngine provides two-tier event streaming.

---

### Task 3: DaemonManager — Auto-start + Binary Resolution

**Files:**
- Create: `strobe-vscode/src/utils/daemon-manager.ts`

The extension must auto-start the daemon if it's not running. This mirrors the Rust proxy logic in `src/mcp/proxy.rs:147-211`:

1. Try connecting to `~/.strobe/strobe.sock`
2. If fails, clean stale `.pid`/`.sock` files
3. Spawn `strobe daemon` process
4. Poll for socket availability (max 5s)

**Step 1: Write `src/utils/daemon-manager.ts`**

```typescript
import * as net from 'net';
import * as path from 'path';
import * as os from 'os';
import * as fs from 'fs';
import * as cp from 'child_process';
import { StrobeClient } from '../client/strobe-client';

const STROBE_DIR = path.join(os.homedir(), '.strobe');
const SOCKET_PATH = path.join(STROBE_DIR, 'strobe.sock');
const PID_PATH = path.join(STROBE_DIR, 'strobe.pid');

export class DaemonManager {
  private binaryPath: string;
  private client: StrobeClient | null = null;

  constructor(extensionPath: string) {
    // Binary bundled at <extensionPath>/bin/strobe
    // During development, fall back to PATH or a configured path
    const bundledPath = path.join(extensionPath, 'bin', 'strobe');
    if (fs.existsSync(bundledPath)) {
      this.binaryPath = bundledPath;
    } else {
      // Development fallback: find strobe in PATH or use target/debug
      this.binaryPath = 'strobe';
    }
  }

  async ensureClient(): Promise<StrobeClient> {
    if (this.client?.isConnected) return this.client;

    // Try connecting to existing daemon
    if (await this.tryConnect()) {
      return this.client!;
    }

    // Start daemon and connect
    await this.startDaemon();
    if (await this.tryConnect(50, 100)) {  // 50 attempts x 100ms = 5s
      return this.client!;
    }

    throw new Error('Daemon failed to start within 5 seconds. Check ~/.strobe/daemon.log');
  }

  private async tryConnect(attempts = 1, delayMs = 0): Promise<boolean> {
    for (let i = 0; i < attempts; i++) {
      if (i > 0 && delayMs > 0) {
        await new Promise(r => setTimeout(r, delayMs));
      }
      try {
        this.client = new StrobeClient();
        await this.client.connect();
        return true;
      } catch {
        this.client = null;
      }
    }
    return false;
  }

  private async startDaemon(): Promise<void> {
    // Ensure ~/.strobe/ exists
    fs.mkdirSync(STROBE_DIR, { recursive: true });

    // Clean stale files (mirrors proxy.rs:177-191)
    this.cleanupStaleFiles();

    // Spawn daemon
    const logPath = path.join(STROBE_DIR, 'daemon.log');
    const logFd = fs.openSync(logPath, 'a');

    const child = cp.spawn(this.binaryPath, ['daemon'], {
      detached: true,
      stdio: ['ignore', 'ignore', logFd],
      env: { ...process.env, RUST_LOG: process.env.RUST_LOG ?? 'info' },
    });

    child.unref();
    fs.closeSync(logFd);
  }

  private cleanupStaleFiles(): void {
    try {
      const pidStr = fs.readFileSync(PID_PATH, 'utf-8').trim();
      const pid = parseInt(pidStr, 10);
      if (isNaN(pid)) return;

      try {
        // Check if process is alive (signal 0 doesn't kill, just checks)
        process.kill(pid, 0);
        // Process is alive — don't clean up
      } catch {
        // Process is dead — clean stale files
        try { fs.unlinkSync(SOCKET_PATH); } catch {}
        try { fs.unlinkSync(PID_PATH); } catch {}
      }
    } catch {
      // No PID file — nothing to clean
    }
  }

  getClient(): StrobeClient | null {
    return this.client?.isConnected ? this.client : null;
  }

  dispose(): void {
    this.client?.disconnect();
    this.client = null;
  }
}
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** DaemonManager can locate the strobe binary, auto-start the daemon, handle stale file cleanup, and provide a connected StrobeClient.

---

### Task 4: Output Channel — Live Event Formatting

**Files:**
- Create: `strobe-vscode/src/output/output-channel.ts`

This formats Strobe events into the Output Channel. Formatting follows the spec:
- `→` for function_enter (with args)
- `←` for function_exit (with return value + duration)
- `⏸` for breakpoint pauses
- `📝` for logpoint messages
- stdout/stderr prefixed

```typescript
import * as vscode from 'vscode';
import { StrobeEvent } from '../client/types';

export class StrobeOutputChannel {
  private channel: vscode.OutputChannel;

  constructor() {
    this.channel = vscode.window.createOutputChannel('Strobe');
  }

  show(): void {
    this.channel.show(true);  // preserveFocus
  }

  clear(): void {
    this.channel.clear();
  }

  appendEvents(events: StrobeEvent[]): void {
    for (const event of events) {
      this.channel.appendLine(this.formatEvent(event));
    }
  }

  appendLine(text: string): void {
    this.channel.appendLine(text);
  }

  dispose(): void {
    this.channel.dispose();
  }

  private formatEvent(event: StrobeEvent): string {
    const ts = this.formatTimestamp(event.timestampNs);
    const eventType = event.eventType ?? (event as Record<string, unknown>).event_type as string | undefined;

    switch (eventType) {
      case 'function_enter':
        return `[${ts}] → ${event.function ?? '??'}(${this.formatArgs(event.arguments)})`;

      case 'function_exit': {
        const dur = event.durationNs != null ? ` [${this.formatDuration(event.durationNs)}]` : '';
        const ret = event.returnValue !== undefined ? ` → ${this.formatValue(event.returnValue)}` : '';
        return `[${ts}] ← ${event.function ?? '??'}${ret}${dur}`;
      }

      case 'stdout':
        return `[${ts}] stdout: ${event.text ?? ''}`;

      case 'stderr':
        return `[${ts}] stderr: ${event.text ?? ''}`;

      case 'pause':
        return `[${ts}] ⏸ PAUSED at ${event.sourceFile ?? '??'}:${event.line ?? '?'} (breakpoint)`;

      case 'logpoint':
        return `[${ts}] 📝 logpoint: ${event.text ?? ''}`;

      case 'crash':
        return `[${ts}] 💥 CRASH: ${event.text ?? 'Process crashed'}`;

      case 'variable_snapshot':
        return `[${ts}] 📊 ${this.formatWatchValues(event.watchValues)}`;

      default:
        return `[${ts}] ${eventType ?? 'unknown'}: ${JSON.stringify(event)}`;
    }
  }

  private formatTimestamp(ns: number): string {
    // Convert nanoseconds since session start to HH:MM:SS.mmm
    const totalMs = ns / 1_000_000;
    const h = Math.floor(totalMs / 3_600_000);
    const m = Math.floor((totalMs % 3_600_000) / 60_000);
    const s = Math.floor((totalMs % 60_000) / 1000);
    const ms = Math.floor(totalMs % 1000);
    return `${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}.${ms.toString().padStart(3, '0')}`;
  }

  private formatDuration(ns: number): string {
    if (ns < 1000) return `${ns}ns`;
    if (ns < 1_000_000) return `${(ns / 1000).toFixed(1)}µs`;
    if (ns < 1_000_000_000) return `${(ns / 1_000_000).toFixed(1)}ms`;
    return `${(ns / 1_000_000_000).toFixed(2)}s`;
  }

  private formatArgs(args: unknown): string {
    if (!args) return '';
    if (Array.isArray(args)) {
      return args.map(a => this.formatValue(a)).join(', ');
    }
    if (typeof args === 'object') {
      return Object.entries(args as Record<string, unknown>)
        .map(([k, v]) => `${k}=${this.formatValue(v)}`)
        .join(', ');
    }
    return String(args);
  }

  private formatValue(val: unknown): string {
    if (val === null || val === undefined) return 'null';
    if (typeof val === 'string') return val.length > 80 ? val.slice(0, 80) + '…' : val;
    if (typeof val === 'number' || typeof val === 'boolean') return String(val);
    return JSON.stringify(val);
  }

  private formatWatchValues(wv: Record<string, unknown> | undefined): string {
    if (!wv) return '';
    return Object.entries(wv).map(([k, v]) => `${k} = ${this.formatValue(v)}`).join(', ');
  }
}
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** Output Channel formats all event types with readable timestamps and duration formatting.

---

### Task 5: Context Menu + Function Identification

**Files:**
- Create: `strobe-vscode/src/editor/function-identifier.ts`
- Create: `strobe-vscode/src/editor/context-menu.ts`
- Create: `strobe-vscode/src/profiles/language-profile.ts`

The context menu command "Strobe: Trace This Function" needs to:
1. Identify the function at the cursor position
2. Format it as a Strobe pattern (e.g., `module::function` for Rust/C++)
3. If a session is active, add the pattern; if not, prompt for binary and launch

**Step 1: Write `src/profiles/language-profile.ts`**

```typescript
export interface LanguageProfile {
  id: string;
  displayName: string;
  filePatterns: string[];
  instrumentationMode: 'native' | 'runtime';
  symbolSource: 'dwarf' | 'runtime';
  patternSeparator: string;  // "::" for native, "." for runtime
}

const rustProfile: LanguageProfile = {
  id: 'rust',
  displayName: 'Rust',
  filePatterns: ['*.rs'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '::',
};

const cppProfile: LanguageProfile = {
  id: 'cpp',
  displayName: 'C/C++',
  filePatterns: ['*.cpp', '*.cc', '*.c', '*.cxx', '*.h', '*.hpp', '*.hxx'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '::',
};

const swiftProfile: LanguageProfile = {
  id: 'swift',
  displayName: 'Swift',
  filePatterns: ['*.swift'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '.',
};

const goProfile: LanguageProfile = {
  id: 'go',
  displayName: 'Go',
  filePatterns: ['*.go'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '.',
};

export const builtinProfiles: LanguageProfile[] = [rustProfile, cppProfile, swiftProfile, goProfile];

export function detectProfile(languageId: string): LanguageProfile | undefined {
  switch (languageId) {
    case 'rust': return rustProfile;
    case 'c': case 'cpp': return cppProfile;
    case 'swift': return swiftProfile;
    case 'go': return goProfile;
    default: return undefined;
  }
}
```

**Step 2: Write `src/editor/function-identifier.ts`**

Uses VS Code's `DocumentSymbolProvider` (from language server — rust-analyzer, clangd, etc.) with regex fallback.

```typescript
import * as vscode from 'vscode';

export interface IdentifiedFunction {
  name: string;
  containerName?: string;
  range: vscode.Range;
}

/**
 * Identify the function at the cursor position.
 * Primary: VS Code DocumentSymbolProvider (from LSP).
 * Fallback: regex heuristics.
 */
export async function identifyFunctionAtCursor(
  document: vscode.TextDocument,
  position: vscode.Position,
): Promise<IdentifiedFunction | undefined> {
  // Try LSP symbols first
  const symbols = await vscode.commands.executeCommand<vscode.DocumentSymbol[]>(
    'vscode.executeDocumentSymbolProvider',
    document.uri,
  );

  if (symbols && symbols.length > 0) {
    const fn = findFunctionContaining(symbols, position);
    if (fn) return fn;
  }

  // Fallback: regex-based identification
  return regexIdentify(document, position);
}

function findFunctionContaining(
  symbols: vscode.DocumentSymbol[],
  position: vscode.Position,
): IdentifiedFunction | undefined {
  for (const sym of symbols) {
    if (sym.range.contains(position)) {
      // Check children first (more specific match)
      if (sym.children.length > 0) {
        const child = findFunctionContaining(sym.children, position);
        if (child) {
          // Prefix with parent name for qualified name
          if (!child.containerName && isContainer(sym)) {
            child.containerName = sym.name;
          }
          return child;
        }
      }
      if (isFunction(sym)) {
        return { name: sym.name, range: sym.range };
      }
    }
  }
  return undefined;
}

function isFunction(sym: vscode.DocumentSymbol): boolean {
  return sym.kind === vscode.SymbolKind.Function
    || sym.kind === vscode.SymbolKind.Method;
}

function isContainer(sym: vscode.DocumentSymbol): boolean {
  return sym.kind === vscode.SymbolKind.Class
    || sym.kind === vscode.SymbolKind.Module
    || sym.kind === vscode.SymbolKind.Namespace
    || sym.kind === vscode.SymbolKind.Struct;
}

// Regex patterns for common function definitions
const FUNCTION_PATTERNS = [
  // Rust: fn name(
  /^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/,
  // C/C++: ReturnType name( or ReturnType Class::name(
  /^\s*(?:[\w:*&<>]+\s+)+(\w+(?:::\w+)*)\s*\(/,
  // Swift: func name(
  /^\s*(?:public\s+|private\s+|internal\s+|fileprivate\s+|open\s+)?(?:static\s+)?func\s+(\w+)/,
  // Go: func name( or func (r Receiver) name(
  /^\s*func\s+(?:\([^)]*\)\s+)?(\w+)\s*\(/,
];

function regexIdentify(
  document: vscode.TextDocument,
  position: vscode.Position,
): IdentifiedFunction | undefined {
  // Search upward from cursor to find the enclosing function definition
  for (let line = position.line; line >= Math.max(0, position.line - 50); line--) {
    const text = document.lineAt(line).text;
    for (const pattern of FUNCTION_PATTERNS) {
      const match = pattern.exec(text);
      if (match) {
        return {
          name: match[1],
          range: new vscode.Range(line, 0, line, text.length),
        };
      }
    }
  }
  return undefined;
}

/**
 * Format an identified function as a Strobe trace pattern.
 */
export function formatPattern(fn: IdentifiedFunction, separator: string): string {
  if (fn.containerName) {
    return `${fn.containerName}${separator}${fn.name}`;
  }
  // For unqualified names, use wildcard prefix to match any namespace
  return `*${separator}${fn.name}`;
}
```

**Step 3: Write `src/editor/context-menu.ts`**

```typescript
import * as vscode from 'vscode';
import { identifyFunctionAtCursor, formatPattern } from './function-identifier';
import { detectProfile } from '../profiles/language-profile';

export interface TraceCommandDeps {
  getSessionId: () => string | undefined;
  addPattern: (pattern: string) => Promise<void>;
  launchAndTrace: (pattern: string) => Promise<void>;
}

export function registerContextMenuCommands(
  context: vscode.ExtensionContext,
  deps: TraceCommandDeps,
): void {
  context.subscriptions.push(
    vscode.commands.registerCommand('strobe.traceFunction', async () => {
      const editor = vscode.window.activeTextEditor;
      if (!editor) return;

      const fn = await identifyFunctionAtCursor(editor.document, editor.selection.active);
      if (!fn) {
        vscode.window.showWarningMessage('Strobe: Could not identify a function at cursor position.');
        return;
      }

      const profile = detectProfile(editor.document.languageId);
      const separator = profile?.patternSeparator ?? '::';
      const pattern = formatPattern(fn, separator);

      if (deps.getSessionId()) {
        // Session active: add pattern to active session
        await deps.addPattern(pattern);
        vscode.window.showInformationMessage(`Strobe: Tracing ${pattern}`);
      } else {
        // No session: prompt for binary and launch
        await deps.launchAndTrace(pattern);
      }
    }),
  );
}
```

**Step 4: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** Right-click context menu identifies functions via LSP (rust-analyzer, clangd) with regex fallback, formats Strobe patterns, and routes to active session or launch flow.

---

### Task 6: Status Bar + Basic Sidebar

**Files:**
- Create: `strobe-vscode/src/utils/status-bar.ts`
- Create: `strobe-vscode/src/sidebar/sidebar-provider.ts`

**Step 1: Write `src/utils/status-bar.ts`**

```typescript
import * as vscode from 'vscode';
import { SessionStatusResponse } from '../client/types';

export class StrobeStatusBar {
  private item: vscode.StatusBarItem;

  constructor() {
    this.item = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
    this.item.command = 'strobe.launch';
    this.setDisconnected();
    this.item.show();
  }

  setDisconnected(): void {
    this.item.text = '$(circle-slash) Strobe';
    this.item.tooltip = 'Strobe: Not connected to daemon';
    this.item.color = undefined;
  }

  setConnected(): void {
    this.item.text = '$(circle-large-outline) Strobe: idle';
    this.item.tooltip = 'Strobe: Connected, no active session';
    this.item.color = new vscode.ThemeColor('statusBar.foreground');
  }

  setSession(status: SessionStatusResponse, sessionId: string): void {
    const events = status.eventCount.toLocaleString();
    const hooks = status.hookedFunctions;
    const icon = status.status === 'paused' ? '$(debug-pause)' : '$(circle-filled)';
    this.item.text = `${icon} Strobe: ${sessionId.split('-').slice(0, -3).join('-') || sessionId} (PID ${status.pid}) | ${events} events | ${hooks} hooks`;
    this.item.tooltip = `Strobe: ${status.status}\nPatterns: ${status.tracePatterns.join(', ') || 'none'}`;
    this.item.color = new vscode.ThemeColor('statusBar.foreground');
  }

  dispose(): void {
    this.item.dispose();
  }
}
```

**Step 2: Write `src/sidebar/sidebar-provider.ts`**

Basic TreeView showing active session info, trace patterns, watches, breakpoints, logpoints.

```typescript
import * as vscode from 'vscode';
import { SessionStatusResponse } from '../client/types';

type TreeNode = SessionNode | CategoryNode | LeafNode;

class SessionNode extends vscode.TreeItem {
  constructor(public status: SessionStatusResponse, public sessionId: string) {
    super(`Session: ${sessionId}`, vscode.TreeItemCollapsibleState.Expanded);
    this.description = `PID ${status.pid} | ${status.eventCount.toLocaleString()} events`;
    this.iconPath = new vscode.ThemeIcon(
      status.status === 'paused' ? 'debug-pause' : 'circle-filled',
    );
  }
}

class CategoryNode extends vscode.TreeItem {
  constructor(
    label: string,
    public items: LeafNode[],
    icon: string,
  ) {
    super(label, items.length > 0 ? vscode.TreeItemCollapsibleState.Expanded : vscode.TreeItemCollapsibleState.None);
    this.description = `(${items.length})`;
    this.iconPath = new vscode.ThemeIcon(icon);
  }
}

class LeafNode extends vscode.TreeItem {
  constructor(label: string, description?: string, icon?: string) {
    super(label, vscode.TreeItemCollapsibleState.None);
    if (description) this.description = description;
    if (icon) this.iconPath = new vscode.ThemeIcon(icon);
  }
}

export class SidebarProvider implements vscode.TreeDataProvider<TreeNode> {
  private _onDidChangeTreeData = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private status: SessionStatusResponse | null = null;
  private sessionId: string | null = null;

  update(sessionId: string, status: SessionStatusResponse): void {
    this.sessionId = sessionId;
    this.status = status;
    this._onDidChangeTreeData.fire();
  }

  clear(): void {
    this.sessionId = null;
    this.status = null;
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: TreeNode): vscode.TreeItem {
    return element;
  }

  getChildren(element?: TreeNode): TreeNode[] {
    if (!element) {
      // Root level
      if (!this.status || !this.sessionId) {
        return [new LeafNode('No active session', 'Launch or attach to begin', 'info')];
      }
      return [new SessionNode(this.status, this.sessionId)];
    }

    if (element instanceof SessionNode) {
      const s = element.status;

      const patternNodes = s.tracePatterns.map(p =>
        new LeafNode(p, `${s.hookedFunctions} hooks`, 'zap'));

      const watchNodes = s.watches.map(w =>
        new LeafNode(w.label, w.typeName ?? '', 'eye'));

      const bpNodes = s.breakpoints.map(bp =>
        new LeafNode(
          bp.function ?? `${bp.file}:${bp.line}`,
          bp.id,
          'circle-filled',
        ));

      const lpNodes = s.logpoints.map(lp =>
        new LeafNode(
          lp.function ?? `${lp.file}:${lp.line}`,
          lp.message,
          'output',
        ));

      return [
        new CategoryNode('Trace Patterns', patternNodes, 'zap'),
        new CategoryNode('Watches', watchNodes, 'eye'),
        new CategoryNode('Breakpoints', bpNodes, 'debug-breakpoint'),
        new CategoryNode('Logpoints', lpNodes, 'output'),
      ];
    }

    if (element instanceof CategoryNode) {
      return element.items;
    }

    return [];
  }
}
```

**Step 3: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** Status bar shows connection state + session info. Sidebar TreeView displays session, patterns, watches, breakpoints, logpoints from `session_status` poll.

---

### Task 7: Wire Everything Together in `extension.ts`

**Files:**
- Modify: `strobe-vscode/src/extension.ts`

This is the integration point that connects all components:

```typescript
import * as vscode from 'vscode';
import { DaemonManager } from './utils/daemon-manager';
import { StrobeOutputChannel } from './output/output-channel';
import { StrobeStatusBar } from './utils/status-bar';
import { SidebarProvider } from './sidebar/sidebar-provider';
import { PollingEngine } from './client/polling-engine';
import { registerContextMenuCommands } from './editor/context-menu';
import { StrobeClient } from './client/strobe-client';
import { SessionStatusResponse, StrobeEvent } from './client/types';

let daemonManager: DaemonManager;
let outputChannel: StrobeOutputChannel;
let statusBar: StrobeStatusBar;
let sidebarProvider: SidebarProvider;
let pollingEngine: PollingEngine | null = null;
let activeSessionId: string | undefined;

export function activate(context: vscode.ExtensionContext): void {
  daemonManager = new DaemonManager(context.extensionPath);
  outputChannel = new StrobeOutputChannel();
  statusBar = new StrobeStatusBar();
  sidebarProvider = new SidebarProvider();

  // Register sidebar
  const treeView = vscode.window.createTreeView('strobe.session', {
    treeDataProvider: sidebarProvider,
  });

  // Register commands
  context.subscriptions.push(
    treeView,
    outputChannel,
    statusBar,

    vscode.commands.registerCommand('strobe.launch', cmdLaunch),
    vscode.commands.registerCommand('strobe.stop', cmdStop),
    vscode.commands.registerCommand('strobe.addTracePattern', cmdAddTracePattern),
  );

  // Register context menu commands
  registerContextMenuCommands(context, {
    getSessionId: () => activeSessionId,
    addPattern: async (pattern: string) => {
      const client = daemonManager.getClient();
      if (!client || !activeSessionId) return;
      await client.trace({ sessionId: activeSessionId, add: [pattern] });
    },
    launchAndTrace: async (pattern: string) => {
      // Prompt for binary, launch, then trace
      const binary = await vscode.window.showInputBox({
        prompt: 'Path to executable',
        placeHolder: '/path/to/binary',
      });
      if (!binary) return;

      const projectRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '.';
      const client = await daemonManager.ensureClient();
      const resp = await client.launch({ command: binary, projectRoot });
      startSession(client, resp.sessionId);
      await client.trace({ sessionId: resp.sessionId, add: [pattern] });
      outputChannel.show();
      vscode.window.showInformationMessage(`Strobe: Tracing ${pattern} on ${resp.sessionId}`);
    },
  });
}

async function cmdLaunch(): Promise<void> {
  try {
    const binary = await vscode.window.showInputBox({
      prompt: 'Path to executable',
      placeHolder: '/path/to/binary',
    });
    if (!binary) return;

    statusBar.setConnected();
    const client = await daemonManager.ensureClient();
    statusBar.setConnected();

    const projectRoot = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '.';
    const resp = await client.launch({ command: binary, projectRoot });

    startSession(client, resp.sessionId);
    outputChannel.show();
    vscode.window.showInformationMessage(`Strobe: Launched ${binary} (session: ${resp.sessionId})`);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
    statusBar.setDisconnected();
  }
}

async function cmdStop(): Promise<void> {
  if (!activeSessionId) {
    vscode.window.showWarningMessage('Strobe: No active session to stop.');
    return;
  }
  try {
    const client = daemonManager.getClient();
    if (client) {
      await client.stop(activeSessionId);
    }
    endSession();
    vscode.window.showInformationMessage('Strobe: Session stopped.');
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
  }
}

async function cmdAddTracePattern(): Promise<void> {
  if (!activeSessionId) {
    vscode.window.showWarningMessage('Strobe: No active session. Launch first.');
    return;
  }

  const pattern = await vscode.window.showInputBox({
    prompt: 'Trace pattern (e.g., myapp::*, @file:parser.cpp)',
    placeHolder: 'module::function',
  });
  if (!pattern) return;

  try {
    const client = daemonManager.getClient();
    if (!client) throw new Error('Not connected');
    const resp = await client.trace({ sessionId: activeSessionId, add: [pattern] });
    vscode.window.showInformationMessage(`Strobe: Tracing ${pattern} (${resp.hookedFunctions} hooks)`);
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
  }
}

function startSession(client: StrobeClient, sessionId: string): void {
  activeSessionId = sessionId;

  // Start polling
  pollingEngine = new PollingEngine(client, sessionId);

  pollingEngine.on('status', (status: SessionStatusResponse) => {
    statusBar.setSession(status, sessionId);
    sidebarProvider.update(sessionId, status);
  });

  pollingEngine.on('events', (events: StrobeEvent[]) => {
    outputChannel.appendEvents(events);
  });

  pollingEngine.on('sessionEnd', () => {
    outputChannel.appendLine(`--- Session ${sessionId} ended ---`);
    endSession();
  });

  pollingEngine.on('eventsDropped', () => {
    outputChannel.appendLine('⚠ Events were dropped (FIFO buffer full). Consider increasing events.maxPerSession.');
  });

  pollingEngine.start();
}

function endSession(): void {
  pollingEngine?.stop();
  pollingEngine = null;
  activeSessionId = undefined;
  statusBar.setConnected();
  sidebarProvider.clear();
}

export function deactivate(): void {
  pollingEngine?.stop();
  daemonManager.dispose();
}
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS — produces `dist/extension.js`.

**Checkpoint:** Complete wiring: launch command prompts for binary → connects to daemon → starts session → polling feeds output channel + sidebar + status bar. Right-click trace identifies function and routes appropriately. Stop command cleans up.

---

### Task 8: Create SVG Icon + Manual Verification

**Files:**
- Create: `strobe-vscode/media/strobe-icon.svg`

**Step 1: Create minimal activity bar icon**

```svg
<svg width="24" height="24" viewBox="0 0 24 24" fill="none" xmlns="http://www.w3.org/2000/svg">
  <path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
```

**Step 2: Build and package**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Step 3: Manual end-to-end verification**

In VS Code Extension Development Host:
1. Open a Rust/C++ project
2. Run "Strobe: Launch Program" from command palette
3. Enter path to a debug-built binary
4. Verify: Status bar shows green with session info
5. Verify: Output Channel "Strobe" shows stdout/stderr
6. Add a trace pattern via command palette
7. Verify: Output Channel shows function enter/exit events
8. Right-click a function → Strobe > Trace This Function
9. Verify: Pattern is added to active session
10. Verify: Sidebar shows session info, patterns
11. Run "Strobe: Stop Session"
12. Verify: Status bar returns to idle, sidebar clears

**Step 4: Test with a simple C program**

```c
// /tmp/test_strobe.c
#include <stdio.h>
void greet(const char* name) { printf("Hello, %s!\n", name); }
int main() { for(int i = 0; i < 3; i++) greet("world"); return 0; }
```

```bash
gcc -g -o /tmp/test_strobe /tmp/test_strobe.c
```

Launch via Strobe, trace `*::greet`, verify 3 function_enter/function_exit pairs appear in Output Channel.

**Checkpoint:** Full M1 feature set verified end-to-end. Ready for commit.

---

## Risk Mitigations

| Risk | Mitigation |
|------|-----------|
| Unix socket from VS Code extension | Node.js `net` module handles Unix sockets natively — same as TCP. Well-tested. |
| Daemon auto-start reliability | Mirrors proven logic from `src/mcp/proxy.rs`. Poll with backoff, max 5s timeout. |
| LSP function identification not available | Regex fallback covers Rust, C/C++, Swift, Go function declarations. |
| Polling latency (200ms status, 500ms events) | Acceptable for M1. Server-push events planned for post-M4. |
| Large event volumes in Output Channel | VS Code Output Channel handles high throughput. Events limited to 200 per poll cycle. |

## File Summary

```
strobe-vscode/
├── package.json                      # Extension manifest
├── tsconfig.json                     # TypeScript config
├── webpack.config.js                 # Bundle config
├── .vscodeignore                     # VSIX exclusions
├── media/
│   └── strobe-icon.svg               # Activity bar icon
├── src/
│   ├── extension.ts                  # activate/deactivate, command registration, wiring
│   ├── client/
│   │   ├── types.ts                  # TypeScript types mirroring Rust MCP types
│   │   ├── strobe-client.ts          # JSON-RPC client over Unix socket
│   │   └── polling-engine.ts         # Two-tier polling (status + events)
│   ├── editor/
│   │   ├── function-identifier.ts    # LSP + regex function detection
│   │   └── context-menu.ts           # "Trace This Function" command
│   ├── output/
│   │   └── output-channel.ts         # Event formatting for Output Channel
│   ├── profiles/
│   │   └── language-profile.ts       # Rust, C++, Swift, Go profiles
│   ├── sidebar/
│   │   └── sidebar-provider.ts       # TreeDataProvider for session tree
│   ├── utils/
│   │   ├── daemon-manager.ts         # Auto-start daemon, binary resolution
│   │   └── status-bar.ts             # Status bar item
│   └── test/
│       └── client.test.ts            # Unit tests
└── dist/
    └── extension.js                  # Webpack output (gitignored)
```

---

## Review Findings

**Reviewed:** 2026-02-10
**Method:** 4 parallel review agents (StrobeClient+protocol, DaemonManager+scaffold, UI components, Editor+wiring)
**Verified:** Critical findings cross-checked against daemon source (`src/daemon/server.rs:format_event`)

### Issues

#### Issue 1: Event field names mismatch — `timestamp_ns` / `duration_ns` are snake_case in daemon
**Severity:** Critical
**Location:** [types.ts:152-169](strobe-vscode/src/client/types.ts#L152-L169), [output-channel.ts:34](strobe-vscode/src/output/output-channel.ts#L34)
**Requirement:** Types must match daemon JSON output
**Problem:** The daemon's `format_event()` in `server.rs:37` uses `"timestamp_ns"` and `"duration_ns"` (snake_case, hand-built `json!{}` macros), but the TS `StrobeEvent` interface uses `timestampNs` / `durationNs` (camelCase). All timestamps and durations will be `undefined`, causing NaN in the output channel.
**Suggested fix:**
```typescript
// In types.ts StrobeEvent:
export interface StrobeEvent {
  id: string;
  timestamp_ns: number;    // was: timestampNs
  eventType?: string;
  function?: string;
  sourceFile?: string;
  line?: number;
  duration_ns?: number;    // was: durationNs
  // ... rest unchanged
}
// Update all references in output-channel.ts accordingly
```

#### Issue 2: `ActiveWatch.onPatterns` should be `on`
**Severity:** Critical
**Location:** [types.ts:87](strobe-vscode/src/client/types.ts#L87)
**Requirement:** Match Rust `ActiveWatch` struct (`src/mcp/types.rs:110`)
**Problem:** Rust struct has `pub on: Option<Vec<String>>` with `rename_all = "camelCase"` — but `on` is a single word, so JSON key is `"on"`, not `"onPatterns"`. Sidebar won't display watch scoping.
**Suggested fix:**
```typescript
export interface ActiveWatch {
  label: string;
  address: string;
  size: number;
  typeName?: string;
  on?: string[];  // was: onPatterns
}
```

#### Issue 3: `variable_snapshot` reads `watchValues` but daemon sends `data`
**Severity:** Critical
**Location:** [output-channel.ts:71](strobe-vscode/src/output/output-channel.ts#L71)
**Requirement:** Match daemon's `format_event` output (`server.rs:56`: `"data": event.arguments`)
**Problem:** Variable snapshot events will render as empty because the code reads `event.watchValues` but the daemon serializes the data under the `"data"` key.
**Suggested fix:**
```typescript
// In StrobeEvent, add:
data?: unknown;  // variable_snapshot payload

// In output-channel.ts:
case 'variable_snapshot': {
  const data = (event as unknown as Record<string, unknown>).data as Record<string, unknown> | undefined;
  return `[${ts}] 📊 ${this.formatWatchValues(data)}`;
}
```

#### Issue 4: `logpoint` events read `text` but daemon sends `logpointMessage`
**Severity:** Critical
**Location:** [output-channel.ts:65](strobe-vscode/src/output/output-channel.ts#L65)
**Requirement:** Match daemon's `format_event` output (`server.rs:87`: `"logpointMessage"`)
**Problem:** Logpoint messages will show empty because the daemon uses `logpointMessage`, not `text`. The `text` field is only for stdout/stderr.
**Suggested fix:**
```typescript
// In StrobeEvent, add:
logpointMessage?: string;

// In output-channel.ts:
case 'logpoint':
  return `[${ts}] 📝 logpoint: ${event.logpointMessage ?? ''}`;
```

#### Issue 5: `crash` events lack structured fields
**Severity:** Critical
**Location:** [output-channel.ts:68](strobe-vscode/src/output/output-channel.ts#L68)
**Requirement:** Match daemon's crash event format (`server.rs:35-46`)
**Problem:** Crash events include `signal`, `faultAddress`, `registers`, `backtrace`, `locals` — but the output channel reads `event.text` which doesn't exist on crash events.
**Suggested fix:**
```typescript
// Add crash fields to StrobeEvent or use dynamic access:
case 'crash': {
  const e = event as unknown as Record<string, unknown>;
  const signal = e.signal ?? 'unknown';
  const addr = e.faultAddress ?? '';
  return `[${ts}] 💥 CRASH: signal ${signal}${addr ? ` at ${addr}` : ''}`;
}
```

#### Issue 6: Unhandled `error` event from PollingEngine crashes extension host
**Severity:** Critical
**Location:** [extension.ts:156-183](strobe-vscode/src/extension.ts#L156-L183)
**Requirement:** Node.js EventEmitter throws if `error` event has no listener
**Problem:** `PollingEngine` emits `'error'` events (polling-engine.ts:74, 103), but `startSession()` in extension.ts doesn't register an `'error'` handler. Any polling error will crash the extension host process.
**Suggested fix:**
```typescript
// In startSession(), add:
pollingEngine.on('error', (err: Error) => {
  outputChannel.appendLine(`⚠ Polling error: ${err.message}`);
});
```

#### Issue 7: `statusBar.setConnected()` called prematurely in `cmdLaunch`
**Severity:** Important
**Location:** [extension.ts:86](strobe-vscode/src/extension.ts#L86)
**Requirement:** Status bar should reflect actual state
**Problem:** `statusBar.setConnected()` is called on line 86 before `ensureClient()` on line 87. If daemon start fails, the status bar shows "connected" until the catch block runs `setDisconnected()`. The duplicate call on line 88 is also redundant.
**Suggested fix:**
```typescript
// Remove line 86, keep only line 88 (after ensureClient succeeds):
const client = await daemonManager.ensureClient();
statusBar.setConnected();
```

#### Issue 8: `onClose` doesn't reject pending promises
**Severity:** Important
**Location:** [strobe-client.ts:262-265](strobe-vscode/src/client/strobe-client.ts#L262-L265)
**Requirement:** Clean resource lifecycle
**Problem:** When the socket closes unexpectedly, `onClose()` sets `_connected = false` and emits `'disconnected'`, but doesn't reject pending request promises. Those promises will hang forever, leaking memory.
**Suggested fix:**
```typescript
private onClose(): void {
  this._connected = false;
  for (const [, p] of this.pending) {
    p.reject(new Error('Connection closed'));
  }
  this.pending.clear();
  this.emit('disconnected');
}
```

#### Issue 9: No try/catch in context-menu traceFunction command
**Severity:** Important
**Location:** [context-menu.ts:19-47](strobe-vscode/src/editor/context-menu.ts#L19-L47)
**Requirement:** Commands must not throw unhandled exceptions
**Problem:** `identifyFunctionAtCursor()`, `deps.addPattern()`, and `deps.launchAndTrace()` can all throw, but there's no try/catch. Errors will surface as ugly "command failed" messages.
**Suggested fix:**
```typescript
vscode.commands.registerCommand('strobe.traceFunction', async () => {
  try {
    // ... existing logic ...
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
  }
});
```

#### Issue 10: PollingEngine doesn't stop on session exit
**Severity:** Important
**Location:** [polling-engine.ts:64-67](strobe-vscode/src/client/polling-engine.ts#L64-L67)
**Requirement:** Stop polling when session ends
**Problem:** When `pollStatus()` detects `status === 'exited'`, it emits `'sessionEnd'` but doesn't call `this.stop()`. Polling continues hitting the daemon with status/query requests for a dead session. The `SESSION_NOT_FOUND` path (line 72) does call `stop()`, but if the session transitions through `exited` first, there's a window of wasted requests.
**Suggested fix:**
```typescript
if (status.status === 'exited' && this.lastStatus !== 'exited') {
  this.emit('sessionEnd', this.sessionId);
  this.stop();  // Add this
}
```

#### Issue 11: `isFunction` misses `SymbolKind.Constructor`
**Severity:** Important
**Location:** [function-identifier.ts:57-62](strobe-vscode/src/editor/function-identifier.ts#L57-L62)
**Requirement:** Identify all traceable function types
**Problem:** C++ constructors/destructors are reported as `SymbolKind.Constructor` by clangd. The current code only checks `Function` and `Method`, so right-clicking a constructor won't offer tracing.
**Suggested fix:**
```typescript
function isFunction(sym: vscode.DocumentSymbol): boolean {
  return sym.kind === vscode.SymbolKind.Function
    || sym.kind === vscode.SymbolKind.Method
    || sym.kind === vscode.SymbolKind.Constructor;
}
```

#### Issue 12: `ensureClient()` race condition
**Severity:** Important
**Location:** [daemon-manager.ts:28-46](strobe-vscode/src/utils/daemon-manager.ts#L28-L46)
**Requirement:** Thread-safe daemon lifecycle
**Problem:** If `cmdLaunch` and `launchAndTrace` run concurrently, both can enter `ensureClient()` simultaneously, spawning two daemons. The daemon's flock prevents dual listen, but one spawn is wasted.
**Suggested fix:**
```typescript
private connectPromise: Promise<StrobeClient> | null = null;

async ensureClient(): Promise<StrobeClient> {
  if (this.client?.isConnected) return this.client;
  if (this.connectPromise) return this.connectPromise;
  this.connectPromise = this._ensureClient();
  try {
    return await this.connectPromise;
  } finally {
    this.connectPromise = null;
  }
}
```

#### Issue 13: Sidebar refreshes every 200ms even when data unchanged
**Severity:** Minor
**Location:** [sidebar-provider.ts:56-60](strobe-vscode/src/sidebar/sidebar-provider.ts#L56-L60)
**Requirement:** Efficient UI updates
**Problem:** `update()` fires `_onDidChangeTreeData` unconditionally on every poll cycle (200ms). This is wasteful — the tree rebuilds even when nothing changed.
**Suggested fix:**
```typescript
update(sessionId: string, status: SessionStatusResponse): void {
  if (this.sessionId === sessionId && this.status?.eventCount === status.eventCount
      && this.status?.hookedFunctions === status.hookedFunctions
      && this.status?.status === status.status) return;
  this.sessionId = sessionId;
  this.status = status;
  this._onDidChangeTreeData.fire();
}
```

#### Issue 14: Rust regex misses `pub(crate)` visibility modifier
**Severity:** Minor
**Location:** [function-identifier.ts:76](strobe-vscode/src/editor/function-identifier.ts#L76)
**Requirement:** Regex fallback for Rust function identification
**Problem:** Rust regex `/^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/` doesn't match `pub(crate) fn foo()` or `pub(super) fn foo()`.
**Suggested fix:**
```typescript
/^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(\w+)/
```

#### Issue 15: `StrobeError.code` not parsed from tool-level errors
**Severity:** Minor
**Location:** [strobe-client.ts:193-196](strobe-vscode/src/client/strobe-client.ts#L193-L196)
**Requirement:** Extract error codes for programmatic handling (e.g., `SESSION_NOT_FOUND`)
**Problem:** When `callTool` throws on `isError`, it creates `StrobeError(text)` without parsing out the error code. The `code` field is never populated for tool errors, only for JSON-RPC level errors.
**Suggested fix:**
```typescript
if (response.isError) {
  const text = response.content?.[0]?.text ?? 'Unknown error';
  // Try to extract error code from the text (daemon includes it)
  const codeMatch = text.match(/^([A-Z_]+):/);
  throw new StrobeError(text, codeMatch?.[1]);
}
```

#### Issue 16: No initial poll on PollingEngine start
**Severity:** Minor
**Location:** [polling-engine.ts:29-43](strobe-vscode/src/client/polling-engine.ts#L29-L43)
**Requirement:** Immediate feedback on session start
**Problem:** `start()` uses `setInterval` which fires after the first interval delay (200ms/500ms), not immediately. There's a brief window where the UI shows stale data.
**Suggested fix:**
```typescript
start(): void {
  if (this.polling) return;
  this.polling = true;
  this.pollStatus();  // Immediate first poll
  this.pollEvents();
  this.statusTimer = setInterval(() => this.pollStatus(), this.statusIntervalMs);
  this.eventTimer = setInterval(() => this.pollEvents(), this.eventIntervalMs);
}
```

### Approved
- [x] Extension scaffold (package.json, tsconfig, webpack, .vscodeignore) — correct
- [x] JSON-RPC client protocol layer (sendRequest, handleMessage, buffer parsing) — correct
- [x] MCP handshake (initialize → notifications/initialized) — correct
- [x] Tool method wrappers (all 8 MCP tools covered) — correct
- [x] Daemon auto-start + stale file cleanup — correct, mirrors proxy.rs
- [x] Binary resolution (bundled + PATH fallback) — correct
- [x] Language profiles (Rust, C++, Swift, Go) — correct
- [x] Function identification (LSP + regex fallback) — correct approach
- [x] Context menu registration + routing — correct
- [x] Status bar (3 states: disconnected/connected/session) — correct
- [x] Sidebar TreeView (session → categories → items) — correct
- [x] Polling engine (two-tier: 200ms status, 500ms events) — correct approach
- [ ] StrobeEvent type fields — Issues 1, 3, 4, 5
- [ ] ActiveWatch type fields — Issue 2
- [ ] Error handling in commands — Issues 6, 7, 9
- [ ] Resource cleanup — Issues 8, 10
- [ ] Function identifier completeness — Issues 11, 14

### Summary
- **Critical: 6** (Issues 1-6 — all data rendering bugs or extension crash)
- **Important: 6** (Issues 7-12 — UX/reliability/resource management)
- **Minor: 4** (Issues 13-16 — polish)
- **Ready to merge: No** — Critical issues must be fixed first
