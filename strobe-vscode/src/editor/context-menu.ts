import * as vscode from 'vscode';
import {
  identifyFunctionAtCursor,
  formatPattern,
} from './function-identifier';
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
