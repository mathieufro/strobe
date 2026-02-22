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
  private settraceInstalled: boolean = false;

  // Default to 3.11 (safe fallback — settrace path). Only upgrade to monitoring
  // when version is confirmed via Py_GetVersion.
  private cpythonVersion: { major: number; minor: number } = { major: 3, minor: 11 };

  // Class-level getter — NOT inside a method body (TypeScript syntax constraint).
  private get useMonitoring(): boolean {
    return this.cpythonVersion.major > 3 ||
      (this.cpythonVersion.major === 3 && this.cpythonVersion.minor >= 12);
  }

  // CPython API function pointers
  private PyRun_SimpleString: NativePointer | null = null;
  private PyGILState_Ensure: NativePointer | null = null;
  private PyGILState_Release: NativePointer | null = null;

  // Cached NativeFunction wrappers (created once, reused)
  private _gilEnsure: NativeFunction<number, []> | null = null;
  private _gilRelease: NativeFunction<void, [number]> | null = null;
  private _pyRun: NativeFunction<number, [NativePointer]> | null = null;

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

    // Detect CPython version for sys.monitoring support (PEP 669, 3.12+)
    const PyGetVersion = findGlobalExport('Py_GetVersion');
    if (PyGetVersion) {
      const fn = new NativeFunction(PyGetVersion, 'pointer', []);
      const versionStr = (fn() as NativePointer).readUtf8String() || '';
      const match = versionStr.match(/^(\d+)\.(\d+)/);
      if (match) {
        this.cpythonVersion = { major: parseInt(match[1]), minor: parseInt(match[2]) };
        send({ type: 'log', message: `PythonTracer: detected CPython ${match[1]}.${match[2]}` });
      }
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
    if (this.traceInstalled) {
      this.teardownTracing();
      this.traceInstalled = false;
      this.settraceInstalled = false;
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
   * Build Python assignment code for the three data lists.
   * These are module-level globals in __main__ that the trace function reads
   * via LOAD_GLOBAL (late binding), so updating them is sufficient — no need
   * to redefine _strobe_trace or call settrace again.
   */
  private buildTraceDataAssignments(): string {
    const hookEntries: string[] = [];
    for (const [funcId, hook] of this.hooks) {
      const file = (hook.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      hookEntries.push(`('${file}', ${hook.line || 0}, ${funcId})`);
    }

    const logpointEntries: string[] = [];
    for (const lp of this.logpoints.values()) {
      const file = (lp.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      const msg = (lp.message || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      logpointEntries.push(`('${file}', ${lp.line}, '${lp.id}', '${msg}')`);
    }

    const bpEntries: string[] = [];
    for (const bp of this.breakpoints.values()) {
      const file = (bp.file || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      const cond = (bp.condition || '').replace(/\\/g, '\\\\').replace(/'/g, "\\'");
      bpEntries.push(`('${file}', ${bp.line}, '${bp.id}', '${cond}', ${bp.hitCount || 0})`);
    }

    return `_strobe_hooks = [${hookEntries.join(', ')}]
_strobe_logpoints = [${logpointEntries.join(', ')}]
_strobe_breakpoints = [${bpEntries.join(', ')}]`;
  }

  /**
   * Install or update the sys.settrace hook with current hook patterns.
   *
   * First call: defines the trace function, callbacks, and installs via
   * settrace_all_threads(). Subsequent calls: only update the data lists.
   * This avoids re-calling settrace_all_threads() from the Frida agent
   * thread, which doesn't properly reinstall for existing Python threads
   * in Python 3.12+ (the root cause of the breakpoint remove+add bug).
   */
  private syncTraceHooks(): void {
    if (!this.traceCallback || !this.PyRun_SimpleString) return;

    // If trace is already installed, just update the data lists.
    // The existing _strobe_trace function reads _strobe_hooks, _strobe_breakpoints,
    // and _strobe_logpoints via LOAD_GLOBAL, so it picks up new values immediately.
    if (this.traceInstalled) {
      let code = this.buildTraceDataAssignments();

      // On 3.12+, settrace may not be installed yet if no breakpoints/logpoints
      // existed during first installation. Install it now if needed.
      const needsSettrace = this.useMonitoring && !this.settraceInstalled
        && (this.breakpoints.size > 0 || this.logpoints.size > 0);
      if (needsSettrace) {
        code += `
import sys, threading
if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(_strobe_trace)
else:
    sys.settrace(_strobe_trace)
    threading.settrace(_strobe_trace)
`;
      }

      const result = this.runPython(code);
      if (result === 0) {
        if (needsSettrace) this.settraceInstalled = true;
        send({ type: 'log', message: `PythonTracer: updated data — ${this.hooks.size} hooks, ${this.logpoints.size} logpoints, ${this.breakpoints.size} breakpoints` });
      } else {
        send({ type: 'log', message: 'PythonTracer: FAILED to update trace data' });
      }
      return;
    }

    // First-time installation — version-aware dual-mode tracing
    const callbackAddr = (this.traceCallback as NativePointer).toString();
    const logCallbackAddr = this.logCallback ? (this.logCallback as NativePointer).toString() : '0';
    const bpHitCallbackAddr = this.bpHitCallback ? (this.bpHitCallback as NativePointer).toString() : '0';
    const dataAssignments = this.buildTraceDataAssignments();

    // Common preamble: callbacks, data lists, bp event, GIL helpers
    const preamble = `
import sys, ctypes, threading, builtins as _b

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
_strobe_bp_event = getattr(_b, '_strobe_bp_event', None) or threading.Event()
setattr(_b, '_strobe_bp_event', _strobe_bp_event)

# Data lists (updated in-place on subsequent syncs without redefining the trace function)
${dataAssignments}
`;

    // Settrace-based trace function for breakpoints/logpoints (needs frame objects)
    // Also used as the full tracer on Python < 3.12
    const settraceBlock = `
def _strobe_trace(frame, event, arg):
    try:
        if event == 'call':
            co = frame.f_code
            fname = co.co_filename
            fline = co.co_firstlineno
            for file_pat, line_pat, fid in _strobe_hooks:
                if fname.endswith(file_pat) and fline == line_pat:
                    _strobe_cb(fname.encode('utf-8'), co.co_name.encode('utf-8'), frame.f_lineno, fid)
                    break
        elif event == 'line':
            fname = frame.f_code.co_filename
            fline = frame.f_lineno
            for lp_file, lp_line, lp_id, lp_msg in _strobe_logpoints:
                if fname.endswith(lp_file) and fline == lp_line:
                    try:
                        import re as _re
                        def _strobe_safe_fmt(m):
                            k = m.group(1)
                            if '__' in k or '.' in k or '[' in k:
                                return m.group(0)
                            _vars = {**frame.f_globals, **frame.f_locals}
                            return str(_vars.get(k, m.group(0)))
                        msg = _re.sub(r'\\{(\\w+)\\}', _strobe_safe_fmt, lp_msg)
                    except Exception as _e:
                        msg = lp_msg + ' [fmt error]'
                    _strobe_log_cb(lp_id.encode(), fline, msg.encode())
                    break
            for bp_file, bp_line, bp_id, bp_cond, bp_hit_count in _strobe_breakpoints:
                if fname.endswith(bp_file) and fline == bp_line:
                    if bp_hit_count > 0:
                        _strobe_bp_hits = getattr(_b, '_strobe_bp_hits', {})
                        _strobe_bp_hits[bp_id] = _strobe_bp_hits.get(bp_id, 0) + 1
                        setattr(_b, '_strobe_bp_hits', _strobe_bp_hits)
                        if _strobe_bp_hits[bp_id] < bp_hit_count:
                            break
                    if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
                        _strobe_bp_hit_cb(bp_id.encode(), fline)
                        # threading.Event.wait() releases the GIL internally in CPython,
                        # allowing the Frida agent thread to call PyRun_SimpleString to resume.
                        # Do NOT use PyEval_SaveThread/RestoreThread — corrupts thread state on 3.14+.
                        _strobe_bp_event.wait()
                        _strobe_bp_event.clear()
                    break
    except Exception as _strobe_err:
        import builtins as _be
        if not hasattr(_be, '_strobe_errors'):
            _be._strobe_errors = []
        _be._strobe_errors.append(f'{type(_strobe_err).__name__}: {_strobe_err}')
    return _strobe_trace
`;

    let pythonCode: string;

    if (this.useMonitoring) {
      // Python 3.12+: sys.monitoring for function enter (interpreter-global),
      // sys.settrace for breakpoints/logpoints (needs frame objects).
      pythonCode = preamble + `
# --- sys.monitoring for PY_START (interpreter-global, no per-thread issues) ---
_strobe_monitoring_ok = False
try:
    sys.monitoring.use_tool_id(0, "strobe")
    sys.monitoring.set_events(0, sys.monitoring.events.PY_START)

    def _strobe_on_start(code, offset):
        fname = code.co_filename
        fline = code.co_firstlineno
        # co_qualname (3.12+) gives the undecorated name; decorators shift co_firstlineno
        # to the decorator line, so allow a small line-number window for decorated functions
        cname = getattr(code, 'co_qualname', code.co_name)
        for file_pat, line_pat, fid in _strobe_hooks:
            if fname.endswith(file_pat) and (fline == line_pat or (fline > 0 and abs(fline - line_pat) <= 5)):
                _strobe_cb(fname.encode('utf-8'), cname.encode('utf-8'), fline, fid)
                return

    sys.monitoring.register_callback(0, sys.monitoring.events.PY_START, _strobe_on_start)
    setattr(_b, '_strobe_monitoring_active', True)
    _strobe_monitoring_ok = True
except ValueError:
    # Tool ID 0 already in use — fall back to settrace
    pass

# --- sys.settrace for breakpoints/logpoints, OR as fallback if monitoring failed ---
` + settraceBlock + `
if _strobe_monitoring_ok:
    # Only install settrace if we have breakpoints or logpoints
    if _strobe_breakpoints or _strobe_logpoints:
        if hasattr(threading, 'settrace_all_threads'):
            threading.settrace_all_threads(_strobe_trace)
        else:
            sys.settrace(_strobe_trace)
            threading.settrace(_strobe_trace)
else:
    # Monitoring failed — full settrace fallback
    if hasattr(threading, 'settrace_all_threads'):
        threading.settrace_all_threads(_strobe_trace)
    else:
        sys.settrace(_strobe_trace)
        threading.settrace(_strobe_trace)
`;
    } else {
      // Python < 3.12: settrace-only approach (existing behavior)
      pythonCode = preamble + settraceBlock + `
if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(_strobe_trace)
else:
    sys.settrace(_strobe_trace)
    threading.settrace(_strobe_trace)
`;
    }

    const result = this.runPython(pythonCode);
    if (result === 0) {
      this.traceInstalled = true;
      // Track whether settrace was installed: always on <3.12, on 3.12+ only if bp/lp exist
      if (!this.useMonitoring || this.breakpoints.size > 0 || this.logpoints.size > 0) {
        this.settraceInstalled = true;
      }
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

    // Cache NativeFunction wrappers (created once, reused across calls)
    if (!this._gilEnsure) {
      this._gilEnsure = new NativeFunction(this.PyGILState_Ensure, 'int', []);
      this._gilRelease = new NativeFunction(this.PyGILState_Release, 'void', ['int']);
      this._pyRun = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);
    }

    const gilState = this._gilEnsure!();
    try {
      const codeBuf = Memory.allocUtf8String(code);
      return this._pyRun!(codeBuf) as number;
    } finally {
      this._gilRelease!(gilState);
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
      // Surgically remove from Python's hook list without full resync.
      // Full syncTraceHooks() + threading.settrace_all_threads() can break
      // tracing when called rapidly in succession (e.g., remove then add).
      this.runPython(`
try:
    _strobe_hooks = [h for h in _strobe_hooks if h[2] != ${id}]
except NameError:
    pass
`);
    }
  }

  removeAllHooks(): void {
    this.hooks.clear();
    this.breakpoints.clear();
    this.logpoints.clear();
    if (this.traceInstalled) {
      this.teardownTracing();
      this.traceInstalled = false;
      this.settraceInstalled = false;
    }
  }

  /** Shared teardown logic for dispose() and removeAllHooks(). */
  private teardownTracing(): void {
    if (this.useMonitoring) {
      this.runPython(`
import sys, threading, builtins as _b
if getattr(_b, '_strobe_monitoring_active', False):
    sys.monitoring.set_events(0, 0)
    sys.monitoring.free_tool_id(0)
    setattr(_b, '_strobe_monitoring_active', False)
if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(None)
else:
    sys.settrace(None)
    threading.settrace(None)
`);
    } else {
      this.runPython(`
import sys, threading
if hasattr(threading, 'settrace_all_threads'):
    threading.settrace_all_threads(None)
else:
    sys.settrace(None)
    threading.settrace(None)
`);
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
    if (this.traceInstalled) {
      // Surgically remove from Python's breakpoint list without full resync.
      // Also signal _strobe_bp_event to unblock any thread that may be paused
      // at this breakpoint. This handles the race where the BP fires again
      // between debug_continue and the removal (the timer can re-trigger the
      // old BP before it's removed from the Python list).
      const safeId = id.replace(/'/g, "\\'");
      this.runPython(`
try:
    _strobe_breakpoints = [bp for bp in _strobe_breakpoints if bp[2] != '${safeId}']
    import builtins as _b
    _b._strobe_bp_event.set()
except NameError:
    pass
`);
    }
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
    if (this.traceInstalled) {
      const safeId = id.replace(/'/g, "\\'");
      this.runPython(`
try:
    _strobe_logpoints = [lp for lp in _strobe_logpoints if lp[2] != '${safeId}']
except NameError:
    pass
`);
    }
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
    // Validate expr is a simple assignment target (variable name, attribute access, subscript)
    // to prevent code injection via expr itself
    if (!/^[a-zA-Z_]\w*(?:\.[a-zA-Z_]\w*|\[\d+\]|\[['"][^'"]*['"]\])*$/.test(expr)) {
      throw new Error(`Invalid write target: ${expr}`);
    }
    // Serialize value as JSON and use json.loads() in Python to safely deserialize,
    // preventing code injection via string interpolation.
    const safeValue = JSON.stringify(value);
    const result = this.runPython(
      `import json as _j; ${expr} = _j.loads(${JSON.stringify(safeValue)})`
    );
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
