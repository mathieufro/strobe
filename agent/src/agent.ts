import { CModuleTracer, HookMode, type FunctionTarget } from './cmodule-tracer.js';
import { createPlatformAdapter, type PlatformAdapter } from './platform.js';
import { RateTracker } from './rate-tracker.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions: FunctionTarget[];
  imageBase?: string;
  mode?: HookMode;
  serializationDepth?: number;
}

interface OutputEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  eventType: 'stdout' | 'stderr';
  text: string;
}

interface BacktraceFrame {
  address: string;
  moduleName: string | null;
  name: string | null;
  fileName: string | null;
  lineNumber: number | null;
}

interface CrashEvent {
  id: string;
  timestampNs: number;
  threadId: number;
  threadName: null;
  eventType: 'crash';
  pid: number;
  signal: string;
  faultAddress: string;
  registers: Record<string, string>;
  backtrace: BacktraceFrame[];
  frameMemory: string | null;
  frameBase: string | null;
  memoryAccess?: { operation: string; address: string };
}

interface ReadRecipe {
  label: string;
  address: string;  // hex
  size: number;
  typeKind: string;  // "int", "uint", "float", "pointer", "bytes"
  derefDepth: number;
  derefOffset: number;
  noSlide?: boolean;  // true for raw user-provided addresses (already absolute)
  struct?: boolean;
  fields?: Array<{
    name: string;
    offset: number;
    size: number;
    typeKind: string;
    typeName?: string;
    isTruncatedStruct?: boolean;
  }>;
}

interface ReadMemoryMessage {
  recipes: ReadRecipe[];
  imageBase?: string;  // For ASLR slide computation (if not already set via hooks)
  poll?: {
    intervalMs: number;
    durationMs: number;
  };
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
    noSlide?: boolean;
    onPatterns?: string[];
  }>;
  exprWatches?: Array<{
    expr: string;
    label: string;
    isGlobal: boolean;
    onPatterns?: string[];
  }>;
}

// Phase 2: Breakpoint interfaces
interface BreakpointState {
  id: string;
  address: NativePointer;
  condition?: string;
  hitCount: number;
  hits: number;
  listener: InvocationListener;
  funcName?: string;
  file?: string;
  line?: number;
}

interface WriteMemoryRecipe {
  label: string;
  address: string;  // hex
  size: number;
  typeKind: string;  // "int", "uint", "float", "pointer"
  value: number;     // numeric value to write
  noSlide?: boolean; // true for raw user-provided addresses
}

interface WriteMemoryMessage {
  recipes: WriteMemoryRecipe[];
  imageBase?: string;
}

interface SetBreakpointMessage {
  address: string;
  id: string;
  condition?: string;
  hitCount?: number;
  funcName?: string;
  file?: string;
  line?: number;
  imageBase?: string;
}

interface RemoveBreakpointMessage {
  id: string;
}

interface SetLogpointMessage {
  address: string;
  id: string;
  message: string;
  condition?: string;
  funcName?: string;
  file?: string;
  line?: number;
  imageBase?: string;
}

interface LogpointState {
  id: string;
  address: NativePointer;
  message: string;
  condition?: string;
  listener: InvocationListener;
  funcName?: string;
  file?: string;
  line?: number;
}

interface OneShotAddress {
  address: string;
  noSlide?: boolean;  // true for runtime addresses (e.g., return address)
}

interface ResumeMessage {
  oneShot?: OneShotAddress[]; // Addresses for one-shot step breakpoints
  imageBase?: string;         // For ASLR slide computation
}

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private eventSeq: number = 0;
  private platform: PlatformAdapter;
  private tracer: CModuleTracer;
  private rateTracker: RateTracker | null = null;
  private funcIdToName: Map<number, string> = new Map();

  // Phase 2: Breakpoint management
  private breakpoints: Map<string, BreakpointState> = new Map(); // id → state
  private breakpointsByAddress: Map<string, string> = new Map(); // address → id
  private pausedThreads: Map<number, string> = new Map(); // threadId → breakpointId
  private logpoints: Map<string, LogpointState> = new Map(); // id → state

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

  // Active poll timer — only one poll at a time, new poll cancels previous
  private activePollTimer: ReturnType<typeof setInterval> | null = null;

  // Interval handles for cleanup
  private outputFlushTimer: ReturnType<typeof setInterval> | null = null;
  private samplingStatsTimer: ReturnType<typeof setInterval> | null = null;

  constructor() {
    this.platform = createPlatformAdapter();
    this.tracer = new CModuleTracer((events) => {
      send({ type: 'events', events });
    }, this.platform);
    this.sessionStartNs = Date.now() * 1000000;

    // Periodic flush for output events
    this.outputFlushTimer = setInterval(() => this.flushOutput(), this.outputFlushInterval);

    // Install crash/exception handler early — before process resumes —
    // so crashes are caught even if the "initialize" message hasn't arrived yet.
    this.installExceptionHandler();

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
    this.tracer.setSessionId(sessionId);

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
      },
      (message: string) => {
        send({ type: 'log', message });
      }
    );

    // Wire rate tracker into the CModule tracer drain loop
    const tracker = this.rateTracker;
    this.tracer.setRateCheck((funcId: number) => tracker.recordCall(funcId));

    // Periodically send sampling stats
    this.samplingStatsTimer = setInterval(() => {
      const stats = this.rateTracker!.getSamplingStats();
      if (stats.some(s => s.samplingEnabled)) {
        send({ type: 'sampling_stats', stats });
      }
    }, 1000);

    send({ type: 'initialized', sessionId });
  }

  handleMessage(message: HookInstruction): void {
    try {
      // Set imageBase for ASLR slide computation (only needs to happen once)
      if (message.imageBase) {
        send({ type: 'log', message: `Setting imageBase=${message.imageBase}` });
        this.tracer.setImageBase(message.imageBase);
        send({ type: 'log', message: `ASLR slide computed` });
      }

      // Set serialization depth if provided
      if (message.serializationDepth) {
        this.tracer.setSerializationDepth(message.serializationDepth);
      }

      const mode: HookMode = message.mode || 'full';
      let installed = 0;
      let failed = 0;

      if (message.action === 'add') {
        for (const func of message.functions) {
          const funcId = this.tracer.installHook(func, mode);
          if (funcId !== null) {
            this.funcIdToName.set(funcId, func.name);
            installed++;
          } else {
            failed++;
          }
        }
        send({ type: 'log', message: `Hooks: ${installed} installed, ${failed} failed` });
      } else if (message.action === 'remove') {
        for (const func of message.functions) {
          this.tracer.removeHook(func.address);
        }
      }

      send({
        type: 'hooks_updated',
        activeCount: this.tracer.activeHookCount()
      });
    } catch (e: any) {
      send({ type: 'log', message: `handleMessage CRASHED: ${e.message}\n${e.stack}` });
      // Still try to send hooks_updated so the worker doesn't hang
      send({
        type: 'hooks_updated',
        activeCount: this.tracer.activeHookCount()
      });
    }
  }

  handleWatches(message: WatchInstruction): void {
    try {
      this.tracer.updateWatches(message.watches);
      if (message.exprWatches) {
        this.tracer.updateExprWatches(message.exprWatches);
      }
      const totalCount = message.watches.length + (message.exprWatches ? message.exprWatches.length : 0);
      send({ type: 'watches_updated', activeCount: totalCount });
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

      // Write crash data to a temp file using synchronous native I/O.
      // This bypasses GLib's async message delivery which may not flush
      // before the OS kills the process (especially on Linux).
      this.writeCrashFile(crashEvent);

      // Also try the normal async path (best effort)
      send({ type: 'events', events: [crashEvent] });

      // Sleep the crashing thread to give GLib time to flush.
      Thread.sleep(0.1);

      // Return false to let the OS handle the crash (terminate the process)
      return false;
    });
  }

  private writeCrashFile(crashEvent: CrashEvent): void {
    try {
      const data = JSON.stringify({ type: 'events', events: [crashEvent] });
      const crashPath = `/tmp/.strobe-crash-${Process.id}.json`;
      const pathBuf = Memory.allocUtf8String(crashPath);
      const dataBuf = Memory.allocUtf8String(data);

      const openFn = new NativeFunction(
        Module.getExportByName(null, 'open'), 'int', ['pointer', 'int', 'int']);
      const writeFn = new NativeFunction(
        Module.getExportByName(null, 'write'), 'long', ['int', 'pointer', 'long']);
      const closeFn = new NativeFunction(
        Module.getExportByName(null, 'close'), 'int', ['int']);

      // O_WRONLY | O_CREAT | O_TRUNC
      const flags = Process.platform === 'linux' ? 0x241 : 0x601;
      const fd = openFn(pathBuf, flags, 0o644) as number;
      if (fd >= 0) {
        writeFn(fd, dataBuf, data.length);
        closeFn(fd);
      }
    } catch (e) {
      // Best effort — crash file write may fail
    }
  }

  private buildCrashEvent(details: ExceptionDetails): CrashEvent {
    const timestamp = this.getTimestampNs();
    const eventId = `${this.sessionId || 'uninitialized'}-crash-${Date.now()}`;

    // Build stack trace using Thread.backtrace
    let backtrace: BacktraceFrame[] = [];
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
    let memoryAccess: CrashEvent['memoryAccess'] = undefined;
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
      registers,
      backtrace,
      frameMemory,
      frameBase,
      memoryAccess,
    };
  }

  private createOutputEvent(fd: number, text: string): OutputEvent {
    return {
      id: this.generateOutputEventId(),
      sessionId: this.sessionId,
      timestampNs: this.getTimestampNs(),
      threadId: Process.getCurrentThreadId(),
      eventType: fd === 1 ? 'stdout' : 'stderr',
      text,
    };
  }

  /** Clean shutdown: flush all buffered data before script teardown. */
  dispose(): void {
    // Stop all timers
    if (this.outputFlushTimer !== null) {
      clearInterval(this.outputFlushTimer);
      this.outputFlushTimer = null;
    }
    if (this.samplingStatsTimer !== null) {
      clearInterval(this.samplingStatsTimer);
      this.samplingStatsTimer = null;
    }
    if (this.activePollTimer !== null) {
      clearInterval(this.activePollTimer);
      this.activePollTimer = null;
    }

    // Flush CModule ring buffer (final drain) and stop its timer
    this.tracer.dispose();

    // Flush any remaining output events
    this.flushOutput();
  }

  handleReadMemory(message: ReadMemoryMessage): void {
    // Ensure ASLR slide is set (may not be if no hooks installed yet)
    if (message.imageBase) {
      this.tracer.setImageBase(message.imageBase);
    }
    const slide = this.tracer.getSlide();

    if (message.poll) {
      this.startReadPoll(message.recipes, slide, message.poll);
      return;
    }

    // One-shot mode: read all targets, send response
    const results = message.recipes.map(recipe => this.readSingleTarget(recipe, slide));
    send({ type: 'read_response', results });
  }

  private readSingleTarget(recipe: ReadRecipe, slide: NativePointer): any {
    try {
      // Raw user-provided addresses are already absolute — don't apply ASLR slide
      const baseAddr = recipe.noSlide
        ? ptr(recipe.address)
        : ptr(recipe.address).add(slide);

      // Handle struct reads
      if (recipe.struct && recipe.fields) {
        const structPtr = baseAddr.readPointer();
        if (structPtr.isNull()) {
          return { label: recipe.label, error: `Null pointer at ${recipe.label}` };
        }
        const fields: Record<string, any> = {};
        for (const field of recipe.fields) {
          if (field.isTruncatedStruct) {
            fields[field.name] = '<struct>';
            continue;
          }
          try {
            const fieldAddr = structPtr.add(field.offset);
            fields[field.name] = this.readTypedValue(fieldAddr, field.size, field.typeKind);
          } catch (e: any) {
            fields[field.name] = `<error: ${e.message}>`;
          }
        }
        return { label: recipe.label, fields };
      }

      // Handle deref chain (e.g. gClock->counter)
      if (recipe.derefDepth > 0) {
        const ptrVal = baseAddr.readPointer();
        if (ptrVal.isNull()) {
          return { label: recipe.label, error: `Null pointer at ${recipe.label.split('->')[0]}` };
        }
        const finalAddr = ptrVal.add(recipe.derefOffset);
        const value = this.readTypedValue(finalAddr, recipe.size, recipe.typeKind);
        return { label: recipe.label, value };
      }

      // Simple direct read
      if (recipe.typeKind === 'bytes') {
        const bytes = baseAddr.readByteArray(recipe.size);
        if (!bytes) return { label: recipe.label, error: 'Failed to read bytes' };
        return { label: recipe.label, value: _arrayBufferToHex(bytes), isBytes: true };
      }

      const value = this.readTypedValue(baseAddr, recipe.size, recipe.typeKind);
      return { label: recipe.label, value };
    } catch (e: any) {
      return { label: recipe.label, error: `Address not readable: ${e.message}` };
    }
  }

  private readTypedValue(addr: NativePointer, size: number, typeKind: string): any {
    // Note: Process.findRangeByAddress() can hang on large macOS binaries with
    // unmapped addresses — skip the pre-check and rely on try/catch in the caller.
    switch (typeKind) {
      case 'float':
        return size === 4 ? addr.readFloat() : addr.readDouble();
      case 'int':
        switch (size) {
          case 1: return addr.readS8();
          case 2: return addr.readS16();
          case 4: return addr.readS32();
          case 8: return addr.readS64().toNumber();
          default: return addr.readS32();
        }
      case 'uint':
        switch (size) {
          case 1: return addr.readU8();
          case 2: return addr.readU16();
          case 4: return addr.readU32();
          case 8: return addr.readU64().toNumber();
          default: return addr.readU32();
        }
      case 'pointer':
        return addr.readPointer().toString();
      default:
        return addr.readU64().toNumber();
    }
  }

  handleWriteMemory(message: WriteMemoryMessage): void {
    if (message.imageBase) {
      this.tracer.setImageBase(message.imageBase);
    }
    const slide = this.tracer.getSlide();

    const results = message.recipes.map(recipe => this.writeSingleTarget(recipe, slide));
    send({ type: 'write_response', results });
  }

  private writeSingleTarget(recipe: WriteMemoryRecipe, slide: NativePointer): any {
    try {
      const addr = recipe.noSlide
        ? ptr(recipe.address)
        : ptr(recipe.address).add(slide);

      // Read previous value first
      const previousValue = this.readTypedValue(addr, recipe.size, recipe.typeKind);

      // Write new value
      this.writeTypedValue(addr, recipe.size, recipe.typeKind, recipe.value);

      // Read back to confirm
      const newValue = this.readTypedValue(addr, recipe.size, recipe.typeKind);

      return { label: recipe.label, address: addr.toString(), previousValue, newValue };
    } catch (e: any) {
      return { label: recipe.label, error: `Write failed: ${e.message}` };
    }
  }

  private writeTypedValue(addr: NativePointer, size: number, typeKind: string, value: number): void {
    // Note: Process.findRangeByAddress() can hang on large macOS binaries —
    // skip pre-check and rely on try/catch in the caller for error handling.

    switch (typeKind) {
      case 'float':
        if (size === 4) addr.writeFloat(value);
        else addr.writeDouble(value);
        break;
      case 'int':
        switch (size) {
          case 1: addr.writeS8(value); break;
          case 2: addr.writeS16(value); break;
          case 4: addr.writeS32(value); break;
          case 8: addr.writeS64(value); break;
          default: addr.writeS32(value);
        }
        break;
      case 'uint':
        switch (size) {
          case 1: addr.writeU8(value); break;
          case 2: addr.writeU16(value); break;
          case 4: addr.writeU32(value); break;
          case 8: addr.writeU64(value); break;
          default: addr.writeU32(value);
        }
        break;
      case 'pointer':
        addr.writePointer(ptr(value));
        break;
      default:
        addr.writeU64(value);
    }
  }

  private startReadPoll(
    recipes: ReadRecipe[],
    slide: NativePointer,
    poll: { intervalMs: number; durationMs: number }
  ): void {
    // Cancel any existing poll before starting a new one
    if (this.activePollTimer !== null) {
      clearInterval(this.activePollTimer);
      this.activePollTimer = null;
    }

    const startTime = Date.now();
    let sampleCount = 0;

    const timer = setInterval(() => {
      const elapsed = Date.now() - startTime;
      if (elapsed >= poll.durationMs) {
        clearInterval(timer);
        this.activePollTimer = null;
        send({ type: 'poll_complete', sampleCount });
        return;
      }

      const data: Record<string, any> = {};
      for (const recipe of recipes) {
        const result = this.readSingleTarget(recipe, slide);
        if (result.error) {
          data[recipe.label] = `<error: ${result.error}>`;
        } else if (result.fields) {
          data[recipe.label] = result.fields;
        } else {
          data[recipe.label] = result.value;
        }
      }

      sampleCount++;
      send({
        type: 'events',
        events: [{
          id: `${this.sessionId}-snap-${sampleCount}`,
          timestampNs: this.getTimestampNs(),
          threadId: Process.getCurrentThreadId(),
          eventType: 'variable_snapshot',
          data,
        }],
      });
    }, poll.intervalMs);

    this.activePollTimer = timer;
  }

  private installOutputCapture(): void {
    const self = this;
    const writePtr = this.platform.resolveWritePtr();
    if (!writePtr) return;

    // Note: inOutputCapture is a process-global flag, not thread-local.
    // In multi-threaded targets, two threads calling write() simultaneously
    // can race on this flag. In practice, Frida's GIL serializes JS execution,
    // and the Device-level output capture (raw_on_output) serves as fallback.
    // The write hook is best-effort for additional capture fidelity.
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
            self.bufferOutputEvent(
              self.createOutputEvent(fd, `[strobe: write of ${count} bytes truncated (>1MB)]`)
            );
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
            self.bufferOutputEvent(
              self.createOutputEvent(fd, text + '\n[strobe: output capture limit reached (50MB), further output truncated]')
            );
            return;
          }

          self.bufferOutputEvent(self.createOutputEvent(fd, text));
        } finally {
          self.inOutputCapture = false;
        }
      }
    });
  }

  // ========== Phase 2: Breakpoint methods ==========

  setBreakpoint(msg: SetBreakpointMessage): void {
    // Apply ASLR slide: address from daemon is DWARF-static
    if (msg.imageBase) {
      this.tracer.setImageBase(msg.imageBase);
    }
    const slide = this.tracer.getSlide();
    const address = ptr(msg.address).add(slide);
    const self = this;

    const listener = Interceptor.attach(address, {
      onEnter(args) {
        const bp = self.breakpoints.get(msg.id);
        if (!bp) return;

        // Evaluate condition if present
        if (bp.condition && !self.evaluateCondition(bp.condition, args, bp.id)) {
          return;
        }

        // Hit count logic
        bp.hits++;
        if (bp.hitCount > 0 && bp.hits < bp.hitCount) {
          return;
        }

        // Notify daemon of pause
        const threadId = Process.getCurrentThreadId();
        self.pausedThreads.set(threadId, bp.id);

        // Capture return address for step-out support
        // ARM64: LR register, x86_64: [RBP+8] - Frida's returnAddress handles both
        const returnAddr = this.returnAddress;

        send({
          type: 'paused',
          threadId,
          breakpointId: bp.id,
          hits: bp.hits,
          funcName: bp.funcName,
          file: bp.file,
          line: bp.line,
          returnAddress: returnAddr ? returnAddr.strip().toString() : null,
        });

        // Block this thread until resume message
        const op = recv(`resume-${threadId}`, (resumeMsg: ResumeMessage) => {
          // Phase 2b: Install one-shot stepping breakpoints
          if (resumeMsg.oneShot && resumeMsg.oneShot.length > 0) {
            // Compute ASLR slide for DWARF-static addresses
            if (resumeMsg.imageBase) {
              self.tracer.setImageBase(resumeMsg.imageBase);
            }
            const stepSlide = self.tracer.getSlide();

            const stepId = `step-${threadId}-${Date.now()}`;
            const listeners: InvocationListener[] = [];
            let fired = false;
            let timeoutId: ReturnType<typeof setTimeout> | null = null;

            const cleanupAll = () => {
              if (fired) return;
              fired = true;
              if (timeoutId !== null) {
                clearTimeout(timeoutId);
                timeoutId = null;
              }
              for (const l of listeners) {
                l.detach();
              }
            };

            for (const entry of resumeMsg.oneShot) {
              // noSlide=true for runtime addresses (e.g., return address)
              const addr = entry.noSlide ? ptr(entry.address) : ptr(entry.address).add(stepSlide);
              const stepListener = Interceptor.attach(addr, {
                onEnter() {
                  cleanupAll();

                  // Send pause event with return address
                  send({
                    type: 'paused',
                    threadId: Process.getCurrentThreadId(),
                    breakpointId: stepId,
                    funcName: null,
                    file: null,
                    line: null,
                    returnAddress: this.returnAddress ? this.returnAddress.strip().toString() : null,
                  });

                  // Block again for next resume
                  const nextOp = recv(`resume-${Process.getCurrentThreadId()}`, () => {});
                  nextOp.wait();
                },
              });
              listeners.push(stepListener);
            }

            // Safety timeout: clean up one-shot hooks after 30s if none fired
            timeoutId = setTimeout(() => {
              if (!fired) {
                cleanupAll();
                send({ type: 'log', message: `One-shot step hooks timed out for thread ${threadId}` });
              }
            }, 30000);
          }
        });
        op.wait(); // CRITICAL: Blocks native thread, releases JS lock

        self.pausedThreads.delete(threadId);
      },
    });

    const breakpointState: BreakpointState = {
      id: msg.id,
      address,
      condition: msg.condition,
      hitCount: msg.hitCount || 0,
      hits: 0,
      listener,
      funcName: msg.funcName,
      file: msg.file,
      line: msg.line,
    };

    this.breakpoints.set(msg.id, breakpointState);
    this.breakpointsByAddress.set(address.toString(), msg.id);

    send({
      type: 'breakpointSet',
      id: msg.id,
      address: address.toString(),
    });
  }

  setLogpoint(msg: SetLogpointMessage): void {
    // Apply ASLR slide: address from daemon is DWARF-static
    if (msg.imageBase) {
      this.tracer.setImageBase(msg.imageBase);
    }
    const slide = this.tracer.getSlide();
    const address = ptr(msg.address).add(slide);

    const listener = Interceptor.attach(address, {
      onEnter: (args) => {
        const lp = this.logpoints.get(msg.id);
        if (!lp) return;

        // Evaluate condition if present
        if (lp.condition) {
          try {
            const argsArray: any[] = [];
            for (let i = 0; i < 10; i++) {
              try { argsArray.push(args[i]); } catch { break; }
            }
            const result = new Function('args', `return (${lp.condition})`)(argsArray);
            if (!Boolean(result)) return;
          } catch {
            return; // Condition evaluation failed, skip
          }
        }

        // Evaluate message template
        let evaluatedMessage = lp.message;
        try {
          const argsArray: any[] = [];
          for (let i = 0; i < 10; i++) {
            try { argsArray.push(args[i]); } catch { break; }
          }
          // Replace {args[N]} placeholders with actual values
          evaluatedMessage = lp.message.replace(/\{args\[(\d+)\]\}/g, (_match, idx) => {
            const i = parseInt(idx);
            return i < argsArray.length ? String(argsArray[i]) : '<undefined>';
          });
          // Replace {threadId} placeholder
          evaluatedMessage = evaluatedMessage.replace(/\{threadId\}/g, String(Process.getCurrentThreadId()));
        } catch (e) {
          evaluatedMessage = `[logpoint eval error: ${e}]`;
        }

        // Send as logpoint event (non-blocking - does NOT pause)
        send({
          type: 'events',
          events: [{
            id: `${this.sessionId}-logpoint-${++this.eventSeq}`,
            timestampNs: this.getTimestampNs(),
            threadId: Process.getCurrentThreadId(),
            eventType: 'logpoint',
            breakpointId: lp.id,
            message: evaluatedMessage,
            functionName: lp.funcName,
            file: lp.file,
            line: lp.line,
          }],
        });
      },
    });

    const logpointState: LogpointState = {
      id: msg.id,
      address,
      message: msg.message,
      condition: msg.condition,
      listener,
      funcName: msg.funcName,
      file: msg.file,
      line: msg.line,
    };

    this.logpoints.set(msg.id, logpointState);

    send({
      type: 'logpointSet',
      id: msg.id,
      address: address.toString(),
    });
  }

  removeLogpoint(id: string): void {
    const lp = this.logpoints.get(id);
    if (!lp) return;

    lp.listener.detach();
    this.logpoints.delete(id);

    send({ type: 'logpointRemoved', id });
  }

  removeBreakpoint(id: string): void {
    const bp = this.breakpoints.get(id);
    if (!bp) {
      send({ type: 'error', message: `Breakpoint ${id} not found` });
      return;
    }

    // NOTE: If threads are paused on this breakpoint, the daemon sends
    // resume messages BEFORE requesting removal. We don't attempt to
    // resume from the agent side — send() goes outbound to the daemon,
    // not to the local recv().wait() handler.

    bp.listener.detach();
    this.breakpoints.delete(id);
    this.breakpointsByAddress.delete(bp.address.toString());

    send({ type: 'breakpointRemoved', id });
  }

  private evaluateCondition(condition: string, args: InvocationArguments, breakpointId?: string): boolean {
    try {
      // Convert args to array for Function context
      const argsArray: any[] = [];
      for (let i = 0; i < 10; i++) {
        try {
          argsArray.push(args[i]);
        } catch {
          break;
        }
      }

      const result = new Function('args', `return (${condition})`)(argsArray);
      return Boolean(result);
    } catch (e) {
      send({
        type: 'conditionError',
        breakpointId: breakpointId || 'unknown',
        condition,
        error: String(e),
      });
      return false;
    }
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

// Frida's recv() is one-shot: must re-register before processing to avoid
// losing messages sent during processing. Message ordering is guaranteed
// by Frida's single-threaded JS execution model. If handleMessage() throws,
// the re-registration has already happened so subsequent messages are safe.
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

function onReadMemoryMessage(message: ReadMemoryMessage): void {
  recv('read_memory', onReadMemoryMessage);
  agent.handleReadMemory(message);
}
recv('read_memory', onReadMemoryMessage);

// Phase 2a: Write memory message handler
function onWriteMemoryMessage(message: WriteMemoryMessage): void {
  recv('write_memory', onWriteMemoryMessage);
  agent.handleWriteMemory(message);
}
recv('write_memory', onWriteMemoryMessage);

// Phase 2: Breakpoint message handlers
function onSetBreakpointMessage(message: SetBreakpointMessage): void {
  recv('setBreakpoint', onSetBreakpointMessage);
  agent.setBreakpoint(message);
}
recv('setBreakpoint', onSetBreakpointMessage);

function onRemoveBreakpointMessage(message: RemoveBreakpointMessage): void {
  recv('removeBreakpoint', onRemoveBreakpointMessage);
  agent.removeBreakpoint(message.id);
}
recv('removeBreakpoint', onRemoveBreakpointMessage);

function onSetLogpointMessage(message: SetLogpointMessage): void {
  recv('setLogpoint', onSetLogpointMessage);
  agent.setLogpoint(message);
}
recv('setLogpoint', onSetLogpointMessage);

function onRemoveLogpointMessage(message: RemoveBreakpointMessage): void {
  recv('removeLogpoint', onRemoveLogpointMessage);
  agent.removeLogpoint(message.id);
}
recv('removeLogpoint', onRemoveLogpointMessage);

// Frida calls rpc.exports.dispose() before script unload — ensures all
// buffered trace events (CModule ring buffer) and output events are flushed.
rpc.exports = {
  dispose() {
    agent.dispose();
  }
};

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
