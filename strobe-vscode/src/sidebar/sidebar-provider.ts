import * as vscode from 'vscode';
import { SessionStatusResponse } from '../client/types';

type TreeNode = SessionNode | CategoryNode | LeafNode;

class SessionNode extends vscode.TreeItem {
  constructor(
    public status: SessionStatusResponse,
    public sessionId: string,
  ) {
    super(
      `Session: ${sessionId}`,
      vscode.TreeItemCollapsibleState.Expanded,
    );
    this.description = `PID ${status.pid} | ${status.eventCount.toLocaleString()} events`;
    this.iconPath = new vscode.ThemeIcon(
      status.status === 'paused' ? 'debug-pause' : 'circle-filled',
    );
  }
}

class CategoryNode extends vscode.TreeItem {
  constructor(
    label: string,
    public items: LeafNode[],
    icon: string,
  ) {
    super(
      label,
      items.length > 0
        ? vscode.TreeItemCollapsibleState.Expanded
        : vscode.TreeItemCollapsibleState.None,
    );
    this.description = `(${items.length})`;
    this.iconPath = new vscode.ThemeIcon(icon);
  }
}

class LeafNode extends vscode.TreeItem {
  constructor(label: string, description?: string, icon?: string) {
    super(label, vscode.TreeItemCollapsibleState.None);
    if (description) this.description = description;
    if (icon) this.iconPath = new vscode.ThemeIcon(icon);
  }
}

export class SidebarProvider
  implements vscode.TreeDataProvider<TreeNode>
{
  private _onDidChangeTreeData = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this._onDidChangeTreeData.event;

  private status: SessionStatusResponse | null = null;
  private sessionId: string | null = null;

  update(sessionId: string, status: SessionStatusResponse): void {
    if (
      this.sessionId === sessionId &&
      this.status?.eventCount === status.eventCount &&
      this.status?.hookedFunctions === status.hookedFunctions &&
      this.status?.status === status.status &&
      this.status?.tracePatterns.length === status.tracePatterns.length &&
      this.status?.breakpoints.length === status.breakpoints.length &&
      this.status?.logpoints.length === status.logpoints.length &&
      this.status?.watches.length === status.watches.length &&
      this.status?.pausedThreads.length === status.pausedThreads.length
    ) {
      return;
    }
    this.sessionId = sessionId;
    this.status = status;
    this._onDidChangeTreeData.fire();
  }

  clear(): void {
    this.sessionId = null;
    this.status = null;
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: TreeNode): vscode.TreeItem {
    return element;
  }

  getChildren(element?: TreeNode): TreeNode[] {
    if (!element) {
      // Root level
      if (!this.status || !this.sessionId) {
        return [
          new LeafNode(
            'No active session',
            'Launch or attach to begin',
            'info',
          ),
        ];
      }
      return [new SessionNode(this.status, this.sessionId)];
    }

    if (element instanceof SessionNode) {
      const s = element.status;

      const patternNodes = s.tracePatterns.map(
        (p) => new LeafNode(p, undefined, 'zap'),
      );

      const watchNodes = s.watches.map(
        (w) => new LeafNode(w.label, w.typeName ?? '', 'eye'),
      );

      const bpNodes = s.breakpoints.map(
        (bp) =>
          new LeafNode(
            bp.function ?? `${bp.file}:${bp.line}`,
            bp.id,
            'circle-filled',
          ),
      );

      const lpNodes = s.logpoints.map(
        (lp) =>
          new LeafNode(
            lp.function ?? `${lp.file}:${lp.line}`,
            lp.message,
            'output',
          ),
      );

      return [
        new CategoryNode('Trace Patterns', patternNodes, 'zap'),
        new CategoryNode('Watches', watchNodes, 'eye'),
        new CategoryNode('Breakpoints', bpNodes, 'debug-breakpoint'),
        new CategoryNode('Logpoints', lpNodes, 'output'),
      ];
    }

    if (element instanceof CategoryNode) {
      return element.items;
    }

    return [];
  }
}
