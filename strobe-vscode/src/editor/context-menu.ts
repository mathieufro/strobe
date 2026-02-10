import * as vscode from 'vscode';
import * as path from 'path';
import {
  identifyFunctionAtCursor,
  formatPattern,
} from './function-identifier';
import { detectProfile } from '../profiles/language-profile';
import { StrobeClient } from '../client/strobe-client';

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
      try {
        const editor = vscode.window.activeTextEditor;
        if (!editor) return;

        const fn = await identifyFunctionAtCursor(
          editor.document,
          editor.selection.active,
        );
        if (!fn) {
          vscode.window.showWarningMessage(
            'Strobe: Could not identify a function at cursor position.',
          );
          return;
        }

        const profile = detectProfile(editor.document.languageId);
        const separator = profile?.patternSeparator ?? '::';
        const pattern = formatPattern(fn, separator);

        if (deps.getSessionId()) {
          await deps.addPattern(pattern);
          vscode.window.showInformationMessage(
            `Strobe: Tracing ${pattern}`,
          );
        } else {
          await deps.launchAndTrace(pattern);
        }
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Strobe: ${msg}`);
      }
    }),
  );
}

export async function setBreakpointAtCursor(
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

  try {
    await client.setBreakpoints({
      sessionId,
      add: [{ file: filePath, line }],
    });
    vscode.window.showInformationMessage(
      `Breakpoint set at ${path.basename(filePath)}:${line}`,
    );
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    vscode.window.showErrorMessage(`Failed to set breakpoint: ${msg}`);
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
    vscode.window.showInformationMessage(
      `Logpoint added at ${path.basename(filePath)}:${line}`,
    );
  } catch (e: unknown) {
    const msg = e instanceof Error ? e.message : String(e);
    vscode.window.showErrorMessage(`Failed to add logpoint: ${msg}`);
  }
}

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
        { label: `Scoped to ${pattern}`, description: 'Only read during this function', value: [pattern] as string[] | undefined },
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
