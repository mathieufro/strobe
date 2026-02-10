import * as vscode from 'vscode';
import { DaemonManager } from './utils/daemon-manager';
import { StrobeOutputChannel } from './output/output-channel';
import { StrobeStatusBar } from './utils/status-bar';
import { SidebarProvider } from './sidebar/sidebar-provider';
import { PollingEngine } from './client/polling-engine';
import {
  registerContextMenuCommands,
  setBreakpointAtCursor,
  addLogpointAtCursor,
} from './editor/context-menu';
import { StrobeClient } from './client/strobe-client';
import { SessionStatusResponse, StrobeEvent } from './client/types';
import { StrobeTestController } from './testing/test-controller';
import { TestCodeLensProvider } from './testing/test-codelens';
import { StrobeDebugAdapter } from './dap/debug-adapter';
import { DecorationManager } from './editor/decorations';

let daemonManager: DaemonManager;
let outputChannel: StrobeOutputChannel;
let statusBar: StrobeStatusBar;
let sidebarProvider: SidebarProvider;
let pollingEngine: PollingEngine | null = null;
let activeSessionId: string | undefined;
let decorationManager: DecorationManager;

export function activate(context: vscode.ExtensionContext): void {
  daemonManager = new DaemonManager(context.extensionPath);
  outputChannel = new StrobeOutputChannel();
  statusBar = new StrobeStatusBar();
  sidebarProvider = new SidebarProvider();
  decorationManager = new DecorationManager();

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

  // Test Explorer
  const testController = new StrobeTestController(
    () => daemonManager.ensureClient(),
    outputChannel,
  );

  // CodeLens for test functions
  const codeLensProvider = new TestCodeLensProvider();
  const codeLensRegistration = vscode.languages.registerCodeLensProvider(
    [{ language: 'rust' }, { language: 'cpp' }, { language: 'c' }],
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
          const sessions = await client.listSessions() as Array<{ sessionId: string }>;
          if (sessions.length > 0) {
            const latest = sessions[sessions.length - 1];
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

        outputChannel.appendLine(`Strobe: Test "${testName}" cancelled`);
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Strobe: ${msg}`);
      }
    },
  );
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

export function deactivate(): void {
  pollingEngine?.stop();
  daemonManager.dispose();
}
