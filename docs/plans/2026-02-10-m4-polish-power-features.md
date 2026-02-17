# M4: Polish & Power Features — Implementation Plan

**Spec:** `docs/specs/2026-02-10-vscode-extension.md` (M4 section, lines 671-684)
**Goal:** Add power-user features to the VS Code extension — memory inspector, live watch viewer with polling, contextual watch scoping, retained session management, settings UI, keyboard shortcuts, theme-aware decorations, Go language profile enhancements, and marketplace preparation.
**Architecture:** All features build on existing `StrobeClient` methods (`readMemory`, `listSessions`, `deleteSession`, `trace` for watches) and the `PollingEngine`. New webview panels for memory inspector and watch viewer. Sidebar gains a "Retained Sessions" section. Settings use VS Code's `configuration` contribution point, mapped to `~/.strobe/settings.json` on save.
**Tech Stack:** VS Code Webview API, `vscode.workspace.getConfiguration()`, existing `StrobeClient` + `PollingEngine`, `@vscode/debugadapter`
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Memory):** Tasks 1, 2 — memory write client method + memory inspector webview panel
- **Stream B (Watches):** Tasks 3, 4 — live watch viewer webview + contextual watch scoping UI
- **Stream C (Sessions):** Task 5 — retained session management sidebar + reopen
- **Stream D (Settings):** Task 6 — VS Code settings contribution + sync to `settings.json`
- **Stream E (Polish):** Tasks 7, 8, 9 — keyboard shortcuts, theme-aware decorations, Go profile
- **Task 10 (Marketplace):** Icon, README, changelog — depends on all above

Streams A-E can start in parallel. Task 10 is serial after all others.

---

### Task 1: StrobeClient — Memory Write + Session List Types

**Files:**
- Modify: `strobe-vscode/src/client/strobe-client.ts` (~4 lines)
- Modify: `strobe-vscode/src/client/types.ts` (~25 lines)

**Step 1: Add `MemoryWriteRequest` and `SessionSummary` types**

In `types.ts`, add after the existing `ReadMemoryResponse`:

```typescript
// ---- debug_memory (write) ----

export interface MemoryWriteRequest {
  sessionId: string;
  action: 'write';
  targets: Array<{
    variable?: string;
    address?: string;
    type?: string;
    value: number | boolean;
  }>;
}

export interface WriteMemoryResponse {
  results: Array<{
    target: string;
    success: boolean;
    error?: string;
  }>;
}

// ---- debug_session (list) ----

export interface SessionSummary {
  sessionId: string;
  binaryPath: string;
  pid: number;
  startedAt: number;
  endedAt?: number;
  status: string;
  retainedAt?: number;
  sizeBytes?: number;
}

export interface ListSessionsResponse {
  sessions: SessionSummary[];
  totalSize: number;
}
```

**Step 2: Add `writeMemory` and typed `listSessions` methods to `StrobeClient`**

In `strobe-client.ts`, add:

```typescript
async writeMemory(req: MemoryWriteRequest): Promise<WriteMemoryResponse> {
  return this.callTool('debug_memory', req) as Promise<WriteMemoryResponse>;
}
```

Update the existing `listSessions` return type:

```typescript
async listSessions(): Promise<ListSessionsResponse> {
  return this.callTool('debug_session', { action: 'list' }) as Promise<ListSessionsResponse>;
}
```

**Verification:** `npm run build` in `strobe-vscode/` — no type errors.

---

### Task 2: Memory Inspector Webview Panel

**Files:**
- Create: `strobe-vscode/src/memory/memory-panel.ts` (~200 lines)
- Modify: `strobe-vscode/src/extension.ts` (~15 lines — register command)
- Modify: `strobe-vscode/package.json` (~8 lines — command contribution)

**Step 1: Create `MemoryPanel` class**

The memory inspector is a webview panel that lets users:
1. Enter a variable name or hex address
2. Read its value (with struct expansion up to depth 3)
3. Edit values in-place (write back via `debug_memory action:write`)
4. Poll a value at a configurable interval

```typescript
// strobe-vscode/src/memory/memory-panel.ts

import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';
import { ReadResult } from '../client/types';

export class MemoryPanel {
  public static readonly viewType = 'strobe.memoryInspector';
  private static currentPanel: MemoryPanel | undefined;

  private readonly panel: vscode.WebviewPanel;
  private readonly client: StrobeClient;
  private sessionId: string;
  private pollTimer: ReturnType<typeof setInterval> | undefined;
  private disposables: vscode.Disposable[] = [];

  static createOrShow(
    client: StrobeClient,
    sessionId: string,
    extensionUri: vscode.Uri,
  ): MemoryPanel {
    if (MemoryPanel.currentPanel) {
      MemoryPanel.currentPanel.sessionId = sessionId;
      MemoryPanel.currentPanel.panel.reveal();
      return MemoryPanel.currentPanel;
    }

    const panel = vscode.window.createWebviewPanel(
      MemoryPanel.viewType,
      'Strobe: Memory Inspector',
      vscode.ViewColumn.Beside,
      { enableScripts: true, retainContextWhenHidden: true },
    );

    MemoryPanel.currentPanel = new MemoryPanel(panel, client, sessionId);
    return MemoryPanel.currentPanel;
  }

  private constructor(
    panel: vscode.WebviewPanel,
    client: StrobeClient,
    sessionId: string,
  ) {
    this.panel = panel;
    this.client = client;
    this.sessionId = sessionId;

    this.panel.webview.html = this.getHtml();

    this.panel.webview.onDidReceiveMessage(
      (msg) => this.handleMessage(msg),
      null,
      this.disposables,
    );

    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);
  }

  private async handleMessage(msg: {
    command: string;
    target?: string;
    type?: string;
    value?: number | boolean;
    depth?: number;
    pollIntervalMs?: number;
  }): Promise<void> {
    switch (msg.command) {
      case 'read': {
        if (!msg.target) return;
        try {
          const isAddr = msg.target.startsWith('0x');
          const result = await this.client.readMemory({
            sessionId: this.sessionId,
            targets: [isAddr
              ? { address: msg.target, type: msg.type }
              : { variable: msg.target }],
            depth: msg.depth ?? 3,
          });
          this.panel.webview.postMessage({
            command: 'readResult',
            results: result.results,
          });
        } catch (e: unknown) {
          const errMsg = e instanceof Error ? e.message : String(e);
          this.panel.webview.postMessage({
            command: 'error',
            message: errMsg,
          });
        }
        break;
      }
      case 'write': {
        if (!msg.target || msg.value === undefined) return;
        try {
          const isAddr = msg.target.startsWith('0x');
          await this.client.writeMemory({
            sessionId: this.sessionId,
            action: 'write',
            targets: [isAddr
              ? { address: msg.target, type: msg.type, value: msg.value }
              : { variable: msg.target, value: msg.value }],
          });
          this.panel.webview.postMessage({
            command: 'writeSuccess',
            target: msg.target,
          });
        } catch (e: unknown) {
          const errMsg = e instanceof Error ? e.message : String(e);
          this.panel.webview.postMessage({
            command: 'error',
            message: errMsg,
          });
        }
        break;
      }
      case 'startPoll': {
        this.stopPoll();
        if (!msg.target) return;
        const intervalMs = msg.pollIntervalMs ?? 500;
        this.pollTimer = setInterval(async () => {
          try {
            const isAddr = msg.target!.startsWith('0x');
            const result = await this.client.readMemory({
              sessionId: this.sessionId,
              targets: [isAddr
                ? { address: msg.target!, type: msg.type }
                : { variable: msg.target! }],
            });
            this.panel.webview.postMessage({
              command: 'pollResult',
              results: result.results,
              timestamp: Date.now(),
            });
          } catch {
            // Suppress poll errors — session may have ended
          }
        }, intervalMs);
        break;
      }
      case 'stopPoll': {
        this.stopPoll();
        break;
      }
    }
  }

  private stopPoll(): void {
    if (this.pollTimer) {
      clearInterval(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  private getHtml(): string {
    // Returns a self-contained HTML document with:
    // - Input field for variable/address
    // - Type dropdown (auto, i32, u32, f32, f64, pointer, etc.)
    // - Depth selector (1-5)
    // - Read/Write/Poll buttons
    // - Results table showing target, type, value, address
    // - Editable value cells for write-back
    // Uses CSS variables from VS Code theme (--vscode-editor-background, etc.)
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <style>
    body {
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--vscode-foreground);
      background: var(--vscode-editor-background);
      padding: 12px;
    }
    .controls { display: flex; gap: 8px; margin-bottom: 12px; flex-wrap: wrap; }
    input, select, button {
      font-family: inherit; font-size: inherit;
      color: var(--vscode-input-foreground);
      background: var(--vscode-input-background);
      border: 1px solid var(--vscode-input-border);
      padding: 4px 8px;
      border-radius: 2px;
    }
    button {
      background: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
      border: none;
      cursor: pointer;
      padding: 4px 12px;
    }
    button:hover { background: var(--vscode-button-hoverBackground); }
    table { width: 100%; border-collapse: collapse; margin-top: 8px; }
    th, td {
      text-align: left; padding: 4px 8px;
      border-bottom: 1px solid var(--vscode-widget-border);
    }
    th { color: var(--vscode-descriptionForeground); font-weight: 600; }
    .editable {
      cursor: pointer;
      border-bottom: 1px dashed var(--vscode-textLink-foreground);
    }
    .error { color: var(--vscode-errorForeground); margin-top: 8px; }
    .poll-indicator { color: var(--vscode-charts-green); font-size: 0.85em; }
    #target { flex: 1; min-width: 200px; }
  </style>
</head>
<body>
  <div class="controls">
    <input id="target" placeholder="Variable name or 0x address" />
    <select id="typeHint">
      <option value="">auto</option>
      <option value="i8">i8</option><option value="u8">u8</option>
      <option value="i16">i16</option><option value="u16">u16</option>
      <option value="i32">i32</option><option value="u32">u32</option>
      <option value="i64">i64</option><option value="u64">u64</option>
      <option value="f32">f32</option><option value="f64">f64</option>
      <option value="pointer">pointer</option>
    </select>
    <select id="depth">
      <option value="1">Depth 1</option>
      <option value="2">Depth 2</option>
      <option value="3" selected>Depth 3</option>
      <option value="4">Depth 4</option>
      <option value="5">Depth 5</option>
    </select>
    <button id="btnRead">Read</button>
    <button id="btnPoll">Poll</button>
    <button id="btnStopPoll" style="display:none">Stop</button>
  </div>
  <div id="error" class="error"></div>
  <table>
    <thead><tr><th>Target</th><th>Type</th><th>Value</th><th>Address</th><th></th></tr></thead>
    <tbody id="results"></tbody>
  </table>
  <script>
    const vscode = acquireVsCodeApi();
    const targetEl = document.getElementById('target');
    const typeEl = document.getElementById('typeHint');
    const depthEl = document.getElementById('depth');
    const resultsEl = document.getElementById('results');
    const errorEl = document.getElementById('error');
    const btnPoll = document.getElementById('btnPoll');
    const btnStopPoll = document.getElementById('btnStopPoll');
    let polling = false;

    document.getElementById('btnRead').addEventListener('click', () => {
      const target = targetEl.value.trim();
      if (!target) return;
      errorEl.textContent = '';
      vscode.postMessage({
        command: 'read',
        target,
        type: typeEl.value || undefined,
        depth: parseInt(depthEl.value),
      });
    });

    btnPoll.addEventListener('click', () => {
      const target = targetEl.value.trim();
      if (!target) return;
      polling = true;
      btnPoll.style.display = 'none';
      btnStopPoll.style.display = '';
      vscode.postMessage({
        command: 'startPoll',
        target,
        type: typeEl.value || undefined,
        pollIntervalMs: 500,
      });
    });

    btnStopPoll.addEventListener('click', () => {
      polling = false;
      btnPoll.style.display = '';
      btnStopPoll.style.display = 'none';
      vscode.postMessage({ command: 'stopPoll' });
    });

    targetEl.addEventListener('keydown', (e) => {
      if (e.key === 'Enter') document.getElementById('btnRead').click();
    });

    window.addEventListener('message', (event) => {
      const msg = event.data;
      if (msg.command === 'readResult' || msg.command === 'pollResult') {
        renderResults(msg.results, msg.command === 'pollResult');
      } else if (msg.command === 'writeSuccess') {
        errorEl.textContent = '';
        // Re-read to show updated value
        vscode.postMessage({
          command: 'read',
          target: msg.target,
          type: typeEl.value || undefined,
          depth: parseInt(depthEl.value),
        });
      } else if (msg.command === 'error') {
        errorEl.textContent = msg.message;
      }
    });

    function renderResults(results, isPoll) {
      resultsEl.innerHTML = '';
      for (const r of results) {
        const tr = document.createElement('tr');
        const valueStr = r.error
          ? r.error
          : r.fields
            ? JSON.stringify(r.fields, null, 1)
            : String(r.value ?? '<null>');
        const canWrite = !r.error && r.fields === undefined;
        tr.innerHTML =
          '<td>' + esc(r.target) + '</td>' +
          '<td>' + esc(r.type || '') + '</td>' +
          '<td' + (canWrite ? ' class="editable" title="Click to edit"' : '') + '>' +
            esc(valueStr) +
            (isPoll ? ' <span class="poll-indicator">\\u25CF</span>' : '') +
          '</td>' +
          '<td>' + esc(r.address || '') + '</td>' +
          '<td>' + (canWrite ? '<button class="write-btn">Write</button>' : '') + '</td>';
        if (canWrite) {
          const td = tr.querySelector('.editable');
          const btn = tr.querySelector('.write-btn');
          btn.addEventListener('click', () => {
            const newVal = prompt('New value for ' + r.target, String(r.value ?? ''));
            if (newVal === null) return;
            const num = Number(newVal);
            vscode.postMessage({
              command: 'write',
              target: r.target,
              type: typeEl.value || r.type || undefined,
              value: isNaN(num) ? (newVal === 'true' ? true : newVal === 'false' ? false : num) : num,
            });
          });
        }
        resultsEl.appendChild(tr);
      }
    }

    function esc(s) {
      const d = document.createElement('div');
      d.textContent = s;
      return d.innerHTML;
    }
  </script>
</body>
</html>`;
  }

  private dispose(): void {
    MemoryPanel.currentPanel = undefined;
    this.stopPoll();
    for (const d of this.disposables) d.dispose();
    this.panel.dispose();
  }
}
```

**Step 2: Register command in `extension.ts`**

Add to the commands section in `activate()`:

```typescript
vscode.commands.registerCommand('strobe.openMemoryInspector', async () => {
  if (!activeSessionId) {
    vscode.window.showWarningMessage('Strobe: No active session. Launch a program first.');
    return;
  }
  const client = daemonManager.getClient();
  if (!client) return;
  MemoryPanel.createOrShow(client, activeSessionId, context.extensionUri);
}),
```

**Step 3: Add command to `package.json`**

In the `contributes.commands` array:

```json
{
  "command": "strobe.openMemoryInspector",
  "title": "Strobe: Open Memory Inspector"
}
```

**Verification:** `npm run build` — compiles. Manually test: launch a program, open memory inspector, type `gTempo` → shows value. Edit → value changes on next read.

---

### Task 3: Live Watch Variable Viewer (Webview)

**Files:**
- Create: `strobe-vscode/src/memory/watch-panel.ts` (~220 lines)
- Modify: `strobe-vscode/src/extension.ts` (~20 lines — register command + wire to polling)
- Modify: `strobe-vscode/package.json` (~4 lines — command)

**Step 1: Create `WatchPanel` class**

The watch panel shows all active watches from the session status and polls their current values. It updates live as the target runs.

```typescript
// strobe-vscode/src/memory/watch-panel.ts

import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';
import { ActiveWatch, ReadResult } from '../client/types';

export class WatchPanel {
  public static readonly viewType = 'strobe.watchViewer';
  private static currentPanel: WatchPanel | undefined;

  private readonly panel: vscode.WebviewPanel;
  private readonly client: StrobeClient;
  private sessionId: string;
  private pollTimer: ReturnType<typeof setInterval> | undefined;
  private watches: ActiveWatch[] = [];
  private disposables: vscode.Disposable[] = [];

  static createOrShow(
    client: StrobeClient,
    sessionId: string,
  ): WatchPanel {
    if (WatchPanel.currentPanel) {
      WatchPanel.currentPanel.sessionId = sessionId;
      WatchPanel.currentPanel.panel.reveal();
      return WatchPanel.currentPanel;
    }

    const panel = vscode.window.createWebviewPanel(
      WatchPanel.viewType,
      'Strobe: Watch Variables',
      vscode.ViewColumn.Beside,
      { enableScripts: true, retainContextWhenHidden: true },
    );

    WatchPanel.currentPanel = new WatchPanel(panel, client, sessionId);
    return WatchPanel.currentPanel;
  }

  private constructor(
    panel: vscode.WebviewPanel,
    client: StrobeClient,
    sessionId: string,
  ) {
    this.panel = panel;
    this.client = client;
    this.sessionId = sessionId;
    this.panel.webview.html = this.getHtml();
    this.panel.webview.onDidReceiveMessage(
      (msg) => this.handleMessage(msg),
      null,
      this.disposables,
    );
    this.panel.onDidDispose(() => this.dispose(), null, this.disposables);

    // Start polling immediately
    this.startPoll();
  }

  /** Called externally from PollingEngine status events to update watch list */
  updateWatches(watches: ActiveWatch[]): void {
    this.watches = watches;
  }

  private async handleMessage(msg: { command: string; label?: string }): Promise<void> {
    if (msg.command === 'removeWatch' && msg.label) {
      try {
        await this.client.trace({
          sessionId: this.sessionId,
          watches: { remove: [msg.label] },
        });
      } catch {
        // Best-effort
      }
    }
  }

  private startPoll(): void {
    this.doPoll(); // immediate first
    this.pollTimer = setInterval(() => this.doPoll(), 1000);
  }

  private async doPoll(): Promise<void> {
    if (this.watches.length === 0) {
      this.panel.webview.postMessage({
        command: 'update',
        watches: [],
        values: [],
      });
      return;
    }

    try {
      const targets = this.watches.map((w) => ({ variable: w.label }));
      const result = await this.client.readMemory({
        sessionId: this.sessionId,
        targets,
      });
      this.panel.webview.postMessage({
        command: 'update',
        watches: this.watches,
        values: result.results,
        timestamp: Date.now(),
      });
    } catch {
      // Session may have ended
    }
  }

  private getHtml(): string {
    // HTML table showing: Label | Type | Value | Scope (on) | Actions
    // Uses VS Code CSS variables for theme-aware styling
    // "Remove" button per watch
    // Auto-updates from polling messages
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <style>
    body {
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      color: var(--vscode-foreground);
      background: var(--vscode-editor-background);
      padding: 12px;
    }
    table { width: 100%; border-collapse: collapse; }
    th, td {
      text-align: left; padding: 4px 8px;
      border-bottom: 1px solid var(--vscode-widget-border);
    }
    th { color: var(--vscode-descriptionForeground); font-weight: 600; }
    .scope { color: var(--vscode-descriptionForeground); font-style: italic; font-size: 0.9em; }
    .value-changed { background: var(--vscode-diffEditor-insertedTextBackground); }
    button {
      background: none; border: none; cursor: pointer;
      color: var(--vscode-errorForeground); font-size: 0.9em;
    }
    .empty { color: var(--vscode-descriptionForeground); padding: 20px; text-align: center; }
    .timestamp { color: var(--vscode-descriptionForeground); font-size: 0.8em; margin-bottom: 8px; }
  </style>
</head>
<body>
  <div id="timestamp" class="timestamp"></div>
  <div id="empty" class="empty">No watches configured. Add watches via the trace command or context menu.</div>
  <table id="table" style="display:none">
    <thead><tr><th>Variable</th><th>Type</th><th>Value</th><th>Scope</th><th></th></tr></thead>
    <tbody id="body"></tbody>
  </table>
  <script>
    const vscode = acquireVsCodeApi();
    const bodyEl = document.getElementById('body');
    const tableEl = document.getElementById('table');
    const emptyEl = document.getElementById('empty');
    const tsEl = document.getElementById('timestamp');
    let prevValues = {};

    window.addEventListener('message', (event) => {
      const msg = event.data;
      if (msg.command === 'update') {
        if (msg.watches.length === 0) {
          tableEl.style.display = 'none';
          emptyEl.style.display = '';
          return;
        }
        tableEl.style.display = '';
        emptyEl.style.display = 'none';
        if (msg.timestamp) {
          tsEl.textContent = 'Updated: ' + new Date(msg.timestamp).toLocaleTimeString();
        }
        bodyEl.innerHTML = '';
        for (let i = 0; i < msg.watches.length; i++) {
          const w = msg.watches[i];
          const v = msg.values[i];
          const valStr = v?.error ? v.error : String(v?.value ?? '<null>');
          const changed = prevValues[w.label] !== undefined && prevValues[w.label] !== valStr;
          prevValues[w.label] = valStr;
          const tr = document.createElement('tr');
          tr.innerHTML =
            '<td>' + esc(w.label) + '</td>' +
            '<td>' + esc(w.typeName || '') + '</td>' +
            '<td' + (changed ? ' class="value-changed"' : '') + '>' + esc(valStr) + '</td>' +
            '<td class="scope">' + (w.on ? esc(w.on.join(', ')) : 'global') + '</td>' +
            '<td><button data-label="' + esc(w.label) + '">\\u2715</button></td>';
          tr.querySelector('button').addEventListener('click', () => {
            vscode.postMessage({ command: 'removeWatch', label: w.label });
          });
          bodyEl.appendChild(tr);
        }
      }
    });

    function esc(s) {
      const d = document.createElement('div');
      d.textContent = s;
      return d.innerHTML;
    }
  </script>
</body>
</html>`;
  }

  private dispose(): void {
    WatchPanel.currentPanel = undefined;
    if (this.pollTimer) clearInterval(this.pollTimer);
    for (const d of this.disposables) d.dispose();
    this.panel.dispose();
  }
}
```

**Step 2: Register command and wire to PollingEngine**

In `extension.ts`, in `activate()` add the command:

```typescript
vscode.commands.registerCommand('strobe.openWatchViewer', async () => {
  if (!activeSessionId) {
    vscode.window.showWarningMessage('Strobe: No active session.');
    return;
  }
  const client = daemonManager.getClient();
  if (!client) return;
  WatchPanel.createOrShow(client, activeSessionId);
}),
```

In `startSession()`, in the `'status'` event handler, add after `sidebarProvider.update()`:

```typescript
// Update watch panel if open
if (WatchPanel.currentPanel) {
  WatchPanel.currentPanel.updateWatches(status.watches);
}
```

Import `WatchPanel` at top of `extension.ts`.

**Step 3: Add command to `package.json`**

```json
{
  "command": "strobe.openWatchViewer",
  "title": "Strobe: Open Watch Viewer"
}
```

**Verification:** `npm run build`. Manually: add a watch via `debug_trace`, open watch viewer → see value updating at 1Hz.

---

### Task 4: Contextual Watch Scoping UI

**Files:**
- Modify: `strobe-vscode/src/editor/context-menu.ts` (~35 lines)
- Modify: `strobe-vscode/src/extension.ts` (~10 lines — register command + deps)
- Modify: `strobe-vscode/package.json` (~10 lines — command + menu entry)

**Step 1: Add "Watch Variable" context menu command**

This command lets users right-click inside a function, enter a variable name, and optionally scope the watch to the enclosing function. It uses `identifyFunctionAtCursor` to auto-detect the scope.

In `context-menu.ts`, add a new export:

```typescript
export async function addWatchAtCursor(
  client: StrobeClient,
  sessionId: string | undefined,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;

  if (!sessionId) {
    vscode.window.showWarningMessage('No active Strobe session. Launch a program first.');
    return;
  }

  const variable = await vscode.window.showInputBox({
    prompt: 'Variable name to watch (e.g., gTempo, gClock->counter)',
    placeHolder: 'gTempo',
  });
  if (!variable) return;

  // Detect enclosing function for scoping
  const fn = await identifyFunctionAtCursor(
    editor.document,
    editor.selection.active,
  );

  let scopePatterns: string[] | undefined;
  if (fn) {
    const profile = detectProfile(editor.document.languageId);
    const separator = profile?.patternSeparator ?? '::';
    const pattern = formatPattern(fn, separator);

    const choice = await vscode.window.showQuickPick(
      [
        { label: `Scoped to ${pattern}`, description: 'Only read during this function', value: [pattern] },
        { label: 'Global', description: 'Read on all traced functions', value: undefined as string[] | undefined },
      ],
      { placeHolder: 'Watch scope' },
    );
    if (!choice) return;
    scopePatterns = choice.value;
  }

  try {
    await client.trace({
      sessionId,
      watches: {
        add: [{
          variable,
          label: variable,
          on: scopePatterns,
        }],
      },
    });
    const scopeMsg = scopePatterns ? ` (scoped to ${scopePatterns.join(', ')})` : '';
    vscode.window.showInformationMessage(
      `Watching ${variable}${scopeMsg}`,
    );
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    vscode.window.showErrorMessage(`Failed to add watch: ${msg}`);
  }
}
```

**Step 2: Register command in `extension.ts`**

```typescript
vscode.commands.registerCommand('strobe.addWatch', async () => {
  const client = await daemonManager.ensureClient();
  await addWatchAtCursor(client, activeSessionId);
}),
```

**Step 3: Add to `package.json` — command and context menu**

Command:
```json
{
  "command": "strobe.addWatch",
  "title": "Strobe: Watch Variable..."
}
```

Add to `strobe.submenu`:
```json
{
  "command": "strobe.addWatch",
  "group": "3_memory"
}
```

**Verification:** `npm run build`. Right-click inside a function → "Strobe" → "Watch Variable..." → enter name → quickpick scope → watch added.

---

### Task 5: Retained Session Management

**Files:**
- Modify: `strobe-vscode/src/sidebar/sidebar-provider.ts` (~60 lines)
- Modify: `strobe-vscode/src/extension.ts` (~35 lines — commands for list/reopen/delete)
- Modify: `strobe-vscode/package.json` (~15 lines — commands + view)

**Step 1: Add "Retained Sessions" tree view**

Add a second tree view (`strobe.retainedSessions`) alongside the existing `strobe.session`. Create a `RetainedSessionsProvider` as a lightweight `TreeDataProvider`.

In `sidebar-provider.ts`, add a new class:

```typescript
export class RetainedSessionsProvider
  implements vscode.TreeDataProvider<RetainedSessionNode>
{
  private _onDidChangeTreeData = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private sessions: SessionSummary[] = [];
  private totalSize = 0;

  update(sessions: SessionSummary[], totalSize: number): void {
    this.sessions = sessions;
    this.totalSize = totalSize;
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: RetainedSessionNode): vscode.TreeItem {
    return element;
  }

  getChildren(): RetainedSessionNode[] {
    if (this.sessions.length === 0) {
      const empty = new RetainedSessionNode(
        'No retained sessions',
        'Stop a session with retain=true to keep it',
      );
      empty.iconPath = new vscode.ThemeIcon('info');
      return [empty];
    }

    return this.sessions.map((s) => {
      const sizeStr = s.sizeBytes
        ? `${(s.sizeBytes / (1024 * 1024)).toFixed(1)} MB`
        : '';
      const node = new RetainedSessionNode(
        path.basename(s.binaryPath),
        `PID ${s.pid} | ${s.status} | ${sizeStr}`,
      );
      node.iconPath = new vscode.ThemeIcon('archive');
      node.contextValue = 'retainedSession';
      node.sessionId = s.sessionId;
      node.tooltip = `Session: ${s.sessionId}\nBinary: ${s.binaryPath}\nRetained: ${s.retainedAt ? new Date(s.retainedAt * 1000).toLocaleString() : ''}`;
      return node;
    });
  }
}

class RetainedSessionNode extends vscode.TreeItem {
  sessionId?: string;
  constructor(label: string, description?: string) {
    super(label, vscode.TreeItemCollapsibleState.None);
    if (description) this.description = description;
  }
}
```

Import `SessionSummary` from `../client/types` and `path` from `path`.

**Step 2: Register view and commands in `extension.ts` and `package.json`**

In `package.json`, add to `contributes.views.strobe`:

```json
{
  "id": "strobe.retainedSessions",
  "name": "Retained Sessions"
}
```

Add commands:
```json
{
  "command": "strobe.refreshRetainedSessions",
  "title": "Strobe: Refresh Retained Sessions",
  "icon": "$(refresh)"
},
{
  "command": "strobe.openRetainedSession",
  "title": "Strobe: Open Retained Session"
},
{
  "command": "strobe.deleteRetainedSession",
  "title": "Strobe: Delete Retained Session",
  "icon": "$(trash)"
}
```

Add inline view actions for retained session nodes:
```json
"view/item/context": [
  {
    "command": "strobe.openRetainedSession",
    "when": "viewItem == retainedSession",
    "group": "inline@1"
  },
  {
    "command": "strobe.deleteRetainedSession",
    "when": "viewItem == retainedSession",
    "group": "inline@2"
  }
],
"view/title": [
  {
    "command": "strobe.refreshRetainedSessions",
    "when": "view == strobe.retainedSessions",
    "group": "navigation"
  }
]
```

In `extension.ts`, create the provider and register commands:

```typescript
const retainedProvider = new RetainedSessionsProvider();

const retainedTreeView = vscode.window.createTreeView('strobe.retainedSessions', {
  treeDataProvider: retainedProvider,
});

// Refresh retained sessions list
async function refreshRetainedSessions(): Promise<void> {
  try {
    const client = await daemonManager.ensureClient();
    const resp = await client.listSessions();
    retainedProvider.update(resp.sessions, resp.totalSize);
  } catch {
    // Not connected yet — leave empty
  }
}

context.subscriptions.push(
  retainedTreeView,
  vscode.commands.registerCommand('strobe.refreshRetainedSessions', refreshRetainedSessions),
  vscode.commands.registerCommand('strobe.openRetainedSession', async (node: { sessionId?: string }) => {
    if (!node?.sessionId) return;
    try {
      const client = await daemonManager.ensureClient();
      // Query the retained session's events
      const events = await client.query({ sessionId: node.sessionId, limit: 200, verbose: true });
      outputChannel.appendLine(`\n--- Retained Session: ${node.sessionId} ---`);
      outputChannel.appendEvents(events.events);
      outputChannel.show();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      vscode.window.showErrorMessage(`Failed to open session: ${msg}`);
    }
  }),
  vscode.commands.registerCommand('strobe.deleteRetainedSession', async (node: { sessionId?: string }) => {
    if (!node?.sessionId) return;
    const confirm = await vscode.window.showWarningMessage(
      `Delete retained session ${node.sessionId}?`,
      { modal: true },
      'Delete',
    );
    if (confirm !== 'Delete') return;
    try {
      const client = await daemonManager.ensureClient();
      await client.deleteSession(node.sessionId);
      await refreshRetainedSessions();
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      vscode.window.showErrorMessage(`Failed to delete: ${msg}`);
    }
  }),
);

// Auto-refresh retained sessions on activate
refreshRetainedSessions();
```

Also update `cmdStop()` to offer retention:

```typescript
async function cmdStop(): Promise<void> {
  if (!activeSessionId) {
    vscode.window.showWarningMessage('Strobe: No active session to stop.');
    return;
  }
  try {
    const retain = await vscode.window.showQuickPick(
      [
        { label: 'Stop', description: 'Discard session data', value: false },
        { label: 'Stop & Retain', description: 'Keep for post-mortem analysis', value: true },
      ],
      { placeHolder: 'Stop session' },
    );
    if (!retain) return; // cancelled
    const client = daemonManager.getClient();
    if (client) {
      await client.stop(activeSessionId, retain.value);
    }
    endSession();
    vscode.window.showInformationMessage(
      `Strobe: Session stopped${retain.value ? ' (retained)' : ''}.`,
    );
    if (retain.value) refreshRetainedSessions();
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
  }
}
```

**Verification:** `npm run build`. Stop a session with "Stop & Retain" → appears in Retained Sessions sidebar. Click → events shown in output. Delete → confirms and removes.

---

### Task 6: Settings UI Integration

**Files:**
- Modify: `strobe-vscode/package.json` (~40 lines — `configuration` contribution)
- Create: `strobe-vscode/src/utils/settings-sync.ts` (~50 lines)
- Modify: `strobe-vscode/src/extension.ts` (~10 lines — register sync listener)

**Step 1: Add `configuration` contribution to `package.json`**

In `contributes`, add:

```json
"configuration": {
  "title": "Strobe",
  "properties": {
    "strobe.events.maxPerSession": {
      "type": "number",
      "default": 200000,
      "minimum": 1,
      "maximum": 10000000,
      "description": "Maximum number of events stored per session (FIFO buffer). Higher values use more memory."
    },
    "strobe.test.statusRetryMs": {
      "type": "number",
      "default": 5000,
      "minimum": 500,
      "maximum": 60000,
      "description": "Test status polling interval in milliseconds."
    },
    "strobe.trace.serializationDepth": {
      "type": "number",
      "default": 3,
      "minimum": 1,
      "maximum": 10,
      "description": "Default serialization depth for function arguments."
    },
    "strobe.memory.pollIntervalMs": {
      "type": "number",
      "default": 500,
      "minimum": 100,
      "maximum": 5000,
      "description": "Default polling interval for memory reads and watch viewer."
    }
  }
}
```

**Step 2: Create settings sync utility**

When a user changes a Strobe setting in VS Code's Settings UI, write it to `~/.strobe/settings.json` so the daemon picks it up.

```typescript
// strobe-vscode/src/utils/settings-sync.ts

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';

const SETTINGS_PATH = path.join(os.homedir(), '.strobe', 'settings.json');

/** Map from VS Code setting key to settings.json key */
const SETTING_MAP: Record<string, string> = {
  'strobe.events.maxPerSession': 'events.maxPerSession',
  'strobe.test.statusRetryMs': 'test.statusRetryMs',
};

export function syncSettingsToDaemon(): void {
  const config = vscode.workspace.getConfiguration('strobe');
  const daemonSettings: Record<string, unknown> = {};

  for (const [vscodeKey, daemonKey] of Object.entries(SETTING_MAP)) {
    const shortKey = vscodeKey.replace('strobe.', '');
    const info = config.inspect(shortKey);
    // Only write values that are explicitly set by the user (not defaults)
    if (info?.globalValue !== undefined || info?.workspaceValue !== undefined) {
      daemonSettings[daemonKey] = config.get(shortKey);
    }
  }

  if (Object.keys(daemonSettings).length === 0) {
    // Don't write an empty file if user hasn't customized anything
    return;
  }

  try {
    const dir = path.dirname(SETTINGS_PATH);
    fs.mkdirSync(dir, { recursive: true });

    // Merge with existing settings (preserve keys we don't manage)
    let existing: Record<string, unknown> = {};
    try {
      existing = JSON.parse(fs.readFileSync(SETTINGS_PATH, 'utf-8'));
    } catch {
      // File doesn't exist or invalid
    }

    const merged = { ...existing, ...daemonSettings };
    fs.writeFileSync(SETTINGS_PATH, JSON.stringify(merged, null, 2) + '\n');
  } catch {
    // Best-effort — user may not have write permissions
  }
}

/** Get an extension-only setting (not synced to daemon) */
export function getExtensionSetting<T>(key: string, defaultValue: T): T {
  return vscode.workspace.getConfiguration('strobe').get(key, defaultValue);
}
```

**Step 3: Register listener in `extension.ts`**

```typescript
import { syncSettingsToDaemon, getExtensionSetting } from './utils/settings-sync';

// In activate():
context.subscriptions.push(
  vscode.workspace.onDidChangeConfiguration((e) => {
    if (e.affectsConfiguration('strobe')) {
      syncSettingsToDaemon();
    }
  }),
);
// Initial sync
syncSettingsToDaemon();
```

**Verification:** Open VS Code Settings → search "strobe" → see all 4 settings. Change `events.maxPerSession` to 500000 → check `~/.strobe/settings.json` updated.

---

### Task 7: Keyboard Shortcuts

**Files:**
- Modify: `strobe-vscode/package.json` (~30 lines — `keybindings` contribution)

Add `keybindings` to `contributes`:

```json
"keybindings": [
  {
    "command": "strobe.launch",
    "key": "ctrl+shift+f5",
    "mac": "cmd+shift+f5"
  },
  {
    "command": "strobe.stop",
    "key": "ctrl+shift+f6",
    "mac": "cmd+shift+f6"
  },
  {
    "command": "strobe.addTracePattern",
    "key": "ctrl+shift+t",
    "mac": "cmd+shift+t",
    "when": "editorTextFocus"
  },
  {
    "command": "strobe.traceFunction",
    "key": "ctrl+shift+f9",
    "mac": "cmd+shift+f9",
    "when": "editorTextFocus"
  },
  {
    "command": "strobe.setBreakpoint",
    "key": "ctrl+shift+f8",
    "mac": "cmd+shift+f8",
    "when": "editorTextFocus"
  },
  {
    "command": "strobe.openMemoryInspector",
    "key": "ctrl+shift+m",
    "mac": "cmd+shift+m"
  },
  {
    "command": "strobe.addWatch",
    "key": "ctrl+shift+w",
    "mac": "cmd+shift+w",
    "when": "editorTextFocus"
  }
]
```

**Verification:** `npm run build`. Open keyboard shortcuts in VS Code → search "strobe" → all bindings visible.

**Note:** Some keybindings may conflict with existing VS Code bindings on certain platforms. The `when` clause on editor-specific commands reduces conflicts. Users can override via their own keybindings.json.

---

### Task 8: Theme-Aware Decoration Styling

**Files:**
- Modify: `strobe-vscode/src/editor/decorations.ts` (~15 lines)

**Step 1: Use `ThemeColor` references for all decoration colors**

The current `DecorationManager` already uses `editorCodeLens.foreground` — good. Enhance to differentiate by metric type:

In the constructor, replace the single `decorationType` with type-specific ones for richer theming:

```typescript
// In the constructor, keep the existing decoration type but update it to use
// configurable opacity and a subtle background for hot functions:
this.decorationType = vscode.window.createTextEditorDecorationType({
  after: {
    color: new vscode.ThemeColor('editorCodeLens.foreground'),
    margin: '0 0 0 2em',
    fontStyle: 'italic',
  },
});

// Add a separate type for "hot" functions (>100 calls)
this.hotDecorationType = vscode.window.createTextEditorDecorationType({
  after: {
    color: new vscode.ThemeColor('editorWarning.foreground'),
    margin: '0 0 0 2em',
    fontStyle: 'italic',
  },
  backgroundColor: new vscode.ThemeColor('diffEditor.insertedTextBackground'),
  isWholeLine: true,
});
```

Update `render()` to partition decorations into normal vs hot:

```typescript
private render(): void {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;

  const filePath = editor.document.uri.fsPath;
  const normalDecorations: vscode.DecorationOptions[] = [];
  const hotDecorations: vscode.DecorationOptions[] = [];

  for (const [_key, stat] of this.stats) {
    if (!stat.file || path.basename(filePath) !== path.basename(stat.file)) continue;
    if (!stat.line) continue;
    const line = stat.line - 1;
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

    const decoration: vscode.DecorationOptions = {
      range: new vscode.Range(line, 0, line, 0),
      renderOptions: {
        after: { contentText: `  // ${parts.join(' | ')}` },
      },
    };

    if (stat.callCount >= 100) {
      hotDecorations.push(decoration);
    } else {
      normalDecorations.push(decoration);
    }
  }

  editor.setDecorations(this.decorationType, normalDecorations);
  editor.setDecorations(this.hotDecorationType, hotDecorations);
}
```

Update `clear()` and `dispose()` to also handle `hotDecorationType`.

**Verification:** `npm run build`. Trace a function called >100 times → warm background highlight. Trace a function called <100 times → normal styling. Verify in both light and dark themes.

---

### Task 9: Go Language Profile Enhancement

**Files:**
- Modify: `strobe-vscode/src/profiles/language-profile.ts` (~10 lines)
- Modify: `strobe-vscode/src/testing/test-discovery.ts` (~40 lines)
- Modify: `strobe-vscode/src/testing/test-codelens.ts` (~3 lines)
- Modify: `strobe-vscode/src/extension.ts` (~1 line — add `go` to CodeLens language list)

**Step 1: Add goroutine awareness note to Go profile**

The Go profile already exists and works for native DWARF instrumentation (Go compiles to native code). The key difference is that Go uses `.` as pattern separator (already correct) and uses `go test` for test discovery.

No change needed to the profile itself — it's already correct:
```typescript
const goProfile: LanguageProfile = {
  id: 'go',
  displayName: 'Go',
  filePatterns: ['*.go'],
  instrumentationMode: 'native',
  symbolSource: 'dwarf',
  patternSeparator: '.',
};
```

**Step 2: Add `GoTestDiscoverer`**

In `test-discovery.ts`, add:

```typescript
export class GoTestDiscoverer implements TestDiscoverer {
  readonly framework = 'go';

  async detect(workspaceFolder: string): Promise<number> {
    const goModPath = path.join(workspaceFolder, 'go.mod');
    try {
      await fs.promises.access(goModPath);
      return 85;
    } catch {
      return 0;
    }
  }

  async listTests(workspaceFolder: string): Promise<DiscoveredTest[]> {
    // Run: go test -list '.*' ./...
    return new Promise((resolve) => {
      const proc = cp.spawn('go', ['test', '-list', '.*', './...'], {
        cwd: workspaceFolder,
        env: process.env,
      });

      let stdout = '';
      proc.stdout.on('data', (d: Buffer) => { stdout += d; });
      proc.stderr.on('data', () => { /* discard */ });

      proc.on('close', (code) => {
        if (code !== 0) {
          resolve([]);
          return;
        }
        const tests: DiscoveredTest[] = [];
        for (const line of stdout.split('\n')) {
          const trimmed = line.trim();
          // go test -list outputs test names, one per line
          // Skip "ok" lines and empty lines
          if (trimmed && !trimmed.startsWith('ok ') && !trimmed.startsWith('?')) {
            tests.push({ name: trimmed });
          }
        }
        resolve(tests);
      });

      proc.on('error', () => resolve([]));
    });
  }
}
```

Update `detectDiscoverer` default list:

```typescript
export async function detectDiscoverer(
  workspaceFolder: string,
  discoverers: TestDiscoverer[] = [new CargoDiscoverer(), new GoTestDiscoverer()],
): Promise<TestDiscoverer | undefined> {
```

**Step 3: Register Go in CodeLens provider**

In `extension.ts`, add `{ language: 'go' }` to the CodeLens registration:

```typescript
const codeLensRegistration = vscode.languages.registerCodeLensProvider(
  [{ language: 'rust' }, { language: 'cpp' }, { language: 'c' }, { language: 'go' }],
  codeLensProvider,
);
```

In `test-codelens.ts`, add Go test function pattern. The existing CodeLens uses VS Code symbols, but for the regex fallback pattern used in the `FUNCTION_PATTERNS`, the Go function pattern `/func\s+(?:Test\w*)\s*\(/` is already matched by the generic Go function regex in `function-identifier.ts`. The CodeLens should detect `func TestXxx` patterns.

**Verification:** `npm run build`. Open a Go project with `go.mod` → test functions detected → CodeLens "Run with Strobe" appears. Right-click Go function → "Strobe: Trace This Function" works with `.` separator.

---

### Task 10: Marketplace Preparation

**Files:**
- Create: `strobe-vscode/media/strobe-icon.svg` (marketplace icon — already referenced in `package.json`)
- Create: `strobe-vscode/CHANGELOG.md`
- Modify: `strobe-vscode/package.json` (~10 lines — icon, repository, badges)

**Step 1: Create marketplace icon**

Create a simple SVG icon at `strobe-vscode/media/strobe-icon.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128">
  <rect width="128" height="128" rx="16" fill="#1e1e2e"/>
  <path d="M64 20 L84 55 L68 55 L78 108 L44 65 L62 65 L50 20 Z" fill="#f9e2af" stroke="#cdd6f4" stroke-width="2"/>
</svg>
```

(Lightning bolt on dark background — represents dynamic instrumentation.)

**Step 2: Add icon reference and metadata to `package.json`**

```json
"icon": "media/strobe-icon.svg",
"repository": {
  "type": "git",
  "url": "https://github.com/example/strobe"
},
"keywords": ["debugger", "instrumentation", "frida", "trace", "profiling", "native"],
"badges": []
```

**Step 3: Create CHANGELOG.md**

```markdown
# Changelog

## 0.1.0 — Initial Release

### Features
- **Launch & Trace:** Right-click any function → "Trace with Strobe" for instant runtime tracing
- **Debug Adapter Protocol:** Full DAP support — breakpoints, stepping, variable inspection, stack traces
- **Memory Inspector:** Read/write arbitrary memory addresses and DWARF variables
- **Live Watch Viewer:** Monitor variables with auto-polling, scoped to specific functions
- **Test Explorer:** Discover and run Rust (Cargo) and Go tests with Strobe instrumentation
- **Retained Sessions:** Stop sessions with retention for post-mortem analysis
- **Inline Decorations:** See call counts, average duration, and return values inline in your code
- **Multi-Language:** Rust, C/C++, Swift, Go support out of the box

### Supported Languages
- Rust (native DWARF, `::` patterns, Cargo test discovery)
- C/C++ (native DWARF, `::` patterns)
- Swift (native DWARF, `.` patterns)
- Go (native DWARF, `.` patterns, `go test` discovery)
```

**Verification:** `npm run build && cd strobe-vscode && npx vsce package --no-dependencies` succeeds and produces a `.vsix` file.

---

## Checkpoint: Full Integration Test

After all tasks complete, verify end-to-end:

1. **Memory Inspector:** Launch a C program with global variables → open memory inspector → read `gTempo` → see value → write `42` → confirm → read again shows `42`.
2. **Watch Viewer:** Add watch on `gClock->counter` scoped to `audio::process` → open watch viewer → see value updating. Remove watch → disappears.
3. **Retained Sessions:** Stop session with "Stop & Retain" → appears in sidebar. Click → events in output. Delete → gone.
4. **Settings:** Change `strobe.events.maxPerSession` in VS Code settings → `~/.strobe/settings.json` updated.
5. **Keyboard Shortcuts:** `Cmd+Shift+F5` launches Strobe. `Cmd+Shift+M` opens memory inspector.
6. **Decorations:** Functions with >100 calls show warm background. Light/dark theme both look good.
7. **Go:** Open Go project → tests discovered → CodeLens visible → right-click → trace with `.` separator.

---

## Commit

Single commit after all tasks pass:

```
feat: M4 polish & power features — memory inspector, watch viewer, session management, settings UI
```
