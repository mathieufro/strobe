import * as vscode from 'vscode';
import * as path from 'path';
import { StrobeEvent } from '../client/types';
import { formatDuration } from '../utils/format';

const DEBOUNCE_MS = 1000;

interface FunctionStats {
  callCount: number;
  totalDurationNs: number;
  lastReturnValue?: string;
  file?: string;
  line?: number;
}

const HOT_THRESHOLD = 100;

export class DecorationManager implements vscode.Disposable {
  private stats = new Map<string, FunctionStats>();
  private decorationType: vscode.TextEditorDecorationType;
  private hotDecorationType: vscode.TextEditorDecorationType;
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

    this.hotDecorationType = vscode.window.createTextEditorDecorationType({
      after: {
        color: new vscode.ThemeColor('editorWarning.foreground'),
        margin: '0 0 0 2em',
        fontStyle: 'italic',
      },
      backgroundColor: new vscode.ThemeColor('diffEditor.insertedTextBackground'),
      isWholeLine: true,
    });

    this.disposables.push(
      vscode.window.onDidChangeActiveTextEditor(() => this.render()),
    );
  }

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
    if (this.debounceTimer) return;
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

      if (stat.callCount >= HOT_THRESHOLD) {
        hotDecorations.push(decoration);
      } else {
        normalDecorations.push(decoration);
      }
    }

    editor.setDecorations(this.decorationType, normalDecorations);
    editor.setDecorations(this.hotDecorationType, hotDecorations);
  }

  clear(): void {
    this.stats.clear();
    this.dirty = false;
    if (this.debounceTimer) {
      clearTimeout(this.debounceTimer);
      this.debounceTimer = undefined;
    }
    for (const editor of vscode.window.visibleTextEditors) {
      editor.setDecorations(this.decorationType, []);
      editor.setDecorations(this.hotDecorationType, []);
    }
  }

  dispose(): void {
    this.clear();
    this.decorationType.dispose();
    this.hotDecorationType.dispose();
    for (const d of this.disposables) d.dispose();
  }
}

function formatValue(v: unknown): string {
  if (v === null || v === undefined) return 'null';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}
