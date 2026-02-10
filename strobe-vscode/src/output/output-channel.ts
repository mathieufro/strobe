import * as vscode from 'vscode';
import { StrobeEvent } from '../client/types';
import { formatDuration } from '../utils/format';

export class StrobeOutputChannel {
  private channel: vscode.OutputChannel;

  constructor() {
    this.channel = vscode.window.createOutputChannel('Strobe');
  }

  show(): void {
    this.channel.show(true); // preserveFocus
  }

  clear(): void {
    this.channel.clear();
  }

  appendEvents(events: StrobeEvent[]): void {
    for (const event of events) {
      this.channel.appendLine(this.formatEvent(event));
    }
  }

  appendLine(text: string): void {
    this.channel.appendLine(text);
  }

  dispose(): void {
    this.channel.dispose();
  }

  private formatEvent(event: StrobeEvent): string {
    const ts = this.formatTimestamp(event.timestamp_ns);
    const eventType = event.eventType;

    switch (eventType) {
      case 'function_enter':
        return `[${ts}] \u2192 ${event.function ?? '??'}(${this.formatArgs(event.arguments)})`;

      case 'function_exit': {
        const dur =
          event.duration_ns != null
            ? ` [${formatDuration(event.duration_ns)}]`
            : '';
        const ret =
          event.returnValue !== undefined
            ? ` \u2192 ${this.formatValue(event.returnValue)}`
            : '';
        return `[${ts}] \u2190 ${event.function ?? '??'}${ret}${dur}`;
      }

      case 'stdout':
        return `[${ts}] stdout: ${event.text ?? ''}`;

      case 'stderr':
        return `[${ts}] stderr: ${event.text ?? ''}`;

      case 'pause':
        return `[${ts}] \u23F8 PAUSED at ${event.sourceFile ?? '??'}:${event.line ?? '?'} (breakpoint)`;

      case 'logpoint':
        return `[${ts}] \uD83D\uDCDD logpoint: ${event.logpointMessage ?? ''}`;

      case 'crash': {
        const signal = event.signal ?? 'unknown';
        const addr = event.faultAddress ? ` at ${event.faultAddress}` : '';
        return `[${ts}] \uD83D\uDCA5 CRASH: signal ${signal}${addr}`;
      }

      case 'variable_snapshot':
        return `[${ts}] \uD83D\uDCCA ${this.formatWatchValues(event.data as Record<string, unknown> | undefined)}`;

      default:
        return `[${ts}] ${eventType ?? 'unknown'}: ${JSON.stringify(event)}`;
    }
  }

  private formatTimestamp(ns: number): string {
    const totalMs = ns / 1_000_000;
    const h = Math.floor(totalMs / 3_600_000);
    const m = Math.floor((totalMs % 3_600_000) / 60_000);
    const s = Math.floor((totalMs % 60_000) / 1000);
    const ms = Math.floor(totalMs % 1000);
    return `${h.toString().padStart(2, '0')}:${m.toString().padStart(2, '0')}:${s.toString().padStart(2, '0')}.${ms.toString().padStart(3, '0')}`;
  }

  private formatArgs(args: unknown): string {
    if (!args) return '';
    if (Array.isArray(args)) {
      return args.map((a) => this.formatValue(a)).join(', ');
    }
    if (typeof args === 'object') {
      return Object.entries(args as Record<string, unknown>)
        .map(([k, v]) => `${k}=${this.formatValue(v)}`)
        .join(', ');
    }
    return String(args);
  }

  private formatValue(val: unknown): string {
    if (val === null || val === undefined) return 'null';
    if (typeof val === 'string')
      return val.length > 80 ? val.slice(0, 80) + '\u2026' : val;
    if (typeof val === 'number' || typeof val === 'boolean')
      return String(val);
    return JSON.stringify(val);
  }

  private formatWatchValues(
    wv: Record<string, unknown> | undefined,
  ): string {
    if (!wv) return '';
    return Object.entries(wv)
      .map(([k, v]) => `${k} = ${this.formatValue(v)}`)
      .join(', ');
  }
}
