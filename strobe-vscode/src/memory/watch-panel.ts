import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';
import { ActiveWatch } from '../client/types';

export class WatchPanel {
  public static readonly viewType = 'strobe.watchViewer';
  private static currentPanel: WatchPanel | undefined;

  private readonly panel: vscode.WebviewPanel;
  private readonly client: StrobeClient;
  private sessionId: string;
  private pollTimer: ReturnType<typeof setInterval> | undefined;
  private watches: ActiveWatch[] = [];
  private disposables: vscode.Disposable[] = [];

  static get instance(): WatchPanel | undefined {
    return WatchPanel.currentPanel;
  }

  static createOrShow(
    client: StrobeClient,
    sessionId: string,
  ): WatchPanel {
    if (WatchPanel.currentPanel) {
      if (WatchPanel.currentPanel.pollTimer) {
        clearInterval(WatchPanel.currentPanel.pollTimer);
        WatchPanel.currentPanel.pollTimer = undefined;
      }
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
    this.doPoll();
    const intervalMs = vscode.workspace.getConfiguration('strobe').get<number>('memory.pollIntervalMs', 500);
    this.pollTimer = setInterval(() => this.doPoll(), intervalMs);
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
      const targets = this.watches.map((w: ActiveWatch & { variable?: string; type?: string }) => {
        if (w.address) return { address: w.address, type: w.type };
        return { variable: w.variable ?? w.label };
      });
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
    const nonce = getNonce();
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
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
    .remove-btn {
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
  <script nonce="${nonce}">
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
          const valStr = v && v.error ? v.error : String(v && v.value != null ? v.value : '<null>');
          const changed = prevValues[w.label] !== undefined && prevValues[w.label] !== valStr;
          prevValues[w.label] = valStr;
          const tr = document.createElement('tr');
          tr.innerHTML =
            '<td>' + esc(w.label) + '</td>' +
            '<td>' + esc(w.typeName || '') + '</td>' +
            '<td' + (changed ? ' class="value-changed"' : '') + '>' + esc(valStr) + '</td>' +
            '<td class="scope">' + (w.on ? esc(w.on.join(', ')) : 'global') + '</td>' +
            '<td><button class="remove-btn" data-label="' + esc(w.label) + '">\\u2715</button></td>';
          tr.querySelector('.remove-btn').addEventListener('click', () => {
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
  }
}

function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let nonce = '';
  for (let i = 0; i < 32; i++) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}
