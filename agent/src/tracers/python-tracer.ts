// agent/src/tracers/python-tracer.ts
// Python tracer using sys.settrace + NativeCallback (version-independent)

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage, ReadMemoryMessage, WriteMemoryMessage } from './tracer.js';
import { findGlobalExport } from '../utils.js';

interface PythonHook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
  file: string;
  line: number;
}

interface PythonBreakpoint {
  id: string;
  file: string;
  line: number;
  condition?: string;
  hitCount?: number;
  currentHits: number;
}

interface PythonLogpoint {
  id: string;
  file: string;
  line: number;
  message: string;
  condition?: string;
}

interface StepState {
  active: boolean;
  threadId: number;
  targetLine?: number;
  targetFile?: string;
}

export class PythonTracer implements Tracer {
  private agent: any;
  private hooks: Map<number, PythonHook> = new Map();
  private breakpoints: Map<string, PythonBreakpoint> = new Map();
  private logpoints: Map<string, PythonLogpoint> = new Map();
  private nextFuncId: number = 1;
  private stepState: StepState = { active: false, threadId: 0 };
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private traceCallback: NativePointer | null = null;
  private traceInstalled: boolean = false;

  // CPython API function pointers
  private PyRun_SimpleString: NativePointer | null = null;
  private PyGILState_Ensure: NativePointer | null = null;
  private PyGILState_Release: NativePointer | null = null;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;

    // Find CPython symbols needed for sys.settrace approach
    this.PyRun_SimpleString = findGlobalExport('PyRun_SimpleString');
    this.PyGILState_Ensure = findGlobalExport('PyGILState_Ensure');
    this.PyGILState_Release = findGlobalExport('PyGILState_Release');

    if (!this.PyRun_SimpleString || !this.PyGILState_Ensure || !this.PyGILState_Release) {
      throw new Error('CPython API symbols not found (PyRun_SimpleString, PyGILState_Ensure, PyGILState_Release)');
    }

    // Start periodic flush timer for event batching
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Create NativeCallback that Python's trace function will call
    // Signature: void callback(const char* file, const char* func, int line, int funcId)
    const self = this;
    this.traceCallback = new NativeCallback(
      function (filePtr: NativePointer, funcPtr: NativePointer, line: number, funcId: number) {
        try {
          const file = filePtr.readUtf8String() || '';
          const funcName = funcPtr.readUtf8String() || '';
          const hook = self.hooks.get(funcId);
          if (hook) {
            self.emitTraceEvent(funcId, hook, { file, funcName, line }, 'entry');
          }
        } catch (e) {
          // Silent — don't break Python execution
        }
      },
      'void', ['pointer', 'pointer', 'int', 'int']
    ) as NativePointer;

    send({ type: 'log', message: `PythonTracer: initialized with NativeCallback at ${this.traceCallback}` });
  }

  dispose(): void {
    // Remove sys.settrace
    if (this.traceInstalled) {
      this.runPython('import sys; sys.settrace(None)');
      this.traceInstalled = false;
    }
    if (this.flushTimer) {
      clearInterval(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushEvents(); // Final flush
    this.hooks.clear();
    this.breakpoints.clear();
    this.logpoints.clear();
  }

  /**
   * Install or update the sys.settrace hook with current hook patterns.
   * Uses Python's own tracing API — version-independent, no struct offsets.
   */
  private syncTraceHooks(): void {
    if (!this.traceCallback || !this.PyRun_SimpleString) return;

    // Get the raw hex address for use in Python ctypes
    const callbackAddr = (this.traceCallback as NativePointer).toString();

    // Build hook lookup entries for Python
    // Each entry: (file_suffix, first_lineno, funcId)
    const hookEntries: string[] = [];
    for (const [funcId, hook] of this.hooks) {
      const file = (hook.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      const line = hook.line || 0;
      hookEntries.push(`('${file}', ${line}, ${funcId})`);
    }

    const pythonCode = `
import sys, ctypes, threading

# Create callback function type: void(char*, char*, int, int)
_STROBE_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int, ctypes.c_int)
_strobe_cb = _STROBE_CB_TYPE(${callbackAddr})

# Build hook lookup: list of (file_suffix, first_lineno, funcId)
_strobe_hooks = [${hookEntries.join(', ')}]

def _strobe_trace(frame, event, arg):
    if event == 'call':
        co = frame.f_code
        fname = co.co_filename
        fline = co.co_firstlineno
        for file_pat, line_pat, fid in _strobe_hooks:
            if fname.endswith(file_pat) and fline == line_pat:
                _strobe_cb(fname.encode('utf-8'), co.co_name.encode('utf-8'), frame.f_lineno, fid)
                break
    return _strobe_trace

# settrace_all_threads (3.12+) applies to ALL existing threads including main.
# sys.settrace only applies to the calling thread (Frida's thread, not main).
if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(_strobe_trace)
else:
    # Fallback for Python < 3.12: set on current + new threads
    sys.settrace(_strobe_trace)
    threading.settrace(_strobe_trace)
`;

    const result = this.runPython(pythonCode);
    if (result === 0) {
      this.traceInstalled = true;
      send({ type: 'log', message: `PythonTracer: sys.settrace installed with ${this.hooks.size} hooks` });
    } else {
      send({ type: 'log', message: 'PythonTracer: FAILED to install sys.settrace' });
    }
  }

  /**
   * Execute Python code via PyRun_SimpleString with GIL management.
   * Returns 0 on success, -1 on failure.
   */
  private runPython(code: string): number {
    if (!this.PyRun_SimpleString || !this.PyGILState_Ensure || !this.PyGILState_Release) {
      return -1;
    }

    const ensure = new NativeFunction(this.PyGILState_Ensure, 'int', []);
    const release = new NativeFunction(this.PyGILState_Release, 'void', ['int']);
    const run = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);

    const gilState = ensure();
    try {
      const codeBuf = Memory.allocUtf8String(code);
      return run(codeBuf) as number;
    } finally {
      release(gilState);
    }
  }

  private emitTraceEvent(funcId: number, hook: PythonHook, frameInfo: any, event: 'entry' | 'exit'): void {
    const eventId = `${this.sessionId}-py-${++this.eventIdCounter}`;
    const timestampNs = Date.now() * 1000000; // ms → ns
    const threadId = Process.getCurrentThreadId();

    const traceEvent: any = {
      id: eventId,
      sessionId: this.sessionId,
      timestampNs,
      threadId,
      eventType: event === 'entry' ? 'function_enter' : 'function_exit',
      functionName: hook.target.name || frameInfo.funcName,
      sourceFile: frameInfo.file,
      lineNumber: frameInfo.line,
      pid: Process.id,
    };

    this.eventBuffer.push(traceEvent);

    // Auto-flush if buffer is getting large
    if (this.eventBuffer.length >= 50) {
      this.flushEvents();
    }
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;

    const hook: PythonHook = {
      funcId,
      target,
      mode,
      file: target.file || target.sourceFile || '',
      line: target.line ?? target.lineNumber ?? 0,
    };

    this.hooks.set(funcId, hook);

    // Defer trace installation until all hooks in this batch are added.
    // The caller (agent handleMessage) will trigger syncTraceHooks via
    // a post-batch callback. But for safety, also install on a microtask.
    if (!this.traceInstalled) {
      setTimeout(() => {
        if (!this.traceInstalled && this.hooks.size > 0) {
          this.syncTraceHooks();
        }
      }, 0);
    }

    return funcId;
  }

  /**
   * Called after a batch of hooks is installed to sync the Python trace function.
   */
  syncAfterBatch(): void {
    if (this.hooks.size > 0) {
      this.syncTraceHooks();
    }
  }

  removeHook(id: number): void {
    this.hooks.delete(id);
    if (this.traceInstalled) {
      this.syncTraceHooks();
    }
  }

  removeAllHooks(): void {
    this.hooks.clear();
    if (this.traceInstalled) {
      this.runPython('import sys; sys.settrace(None)');
      this.traceInstalled = false;
    }
  }

  activeHookCount(): number {
    return this.hooks.size;
  }

  installBreakpoint(msg: BreakpointMessage): void {
    if (!msg.file || msg.line === undefined) {
      throw new Error('Python breakpoints require file and line');
    }

    const bp: PythonBreakpoint = {
      id: msg.id,
      file: msg.file,
      line: msg.line,
      condition: msg.condition,
      hitCount: msg.hitCount,
      currentHits: 0,
    };

    this.breakpoints.set(msg.id, bp);
  }

  removeBreakpoint(id: string): void {
    this.breakpoints.delete(id);
  }

  installStepHooks(msg: StepHooksMessage): void {
    this.stepState = {
      active: true,
      threadId: msg.threadId,
    };
  }

  installLogpoint(msg: LogpointMessage): void {
    if (!msg.file || msg.line === undefined) {
      throw new Error('Python logpoints require file and line');
    }

    const lp: PythonLogpoint = {
      id: msg.id,
      file: msg.file,
      line: msg.line,
      message: msg.message,
      condition: msg.condition,
    };

    this.logpoints.set(msg.id, lp);
  }

  removeLogpoint(id: string): void {
    this.logpoints.delete(id);
  }

  readVariable(expr: string): any {
    // Use PyRun_SimpleString to evaluate expression
    const result = this.runPython(`
import json as _j
__strobe_result = _j.dumps(${expr})
`);
    if (result !== 0) {
      throw new Error(`Failed to evaluate: ${expr}`);
    }
    return '<python value>'; // Placeholder — full implementation needs C API
  }

  writeVariable(expr: string, value: any): void {
    const valueStr = typeof value === 'string' ? `"${value}"` : String(value);
    const result = this.runPython(`${expr} = ${valueStr}`);
    if (result !== 0) {
      throw new Error(`Failed to write: ${expr}`);
    }
  }

  // No-op for interpreted languages
  setImageBase(imageBase: string): void {}

  getSlide(): NativePointer {
    return ptr(0);
  }

  resolvePattern(pattern: string): ResolvedTarget[] {
    return [];
  }
}
