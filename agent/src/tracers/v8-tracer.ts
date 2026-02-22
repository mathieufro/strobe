// agent/src/tracers/v8-tracer.ts
// V8 runtime tracer — runs INSIDE Node.js's own V8 context.
// Requires Frida script runtime = V8 (set by spawner.rs for JS sessions).

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage,
         StepHooksMessage, LogpointMessage } from './tracer.js';

interface V8Hook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
}

// These globals are available because we're running in Node.js's V8 context
declare const require: any;
declare const process: { version: string; pid: number };

export class V8Tracer implements Tracer {
  private agent: any;
  private hooks: Map<number, V8Hook> = new Map();
  private nextFuncId: number = 1;
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  // Track wrapped functions to avoid double-wrapping
  private wrappedFns: WeakSet<Function> = new WeakSet();
  private origCompile: Function | null = null;
  private esmHooksRegistered: boolean = false;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Patch Module._compile to intercept newly-loaded modules
    try {
      const Module = require('module') as any;
      const self = this;
      const original = Module.prototype._compile;
      this.origCompile = original;

      Module.prototype._compile = function(content: string, filename: string) {
        const result = original.call(this, content, filename);
        // After module is compiled and exports are populated, wrap matching functions
        try { self.wrapModuleExports(this.exports, filename); } catch {}
        return result;
      };
    } catch (e) {
      send({ type: 'log', message: `V8Tracer: failed to patch Module._compile: ${e}` });
    }

    // Install global trace bridge for ESM hooks.
    // ESM hook scripts call globalThis.__strobe_trace() which dispatches to Strobe events.
    try {
      const self2 = this;

      // Pattern sharing: hook script reads this to know which functions to instrument.
      // Updated by installHook() whenever patterns change.
      (globalThis as any).__strobe_hooks = [];

      (globalThis as any).__strobe_trace = function(
        event: string, funcName: string, file: string, line: number
      ) {
        const cleanFile = file.startsWith('file://') ? file.slice(7) : file;
        // Match against active hooks — require BOTH name and file to match
        for (const [, hook] of self2.hooks) {
          const nameMatch = hook.target.name === funcName;
          const fileMatch = hook.target.file && cleanFile.endsWith(hook.target.file);
          if (nameMatch && fileMatch) {
            if (event === 'enter') {
              self2.emitEvent(hook.funcId, hook, cleanFile, 'entry');
            } else if (event === 'exit') {
              self2.emitEvent(hook.funcId, hook, cleanFile, 'exit');
            }
            return;
          }
          // Fall back to name-only match if no file context in the hook
          if (nameMatch && !hook.target.file) {
            if (event === 'enter') {
              self2.emitEvent(hook.funcId, hook, cleanFile, 'entry');
            }
            return;
          }
        }
      };
    } catch (e) {
      send({ type: 'log', message: `V8Tracer: failed to install __strobe_trace: ${e}` });
    }

    // Register ESM module hooks for dynamic import() interception
    this.registerEsmHooks();

    send({ type: 'log', message: `V8Tracer: initialized (V8 runtime, Node.js ${process.version})` });
  }

  dispose(): void {
    // Restore original _compile
    if (this.origCompile) {
      try {
        const Module = require('module') as any;
        Module.prototype._compile = this.origCompile;
      } catch {}
      this.origCompile = null;
    }
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null; }
    this.flushEvents();
    this.hooks.clear();
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;
    this.hooks.set(funcId, { funcId, target, mode });

    // Sync patterns to globalThis for ESM hooks to read
    try {
      (globalThis as any).__strobe_hooks = Array.from(this.hooks.values()).map(h => ({
        name: h.target.name,
        file: h.target.file || '',
        line: h.target.line || 0,
      }));
    } catch {}

    // Immediately wrap already-loaded modules that match
    try {
      const cache = (require as any).cache ?? {};
      for (const [id, mod] of Object.entries(cache) as any[]) {
        if (mod?.exports && this.fileMatches(id, target)) {
          this.wrapModuleExports(mod.exports, id);
        }
      }
    } catch {}

    return funcId;
  }

  removeHook(id: number): void {
    // Hooks are effectively removed by checking this.hooks in the wrapper
    this.hooks.delete(id);
  }

  removeAllHooks(): void { this.hooks.clear(); }
  activeHookCount(): number { return this.hooks.size; }

  installBreakpoint(_msg: BreakpointMessage): void { /* Phase 2: use V8 Inspector CDP */ }
  removeBreakpoint(_id: string): void {}
  installStepHooks(_msg: StepHooksMessage): void {}
  installLogpoint(_msg: LogpointMessage): void {}
  removeLogpoint(_id: string): void {}

  readVariable(expr: string): any {
    // Running in V8 context — can eval globals directly
    try {
      // indirect eval = global scope
      const value = (0, eval)(expr);
      return JSON.parse(JSON.stringify(value, null, 0));
    } catch {}
    // Fall back: search module exports
    try {
      const cache = (require as any).cache ?? {};
      for (const mod of Object.values(cache) as any[]) {
        if (mod?.exports?.[expr] !== undefined) {
          return JSON.parse(JSON.stringify(mod.exports[expr]));
        }
      }
    } catch {}
    return { error: `Cannot access '${expr}' — not in global scope or module exports` };
  }

  writeVariable(expr: string, value: any): void {
    // Validate expr is a simple assignment target (variable name, attribute access, subscript)
    // to prevent code injection via expr
    if (!/^[a-zA-Z_$]\w*(?:\.[a-zA-Z_$]\w*|\[\d+\]|\[['"][^'"]*['"]\])*$/.test(expr)) {
      throw new Error(`Invalid write target: ${expr}`);
    }
    try {
      new Function('__v', `${expr} = __v`)(value);
    } catch (e) {
      throw new Error(`Failed to write '${expr}': ${e}`);
    }
  }

  setImageBase(_base: string): void {}
  getSlide(): NativePointer { return ptr(0); }

  // ── Private helpers ─────────────────────────────────────────────────

  // Called from initialize(), not installHook().
  // Registers module.registerHooks() for intercepting future ESM import() calls.
  // Only intercepts future loads — static imports that are already loaded are unreachable.
  // The spawn-time --import hook covers the static import case.
  private registerEsmHooks(): void {
    if (this.esmHooksRegistered) return;

    try {
      const mod = require('node:module') as any;
      if (typeof mod.registerHooks === 'function') {
        mod.registerHooks({
          load(url: string, context: any, nextLoad: Function) {
            // Transform ESM source to inject __strobe_trace calls at function entry
            const transformResult = (result: any) => {
              // Only transform user ESM code (not node_modules, not node: builtins)
              if (result.format === 'module' && result.source &&
                  !url.includes('node_modules') && !url.startsWith('node:')) {
                const source = typeof result.source === 'string'
                  ? result.source
                  : new ((globalThis as any).TextDecoder)().decode(result.source);
                const fnRegex = /^(\s*export\s+(?:default\s+)?(?:async\s+)?function\s+)(\w+)\s*\(([^)]*)\)\s*\{/gm;
                let transformed = source;
                const safeUrl = url.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
                let m;
                while ((m = fnRegex.exec(source)) !== null) {
                  const [full, prefix, name, params] = m;
                  const safeName = name.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
                  transformed = transformed.replace(full,
                    `${prefix}${name}(${params}) {\n` +
                    `  if (typeof globalThis.__strobe_trace === 'function') ` +
                    `globalThis.__strobe_trace('enter', '${safeName}', '${safeUrl}', 0);`);
                }
                return { ...result, source: transformed };
              }
              return result;
            };

            // nextLoad may return a Promise in some Node versions — handle both
            const resultOrPromise = nextLoad(url, context);
            if (resultOrPromise && typeof resultOrPromise.then === 'function') {
              return resultOrPromise.then(transformResult);
            }
            return transformResult(resultOrPromise);
          }
        });
        this.esmHooksRegistered = true;
        send({ type: 'log', message: 'V8Tracer: ESM hooks registered via module.registerHooks()' });
      }
    } catch (e) {
      send({ type: 'log', message: `V8Tracer: ESM hook registration not available: ${e}` });
    }
  }

  private fileMatches(filename: string, target: ResolvedTarget): boolean {
    if (!target.file) return false;
    // Match by file suffix (target.file may be relative)
    return filename.endsWith(target.file) ||
           filename.endsWith(target.file.replace(/\.[^.]+$/, '.js')); // .ts → .js
  }

  private wrapModuleExports(exports: any, filename: string): void {
    if (!exports || typeof exports !== 'object' && typeof exports !== 'function') return;
    this.wrapObject(exports, filename, '');
  }

  private wrapObject(obj: any, filename: string, prefix: string): void {
    if (!obj) return;
    const seen = new Set<any>();

    const processKey = (container: any, key: string, depth: number, currentPrefix: string) => {
      if (depth > 3) return; // Limit recursion
      const val = container[key];
      if (typeof val === 'function' && !this.wrappedFns.has(val)) {
        const qualifiedName = currentPrefix ? `${currentPrefix}.${key}` : key;

        // Find matching hook
        let matchedHook: V8Hook | null = null;
        for (const [, hook] of this.hooks) {
          if (!this.fileMatches(filename, hook.target)) continue;
          if (hook.target.name === qualifiedName || hook.target.name === key) {
            matchedHook = hook;
            break;
          }
        }

        if (matchedHook) {
          const hook = matchedHook;
          const self = this;
          const wrapped = new Proxy(val, {
            apply(target, thisArg, args) {
              self.emitEvent(hook.funcId, hook, filename, 'entry');
              let result: any;
              try {
                result = Reflect.apply(target, thisArg, args);
              } catch (e) {
                self.emitEvent(hook.funcId, hook, filename, 'exit');
                throw e;
              }
              // Handle async functions
              if (result && typeof result.then === 'function') {
                return result.then((v: any) => {
                  self.emitEvent(hook.funcId, hook, filename, 'exit');
                  return v;
                }, (e: any) => {
                  self.emitEvent(hook.funcId, hook, filename, 'exit');
                  throw e;
                });
              }
              self.emitEvent(hook.funcId, hook, filename, 'exit');
              return result;
            }
          });
          this.wrappedFns.add(val); // Mark original as wrapped
          try { container[key] = wrapped; } catch {} // May be non-writable
        }
      }

      // Recurse into plain objects (e.g. class instances, namespace objects)
      if (typeof val === 'object' && val !== null && !seen.has(val)) {
        seen.add(val);
        const childPrefix = currentPrefix ? `${currentPrefix}.${key}` : key;
        for (const k of Object.keys(val)) {
          processKey(val, k, depth + 1, childPrefix);
        }
      }
    };

    for (const key of Object.keys(obj)) {
      processKey(obj, key, 0, prefix);
    }
    // Also wrap prototype methods for classes
    if (typeof obj === 'function' && obj.prototype) {
      for (const key of Object.getOwnPropertyNames(obj.prototype)) {
        if (key !== 'constructor') processKey(obj.prototype, key, 0, prefix);
      }
    }
  }

  private emitEvent(funcId: number, hook: V8Hook, filename: string, event: 'entry' | 'exit'): void {
    this.eventBuffer.push({
      id: `${this.sessionId}-v8-${++this.eventIdCounter}`,
      sessionId: this.sessionId,
      timestampNs: Date.now() * 1_000_000,
      threadId: 0, // Node.js is single-threaded for JS (worker_threads aside)
      eventType: event === 'entry' ? 'function_enter' : 'function_exit',
      functionName: hook.target.name,
      sourceFile: filename,
      lineNumber: hook.target.line,
      pid: process.pid,
    });
    if (this.eventBuffer.length >= 50) this.flushEvents();
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }
}
