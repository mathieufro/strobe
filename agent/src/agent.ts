import { CModuleTracer, HookMode, type FunctionTarget } from './cmodule-tracer.js';
import { createPlatformAdapter, type PlatformAdapter } from './platform.js';
import { RateTracker } from './rate-tracker.js';
import { findGlobalExport } from './utils.js';
import { Tracer, type ResolvedTarget as TracerResolvedTarget } from './tracers/tracer.js';
import { NativeTracer } from './tracers/native-tracer.js';
import { PythonTracer } from './tracers/python-tracer.js';

interface HookInstruction {
  action: 'add' | 'remove';
  functions?: FunctionTarget[];  // Address-based targets (native)
  targets?: Array<{              // file:line-based targets (interpreted)
    file: string;
    line: number;
    name: string;
  }>;
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
  exceptionType?: string;
  exceptionMessage?: string | null;
  throwBacktrace?: BacktraceFrame[];
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
  address?: string;
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
  address?: string;
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

interface InstallStepHooksMessage {
  threadId: number;
  oneShot: OneShotAddress[];
  imageBase?: string;
  returnAddress?: string | null; // Carried forward from original BP for step-out support
}

// Type aliases for Tracer interface compatibility
type ResolvedTarget = FunctionTarget;
type BreakpointMessage = SetBreakpointMessage;
type LogpointMessage = SetLogpointMessage;
type StepHooksMessage = InstallStepHooksMessage;

// Runtime detection types
type RuntimeType = 'native' | 'cpython' | 'v8' | 'jsc';

/**
 * Detect the target process runtime by probing for known symbols.
 * Returns 'native' if no interpreter runtime is detected.
 */
function detectRuntime(): RuntimeType {
  // Check for Python (CPython) symbols
  if (findGlobalExport('_PyEval_EvalFrameDefault') ||
      findGlobalExport('Py_Initialize') ||
      findGlobalExport('PyRun_SimpleString')) {
    return 'cpython';
  }

  // Check for V8 (Node.js, Chrome, etc.) symbols
  if (findGlobalExport('_ZN2v88internal7Isolate7currentEv') ||
      findGlobalExport('_ZN2v85Locker4LockEv')) {
    return 'v8';
  }

  // Check for JavaScriptCore (Safari, iOS, etc.) symbols
  if (findGlobalExport('JSGlobalContextCreate') ||
      findGlobalExport('JSEvaluateScript')) {
    return 'jsc';
  }

  // Default to native (C/C++/Rust/etc.)
  return 'native';
}

/**
 * Factory function to create the appropriate tracer for the detected runtime.
 */
function createTracer(runtime: RuntimeType, agent: any): Tracer {
  switch (runtime) {
    case 'native':
      return new NativeTracer(agent);
    case 'cpython':
      return new PythonTracer(agent);
    case 'v8':
    case 'jsc':
      throw new Error(`Runtime '${runtime}' tracer not yet implemented`);
    default:
      return new NativeTracer(agent);
  }
}

class StrobeAgent {
  private sessionId: string = '';
  private sessionStartNs: number = 0;
  private eventSeq: number = 0;
  private platform: PlatformAdapter;
  private cmoduleTracer: CModuleTracer;  // Internal CModule-based tracer
  public tracer: Tracer;                  // Public Tracer interface
  private rateTracker: RateTracker | null = null;
  private funcIdToName: Map<number, string> = new Map();
  private funcIdToAddress: Map<number, string> = new Map();  // Track funcId → address for removal
  private addressToFuncId: Map<string, number> = new Map();  // Track address → funcId for removal
  private fileLineToFuncId: Map<string, number> = new Map(); // "file:line" → funcId for interpreted hook removal

  // Phase 2: Breakpoint management
  private breakpoints: Map<string, BreakpointState> = new Map(); // id → state
  private breakpointsByAddress: Map<string, string> = new Map(); // address → id
  private pausedThreads: Map<number, string> = new Map(); // threadId → breakpointId
  private logpoints: Map<string, LogpointState> = new Map(); // id → state
  private steppingThreads: Set<number> = new Set(); // threads with active step hooks

  // Output event buffering (low-frequency, stays in JS)
  private outputBuffer: OutputEvent[] = [];
  private outputIdCounter: number = 0;
  private outputFlushInterval: number = 10; // ms
  private maxOutputBufferSize: number = 1000;

  // Re-entrancy guard for write(2) interception
  private inOutputCapture: boolean = false;

  // Last C++ exception captured by __cxa_throw hook (overwritten each throw)
  private lastException: {
    type: string;
    message: string | null;
    backtrace: BacktraceFrame[];
  } | null = null;

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
    this.cmoduleTracer = new CModuleTracer((events) => {
      send({ type: 'events', events });
    }, this.platform);

    // Create language-appropriate tracer based on runtime detection
    const runtime = detectRuntime();
    this.tracer = createTracer(runtime, this);
    send({ type: 'runtime_detected', runtime });

    this.sessionStartNs = Date.now() * 1000000;

    // For interpreted runtimes, skip native hooks that may conflict
    const isInterpreted = runtime !== 'native';

    // Periodic flush for output events
    this.outputFlushTimer = setInterval(() => this.flushOutput(), this.outputFlushInterval);

    if (!isInterpreted) {
      // Install crash/exception handler early — before process resumes —
      // so crashes are caught even if the "initialize" message hasn't arrived yet.
      this.installExceptionHandler();

      // Hook __cxa_throw to capture C++ exception type, message, and throw-site
      // backtrace before stack unwinding destroys the context.
      this.installThrowHook();

      // Intercept write(2) for stdout/stderr capture (non-fatal if it fails,
      // e.g. with ASAN-instrumented binaries where write() isn't hookable)
      try {
        this.installOutputCapture();
      } catch (e) {
        // Output capture is best-effort; function tracing still works
      }
    } else {
      send({ type: 'log', message: `Skipping native hooks for ${runtime} runtime (output captured via Device signal)` });
    }
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.sessionStartNs = Date.now() * 1000000;

    // Initialize Tracer interface
    this.tracer.initialize(sessionId);

    // Initialize CModule-specific functionality
    this.cmoduleTracer.setSessionId(sessionId);

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
    this.cmoduleTracer.setRateCheck((funcId: number) => tracker.recordCall(funcId));

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
      // Debug: log what we received
      send({ type: 'log', message: `handleMessage received: action=${message.action}, ` +
        `functions=${message.functions ? message.functions.length : 'none'}, ` +
        `targets=${message.targets ? message.targets.length : 'none'}` });

      // Set imageBase for ASLR slide computation (only needs to happen once)
      if (message.imageBase) {
        send({ type: 'log', message: `Setting imageBase=${message.imageBase}` });
        this.tracer.setImageBase(message.imageBase);
        send({ type: 'log', message: `ASLR slide computed` });
      }

      // Set serialization depth if provided (CModule-specific)
      if (message.serializationDepth) {
        this.cmoduleTracer.setSerializationDepth(message.serializationDepth);
      }

      const mode: HookMode = message.mode || 'full';
      let installed = 0;
      let failed = 0;

      if (message.action === 'add') {
        // Handle address-based targets (native)
        if (message.functions) {
          for (const func of message.functions) {
            const funcId = this.tracer.installHook({
              address: func.address,
              name: func.name,
              nameRaw: func.nameRaw,
              sourceFile: func.sourceFile,
              lineNumber: func.lineNumber,
            }, mode);
            if (funcId !== null) {
              this.funcIdToName.set(funcId, func.name);
              this.funcIdToAddress.set(funcId, func.address);
              this.addressToFuncId.set(func.address, funcId);
              installed++;
            } else {
              failed++;
            }
          }
        }

        // Handle file:line targets (interpreted)
        if (message.targets) {
          for (const target of message.targets) {
            const funcId = this.tracer.installHook({
              file: target.file,
              line: target.line,
              name: target.name,
            }, mode);
            if (funcId !== null) {
              this.funcIdToName.set(funcId, target.name);
              this.fileLineToFuncId.set(`${target.file}:${target.line}`, funcId);
              installed++;
            } else {
              failed++;
            }
          }
        }

        // For Python tracer, sync sys.settrace after batch
        if ('syncAfterBatch' in this.tracer) {
          (this.tracer as any).syncAfterBatch();
        }

        send({ type: 'log', message: `Hooks: ${installed} installed, ${failed} failed` });
      } else if (message.action === 'remove') {
        // Remove address-based hooks
        if (message.functions) {
          for (const func of message.functions) {
            // Look up funcId from address, then remove via tracer interface
            const funcId = this.addressToFuncId.get(func.address);
            if (funcId !== undefined) {
              this.tracer.removeHook(funcId);
            }
          }
        }
        // Remove file:line hooks (interpreted tracers)
        if (message.targets) {
          for (const target of message.targets) {
            const key = `${target.file}:${target.line}`;
            const funcId = this.fileLineToFuncId.get(key);
            if (funcId !== undefined) {
              this.tracer.removeHook(funcId);
              this.fileLineToFuncId.delete(key);
            }
          }
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
      this.cmoduleTracer.updateWatches(message.watches);
      if (message.exprWatches) {
        this.cmoduleTracer.updateExprWatches(message.exprWatches);
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

  private installThrowHook(): void {
    // __cxa_throw(void* thrown_exception, std::type_info* tinfo, void (*dest)(void*))
    const cxaThrow = findGlobalExport('__cxa_throw');
    if (!cxaThrow) {
      // C programs or statically linked C++ without __cxa_throw — no exception tracing
      return;
    }

    // Pre-resolve __cxa_demangle for type name demangling
    const cxaDemanglePtr = findGlobalExport('__cxa_demangle');
    let demangleFn: NativeFunction<NativePointer, [NativePointer, NativePointer, NativePointer, NativePointer]> | null = null;
    if (cxaDemanglePtr) {
      demangleFn = new NativeFunction(cxaDemanglePtr, 'pointer', ['pointer', 'pointer', 'pointer', 'pointer']);
    }

    const freePtr = findGlobalExport('free');
    let freeFn: NativeFunction<void, [NativePointer]> | null = null;
    if (freePtr) {
      freeFn = new NativeFunction(freePtr, 'void', ['pointer']);
    }

    const self = this;

    Interceptor.attach(cxaThrow, {
      onEnter(args) {
        try {
          const thrownException = args[0]; // void* — the exception object
          const tinfo = args[1];           // std::type_info*

          // 1. Read exception type name from type_info
          // Itanium ABI layout: [vtable_ptr, const char* __name]
          let exceptionType = '<unknown>';
          try {
            const namePtr = tinfo.add(Process.pointerSize).readPointer();
            const mangledName = namePtr.readCString();
            if (mangledName) {
              // Try to demangle
              if (demangleFn) {
                const mangledBuf = Memory.allocUtf8String(mangledName);
                const statusBuf = Memory.alloc(4);
                const result = demangleFn(mangledBuf, NULL, NULL, statusBuf);
                const status = statusBuf.readS32();
                if (status === 0 && !result.isNull()) {
                  exceptionType = result.readCString() ?? mangledName;
                  if (freeFn) freeFn(result);
                } else {
                  exceptionType = mangledName;
                }
              } else {
                exceptionType = mangledName;
              }
            }
          } catch (_) {
            // type_info read failed
          }

          // 2. Try to read what() for std::exception subclasses
          // Itanium ABI vtable: vptr[0] = complete dtor, vptr[1] = deleting dtor, vptr[2] = what()
          let message: string | null = null;
          try {
            const vptr = thrownException.readPointer();
            if (!vptr.isNull()) {
              const whatFnPtr = vptr.add(Process.pointerSize * 2).readPointer();
              if (!whatFnPtr.isNull()) {
                const whatNative = new NativeFunction(whatFnPtr, 'pointer', ['pointer']);
                const resultPtr = whatNative(thrownException) as NativePointer;
                if (!resultPtr.isNull()) {
                  message = resultPtr.readCString();
                }
              }
            }
          } catch (_) {
            // what() failed — expected for non-std::exception types (throw 42, etc.)
          }

          // 3. Capture throw-site backtrace
          let backtrace: BacktraceFrame[] = [];
          try {
            const frames = Thread.backtrace(this.context, Backtracer.ACCURATE);
            backtrace = frames.slice(0, 20).map((addr: NativePointer) => {
              const sym = DebugSymbol.fromAddress(addr);
              return {
                address: addr.toString(),
                moduleName: sym.moduleName,
                name: sym.name,
                fileName: sym.fileName,
                lineNumber: sym.lineNumber,
              };
            });
          } catch (_) {
            // Backtrace capture failed
          }

          // Store — subsequent throws overwrite; only the last (uncaught) one matters
          self.lastException = { type: exceptionType, message, backtrace };
        } catch (_) {
          // Never crash the target process
        }
      }
    });

    send({ type: 'log', message: 'C++ exception tracing active (__cxa_throw hooked)' });
  }

  private writeCrashFile(crashEvent: CrashEvent): void {
    try {
      const data = JSON.stringify({ type: 'events', events: [crashEvent] });
      const crashPath = `/tmp/.strobe-crash-${Process.id}.json`;
      const pathBuf = Memory.allocUtf8String(crashPath);
      const dataBuf = Memory.allocUtf8String(data);

      // Get system library exports - try main module first
      const libcModule = Process.enumerateModules().find(m => m.name.includes('libc')) || Process.mainModule;
      const openFn = new NativeFunction(
        libcModule.getExportByName('open'), 'int', ['pointer', 'int', 'int']);
      const writeFn = new NativeFunction(
        libcModule.getExportByName('write'), 'long', ['int', 'pointer', 'long']);
      const closeFn = new NativeFunction(
        libcModule.getExportByName('close'), 'int', ['int']);

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

    const crashEvent: CrashEvent = {
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

    // Enrich with C++ exception info captured by __cxa_throw hook
    if (this.lastException) {
      crashEvent.exceptionType = this.lastException.type;
      crashEvent.exceptionMessage = this.lastException.message;
      crashEvent.throwBacktrace = this.lastException.backtrace;
    }

    return crashEvent;
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

  handleNativeReadMemory(message: ReadMemoryMessage): void {
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

  handleNativeWriteMemory(message: WriteMemoryMessage): void {
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

  setNativeBreakpoint(msg: SetBreakpointMessage): void {
    // Apply ASLR slide: address from daemon is DWARF-static
    if (msg.imageBase) {
      this.tracer.setImageBase(msg.imageBase);
    }
    const slide = this.tracer.getSlide();
    const address = ptr(msg.address!).add(slide);
    const self = this;

    const listener = Interceptor.attach(address, {
      onEnter(args) {
        const bp = self.breakpoints.get(msg.id);
        if (!bp) return;

        // Suppress user breakpoints for threads that are actively stepping.
        // Without this, a step-over from a breakpoint in a loop would re-trigger
        // the original breakpoint on the next iteration before the step hook fires.
        const tid = Process.getCurrentThreadId();
        if (self.steppingThreads.has(tid)) return;

        // Evaluate condition if present
        if (bp.condition && !self.evaluateCondition(bp.condition, args, bp.id)) {
          return;
        }

        // Hit count logic: pause only on the Nth hit, skip all others
        bp.hits++;
        if (bp.hitCount > 0 && bp.hits !== bp.hitCount) {
          return;
        }

        // Notify daemon of pause
        const threadId = Process.getCurrentThreadId();
        self.pausedThreads.set(threadId, bp.id);

        // Capture return address for step-out support
        // ARM64: LR register, x86_64: [RBP+8] - Frida's returnAddress handles both
        const returnAddr = this.returnAddress;

        // Capture backtrace (same pattern as buildCrashEvent)
        let backtrace: BacktraceFrame[] = [];
        try {
          const frames = Thread.backtrace(this.context, Backtracer.ACCURATE);
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
        } catch (_) {
          // Backtrace may fail in some contexts
        }

        // Capture first 8 arguments (best-effort)
        const capturedArgs: Array<{ index: number; value: string }> = [];
        for (let i = 0; i < 8; i++) {
          try {
            capturedArgs.push({ index: i, value: args[i].toString() });
          } catch {
            break;
          }
        }

        send({
          type: 'paused',
          threadId,
          breakpointId: bp.id,
          hits: bp.hits,
          funcName: bp.funcName,
          file: bp.file,
          line: bp.line,
          returnAddress: returnAddr ? returnAddr.strip().toString() : null,
          backtrace,
          arguments: capturedArgs,
        });

        // Block this thread until resume message.
        // One-shot step hooks (if any) are installed via a separate 'installStepHooks'
        // message BEFORE this resume arrives. This avoids installing Interceptor hooks
        // inside recv callbacks, which causes send() delivery failures in Frida.
        const op = recv(`resume-${threadId}`, () => {});
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

  setNativeLogpoint(msg: SetLogpointMessage): void {
    // Apply ASLR slide: address from daemon is DWARF-static
    if (msg.imageBase) {
      this.tracer.setImageBase(msg.imageBase);
    }
    const slide = this.tracer.getSlide();
    const address = ptr(msg.address!).add(slide);

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

  removeNativeLogpoint(id: string): void {
    const lp = this.logpoints.get(id);
    if (!lp) return;

    lp.listener.detach();
    this.logpoints.delete(id);

    send({ type: 'logpointRemoved', id });
  }

  /** Called by PythonTracer's bpHitCallback when a Python breakpoint is reached. */
  emitBreakpointHit(id: string, line: number): void {
    send({
      type: 'paused',
      threadId: Process.getCurrentThreadId(),
      breakpointId: id,
      hits: 1,
      funcName: null,
      lineNumber: line,
    });
  }

  removeNativeBreakpoint(id: string): void {
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

  /**
   * Install one-shot step breakpoints at specified addresses.
   * Called from a top-level recv handler (NOT from inside another recv callback)
   * to avoid Frida's send() delivery issues with nested Interceptor contexts.
   *
   * When a step hook fires, it sends a 'paused' notification and blocks with
   * a simple recv('resume-{tid}').wait(). The daemon will send another
   * 'installStepHooks' message before 'resume-{tid}' if further stepping is needed.
   */
  installNativeStepHooks(msg: InstallStepHooksMessage): void {
    if (!msg.oneShot || msg.oneShot.length === 0) return;

    const threadId = msg.threadId;
    // Carried-forward return address from the original breakpoint (for step-out)
    const carriedReturnAddress = msg.returnAddress || null;

    // Compute ASLR slide for DWARF-static addresses
    if (msg.imageBase) {
      this.tracer.setImageBase(msg.imageBase);
    }
    const stepSlide = this.tracer.getSlide();
    const self = this;

    // Mark this thread as stepping — suppresses user breakpoints until step completes.
    this.steppingThreads.add(threadId);

    const stepId = `step-${threadId}-${Date.now()}`;
    const listeners: InvocationListener[] = [];
    let fired = false;
    let timeoutId: ReturnType<typeof setTimeout> | null = null;

    const cleanupAll = () => {
      if (fired) return;
      fired = true;
      self.steppingThreads.delete(threadId);
      if (timeoutId !== null) {
        clearTimeout(timeoutId);
        timeoutId = null;
      }
      for (const l of listeners) {
        l.detach();
      }
    };

    for (const entry of msg.oneShot) {
      // noSlide=true for runtime addresses (e.g., return address)
      const addr = entry.noSlide ? ptr(entry.address) : ptr(entry.address).add(stepSlide);
      try {
        const stepListener = Interceptor.attach(addr, {
          onEnter() {
            cleanupAll();

            const tid = Process.getCurrentThreadId();
            self.pausedThreads.set(tid, stepId);

            // For noSlide entries (return addresses), convert runtime → DWARF-static
            // so the daemon can compute next-line for further stepping.
            const dwarfAddr = entry.noSlide
              ? addr.sub(stepSlide).toString()
              : entry.address;

            send({
              type: 'paused',
              threadId: tid,
              breakpointId: stepId,
              funcName: null,
              file: null,
              line: null,
              // Use carried-forward return address from the original breakpoint.
              // Step hooks can't reliably capture return addresses because after
              // recv().wait() unblocks, Frida's trampoline is on the stack.
              returnAddress: carriedReturnAddress,
              address: dwarfAddr,
            });

            // Block until resume. Step hooks for the next step (if any) will be
            // installed via a separate 'installStepHooks' message before this
            // resume arrives, keeping hook installation at top-level context.
            const op = recv(`resume-${tid}`, () => {});
            op.wait();

            self.pausedThreads.delete(tid);
          },
        });
        listeners.push(stepListener);
      } catch (e: any) {
        // Interceptor.attach can fail on addresses inside Frida trampolines
        // (e.g., return address captured from a previous step hook).
        // This is expected — just skip this address and rely on other hooks.
        send({ type: 'log', message: `installStepHooks: skipping ${addr} (${e.message})` });
      }
    }

    // Safety timeout: clean up one-shot hooks after 30s if none fired
    timeoutId = setTimeout(() => {
      if (!fired) {
        cleanupAll();
        send({ type: 'log', message: `One-shot step hooks timed out for thread ${threadId}` });
      }
    }, 30000);
  }

  // ========== Public wrapper methods for NativeTracer delegation ==========

  /**
   * Wrapper for NativeTracer to install hooks via CModuleTracer.
   * Returns funcId on success, null on failure.
   */
  public installNativeHook(target: ResolvedTarget, mode: HookMode): number | null {
    return this.cmoduleTracer.installHook(target, mode);
  }

  /**
   * Wrapper for NativeTracer to remove hooks via CModuleTracer.
   * Takes funcId, looks up address, and removes the hook.
   */
  public removeNativeHook(funcId: number): void {
    const address = this.funcIdToAddress.get(funcId);
    if (address) {
      this.cmoduleTracer?.removeHook(address);
      this.funcIdToAddress.delete(funcId);
      this.addressToFuncId.delete(address);
      this.funcIdToName.delete(funcId);
    }
  }

  /**
   * Wrapper for NativeTracer to remove all hooks via CModuleTracer.
   */
  public removeAllNativeHooks(): void {
    // Iterate over all funcIds and remove them
    const funcIds = Array.from(this.funcIdToAddress.keys());
    for (const funcId of funcIds) {
      this.removeNativeHook(funcId);
    }
  }

  /**
   * Wrapper for NativeTracer to get active hook count.
   */
  public getActiveHookCount(): number {
    return this.cmoduleTracer.activeHookCount();
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
let agent: StrobeAgent;
try {
  agent = new StrobeAgent();
} catch (e: any) {
  send({ type: 'log', message: 'StrobeAgent constructor CRASHED: ' + e.message + '\n' + e.stack });
  throw e;
}

// Immediate startup message
send({ type: 'agent_loaded', message: 'Strobe agent loaded and ready' });

// Message handler
recv('initialize', (message: { sessionId: string }) => {
  send({ type: 'log', message: 'Received initialize: ' + JSON.stringify(message) });
  try {
    agent.initialize(message.sessionId);
    send({ type: 'log', message: 'Initialize completed successfully' });
  } catch (e: any) {
    send({ type: 'log', message: 'Initialize CRASHED: ' + e.message + '\n' + e.stack });
  }
});

// Frida's recv() is one-shot: must re-register before processing to avoid
// losing messages sent during processing. Message ordering is guaranteed
// by Frida's single-threaded JS execution model. If handleMessage() throws,
// the re-registration has already happened so subsequent messages are safe.
function onHooksMessage(message: HookInstruction): void {
  // Re-register BEFORE processing — recv() is one-shot in Frida.
  // Without this, only the first hooks message is ever received.
  recv('hooks', onHooksMessage);
  const count = (message.functions?.length || 0) + (message.targets?.length || 0);
  send({ type: 'log', message: `Received hooks: action=${message.action} count=${count} imageBase=${message.imageBase}` });
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
  // Delegate to tracer if it implements handleReadMemory (optional method)
  if (agent.tracer.handleReadMemory) {
    agent.tracer.handleReadMemory(message);
  }
}
recv('read_memory', onReadMemoryMessage);

// Phase 2a: Write memory message handler
function onWriteMemoryMessage(message: WriteMemoryMessage): void {
  recv('write_memory', onWriteMemoryMessage);
  // Delegate to tracer if it implements handleWriteMemory (optional method)
  if (agent.tracer.handleWriteMemory) {
    agent.tracer.handleWriteMemory(message);
  }
}
recv('write_memory', onWriteMemoryMessage);

// Phase 2: Breakpoint message handlers
function onSetBreakpointMessage(message: SetBreakpointMessage): void {
  recv('setBreakpoint', onSetBreakpointMessage);
  agent.tracer.installBreakpoint(message);
}
recv('setBreakpoint', onSetBreakpointMessage);

function onRemoveBreakpointMessage(message: RemoveBreakpointMessage): void {
  recv('removeBreakpoint', onRemoveBreakpointMessage);
  agent.tracer.removeBreakpoint(message.id);
}
recv('removeBreakpoint', onRemoveBreakpointMessage);

function onSetLogpointMessage(message: SetLogpointMessage): void {
  recv('setLogpoint', onSetLogpointMessage);
  agent.tracer.installLogpoint(message);
}
recv('setLogpoint', onSetLogpointMessage);

function onRemoveLogpointMessage(message: RemoveBreakpointMessage): void {
  recv('removeLogpoint', onRemoveLogpointMessage);
  agent.tracer.removeLogpoint(message.id);
}
recv('removeLogpoint', onRemoveLogpointMessage);

// Phase 2: Step hook installation (sent as separate message before resume)
function onInstallStepHooksMessage(message: InstallStepHooksMessage): void {
  recv('installStepHooks', onInstallStepHooksMessage);
  agent.tracer.installStepHooks(message);
}
recv('installStepHooks', onInstallStepHooksMessage);

// Eval variable message handler for interpreted languages
function onEvalVariableMessage(message: { expr: string; label?: string }): void {
  recv('eval_variable', onEvalVariableMessage);
  try {
    const value = agent.tracer.readVariable(message.expr);
    // Python eval errors come back as {error: "..."} — propagate as error field
    if (value && typeof value === 'object' && 'error' in value && Object.keys(value).length === 1) {
      send({ type: 'eval_response', label: message.label || message.expr, error: value.error });
    } else {
      send({ type: 'eval_response', label: message.label || message.expr, value });
    }
  } catch (e: any) {
    send({ type: 'eval_response', label: message.label || message.expr, error: e.message });
  }
}
recv('eval_variable', onEvalVariableMessage);

// Runtime resolve message handler for agent-side resolution fallback
function onResolveMessage(message: { patterns: string[] }): void {
  recv('resolve', onResolveMessage);
  if (agent.tracer.resolvePattern) {
    const targets: TracerResolvedTarget[] = [];
    for (const pattern of message.patterns) {
      targets.push(...agent.tracer.resolvePattern(pattern));
    }
    send({ type: 'resolved', targets });
  } else {
    send({ type: 'resolved', targets: [] });
  }
}
recv('resolve', onResolveMessage);

// Python breakpoint resume: daemon sends this to unblock a suspended Python thread
function onResumePythonBp(_message: {}): void {
  recv('resume_python_bp', onResumePythonBp);
  if ('resumePythonBreakpoint' in agent.tracer) {
    (agent.tracer as any).resumePythonBreakpoint();
  }
  // Signal back so SetBreakpoint command handler doesn't timeout waiting for confirmation
  send({ type: 'breakpointSet', id: 'resume', activeCount: 0 });
}
recv('resume_python_bp', onResumePythonBp);

// Frida calls rpc.exports.dispose() before script unload — ensures all
// buffered trace events (CModule ring buffer) and output events are flushed.
rpc.exports = {
  dispose() {
    agent.dispose();
  }
};

// Export for potential direct usage
(globalThis as any).strobeAgent = agent;
