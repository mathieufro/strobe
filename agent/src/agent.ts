import { HookInstaller, HookMode } from './hooks.js';
import { RateTracker } from './rate-tracker.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
  imageBase?: string;
  mode?: HookMode;
  serializationDepth?: number;
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

interface WatchInstruction {
  watches: Array<{
    address: string;
    size: number;
    label: string;
    derefDepth: number;
    derefOffset: number;
    typeKind: string;
    isGlobal: boolean;
    onFuncIds: number[];
  }>;
  exprWatches?: Array<{
    expr: string;
    label: string;
    isGlobal: boolean;
    onFuncIds: number[];
  }>;
}

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private hookInstaller: HookInstaller;
  private rateTracker: RateTracker | null = null;
  private funcIdToName: Map<number, string> = new Map();
  private nextFuncId: number = 0;

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
      // Note: Full sampling integration would intercept events here
      // For now, events flow through unchanged
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

    // Initialize rate tracker for hot function detection
    this.rateTracker = new RateTracker(
      this.funcIdToName,
      (funcId: number, enabled: boolean, rate: number) => {
        send({
          type: 'sampling_state_change',
          funcId,
          funcName: this.funcIdToName.get(funcId) || `func_${funcId}`,
          enabled,
          sampleRate: rate,
        });
      }
    );

    // Wire rate tracker into the CModule tracer drain loop
    const tracker = this.rateTracker;
    this.hookInstaller.setRateCheck((funcId: number) => tracker.recordCall(funcId));

    // Periodically send sampling stats
    setInterval(() => {
      if (this.rateTracker) {
        const stats = this.rateTracker.getSamplingStats();
        if (stats.some(s => s.samplingEnabled)) {
          send({ type: 'sampling_stats', stats });
        }
      }
    }, 1000);

    // Install crash/exception handler
    this.installExceptionHandler();

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

      // Set serialization depth if provided
      if (message.serializationDepth) {
        this.hookInstaller.setSerializationDepth(message.serializationDepth);
      }

      const mode: HookMode = message.mode || 'full';
      let installed = 0;
      let failed = 0;

      if (message.action === 'add') {
        for (const func of message.functions) {
          if (this.hookInstaller.installHook(func, mode)) {
            // Track funcId -> name mapping for rate tracker
            const funcId = this.nextFuncId++;
            this.funcIdToName.set(funcId, func.name);
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

  handleWatches(message: WatchInstruction): void {
    try {
      this.hookInstaller.updateWatches(message.watches);
      if (message.exprWatches) {
        this.hookInstaller.updateExprWatches(message.exprWatches);
      }
      send({ type: 'watches_updated', activeCount: message.watches.length });
    } catch (e: any) {
      send({ type: 'log', message: `handleWatches error: ${e.message}` });
      send({ type: 'watches_updated', activeCount: 0 });
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

  private installExceptionHandler(): void {
    Process.setExceptionHandler((details) => {
      const crashEvent = this.buildCrashEvent(details);
      send({ type: 'events', events: [crashEvent] });

      // Return false to let the OS handle the crash (terminate the process)
      // The event will be flushed because send() is synchronous
      return false;
    });
  }

  private buildCrashEvent(details: ExceptionDetails): any {
    const timestamp = this.getTimestampNs();
    const eventId = `${this.sessionId}-crash-${Date.now()}`;

    // Build stack trace using Thread.backtrace
    let backtrace: any[] = [];
    try {
      const frames = Thread.backtrace(details.context, Backtracer.ACCURATE);
      backtrace = frames.map((addr: NativePointer) => {
        const sym = DebugSymbol.fromAddress(addr);
        return {
          address: addr.toString(),
          moduleName: sym.moduleName,
          name: sym.name,
          fileName: sym.fileName,
          lineNumber: sym.lineNumber,
        };
      });
    } catch (e) {
      // Backtrace may fail in some crash scenarios
    }

    // Capture register state from crash context
    const registers: Record<string, string> = {};
    const ctx = details.context as any;
    // ARM64 registers
    if (Process.arch === 'arm64') {
      for (let i = 0; i <= 28; i++) {
        const regName = `x${i}`;
        if (ctx[regName]) registers[regName] = ctx[regName].toString();
      }
      if (ctx.fp) registers.fp = ctx.fp.toString();
      if (ctx.lr) registers.lr = ctx.lr.toString();
      if (ctx.sp) registers.sp = ctx.sp.toString();
      if (ctx.pc) registers.pc = ctx.pc.toString();
    }
    // x86_64 registers
    else if (Process.arch === 'x64') {
      for (const reg of ['rax','rbx','rcx','rdx','rsi','rdi','rbp','rsp',
                         'r8','r9','r10','r11','r12','r13','r14','r15','rip']) {
        if (ctx[reg]) registers[reg] = ctx[reg].toString();
      }
    }

    // Read stack frame memory around frame pointer (for local variable resolution)
    let frameMemory: string | null = null;
    let frameBase: string | null = null;
    try {
      const fp = Process.arch === 'arm64' ? ctx.fp : ctx.rbp;
      if (fp && !fp.isNull()) {
        frameBase = fp.toString();
        // Read 512 bytes below and 128 bytes above FP
        const readBase = fp.sub(512);
        const data = readBase.readByteArray(640);
        if (data) {
          frameMemory = _arrayBufferToHex(data);
        }
      }
    } catch (e) {
      // Frame memory read may fail
    }

    // Memory access details (for access violations)
    let memoryAccess: any = undefined;
    if (details.memory) {
      memoryAccess = {
        operation: details.memory.operation,
        address: details.memory.address.toString(),
      };
    }

    return {
      id: eventId,
      timestampNs: timestamp,
      threadId: Process.getCurrentThreadId(),
      threadName: null,
      eventType: 'crash',
      pid: Process.id,
      signal: details.type,
      faultAddress: details.address.toString(),
      registers: registers,
      backtrace: backtrace,
      frameMemory: frameMemory,
      frameBase: frameBase,
      memoryAccess: memoryAccess,
    };
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

function _arrayBufferToHex(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let hex = '';
  for (let i = 0; i < bytes.length; i++) {
    hex += bytes[i].toString(16).padStart(2, '0');
  }
  return hex;
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

function onHooksMessage(message: HookInstruction): void {
  // Re-register BEFORE processing — recv() is one-shot in Frida.
  // Without this, only the first hooks message is ever received.
  recv('hooks', onHooksMessage);
  send({ type: 'log', message: `Received hooks: action=${message.action} count=${message.functions.length} imageBase=${message.imageBase}` });
  agent.handleMessage(message);
}
recv('hooks', onHooksMessage);

function onWatchesMessage(message: WatchInstruction): void {
  recv('watches', onWatchesMessage);
  agent.handleWatches(message);
}
recv('watches', onWatchesMessage);

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
