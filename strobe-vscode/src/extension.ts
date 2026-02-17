import * as vscode from 'vscode';
import { DaemonManager } from './utils/daemon-manager';
import { StrobeOutputChannel } from './output/output-channel';
import { StrobeStatusBar } from './utils/status-bar';
import { SidebarProvider, RetainedSessionsProvider } from './sidebar/sidebar-provider';
import { PollingEngine } from './client/polling-engine';
import {
  registerContextMenuCommands,
  setBreakpointAtCursor,
  addLogpointAtCursor,
  addWatchAtCursor,
} from './editor/context-menu';
import { StrobeClient } from './client/strobe-client';
import { SessionStatusResponse, StrobeEvent } from './client/types';
import { StrobeTestController } from './testing/test-controller';
import { TestCodeLensProvider } from './testing/test-codelens';
import { StrobeDebugAdapter } from './dap/debug-adapter';
import { DecorationManager } from './editor/decorations';
import { MemoryPanel } from './memory/memory-panel';
import { WatchPanel } from './memory/watch-panel';
import { syncSettingsToDaemon } from './utils/settings-sync';

let daemonManager: DaemonManager;
let outputChannel: StrobeOutputChannel;
let statusBar: StrobeStatusBar;
let sidebarProvider: SidebarProvider;
let pollingEngine: PollingEngine | null = null;
let activeSessionId: string | undefined;
let decorationManager: DecorationManager;
let retainedProvider: RetainedSessionsProvider;

export function activate(context: vscode.ExtensionContext): void {
  daemonManager = new DaemonManager(context.extensionPath);
  outputChannel = new StrobeOutputChannel();
  statusBar = new StrobeStatusBar();
  sidebarProvider = new SidebarProvider();
  decorationManager = new DecorationManager();
  retainedProvider = new RetainedSessionsProvider();

  // Register sidebar
  const treeView = vscode.window.createTreeView('strobe.session', {
    treeDataProvider: sidebarProvider,
  });

  const retainedTreeView = vscode.window.createTreeView('strobe.retainedSessions', {
    treeDataProvider: retainedProvider,
  });

  // Register commands
  context.subscriptions.push(
    treeView,
    retainedTreeView,
    outputChannel,
    statusBar,

    vscode.commands.registerCommand('strobe.launch', cmdLaunch),
    vscode.commands.registerCommand('strobe.stop', cmdStop),
    vscode.commands.registerCommand(
      'strobe.addTracePattern',
      cmdAddTracePattern,
    ),

    // M4: Memory inspector + watch viewer
    vscode.commands.registerCommand('strobe.openMemoryInspector', async () => {
      if (!activeSessionId) {
        vscode.window.showWarningMessage('Strobe: No active session. Launch a program first.');
        return;
      }
      const client = daemonManager.getClient();
      if (!client) return;
      MemoryPanel.createOrShow(client, activeSessionId);
    }),
    vscode.commands.registerCommand('strobe.openWatchViewer', async () => {
      if (!activeSessionId) {
        vscode.window.showWarningMessage('Strobe: No active session.');
        return;
      }
      const client = daemonManager.getClient();
      if (!client) return;
      WatchPanel.createOrShow(client, activeSessionId);
    }),
    vscode.commands.registerCommand('strobe.addWatch', async () => {
      const client = await daemonManager.ensureClient();
      await addWatchAtCursor(client, activeSessionId);
    }),

    // M4: Retained sessions
    vscode.commands.registerCommand('strobe.refreshRetainedSessions', refreshRetainedSessions),
    vscode.commands.registerCommand('strobe.openRetainedSession', async (node: { sessionId?: string }) => {
      if (!node?.sessionId) return;
      try {
        const client = await daemonManager.ensureClient();
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

    // M4: Settings sync
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('strobe')) {
        syncSettingsToDaemon();
      }
    }),
  );

  // Initial settings sync and retained sessions refresh
  syncSettingsToDaemon();
  refreshRetainedSessions();

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

  // Test Explorer
  const testController = new StrobeTestController(
    () => daemonManager.ensureClient(),
    outputChannel,
  );

  // CodeLens for test functions
  const codeLensProvider = new TestCodeLensProvider();
  const codeLensRegistration = vscode.languages.registerCodeLensProvider(
    [{ language: 'rust' }, { language: 'cpp' }, { language: 'c' }, { language: 'go' }],
    codeLensProvider,
  );

  // Test commands
  context.subscriptions.push(
    testController,
    codeLensRegistration,

    vscode.commands.registerCommand('strobe.refreshTests', () =>
      testController.refresh(),
    ),

    vscode.commands.registerCommand(
      'strobe.runSingleTest',
      (testName: string) => cmdRunSingleTest(testName, false),
    ),

    vscode.commands.registerCommand(
      'strobe.debugSingleTest',
      (testName: string) => cmdRunSingleTest(testName, true),
    ),
  );

  // DAP debug adapter factory
  context.subscriptions.push(
    decorationManager,

    vscode.debug.registerDebugAdapterDescriptorFactory('strobe', {
      createDebugAdapterDescriptor() {
        return new vscode.DebugAdapterInlineImplementation(
          new StrobeDebugAdapter(daemonManager),
        );
      },
    }),

    // Breakpoint + logpoint context menu commands
    vscode.commands.registerCommand('strobe.setBreakpoint', async () => {
      const client = await daemonManager.ensureClient();
      await setBreakpointAtCursor(client, activeSessionId);
    }),
    vscode.commands.registerCommand('strobe.addLogpoint', async () => {
      const client = await daemonManager.ensureClient();
      await addLogpointAtCursor(client, activeSessionId);
    }),

    // Sync DAP session with existing UI (no PollingEngine — DAP adapter polls internally)
    vscode.debug.onDidStartDebugSession(async (session) => {
      if (session.type === 'strobe') {
        try {
          const client = await daemonManager.ensureClient();
          const resp = await client.listSessions();
          if (resp.sessions.length > 0) {
            const latest = resp.sessions[resp.sessions.length - 1];
            if (latest.sessionId) {
              activeSessionId = latest.sessionId;
              statusBar.setConnected();
            }
          }
        } catch {
          // Best-effort sync
        }
      }
    }),

    vscode.debug.onDidTerminateDebugSession((session) => {
      if (session.type === 'strobe') {
        activeSessionId = undefined;
        statusBar.setConnected();
        sidebarProvider.clear();
        decorationManager.clear();
      }
    }),
  );
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

const SINGLE_TEST_TIMEOUT_MS = 10 * 60 * 1000; // 10 minutes

async function cmdRunSingleTest(
  testName: string,
  debug: boolean,
): Promise<void> {
  const workspaceFolder =
    vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (!workspaceFolder) return;

  await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: `Strobe: ${debug ? 'Debugging' : 'Running'} test "${testName}"`,
      cancellable: true,
    },
    async (_progress, token) => {
      try {
        const client = await daemonManager.ensureClient();
        const resp = await client.runTest({
          projectRoot: workspaceFolder,
          test: testName,
          tracePatterns: debug ? [`*::${testName}`] : undefined,
        });

        outputChannel.appendLine(
          `Strobe: ${debug ? 'Debugging' : 'Running'} test "${testName}" (${resp.framework})`,
        );
        if (debug) outputChannel.show();

        const startTime = Date.now();

        while (!token.isCancellationRequested) {
          if (Date.now() - startTime > SINGLE_TEST_TIMEOUT_MS) {
            vscode.window.showErrorMessage(
              `Strobe: Test "${testName}" timed out after 10 minutes`,
            );
            return;
          }

          const status = await client.testStatus(resp.testRunId);

          // In debug mode, start session polling for trace events
          if (debug && status.sessionId && !activeSessionId) {
            startSession(client, status.sessionId);
          }

          if (status.status === 'completed') {
            const r = status.result;
            if (r && r.failures.length === 0) {
              vscode.window.showInformationMessage(
                `Strobe: Test "${testName}" passed`,
              );
            } else if (r && r.failures.length > 0) {
              const fail = r.failures[0];
              let msg = `Test "${testName}" failed: ${fail.message}`;
              if (fail.suggestedTraces.length > 0) {
                msg += ` \u2014 Suggested traces: ${fail.suggestedTraces.join(', ')}`;
              }
              vscode.window.showErrorMessage(`Strobe: ${msg}`);
            }
            return;
          } else if (status.status === 'failed') {
            vscode.window.showErrorMessage(
              `Strobe: Test run failed \u2014 ${status.error ?? 'unknown error'}`,
            );
            return;
          }

          await new Promise((r) => setTimeout(r, 1000));
        }

        // Stop the daemon-side session if available
        if (token.isCancellationRequested && resp.testRunId) {
          try {
            const cancelStatus = await client.testStatus(resp.testRunId);
            if (cancelStatus.sessionId) {
              await client.stop(cancelStatus.sessionId);
            }
          } catch {
            // Best effort
          }
        }

        outputChannel.appendLine(`Strobe: Test "${testName}" cancelled`);
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Strobe: ${msg}`);
      }
    },
  );
}

function startSession(client: StrobeClient, sessionId: string): void {
  if (pollingEngine) {
    pollingEngine.stop();
    pollingEngine = null;
  }

  activeSessionId = sessionId;

  // Start polling
  pollingEngine = new PollingEngine(client, sessionId);

  pollingEngine.on('status', (status: SessionStatusResponse) => {
    statusBar.setSession(status, sessionId);
    sidebarProvider.update(sessionId, status);
    // Update watch panel if open
    if (WatchPanel.instance) {
      WatchPanel.instance.updateWatches(status.watches);
    }
  });

  pollingEngine.on('events', (events: StrobeEvent[]) => {
    outputChannel.appendEvents(events);
    decorationManager.onEvents(events);
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
  decorationManager.clear();
}

async function refreshRetainedSessions(): Promise<void> {
  try {
    const client = await daemonManager.ensureClient();
    const resp = await client.listSessions();
    retainedProvider.update(resp.sessions, resp.totalSize);
  } catch {
    // Not connected yet — leave empty
  }
}

export function deactivate(): void {
  try {
    pollingEngine?.stop();
  } catch {
    // Best-effort cleanup
  }
  try {
    daemonManager.dispose();
  } catch {
    // Best-effort cleanup
  }
}
