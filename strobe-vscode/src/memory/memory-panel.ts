import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';

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
        const target = msg.target;
        const type = msg.type;
        this.pollTimer = setInterval(async () => {
          try {
            const isAddr = target.startsWith('0x');
            const result = await this.client.readMemory({
              sessionId: this.sessionId,
              targets: [isAddr
                ? { address: target, type }
                : { variable: target }],
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
    .error { color: var(--vscode-errorForeground); margin-top: 8px; }
    .poll-indicator { color: var(--vscode-charts-green); font-size: 0.85em; }
    #target { flex: 1; min-width: 200px; }
    .write-btn {
      background: var(--vscode-button-secondaryBackground);
      color: var(--vscode-button-secondaryForeground);
      padding: 2px 8px;
      font-size: 0.9em;
    }
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
          '<td>' + esc(valueStr) +
            (isPoll ? ' <span class="poll-indicator">\\u25CF</span>' : '') +
          '</td>' +
          '<td>' + esc(r.address || '') + '</td>' +
          '<td>' + (canWrite ? '<button class="write-btn">Write</button>' : '') + '</td>';
        if (canWrite) {
          tr.querySelector('.write-btn').addEventListener('click', () => {
            const newVal = prompt('New value for ' + r.target, String(r.value ?? ''));
            if (newVal === null) return;
            const num = Number(newVal);
            vscode.postMessage({
              command: 'write',
              target: r.target,
              type: typeEl.value || r.type || undefined,
              value: isNaN(num) ? (newVal === 'true') : num,
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
