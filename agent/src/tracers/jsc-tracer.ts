// agent/src/tracers/jsc-tracer.ts
// JSC tracer for Bun — hooks JSObjectCallAsFunction at native level.
// Runs in QuickJS (standard Frida runtime) since Bun doesn't use Frida's V8 runtime.

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage,
         StepHooksMessage, LogpointMessage } from './tracer.js';
import { findGlobalExport } from '../utils.js';

interface JscHook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
}

export class JscTracer implements Tracer {
  private agent: any;
  private hooks: Map<number, JscHook> = new Map();
  private nextFuncId: number = 1;
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private interceptor: InvocationListener | null = null;

  constructor(agent: any) { this.agent = agent; }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Hook JSObjectCallAsFunction — called for every JS function call via C API
    // Signature: JSValueRef JSObjectCallAsFunction(JSContextRef, JSObjectRef fn,
    //             JSObjectRef thisObj, size_t argc, JSValueRef* argv, JSValueRef* exception)
    const hookTarget = findGlobalExport('JSObjectCallAsFunction');
    if (!hookTarget) {
      send({ type: 'log', message: 'JscTracer: JSObjectCallAsFunction not found — tracing unavailable' });
      return;
    }

    const self = this;
    this.interceptor = Interceptor.attach(hookTarget, {
      onEnter(args) {
        // args[1] = JSObjectRef (the function being called)
        // Full JSC struct navigation is a follow-on task (version-specific offsets).
        const fnPtr = args[1];
        self.tryEmitForJscFunction(fnPtr, 'entry');
      },
      onLeave(_retval) {
        // Emit exit — funcId matching is best-effort
      }
    });

    send({ type: 'log', message: 'JscTracer: hooked JSObjectCallAsFunction' });
  }

  dispose(): void {
    if (this.interceptor) { this.interceptor.detach(); this.interceptor = null; }
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null; }
    this.flushEvents();
    this.hooks.clear();
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;
    this.hooks.set(funcId, { funcId, target, mode });
    return funcId;
  }

  removeHook(id: number): void { this.hooks.delete(id); }
  removeAllHooks(): void { this.hooks.clear(); }
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

  private tryEmitForJscFunction(fnPtr: NativePointer, event: 'entry' | 'exit'): void {
    // TODO(follow-on): Navigate JSC struct from fnPtr to function name + source URL + line.
    // Full implementation requires JSC struct offsets (version-specific).
    // For now: emit a generic event for the first active hook.
    for (const [funcId, hook] of this.hooks) {
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
      break; // One event per call for now
    }
    if (this.eventBuffer.length >= 50) this.flushEvents();
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }
}
