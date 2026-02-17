import * as vscode from 'vscode';
import { SessionStatusResponse } from '../client/types';

export class StrobeStatusBar {
  private item: vscode.StatusBarItem;

  constructor() {
    this.item = vscode.window.createStatusBarItem(
      vscode.StatusBarAlignment.Left,
      100,
    );
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
    const icon =
      status.status === 'paused' ? '$(debug-pause)' : '$(circle-filled)';
    // Show short session name (strip timestamp suffix)
    const shortName =
      sessionId.split('-').slice(0, -3).join('-') || sessionId;
    this.item.text = `${icon} Strobe: ${shortName} (PID ${status.pid}) | ${events} events | ${hooks} hooks`;
    this.item.tooltip = `Strobe: ${status.status}\nPatterns: ${status.tracePatterns.join(', ') || 'none'}`;
    this.item.color = new vscode.ThemeColor('statusBar.foreground');
  }

  dispose(): void {
    this.item.dispose();
  }
}
