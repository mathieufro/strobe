import { Serializer } from './serializer.js';
import { HookInstaller } from './hooks.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
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

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private serializer: Serializer;
  private hookInstaller: HookInstaller;
  private eventBuffer: TraceEvent[] = [];
  private eventIdCounter: number = 0;
  private flushInterval: number = 10; // ms
  private maxBufferSize: number = 1000;

  // Track call stack per thread for parent tracking
  private callStacks: Map<number, string[]> = new Map();

  constructor() {
    this.serializer = new Serializer();
    this.hookInstaller = new HookInstaller(this.onEnter.bind(this), this.onLeave.bind(this));
    this.sessionStartNs = Date.now() * 1000000;

    // Periodic flush
    setInterval(() => this.flush(), this.flushInterval);

    // Intercept write(2) for stdout/stderr capture
    this.installOutputCapture();
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.sessionStartNs = Date.now() * 1000000;
    send({ type: 'initialized', sessionId });
  }

  handleMessage(message: HookInstruction): void {
    if (message.action === 'add') {
      for (const func of message.functions) {
        this.hookInstaller.installHook(func);
      }
    } else if (message.action === 'remove') {
      for (const func of message.functions) {
        this.hookInstaller.removeHook(func.address);
      }
    }

    send({
      type: 'hooks_updated',
      activeCount: this.hookInstaller.activeHookCount()
    });
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

  private bufferEvent(event: TraceEvent): void {
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
        const fd = args[0].toInt32();
        if (fd !== 1 && fd !== 2) return;

        const buf = args[1];
        const count = args[2].toInt32();
        if (count <= 0 || count > 1048576) return; // Skip empty or >1MB writes

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

        const event: OutputEvent = {
          id: self.generateEventId(),
          sessionId: self.sessionId,
          timestampNs: self.getTimestampNs(),
          threadId: Process.getCurrentThreadId(),
          eventType: fd === 1 ? 'stdout' : 'stderr',
          text,
        };

        self.bufferOutputEvent(event);
      }
    });
  }

  private bufferOutputEvent(event: OutputEvent): void {
    // Reuse the same event buffer and flush mechanism
    this.eventBuffer.push(event as any);

    if (this.eventBuffer.length >= this.maxBufferSize) {
      this.flush();
    }
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
  send({ type: 'log', message: 'Received hooks: ' + JSON.stringify(message) });
  agent.handleMessage(message);
});

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
