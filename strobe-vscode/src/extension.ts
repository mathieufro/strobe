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
    vscode.commands.registerCommand(
      'strobe.addTracePattern',
      cmdAddTracePattern,
    ),
  );

  // Register context menu commands
  registerContextMenuCommands(context, {
    getSessionId: () => activeSessionId,
    addPattern: async (pattern: string) => {
      const client = daemonManager.getClient();
      if (!client || !activeSessionId) return;
      await client.trace({
        sessionId: activeSessionId,
        add: [pattern],
      });
    },
    launchAndTrace: async (pattern: string) => {
      const binary = await vscode.window.showInputBox({
        prompt: 'Path to executable',
        placeHolder: '/path/to/binary',
      });
      if (!binary) return;

      const projectRoot =
        vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '.';
      const client = await daemonManager.ensureClient();
      const resp = await client.launch({ command: binary, projectRoot });
      startSession(client, resp.sessionId);
      await client.trace({
        sessionId: resp.sessionId,
        add: [pattern],
      });
      outputChannel.show();
      vscode.window.showInformationMessage(
        `Strobe: Tracing ${pattern} on ${resp.sessionId}`,
      );
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

    const client = await daemonManager.ensureClient();
    statusBar.setConnected();

    const projectRoot =
      vscode.workspace.workspaceFolders?.[0]?.uri.fsPath ?? '.';
    const resp = await client.launch({ command: binary, projectRoot });

    startSession(client, resp.sessionId);
    outputChannel.show();
    vscode.window.showInformationMessage(
      `Strobe: Launched ${binary} (session: ${resp.sessionId})`,
    );
  } catch (err: unknown) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Strobe: ${msg}`);
    statusBar.setDisconnected();
  }
}

async function cmdStop(): Promise<void> {
  if (!activeSessionId) {
    vscode.window.showWarningMessage(
      'Strobe: No active session to stop.',
    );
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
    vscode.window.showWarningMessage(
      'Strobe: No active session. Launch first.',
    );
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
    const resp = await client.trace({
      sessionId: activeSessionId,
      add: [pattern],
    });
    vscode.window.showInformationMessage(
      `Strobe: Tracing ${pattern} (${resp.hookedFunctions} hooks)`,
    );
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
    outputChannel.appendLine(
      '\u26A0 Events were dropped (FIFO buffer full). Consider increasing events.maxPerSession.',
    );
  });

  pollingEngine.on('error', (err: Error) => {
    outputChannel.appendLine(`\u26A0 Polling error: ${err.message}`);
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
