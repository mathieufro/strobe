import { HookInstaller, HookMode } from './hooks.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
  imageBase?: string;
  mode?: HookMode;
}

interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

interface OutputEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  eventType: 'stdout' | 'stderr';
  text: string;
}

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private hookInstaller: HookInstaller;

  // Output event buffering (low-frequency, stays in JS)
  private outputBuffer: OutputEvent[] = [];
  private outputIdCounter: number = 0;
  private outputFlushInterval: number = 10; // ms
  private maxOutputBufferSize: number = 1000;

  // Re-entrancy guard for write(2) interception
  private inOutputCapture: boolean = false;

  // Per-session output capture limit (50MB)
  private outputBytesCapture: number = 0;
  private maxOutputBytes: number = 50 * 1024 * 1024;

  constructor() {
    this.hookInstaller = new HookInstaller((events) => {
      send({ type: 'events', events });
    });
    this.sessionStartNs = Date.now() * 1000000;

    // Periodic flush for output events
    setInterval(() => this.flushOutput(), this.outputFlushInterval);

    // Intercept write(2) for stdout/stderr capture (non-fatal if it fails,
    // e.g. with ASAN-instrumented binaries where write() isn't hookable)
    try {
      this.installOutputCapture();
    } catch (e) {
      // Output capture is best-effort; function tracing still works
    }
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.sessionStartNs = Date.now() * 1000000;
    this.hookInstaller.setSessionId(sessionId);
    send({ type: 'initialized', sessionId });
  }

  handleMessage(message: HookInstruction): void {
    try {
      // Set imageBase for ASLR slide computation (only needs to happen once)
      if (message.imageBase) {
        send({ type: 'log', message: `Setting imageBase=${message.imageBase}` });
        this.hookInstaller.setImageBase(message.imageBase);
        send({ type: 'log', message: `ASLR slide computed` });
      }

      const mode: HookMode = message.mode || 'full';
      let installed = 0;
      let failed = 0;

      if (message.action === 'add') {
        for (const func of message.functions) {
          if (this.hookInstaller.installHook(func, mode)) {
            installed++;
          } else {
            failed++;
          }
        }
        send({ type: 'log', message: `Hooks: ${installed} installed, ${failed} failed` });
      } else if (message.action === 'remove') {
        for (const func of message.functions) {
          this.hookInstaller.removeHook(func.address);
        }
      }

      send({
        type: 'hooks_updated',
        activeCount: this.hookInstaller.activeHookCount()
      });
    } catch (e: any) {
      send({ type: 'log', message: `handleMessage CRASHED: ${e.message}\n${e.stack}` });
      // Still try to send hooks_updated so the worker doesn't hang
      send({
        type: 'hooks_updated',
        activeCount: this.hookInstaller.activeHookCount()
      });
    }
  }

  private bufferOutputEvent(event: OutputEvent): void {
    this.outputBuffer.push(event);

    if (this.outputBuffer.length >= this.maxOutputBufferSize) {
      this.flushOutput();
    }
  }

  private flushOutput(): void {
    if (this.outputBuffer.length === 0) return;

    const events = this.outputBuffer;
    this.outputBuffer = [];

    send({ type: 'events', events });
  }

  private generateOutputEventId(): string {
    return `${this.sessionId}-out-${++this.outputIdCounter}`;
  }

  private getTimestampNs(): number {
    return Date.now() * 1000000 - this.sessionStartNs;
  }

  private installOutputCapture(): void {
    const self = this;
    const writePtr = Process.getModuleByName('libSystem.B.dylib').getExportByName('write');
    if (!writePtr) return;

    Interceptor.attach(writePtr, {
      onEnter(args) {
        // Re-entrancy guard: skip if we're already inside an intercepted write
        // (e.g. from Frida's own send() calling write())
        if (self.inOutputCapture) return;

        const fd = args[0].toInt32();
        if (fd !== 1 && fd !== 2) return;

        // Check per-session output limit
        if (self.outputBytesCapture >= self.maxOutputBytes) return;

        const buf = args[1];
        const count = args[2].toInt32();
        if (count <= 0) return;

        // For writes >1MB, emit a truncation indicator instead of silently dropping
        if (count > 1048576) {
          self.inOutputCapture = true;
          try {
            const event: OutputEvent = {
              id: self.generateOutputEventId(),
              sessionId: self.sessionId,
              timestampNs: self.getTimestampNs(),
              threadId: Process.getCurrentThreadId(),
              eventType: fd === 1 ? 'stdout' : 'stderr',
              text: `[strobe: write of ${count} bytes truncated (>1MB)]`,
            };
            self.bufferOutputEvent(event);
          } finally {
            self.inOutputCapture = false;
          }
          return;
        }

        self.inOutputCapture = true;
        try {
          let text: string;
          try {
            text = buf.readUtf8String(count) ?? '';
          } catch {
            try {
              text = buf.readCString(count) ?? '';
            } catch {
              return; // Can't read buffer, skip
            }
          }

          if (text.length === 0) return;

          self.outputBytesCapture += count;

          // Check if we just exceeded the limit
          if (self.outputBytesCapture >= self.maxOutputBytes) {
            const event: OutputEvent = {
              id: self.generateOutputEventId(),
              sessionId: self.sessionId,
              timestampNs: self.getTimestampNs(),
              threadId: Process.getCurrentThreadId(),
              eventType: fd === 1 ? 'stdout' : 'stderr',
              text: text + '\n[strobe: output capture limit reached (50MB), further output truncated]',
            };
            self.bufferOutputEvent(event);
            return;
          }

          const event: OutputEvent = {
            id: self.generateOutputEventId(),
            sessionId: self.sessionId,
            timestampNs: self.getTimestampNs(),
            threadId: Process.getCurrentThreadId(),
            eventType: fd === 1 ? 'stdout' : 'stderr',
            text,
          };

          self.bufferOutputEvent(event);
        } finally {
          self.inOutputCapture = false;
        }
      }
    });
  }
}

// Global agent instance
const agent = new StrobeAgent();

// Immediate startup message
send({ type: 'agent_loaded', message: 'Strobe agent loaded and ready' });

// Message handler
recv('initialize', (message: { sessionId: string }) => {
  send({ type: 'log', message: 'Received initialize: ' + JSON.stringify(message) });
  agent.initialize(message.sessionId);
});

recv('hooks', (message: HookInstruction) => {
  send({ type: 'log', message: `Received hooks: action=${message.action} count=${message.functions.length} imageBase=${message.imageBase}` });
  agent.handleMessage(message);
});

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
