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
  private logCallback: NativePointer | null = null;
  private bpHitCallback: NativePointer | null = null;
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

    const self2 = this;
    this.logCallback = new NativeCallback(
      function (lpIdPtr: NativePointer, lineNum: number, msgPtr: NativePointer) {
        try {
          const lpId = lpIdPtr.readUtf8String() ?? '';
          const msg = msgPtr.readUtf8String() ?? '';
          self2.emitLogpointEvent(lpId, lineNum, msg);
        } catch {}
      },
      'void', ['pointer', 'int', 'pointer']
    ) as NativePointer;

    const self3 = this;
    this.bpHitCallback = new NativeCallback(
      function (idPtr: NativePointer, lineNum: number) {
        try {
          const id = idPtr.readUtf8String() ?? '';
          self3.agent.emitBreakpointHit(id, lineNum);
        } catch {}
      },
      'void', ['pointer', 'int']
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

    // Get the raw hex addresses for use in Python ctypes
    const callbackAddr = (this.traceCallback as NativePointer).toString();
    const logCallbackAddr = this.logCallback ? (this.logCallback as NativePointer).toString() : '0';
    const bpHitCallbackAddr = this.bpHitCallback ? (this.bpHitCallback as NativePointer).toString() : '0';

    // Build hook lookup entries for Python: (file_suffix, first_lineno, funcId)
    const hookEntries: string[] = [];
    for (const [funcId, hook] of this.hooks) {
      const file = (hook.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      hookEntries.push(`('${file}', ${hook.line || 0}, ${funcId})`);
    }

    // Build logpoint entries: (file_suffix, line, lp_id, msg_template)
    const logpointEntries: string[] = [];
    for (const lp of this.logpoints.values()) {
      const file = (lp.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      const msg = (lp.message || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      logpointEntries.push(`('${file}', ${lp.line}, '${lp.id}', '${msg}')`);
    }

    // Build breakpoint entries: (file_suffix, line, bp_id, condition)
    const bpEntries: string[] = [];
    for (const bp of this.breakpoints.values()) {
      const file = (bp.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      const cond = (bp.condition || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      bpEntries.push(`('${file}', ${bp.line}, '${bp.id}', '${cond}')`);
    }

    const hasLineEvents = logpointEntries.length > 0 || bpEntries.length > 0;

    const lineHandler = hasLineEvents ? `
    elif event == 'line':
        fname = frame.f_code.co_filename
        fline = frame.f_lineno
        for lp_file, lp_line, lp_id, lp_msg in _strobe_logpoints:
            if fname.endswith(lp_file) and fline == lp_line:
                try:
                    msg = lp_msg.format(**{**frame.f_globals, **frame.f_locals})
                except Exception as _e:
                    msg = f'{lp_msg} [fmt error: {_e}]'
                _strobe_log_cb(lp_id.encode(), fline, msg.encode())
                break
        for bp_file, bp_line, bp_id, bp_cond in _strobe_breakpoints:
            if fname.endswith(bp_file) and fline == bp_line:
                if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
                    _strobe_bp_hit_cb(bp_id.encode(), fline)
                    _strobe_bp_event.wait()
                    _strobe_bp_event.clear()
                break` : '';

    const pythonCode = `
import sys, ctypes, threading

# Trace callback: void(char*, char*, int, int)
_STROBE_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int, ctypes.c_int)
_strobe_cb = _STROBE_CB_TYPE(${callbackAddr})

# Logpoint callback: void(char*, int, char*)
_STROBE_LOG_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p)
_strobe_log_cb = _STROBE_LOG_CB_TYPE(${logCallbackAddr})

# Breakpoint hit callback: void(char*, int)
_STROBE_BP_HIT_CB_TYPE = ctypes.CFUNCTYPE(None, ctypes.c_char_p, ctypes.c_int)
_strobe_bp_hit_cb = _STROBE_BP_HIT_CB_TYPE(${bpHitCallbackAddr})

# Suspension event for Python breakpoints — persisted in builtins across resync calls
import builtins as _b
_strobe_bp_event = getattr(_b, '_strobe_bp_event', None) or threading.Event()
setattr(_b, '_strobe_bp_event', _strobe_bp_event)

# Hook lookup: (file_suffix, first_lineno, funcId)
_strobe_hooks = [${hookEntries.join(', ')}]

# Logpoint lookup: (file_suffix, line, lp_id, msg_template)
_strobe_logpoints = [${logpointEntries.join(', ')}]

# Breakpoint lookup: (file_suffix, line, bp_id, condition)
_strobe_breakpoints = [${bpEntries.join(', ')}]

def _strobe_trace(frame, event, arg):
    if event == 'call':
        co = frame.f_code
        fname = co.co_filename
        fline = co.co_firstlineno
        for file_pat, line_pat, fid in _strobe_hooks:
            if fname.endswith(file_pat) and fline == line_pat:
                _strobe_cb(fname.encode('utf-8'), co.co_name.encode('utf-8'), frame.f_lineno, fid)
                break${lineHandler}
    return _strobe_trace

if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(_strobe_trace)
else:
    sys.settrace(_strobe_trace)
    threading.settrace(_strobe_trace)
`;

    const result = this.runPython(pythonCode);
    if (result === 0) {
      this.traceInstalled = true;
      send({ type: 'log', message: `PythonTracer: sys.settrace installed with ${this.hooks.size} hooks, ${this.logpoints.size} logpoints, ${this.breakpoints.size} breakpoints` });
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

  private emitLogpointEvent(lpId: string, line: number, msg: string): void {
    const eventId = `${this.sessionId}-pylp-${++this.eventIdCounter}`;
    this.eventBuffer.push({
      id: eventId,
      sessionId: this.sessionId,
      timestampNs: Date.now() * 1_000_000,
      threadId: Process.getCurrentThreadId(),
      eventType: 'stdout',
      text: `[logpoint ${lpId}] ${msg}\n`,
      pid: Process.id,
    });
    if (this.eventBuffer.length >= 50) this.flushEvents();
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
    this.syncTraceHooks();
  }

  removeBreakpoint(id: string): void {
    this.breakpoints.delete(id);
    if (this.traceInstalled) this.syncTraceHooks();
  }

  resumePythonBreakpoint(): void {
    // Signal the waiting threading.Event in the target Python process
    this.runPython('import builtins as _b; _b._strobe_bp_event.set()');
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
    this.syncTraceHooks();
  }

  removeLogpoint(id: string): void {
    this.logpoints.delete(id);
    if (this.traceInstalled) this.syncTraceHooks();
  }

  readVariable(expr: string): any {
    // Two-step approach:
    // 1. Python eval → store JSON result string in builtins._strobe_eval_result
    // 2. Read it back via Python C API (PyImport/PyObject/PyUnicode)
    const safeExpr = JSON.stringify(expr);
    const retCode = this.runPython(`
import json as _j, builtins as _b
try:
    _b._strobe_eval_result = _j.dumps(eval(${safeExpr}), default=str)
except Exception as _e:
    _b._strobe_eval_result = _j.dumps({"error": str(_e)})
`);
    if (retCode !== 0) {
      return { error: 'PyRun_SimpleString failed' };
    }

    // Read the result back using Python C API
    return this.readPythonBuiltin('_strobe_eval_result');
  }

  /** Read a string attribute from Python builtins module via C API. */
  private readPythonBuiltin(attrName: string): any {
    if (!this.PyGILState_Ensure || !this.PyGILState_Release) {
      return { error: 'GIL functions not available' };
    }
    const libpython = this.findLibPython();
    if (!libpython) return { error: 'Cannot find libpython' };

    const PyImport_ImportModule = new NativeFunction(
      libpython.getExportByName('PyImport_ImportModule'), 'pointer', ['pointer']
    );
    const PyObject_GetAttrString = new NativeFunction(
      libpython.getExportByName('PyObject_GetAttrString'), 'pointer', ['pointer', 'pointer']
    );
    const PyUnicode_AsUTF8 = new NativeFunction(
      libpython.getExportByName('PyUnicode_AsUTF8'), 'pointer', ['pointer']
    );
    const Py_DecRef = new NativeFunction(
      libpython.getExportByName('Py_DecRef'), 'void', ['pointer']
    );

    const ensure = new NativeFunction(this.PyGILState_Ensure, 'int', []);
    const release = new NativeFunction(this.PyGILState_Release, 'void', ['int']);

    const gilState = ensure();
    try {
      const modName = Memory.allocUtf8String('builtins');
      const attrStr = Memory.allocUtf8String(attrName);

      const builtinsMod = PyImport_ImportModule(modName) as NativePointer;
      if (builtinsMod.isNull()) return { error: 'Failed to import builtins' };

      const resultObj = PyObject_GetAttrString(builtinsMod, attrStr) as NativePointer;
      Py_DecRef(builtinsMod);
      if (resultObj.isNull()) return { error: `builtins.${attrName} not found` };

      const utf8Ptr = PyUnicode_AsUTF8(resultObj) as NativePointer;
      const resultStr = utf8Ptr.isNull() ? null : utf8Ptr.readUtf8String();
      Py_DecRef(resultObj);

      if (!resultStr) return { error: 'Failed to read UTF-8 from Python string' };
      try {
        return JSON.parse(resultStr);
      } catch {
        return { error: `Invalid JSON: ${resultStr}` };
      }
    } finally {
      release(gilState);
    }
  }

  /** Find the loaded libpython module. */
  private findLibPython(): Module | null {
    if ((this as any)._libPython) return (this as any)._libPython;
    for (const mod of Process.enumerateModules()) {
      if (/libpython3|python3\.\d+/.test(mod.name)) {
        (this as any)._libPython = mod;
        return mod;
      }
    }
    // On macOS/Linux the Python executable itself may export symbols
    const main = Process.mainModule;
    try {
      main.getExportByName('PyImport_ImportModule');
      (this as any)._libPython = main;
      return main;
    } catch {
      return null;
    }
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
