// agent/src/tracers/python-tracer.ts
// Python tracer using CPython frame evaluation hooks

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage, ReadMemoryMessage, WriteMemoryMessage } from './tracer.js';

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
  private frameEvalHook?: any;
  private stepState: StepState = { active: false, threadId: 0 };

  // CPython API function pointers
  private PyEval_EvalFrameDefault: NativePointer | null = null;
  private PyFrame_GetCode: NativePointer | null = null;
  private PyCode_GetCode: NativePointer | null = null;
  private PyUnicode_AsUTF8: NativePointer | null = null;
  private PyRun_SimpleString: NativePointer | null = null;
  private PyGILState_Ensure: NativePointer | null = null;
  private PyGILState_Release: NativePointer | null = null;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    // Find CPython symbols
    this.PyEval_EvalFrameDefault = Module.findExportByName(null, '_PyEval_EvalFrameDefault');
    this.PyFrame_GetCode = Module.findExportByName(null, 'PyFrame_GetCode');
    this.PyCode_GetCode = Module.findExportByName(null, 'PyCode_GetCode');
    this.PyUnicode_AsUTF8 = Module.findExportByName(null, 'PyUnicode_AsUTF8');
    this.PyRun_SimpleString = Module.findExportByName(null, 'PyRun_SimpleString');
    this.PyGILState_Ensure = Module.findExportByName(null, 'PyGILState_Ensure');
    this.PyGILState_Release = Module.findExportByName(null, 'PyGILState_Release');

    if (!this.PyEval_EvalFrameDefault) {
      throw new Error('CPython symbols not found - is this a Python process?');
    }

    // Hook frame evaluation to intercept function calls
    this.installFrameEvalHook();
  }

  dispose(): void {
    if (this.frameEvalHook) {
      this.frameEvalHook.detach();
      this.frameEvalHook = null;
    }
    this.hooks.clear();
    this.breakpoints.clear();
    this.logpoints.clear();
  }

  private installFrameEvalHook(): void {
    if (!this.PyEval_EvalFrameDefault) return;

    const self = this;
    this.frameEvalHook = Interceptor.attach(this.PyEval_EvalFrameDefault, {
      onEnter(args: any) {
        // args[0] = PyThreadState*
        // args[1] = PyFrameObject*
        // args[2] = int throwflag
        const frame = args[1];

        try {
          const frameInfo = self.extractFrameInfo(frame);
          if (!frameInfo) return;

          // Check if this frame matches any active hooks
          for (const [funcId, hook] of self.hooks) {
            if (self.matchesHook(frameInfo, hook)) {
              self.emitTraceEvent(funcId, hook, frameInfo, 'entry');
            }
          }

          // Check breakpoints
          for (const [id, bp] of self.breakpoints) {
            if (self.matchesBreakpoint(frameInfo, bp)) {
              self.handleBreakpointHit(id, bp, frameInfo);
            }
          }

          // Check logpoints
          for (const [id, lp] of self.logpoints) {
            if (self.matchesLogpoint(frameInfo, lp)) {
              self.handleLogpointHit(id, lp, frameInfo);
            }
          }

          // Check step state
          if (self.stepState.active) {
            if (self.matchesStepTarget(frameInfo)) {
              self.handleStepHit(frameInfo);
            }
          }
        } catch (e: any) {
          // Silent failure to avoid breaking Python execution
        }
      },
    });
  }

  private extractFrameInfo(frame: NativePointer): { file: string; line: number; funcName: string } | null {
    try {
      // PyFrameObject structure (CPython 3.11+):
      // - f_back: PyFrameObject*
      // - f_code: PyCodeObject*
      // - f_lineno: int

      // Get PyCodeObject* from frame
      const codeObj = frame.add(Process.pointerSize * 2).readPointer(); // f_code offset
      if (codeObj.isNull()) return null;

      // Get co_filename from PyCodeObject
      const coFilename = codeObj.add(Process.pointerSize * 4).readPointer(); // co_filename offset
      if (coFilename.isNull()) return null;

      // Get co_name from PyCodeObject
      const coName = codeObj.add(Process.pointerSize * 5).readPointer(); // co_name offset
      if (coName.isNull()) return null;

      // Convert PyUnicode to UTF-8
      const filenamePtr = this.PyUnicode_AsUTF8
        ? new NativeFunction(this.PyUnicode_AsUTF8, 'pointer', ['pointer'])(coFilename)
        : null;
      const namePtr = this.PyUnicode_AsUTF8
        ? new NativeFunction(this.PyUnicode_AsUTF8, 'pointer', ['pointer'])(coName)
        : null;

      if (!filenamePtr || filenamePtr.isNull() || !namePtr || namePtr.isNull()) return null;

      const file = filenamePtr.readUtf8String() || '';
      const funcName = namePtr.readUtf8String() || '';

      // Get f_lineno
      const linenoOffset = Process.pointerSize * 8; // f_lineno offset
      const line = frame.add(linenoOffset).readInt();

      return { file, line, funcName };
    } catch (e: any) {
      return null;
    }
  }

  private matchesHook(frameInfo: { file: string; line: number; funcName: string }, hook: PythonHook): boolean {
    // Match by file and line if target specifies them
    if (hook.target.file && hook.target.line !== undefined) {
      return frameInfo.file.includes(hook.target.file) && frameInfo.line === hook.target.line;
    }
    // Otherwise match by function name
    if (hook.target.name) {
      return frameInfo.funcName === hook.target.name || frameInfo.funcName.endsWith('.' + hook.target.name);
    }
    return false;
  }

  private matchesBreakpoint(frameInfo: { file: string; line: number }, bp: PythonBreakpoint): boolean {
    return frameInfo.file.includes(bp.file) && frameInfo.line === bp.line;
  }

  private matchesLogpoint(frameInfo: { file: string; line: number }, lp: PythonLogpoint): boolean {
    return frameInfo.file.includes(lp.file) && frameInfo.line === lp.line;
  }

  private matchesStepTarget(frameInfo: { file: string; line: number }): boolean {
    if (!this.stepState.targetFile || this.stepState.targetLine === undefined) return true;
    return frameInfo.file.includes(this.stepState.targetFile) && frameInfo.line === this.stepState.targetLine;
  }

  private emitTraceEvent(funcId: number, hook: PythonHook, frameInfo: any, event: 'entry' | 'exit'): void {
    const msg = {
      type: 'trace_event',
      funcId,
      funcName: hook.target.name,
      event,
      timestamp: Date.now(),
      sourceFile: frameInfo.file,
      lineNumber: frameInfo.line,
    };
    send(msg);
  }

  private handleBreakpointHit(id: string, bp: PythonBreakpoint, frameInfo: any): void {
    bp.currentHits++;

    // Check hit count condition
    if (bp.hitCount !== undefined && bp.currentHits < bp.hitCount) {
      return;
    }

    // Check conditional expression
    if (bp.condition) {
      try {
        const result = this.evaluatePythonExpression(bp.condition);
        if (!result) return;
      } catch (e: any) {
        // Condition evaluation failed, skip
        return;
      }
    }

    send({
      type: 'breakpoint_hit',
      id: bp.id,
      file: frameInfo.file,
      line: frameInfo.line,
      funcName: frameInfo.funcName,
      hitCount: bp.currentHits,
    });
  }

  private handleLogpointHit(id: string, lp: PythonLogpoint, frameInfo: any): void {
    // Check conditional expression
    if (lp.condition) {
      try {
        const result = this.evaluatePythonExpression(lp.condition);
        if (!result) return;
      } catch (e: any) {
        return;
      }
    }

    // Interpolate variables in message (simple {var} syntax)
    let message = lp.message;
    const varMatches = message.match(/\{([^}]+)\}/g);
    if (varMatches) {
      for (const match of varMatches) {
        const varName = match.slice(1, -1);
        try {
          const value = this.readVariable(varName);
          message = message.replace(match, String(value));
        } catch (e: any) {
          message = message.replace(match, '<error>');
        }
      }
    }

    send({
      type: 'logpoint_output',
      id: lp.id,
      message,
      file: frameInfo.file,
      line: frameInfo.line,
    });
  }

  private handleStepHit(frameInfo: any): void {
    this.stepState.active = false;
    send({
      type: 'step_complete',
      file: frameInfo.file,
      line: frameInfo.line,
      funcName: frameInfo.funcName,
    });
  }

  private evaluatePythonExpression(expr: string): any {
    if (!this.PyRun_SimpleString) return null;

    // Acquire GIL
    const gilState = this.PyGILState_Ensure
      ? new NativeFunction(this.PyGILState_Ensure, 'int', [])()
      : 0;

    try {
      // Use PyRun_SimpleString to evaluate expression
      // This is simplified - real implementation would use PyEval_GetGlobals/GetLocals
      const code = Memory.allocUtf8String(`__strobe_result = ${expr}`);
      const runFunc = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);
      const result = runFunc(code);

      return result === 0; // 0 = success
    } finally {
      // Release GIL
      if (this.PyGILState_Release) {
        new NativeFunction(this.PyGILState_Release, 'void', ['int'])(gilState);
      }
    }
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
    return funcId;
  }

  removeHook(id: number): void {
    this.hooks.delete(id);
  }

  removeAllHooks(): void {
    this.hooks.clear();
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
    // For Python, stepping is frame-based, not address-based
    // Enable step state to trigger on next frame
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
    if (!this.PyRun_SimpleString) {
      throw new Error('PyRun_SimpleString not available');
    }

    // Acquire GIL
    const gilState = this.PyGILState_Ensure
      ? new NativeFunction(this.PyGILState_Ensure, 'int', [])()
      : 0;

    try {
      // Evaluate expression and store in temporary variable
      // Real implementation would use PyEval_EvalCode with proper frame locals
      const code = Memory.allocUtf8String(`
import json
__strobe_result = json.dumps(${expr})
`);
      const runFunc = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);
      const result = runFunc(code);

      if (result !== 0) {
        throw new Error(`Failed to evaluate: ${expr}`);
      }

      // Read __strobe_result (simplified - real implementation would use C API)
      return '<python value>'; // Placeholder
    } finally {
      // Release GIL
      if (this.PyGILState_Release) {
        new NativeFunction(this.PyGILState_Release, 'void', ['int'])(gilState);
      }
    }
  }

  writeVariable(expr: string, value: any): void {
    if (!this.PyRun_SimpleString) {
      throw new Error('PyRun_SimpleString not available');
    }

    // Acquire GIL
    const gilState = this.PyGILState_Ensure
      ? new NativeFunction(this.PyGILState_Ensure, 'int', [])()
      : 0;

    try {
      // Execute assignment
      const valueStr = typeof value === 'string' ? `"${value}"` : String(value);
      const code = Memory.allocUtf8String(`${expr} = ${valueStr}`);
      const runFunc = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);
      const result = runFunc(code);

      if (result !== 0) {
        throw new Error(`Failed to write: ${expr}`);
      }
    } finally {
      // Release GIL
      if (this.PyGILState_Release) {
        new NativeFunction(this.PyGILState_Release, 'void', ['int'])(gilState);
      }
    }
  }

  // No-op for interpreted languages
  setImageBase(imageBase: string): void {
    // Python has no ASLR
  }

  getSlide(): NativePointer {
    return ptr(0);
  }

  // Runtime pattern resolution (optional enhancement)
  resolvePattern(pattern: string): ResolvedTarget[] {
    // Could use Python's inspect module to find functions at runtime
    // For now, return empty (daemon will handle via PythonResolver)
    return [];
  }
}
