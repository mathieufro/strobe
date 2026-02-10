import { EventEmitter } from 'events';
import { StrobeClient } from './strobe-client';
import { StrobeEvent, SessionStatusResponse } from './types';

export interface PollingEngineEvents {
  events: (events: StrobeEvent[]) => void;
  status: (status: SessionStatusResponse) => void;
  sessionEnd: (sessionId: string) => void;
  eventsDropped: () => void;
  error: (err: Error) => void;
}

export class PollingEngine extends EventEmitter {
  private statusTimer: ReturnType<typeof setInterval> | null = null;
  private eventTimer: ReturnType<typeof setInterval> | null = null;
  private cursor: number | undefined;
  private lastStatus: string | undefined;
  private polling = false;

  constructor(
    private client: StrobeClient,
    private sessionId: string,
    private statusIntervalMs = 200,
    private eventIntervalMs = 500,
  ) {
    super();
  }

  start(): void {
    if (this.polling) return;
    this.polling = true;

    // Immediate first poll
    this.pollStatus();
    this.pollEvents();

    // Fast path: session status
    this.statusTimer = setInterval(
      () => this.pollStatus(),
      this.statusIntervalMs,
    );

    // Event path: incremental query
    this.eventTimer = setInterval(
      () => this.pollEvents(),
      this.eventIntervalMs,
    );
  }

  stop(): void {
    this.polling = false;
    if (this.statusTimer) {
      clearInterval(this.statusTimer);
      this.statusTimer = null;
    }
    if (this.eventTimer) {
      clearInterval(this.eventTimer);
      this.eventTimer = null;
    }
  }

  private async pollStatus(): Promise<void> {
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.emit('status', status);

      // Detect session end
      if (status.status === 'exited' && this.lastStatus !== 'exited') {
        this.emit('sessionEnd', this.sessionId);
        this.stop();
      }
      this.lastStatus = status.status;
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      if (msg.includes('SESSION_NOT_FOUND')) {
        this.emit('sessionEnd', this.sessionId);
        this.stop();
      } else {
        this.emit('error', err instanceof Error ? err : new Error(msg));
      }
    }
  }

  private async pollEvents(): Promise<void> {
    try {
      const resp = await this.client.query({
        sessionId: this.sessionId,
        afterEventId: this.cursor,
        limit: 200,
        verbose: true,
      });

      if (resp.events.length > 0) {
        this.emit('events', resp.events);
      }

      if (resp.lastEventId !== undefined) {
        this.cursor = resp.lastEventId;
      }

      if (resp.eventsDropped) {
        this.emit('eventsDropped');
      }
    } catch (err: unknown) {
      // Suppress SESSION_NOT_FOUND (handled by status poll)
      const msg = err instanceof Error ? err.message : String(err);
      if (!msg.includes('SESSION_NOT_FOUND')) {
        this.emit('error', err instanceof Error ? err : new Error(msg));
      }
    }
  }
}
