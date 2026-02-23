// agent/src/tracers/jsc-tracer.ts
// JSC tracer for Bun — hooks JSObjectCallAsFunction at native level.
// Runs in QuickJS (standard Frida runtime) since Bun doesn't use Frida's V8 runtime.
//
// Multi-hook attribution: uses public JSC C API (JSObjectGetProperty) to read
// the .name property of JSObjectRef, then matches against registered hooks.
// Falls back to single-hook-only behavior if JSC API symbols aren't found.

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage,
         StepHooksMessage, LogpointMessage, TracerCapabilities } from './tracer.js';
import { findGlobalExport } from '../utils.js';

interface JscHook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
}

export class JscTracer implements Tracer {
  private agent: any;
  private hooks: Map<number, JscHook> = new Map();
  private hooksByName: Map<string, JscHook> = new Map(); // name → hook for fast lookup
  private nextFuncId: number = 1;
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private interceptor: InvocationListener | null = null;

  // JSC C API function pointers for reading function .name
  private jscApiAvailable: boolean = false;
  private JSStringCreateWithUTF8CString: NativeFunction<NativePointer, [NativePointer]> | null = null;
  private JSObjectGetProperty: NativeFunction<NativePointer, [NativePointer, NativePointer, NativePointer, NativePointer]> | null = null;
  private JSValueToStringCopy: NativeFunction<NativePointer, [NativePointer, NativePointer, NativePointer]> | null = null;
  private JSStringGetMaximumUTF8CStringSize: NativeFunction<number, [NativePointer]> | null = null;
  private JSStringGetUTF8CString: NativeFunction<number, [NativePointer, NativePointer, number]> | null = null;
  private JSStringRelease: NativeFunction<void, [NativePointer]> | null = null;

  // Cache: JSObjectRef pointer string → function name (avoids repeated API calls)
  private fnNameCache: Map<string, string> = new Map();

  // Whether JSObjectCallAsFunction was found (set during initialize)
  private hookTargetFound: boolean = false;

  constructor(agent: any) { this.agent = agent; }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Resolve JSC C API exports for function name resolution
    this.resolveJscApi();

    // Hook JSObjectCallAsFunction — called for every JS function call via C API
    // Signature: JSValueRef JSObjectCallAsFunction(JSContextRef, JSObjectRef fn,
    //             JSObjectRef thisObj, size_t argc, JSValueRef* argv, JSValueRef* exception)
    const hookTarget = findGlobalExport('JSObjectCallAsFunction');
    if (!hookTarget) {
      send({ type: 'log', message: 'JscTracer: JSObjectCallAsFunction not found — tracing unavailable' });
      return;
    }
    this.hookTargetFound = true;

    const self = this;
    this.interceptor = Interceptor.attach(hookTarget, {
      onEnter(args) {
        if (self.hooks.size === 0) return; // fast-path when no hooks
        const ctx = args[0];
        const fnPtr = args[1];
        (this as any)._strobeCtx = ctx;
        (this as any)._strobeFnPtr = fnPtr;
        self.tryEmitForJscFunction(ctx, fnPtr, 'entry');
      },
      onLeave(_retval) {
        if (self.hooks.size === 0) return; // fast-path
        const ctx = (this as any)._strobeCtx;
        const fnPtr = (this as any)._strobeFnPtr;
        if (fnPtr) self.tryEmitForJscFunction(ctx, fnPtr, 'exit');
      }
    });

    send({ type: 'log', message: `JscTracer: hooked JSObjectCallAsFunction (multi-hook: ${this.jscApiAvailable})` });
  }

  private resolveJscApi(): void {
    const syms = {
      JSStringCreateWithUTF8CString: findGlobalExport('JSStringCreateWithUTF8CString'),
      JSObjectGetProperty: findGlobalExport('JSObjectGetProperty'),
      JSValueToStringCopy: findGlobalExport('JSValueToStringCopy'),
      JSStringGetMaximumUTF8CStringSize: findGlobalExport('JSStringGetMaximumUTF8CStringSize'),
      JSStringGetUTF8CString: findGlobalExport('JSStringGetUTF8CString'),
      JSStringRelease: findGlobalExport('JSStringRelease'),
    };

    if (Object.values(syms).every(s => s !== null)) {
      this.JSStringCreateWithUTF8CString = new NativeFunction(syms.JSStringCreateWithUTF8CString!, 'pointer', ['pointer']);
      this.JSObjectGetProperty = new NativeFunction(syms.JSObjectGetProperty!, 'pointer', ['pointer', 'pointer', 'pointer', 'pointer']);
      this.JSValueToStringCopy = new NativeFunction(syms.JSValueToStringCopy!, 'pointer', ['pointer', 'pointer', 'pointer']);
      this.JSStringGetMaximumUTF8CStringSize = new NativeFunction(syms.JSStringGetMaximumUTF8CStringSize!, 'int', ['pointer']);
      this.JSStringGetUTF8CString = new NativeFunction(syms.JSStringGetUTF8CString!, 'int', ['pointer', 'pointer', 'int']);
      this.JSStringRelease = new NativeFunction(syms.JSStringRelease!, 'void', ['pointer']);
      this.jscApiAvailable = true;
    } else {
      const missing = Object.entries(syms).filter(([, v]) => v === null).map(([k]) => k);
      send({ type: 'log', message: `JscTracer: JSC API partially missing (${missing.join(', ')}) — single-hook only` });
    }
  }

  dispose(): void {
    if (this.interceptor) { this.interceptor.detach(); this.interceptor = null; }
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null; }
    this.flushEvents();
    this.hooks.clear();
    this.hooksByName.clear();
    this.fnNameCache.clear();
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;
    const hook: JscHook = { funcId, target, mode };
    this.hooks.set(funcId, hook);
    if (target.name) {
      this.hooksByName.set(target.name, hook);
    }
    return funcId;
  }

  removeHook(id: number): void {
    const hook = this.hooks.get(id);
    if (hook?.target.name) this.hooksByName.delete(hook.target.name);
    this.hooks.delete(id);
  }

  removeAllHooks(): void {
    this.hooks.clear();
    this.hooksByName.clear();
    this.fnNameCache.clear();
  }

  activeHookCount(): number { return this.hooks.size; }

  installBreakpoint(_msg: BreakpointMessage): void {}
  removeBreakpoint(_id: string): void {}
  installStepHooks(_msg: StepHooksMessage): void {}
  installLogpoint(_msg: LogpointMessage): void {}
  removeLogpoint(_id: string): void {}
  readVariable(_expr: string): any { return null; }
  writeVariable(_expr: string, _value: any): void {}
  setImageBase(_base: string): void {}
  getSlide(): NativePointer { return ptr(0); }

  /**
   * Read the .name property of a JSObjectRef via public JSC C API.
   * Returns the function name or null if it can't be read.
   */
  private getFunctionName(ctx: NativePointer, fnObj: NativePointer): string | null {
    if (!this.jscApiAvailable) return null;

    // Check cache first
    const ptrKey = fnObj.toString();
    const cached = this.fnNameCache.get(ptrKey);
    if (cached !== undefined) return cached;

    try {
      // JSStringRef nameStr = JSStringCreateWithUTF8CString("name")
      const nameBuf = Memory.allocUtf8String('name');
      const nameJsStr = this.JSStringCreateWithUTF8CString!(nameBuf);
      if (nameJsStr.isNull()) return null;

      try {
        // JSValueRef val = JSObjectGetProperty(ctx, fnObj, nameStr, &exception)
        const exception = Memory.alloc(Process.pointerSize);
        exception.writePointer(ptr(0));
        const val = this.JSObjectGetProperty!(ctx, fnObj, nameJsStr, exception);

        if (val.isNull() || !exception.readPointer().isNull()) return null;

        // JSStringRef valStr = JSValueToStringCopy(ctx, val, &exception)
        exception.writePointer(ptr(0));
        const valStr = this.JSValueToStringCopy!(ctx, val, exception);
        if (valStr.isNull()) return null;

        try {
          // Read UTF-8 from JSStringRef
          const maxSize = this.JSStringGetMaximumUTF8CStringSize!(valStr);
          if (maxSize <= 0 || maxSize > 4096) return null;
          const buf = Memory.alloc(maxSize);
          this.JSStringGetUTF8CString!(valStr, buf, maxSize);
          const name = buf.readUtf8String() || '';

          // Cache result (limit cache size to avoid unbounded growth)
          if (this.fnNameCache.size > 10000) this.fnNameCache.clear();
          this.fnNameCache.set(ptrKey, name);
          return name;
        } finally {
          this.JSStringRelease!(valStr);
        }
      } finally {
        this.JSStringRelease!(nameJsStr);
      }
    } catch {
      return null;
    }
  }

  private tryEmitForJscFunction(ctx: NativePointer, fnPtr: NativePointer, event: 'entry' | 'exit'): void {
    let hook: JscHook | undefined;

    if (this.hooks.size === 1) {
      // Single hook: fast path — no name lookup needed, attribution is unambiguous
      hook = this.hooks.values().next().value;
    } else if (this.jscApiAvailable) {
      // Multi-hook: resolve function name via JSC C API and match against hooks
      const name = this.getFunctionName(ctx, fnPtr);
      if (name) {
        hook = this.hooksByName.get(name);
      }
    }
    // If no API or name not resolved, skip (can't attribute)

    if (!hook) return;

    this.eventBuffer.push({
      id: `${this.sessionId}-jsc-${++this.eventIdCounter}`,
      sessionId: this.sessionId,
      timestampNs: Date.now() * 1_000_000,
      threadId: Process.getCurrentThreadId(),
      eventType: event === 'entry' ? 'function_enter' : 'function_exit',
      functionName: hook.target.name,
      sourceFile: hook.target.file,
      lineNumber: hook.target.line,
      pid: Process.id,
    });
    if (this.eventBuffer.length >= 50) this.flushEvents();
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }

  resolvePattern(_pattern: string): ResolvedTarget[] {
    return [];
  }

  getCapabilities(): TracerCapabilities {
    if (!this.hookTargetFound) {
      return {
        functionTracing: false,
        breakpoints: false,
        stepping: false,
        runtimeDetail: 'Bun (JSC, symbols stripped)',
        limitations: [
          "Bun's release binary strips all JSC symbols, which disables function tracing, breakpoints, and stepping. " +
          "To get full Strobe instrumentation, build Bun from source in debug mode: " +
          "git clone https://github.com/oven-sh/bun && cd bun && bun run build — " +
          "then use ./build/debug/bun-debug instead of bun. " +
          "Debug builds preserve JSC symbols needed for function tracing.",
        ],
      };
    }

    if (this.jscApiAvailable) {
      return {
        functionTracing: true,
        breakpoints: false,
        stepping: false,
        runtimeDetail: 'Bun (JSC, multi-hook)',
        limitations: [
          "Breakpoints and stepping are not yet supported for Bun/JSC. " +
          "Use function tracing via debug_trace and logpoints via debug_breakpoint with a 'message' field.",
        ],
      };
    }

    return {
      functionTracing: true,
      breakpoints: false,
      stepping: false,
      runtimeDetail: 'Bun (JSC, single-hook only)',
      limitations: [
        "JSC C API symbols missing — only one function can be traced at a time. " +
        "For multi-function tracing, build Bun from source in debug mode: " +
        "git clone https://github.com/oven-sh/bun && cd bun && bun run build — " +
        "then use ./build/debug/bun-debug instead of bun.",
        "Breakpoints and stepping are not yet supported for Bun/JSC. " +
        "Use function tracing via debug_trace and logpoints via debug_breakpoint with a 'message' field.",
      ],
    };
  }
}
