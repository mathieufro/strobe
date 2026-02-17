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
  private statusInFlight = false;
  private eventsInFlight = false;
  private consecutiveErrors = 0;

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
    if (this.statusInFlight) return;
    this.statusInFlight = true;
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.consecutiveErrors = 0;
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
        this.consecutiveErrors++;
        if (this.consecutiveErrors > 5) {
          this.emit('error', err instanceof Error ? err : new Error(msg));
          this.stop();
        }
      }
    } finally {
      this.statusInFlight = false;
    }
  }

  private async pollEvents(): Promise<void> {
    if (this.eventsInFlight) return;
    this.eventsInFlight = true;
    try {
      const resp = await this.client.query({
        sessionId: this.sessionId,
        afterEventId: this.cursor,
        limit: 200,
        verbose: true,
      });

      if (resp.lastEventId != null) {
        this.cursor = resp.lastEventId;
      }

      if (resp.events.length > 0) {
        this.emit('events', resp.events);
      }

      if (resp.eventsDropped) {
        this.emit('eventsDropped');
      }
    } catch {
      // Errors handled by pollStatus consecutiveErrors counter
    } finally {
      this.eventsInFlight = false;
    }
  }
}
