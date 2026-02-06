import { Serializer } from './serializer.js';
import { HookInstaller } from './hooks.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
  imageBase?: string;
}

interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

interface TraceEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  parentEventId: string | null;
  eventType: 'function_enter' | 'function_exit';
  functionName: string;
  functionNameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
  arguments?: any[];
  returnValue?: any;
  durationNs?: number;
}

interface OutputEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  eventType: 'stdout' | 'stderr';
  text: string;
}

type BufferedEvent = TraceEvent | OutputEvent;

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private serializer: Serializer;
  private hookInstaller: HookInstaller;
  private eventBuffer: BufferedEvent[] = [];
  private eventIdCounter: number = 0;
  private flushInterval: number = 10; // ms
  private maxBufferSize: number = 1000;

  // Track call stack per thread for parent tracking
  private callStacks: Map<number, string[]> = new Map();

  // Re-entrancy guard for write(2) interception
  private inOutputCapture: boolean = false;

  // Per-session output capture limit (50MB)
  private outputBytesCapture: number = 0;
  private maxOutputBytes: number = 50 * 1024 * 1024;

  constructor() {
    this.serializer = new Serializer();
    this.hookInstaller = new HookInstaller(this.onEnter.bind(this), this.onLeave.bind(this));
    this.sessionStartNs = Date.now() * 1000000;

    // Periodic flush
    setInterval(() => this.flush(), this.flushInterval);

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

      let installed = 0;
      let failed = 0;

      if (message.action === 'add') {
        for (const func of message.functions) {
          if (this.hookInstaller.installHook(func)) {
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

  private onEnter(
    threadId: number,
    func: FunctionTarget,
    args: NativePointer[]
  ): string {
    const eventId = this.generateEventId();
    const stack = this.callStacks.get(threadId) || [];
    const parentId = stack.length > 0 ? stack[stack.length - 1] : null;

    // Push this event onto call stack
    stack.push(eventId);
    this.callStacks.set(threadId, stack);

    const event: TraceEvent = {
      id: eventId,
      sessionId: this.sessionId,
      timestampNs: this.getTimestampNs(),
      threadId,
      parentEventId: parentId,
      eventType: 'function_enter',
      functionName: func.name,
      functionNameRaw: func.nameRaw,
      sourceFile: func.sourceFile,
      lineNumber: func.lineNumber,
      arguments: args.map(arg => this.serializer.serialize(arg)),
    };

    this.bufferEvent(event);
    return eventId;
  }

  private onLeave(
    threadId: number,
    func: FunctionTarget,
    retval: NativePointer,
    enterEventId: string,
    enterTimestampNs: number
  ): void {
    const now = this.getTimestampNs();

    // Pop from call stack
    const stack = this.callStacks.get(threadId) || [];
    stack.pop();
    this.callStacks.set(threadId, stack);

    const event: TraceEvent = {
      id: this.generateEventId(),
      sessionId: this.sessionId,
      timestampNs: now,
      threadId,
      parentEventId: enterEventId,
      eventType: 'function_exit',
      functionName: func.name,
      functionNameRaw: func.nameRaw,
      sourceFile: func.sourceFile,
      lineNumber: func.lineNumber,
      returnValue: this.serializer.serialize(retval),
      durationNs: now - enterTimestampNs,
    };

    this.bufferEvent(event);
  }

  private bufferEvent(event: BufferedEvent): void {
    this.eventBuffer.push(event);

    if (this.eventBuffer.length >= this.maxBufferSize) {
      this.flush();
    }
  }

  private flush(): void {
    if (this.eventBuffer.length === 0) return;

    const events = this.eventBuffer;
    this.eventBuffer = [];

    send({ type: 'events', events });
  }

  private generateEventId(): string {
    return `${this.sessionId}-${++this.eventIdCounter}`;
  }

  private getTimestampNs(): number {
    return Date.now() * 1000000 - this.sessionStartNs;
  }

  private installOutputCapture(): void {
    const self = this;
    const writePtr = Module.getExportByName(null, 'write');
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
              id: self.generateEventId(),
              sessionId: self.sessionId,
              timestampNs: self.getTimestampNs(),
              threadId: Process.getCurrentThreadId(),
              eventType: fd === 1 ? 'stdout' : 'stderr',
              text: `[strobe: write of ${count} bytes truncated (>1MB)]`,
            };
            self.bufferEvent(event);
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
              id: self.generateEventId(),
              sessionId: self.sessionId,
              timestampNs: self.getTimestampNs(),
              threadId: Process.getCurrentThreadId(),
              eventType: fd === 1 ? 'stdout' : 'stderr',
              text: text + '\n[strobe: output capture limit reached (50MB), further output truncated]',
            };
            self.bufferEvent(event);
            return;
          }

          const event: OutputEvent = {
            id: self.generateEventId(),
            sessionId: self.sessionId,
            timestampNs: self.getTimestampNs(),
            threadId: Process.getCurrentThreadId(),
            eventType: fd === 1 ? 'stdout' : 'stderr',
            text,
          };

          self.bufferEvent(event);
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
