import {
  DebugSession,
  InitializedEvent,
  TerminatedEvent,
  StoppedEvent,
  ContinuedEvent,
  OutputEvent,
  Thread,
  StackFrame,
  Scope,
  Source,
} from '@vscode/debugadapter';
import { DebugProtocol } from '@vscode/debugprotocol';
import { StrobeLaunchConfig, validateLaunchConfig } from './launch-config';
import { StrobeClient } from '../client/strobe-client';
import { DaemonManager } from '../utils/daemon-manager';
import {
  PausedThreadInfo,
  SessionStatusResponse,
  ReadMemoryResponse,
} from '../client/types';
import * as path from 'path';

const POLL_INTERVAL_MS = 200;

export class StrobeDebugAdapter extends DebugSession {
  private client: StrobeClient | undefined;
  private daemonManager: DaemonManager;
  private sessionId: string | undefined;
  private pollTimer: ReturnType<typeof setTimeout> | undefined;
  private isPolling = false;

  // variablesReference tracking (reset on each stop)
  private nextVarRef = 1;
  private varRefMap = new Map<number, VarRefData>();

  // Cached pause state (refreshed on each stop)
  private pausedThreads: PausedThreadInfo[] = [];
  private lastStatus: SessionStatusResponse | undefined;

  // Track Strobe breakpoint/logpoint IDs per source file for removal
  private trackedBreakpointIds = new Map<string, string[]>();
  private trackedLogpointIds = new Map<string, string[]>();

  // Thread tracking: maps Frida thread IDs to DAP thread IDs
  private threadMap = new Map<number, number>();
  private reverseThreadMap = new Map<number, number>();
  private nextDapThreadId = 1;

  constructor(daemonManager: DaemonManager) {
    super();
    this.daemonManager = daemonManager;
    this.setDebuggerLinesStartAt1(true);
    this.setDebuggerColumnsStartAt1(true);
  }

  // ---- Lifecycle ----

  protected initializeRequest(
    response: DebugProtocol.InitializeResponse,
    _args: DebugProtocol.InitializeRequestArguments,
  ): void {
    response.body = {
      supportsConfigurationDoneRequest: true,
      supportsFunctionBreakpoints: true,
      supportsConditionalBreakpoints: true,
      supportsHitConditionalBreakpoints: true,
      supportsLogPoints: true,
      supportsEvaluateForHovers: true,
      supportsSteppingGranularity: false,
      supportsTerminateRequest: true,
    };
    this.sendResponse(response);
    this.sendEvent(new InitializedEvent());
  }

  protected async launchRequest(
    response: DebugProtocol.LaunchResponse,
    args: DebugProtocol.LaunchRequestArguments,
  ): Promise<void> {
    const config = args as StrobeLaunchConfig;
    const error = validateLaunchConfig(config);
    if (error) {
      response.success = false;
      response.message = error;
      this.sendResponse(response);
      return;
    }

    try {
      this.client = await this.daemonManager.ensureClient();

      const result = await this.client.launch({
        command: config.program,
        args: config.args,
        cwd: config.cwd,
        projectRoot: config.cwd || path.dirname(config.program),
        env: config.env,
      });
      this.sessionId = result.sessionId;

      if (config.tracePatterns && config.tracePatterns.length > 0) {
        await this.client.trace({
          sessionId: this.sessionId,
          add: config.tracePatterns,
        });
      }

      this.startPolling();
      this.sendResponse(response);
    } catch (e: unknown) {
      response.success = false;
      response.message = e instanceof Error ? e.message : String(e);
      this.sendResponse(response);
    }
  }

  protected configurationDoneRequest(
    response: DebugProtocol.ConfigurationDoneResponse,
    _args: DebugProtocol.ConfigurationDoneArguments,
  ): void {
    this.sendResponse(response);
  }

  protected async terminateRequest(
    response: DebugProtocol.TerminateResponse,
    _args: DebugProtocol.TerminateArguments,
  ): Promise<void> {
    await this.stopSession();
    this.sendResponse(response);
  }

  protected async disconnectRequest(
    response: DebugProtocol.DisconnectResponse,
    _args: DebugProtocol.DisconnectArguments,
  ): Promise<void> {
    await this.stopSession();
    this.sendResponse(response);
  }

  // ---- Breakpoints ----

  protected async setBreakPointsRequest(
    response: DebugProtocol.SetBreakpointsResponse,
    args: DebugProtocol.SetBreakpointsArguments,
  ): Promise<void> {
    const sourcePath = args.source.path || '';
    const requested = args.breakpoints || [];

    if (!this.client || !this.sessionId) {
      response.body = {
        breakpoints: requested.map((bp) => ({
          verified: false,
          line: bp.line,
          message: 'Session not started',
        })),
      };
      this.sendResponse(response);
      return;
    }

    try {
      // Remove old breakpoints/logpoints for this source file (DAP set = replace)
      const oldBpIds = this.trackedBreakpointIds.get(sourcePath) || [];
      const oldLpIds = this.trackedLogpointIds.get(sourcePath) || [];
      const removeIds = [...oldBpIds, ...oldLpIds];
      if (removeIds.length > 0) {
        await this.client.setBreakpoints({
          sessionId: this.sessionId,
          remove: removeIds,
        });
      }

      // Partition requested items into breakpoints vs logpoints by index
      const bpIndices: number[] = [];
      const lpIndices: number[] = [];
      const targets = requested.map((bp, i) => {
        if (bp.logMessage) {
          lpIndices.push(i);
        } else {
          bpIndices.push(i);
        }
        return {
          file: sourcePath,
          line: bp.line,
          condition: bp.condition,
          hitCount: bp.hitCondition ? parseInt(bp.hitCondition, 10) : undefined,
          message: bp.logMessage,
        };
      });

      const result = await this.client.setBreakpoints({
        sessionId: this.sessionId,
        add: targets,
      });

      // Map response arrays back to the correct request indices
      const dapBreakpoints: DebugProtocol.Breakpoint[] = new Array(requested.length);
      const newBpIds: string[] = [];
      const newLpIds: string[] = [];

      const resultBps = result.breakpoints || [];
      for (let i = 0; i < bpIndices.length; i++) {
        const reqIdx = bpIndices[i];
        const strobeBp = resultBps[i];
        if (strobeBp) {
          newBpIds.push(strobeBp.id);
          dapBreakpoints[reqIdx] = {
            verified: true,
            line: strobeBp.line || requested[reqIdx].line,
            source: args.source,
          };
        } else {
          dapBreakpoints[reqIdx] = {
            verified: false,
            line: requested[reqIdx].line,
            message: 'Could not resolve breakpoint location',
          };
        }
      }

      const resultLps = result.logpoints || [];
      for (let i = 0; i < lpIndices.length; i++) {
        const reqIdx = lpIndices[i];
        const strobeLp = resultLps[i];
        if (strobeLp) {
          newLpIds.push(strobeLp.id);
          dapBreakpoints[reqIdx] = {
            verified: true,
            line: strobeLp.line || requested[reqIdx].line,
            source: args.source,
          };
        } else {
          dapBreakpoints[reqIdx] = {
            verified: false,
            line: requested[reqIdx].line,
            message: 'Could not resolve logpoint location',
          };
        }
      }

      this.trackedBreakpointIds.set(sourcePath, newBpIds);
      this.trackedLogpointIds.set(sourcePath, newLpIds);

      response.body = { breakpoints: dapBreakpoints };
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : 'Failed to set breakpoint';
      response.body = {
        breakpoints: requested.map((bp) => ({
          verified: false,
          line: bp.line,
          message: msg,
        })),
      };
    }

    this.sendResponse(response);
  }

  protected async setFunctionBreakPointsRequest(
    response: DebugProtocol.SetFunctionBreakpointsResponse,
    args: DebugProtocol.SetFunctionBreakpointsArguments,
  ): Promise<void> {
    if (!this.client || !this.sessionId) {
      response.body = {
        breakpoints: args.breakpoints.map(() => ({ verified: false })),
      };
      this.sendResponse(response);
      return;
    }

    try {
      const targets = args.breakpoints.map((bp) => ({
        function: bp.name,
        condition: bp.condition,
        hitCount: bp.hitCondition ? parseInt(bp.hitCondition, 10) : undefined,
      }));

      const result = await this.client.setBreakpoints({
        sessionId: this.sessionId,
        add: targets,
      });

      response.body = {
        breakpoints: (result.breakpoints || []).map((bp) => ({
          verified: true,
          source: bp.file ? new Source(path.basename(bp.file), bp.file) : undefined,
          line: bp.line,
        })),
      };
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : 'Failed to set breakpoint';
      response.body = {
        breakpoints: args.breakpoints.map(() => ({
          verified: false,
          message: msg,
        })),
      };
    }

    this.sendResponse(response);
  }

  // ---- Execution Control ----

  protected async continueRequest(
    response: DebugProtocol.ContinueResponse,
    _args: DebugProtocol.ContinueArguments,
  ): Promise<void> {
    await this.doStep('continue');
    response.body = { allThreadsContinued: true };
    this.sendResponse(response);
  }

  protected async nextRequest(
    response: DebugProtocol.NextResponse,
    _args: DebugProtocol.NextArguments,
  ): Promise<void> {
    await this.doStep('step-over');
    this.sendResponse(response);
  }

  protected async stepInRequest(
    response: DebugProtocol.StepInResponse,
    _args: DebugProtocol.StepInArguments,
  ): Promise<void> {
    await this.doStep('step-into');
    this.sendResponse(response);
  }

  protected async stepOutRequest(
    response: DebugProtocol.StepOutResponse,
    _args: DebugProtocol.StepOutArguments,
  ): Promise<void> {
    await this.doStep('step-out');
    this.sendResponse(response);
  }

  private async doStep(action: 'continue' | 'step-over' | 'step-into' | 'step-out'): Promise<void> {
    if (!this.client || !this.sessionId) return;
    try {
      await this.client.continue(this.sessionId, action);
      this.pausedThreads = [];
      this.resetVarRefs();
      this.sendEvent(new ContinuedEvent(1, true));
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      this.sendEvent(new OutputEvent(`Step failed: ${msg}\n`, 'console'));
    }
  }

  // ---- Threads / Stack / Scopes / Variables ----

  protected threadsRequest(response: DebugProtocol.ThreadsResponse): void {
    if (this.pausedThreads.length === 0) {
      response.body = { threads: [new Thread(1, 'main')] };
    } else {
      response.body = {
        threads: this.pausedThreads.map((pt) => {
          const dapId = this.getDapThreadId(pt.threadId);
          return new Thread(dapId, `Thread ${pt.threadId}`);
        }),
      };
    }
    this.sendResponse(response);
  }

  protected stackTraceRequest(
    response: DebugProtocol.StackTraceResponse,
    args: DebugProtocol.StackTraceArguments,
  ): void {
    const fridaThreadId = this.reverseThreadMap.get(args.threadId);
    const paused = this.pausedThreads.find((pt) =>
      fridaThreadId !== undefined ? pt.threadId === fridaThreadId : false,
    );

    if (!paused) {
      response.body = { stackFrames: [], totalFrames: 0 };
      this.sendResponse(response);
      return;
    }

    const frames: DebugProtocol.StackFrame[] = [];

    // Frame 0: the breakpoint location itself
    frames.push(new StackFrame(
      this.allocFrameId(args.threadId, 0),
      paused.function || '<unknown>',
      paused.file ? new Source(path.basename(paused.file), paused.file) : undefined,
      paused.line || 0,
      0,
    ));

    // Remaining frames from backtrace
    const bt = paused.backtrace || [];
    for (let i = 0; i < bt.length; i++) {
      const frame = bt[i];
      frames.push(new StackFrame(
        this.allocFrameId(args.threadId, i + 1),
        frame.functionName || frame.moduleName || `0x${frame.address}`,
        frame.file ? new Source(path.basename(frame.file), frame.file) : undefined,
        frame.line || 0,
        0,
      ));
    }

    const start = args.startFrame || 0;
    const levels = args.levels || frames.length;
    const paged = frames.slice(start, start + levels);

    response.body = { stackFrames: paged, totalFrames: frames.length };
    this.sendResponse(response);
  }

  protected scopesRequest(
    response: DebugProtocol.ScopesResponse,
    args: DebugProtocol.ScopesArguments,
  ): void {
    const scopes: DebugProtocol.Scope[] = [];
    const frameInfo = this.getFrameInfo(args.frameId);

    if (frameInfo && frameInfo.frameIndex === 0) {
      const argsRef = this.allocVarRef({ type: 'arguments', frameId: args.frameId });
      scopes.push(new Scope('Arguments', argsRef, false));

      const localsRef = this.allocVarRef({ type: 'locals', frameId: args.frameId });
      scopes.push(new Scope('Locals', localsRef, false));
    }

    const globalsRef = this.allocVarRef({ type: 'globals', frameId: args.frameId });
    scopes.push(new Scope('Globals', globalsRef, true));

    response.body = { scopes };
    this.sendResponse(response);
  }

  protected async variablesRequest(
    response: DebugProtocol.VariablesResponse,
    args: DebugProtocol.VariablesArguments,
  ): Promise<void> {
    const refData = this.varRefMap.get(args.variablesReference);
    if (!refData) {
      response.body = { variables: [] };
      this.sendResponse(response);
      return;
    }

    const variables: DebugProtocol.Variable[] = [];

    if (refData.type === 'arguments') {
      const paused = this.findPausedThreadForFrame(refData.frameId);
      const capturedArgs = paused?.arguments || [];
      for (const arg of capturedArgs) {
        variables.push({
          name: `arg${arg.index}`,
          value: arg.value,
          variablesReference: 0,
        });
      }
    } else if (refData.type === 'locals') {
      variables.push({
        name: '(locals)',
        value: 'Not available \u2014 requires DWARF local variable support',
        variablesReference: 0,
      });
    } else if (refData.type === 'globals') {
      if (this.client && this.sessionId && this.lastStatus) {
        const watches = this.lastStatus.watches || [];
        for (const w of watches) {
          try {
            const result = await this.client.readMemory({
              sessionId: this.sessionId,
              targets: [{ variable: w.label }],
            });
            variables.push({
              name: w.label,
              value: this.formatReadResult(result),
              type: w.typeName,
              variablesReference: 0,
            });
          } catch {
            variables.push({
              name: w.label,
              value: '<unavailable>',
              variablesReference: 0,
            });
          }
        }
      }
    } else if (refData.type === 'struct') {
      const children = refData.children || [];
      for (const child of children) {
        variables.push({
          name: child.name,
          value: child.value,
          type: child.type,
          variablesReference: child.childRef || 0,
        });
      }
    }

    response.body = { variables };
    this.sendResponse(response);
  }

  protected async evaluateRequest(
    response: DebugProtocol.EvaluateResponse,
    args: DebugProtocol.EvaluateArguments,
  ): Promise<void> {
    if (!this.client || !this.sessionId) {
      response.success = false;
      response.message = 'No active session';
      this.sendResponse(response);
      return;
    }

    try {
      const result = await this.client.readMemory({
        sessionId: this.sessionId,
        targets: [{ variable: args.expression }],
      });
      response.body = {
        result: this.formatReadResult(result),
        variablesReference: 0,
      };
    } catch (e: unknown) {
      const msg = e instanceof Error ? e.message : String(e);
      response.body = {
        result: `<error: ${msg}>`,
        variablesReference: 0,
      };
    }

    this.sendResponse(response);
  }

  // ---- Polling ----

  private startPolling(): void {
    this.schedulePoll();
  }

  private schedulePoll(): void {
    this.pollTimer = setTimeout(() => this.doPoll(), POLL_INTERVAL_MS);
  }

  private async doPoll(): Promise<void> {
    if (this.isPolling) return;
    this.isPolling = true;
    try {
      await this.pollStatus();
    } finally {
      this.isPolling = false;
      // Schedule next poll only if we haven't been stopped
      if (this.pollTimer !== undefined) {
        this.schedulePoll();
      }
    }
  }

  private async pollStatus(): Promise<void> {
    if (!this.client || !this.sessionId) return;
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.lastStatus = status;

      if (status.status === 'paused' && status.pausedThreads && status.pausedThreads.length > 0) {
        const newPause = this.pausedThreads.length === 0;
        this.pausedThreads = status.pausedThreads;

        if (newPause) {
          this.resetVarRefs();
          for (const pt of this.pausedThreads) {
            this.getDapThreadId(pt.threadId);
          }
          // Send StoppedEvent with allThreadsStopped so VS Code shows all threads
          const firstPaused = this.pausedThreads[0];
          const dapThreadId = this.getDapThreadId(firstPaused.threadId);
          const evt = new StoppedEvent('breakpoint', dapThreadId);
          (evt.body as Record<string, unknown>).allThreadsStopped = true;
          this.sendEvent(evt);
        }
      } else if (status.status === 'running') {
        if (this.pausedThreads.length > 0) {
          this.pausedThreads = [];
          this.resetVarRefs();
        }
      } else if (status.status === 'exited') {
        this.stopPolling();
        this.sendEvent(new TerminatedEvent());
      }
    } catch {
      this.stopPolling();
      this.sendEvent(new TerminatedEvent());
    }
  }

  // ---- Helpers ----

  private getDapThreadId(fridaThreadId: number): number {
    let dapId = this.threadMap.get(fridaThreadId);
    if (dapId === undefined) {
      dapId = this.nextDapThreadId++;
      this.threadMap.set(fridaThreadId, dapId);
      this.reverseThreadMap.set(dapId, fridaThreadId);
    }
    return dapId;
  }

  private allocFrameId(threadId: number, frameIndex: number): number {
    return (threadId << 16) | (frameIndex & 0xffff);
  }

  private getFrameInfo(frameId: number): { threadId: number; frameIndex: number } {
    const threadId = (frameId >>> 16) & 0xffff;
    const frameIndex = frameId & 0xffff;
    return { threadId, frameIndex };
  }

  private findPausedThreadForFrame(frameId: number | undefined): PausedThreadInfo | undefined {
    if (frameId === undefined) return this.pausedThreads[0];
    const { threadId: dapThreadId } = this.getFrameInfo(frameId);
    const fridaThreadId = this.reverseThreadMap.get(dapThreadId);
    if (fridaThreadId === undefined) return this.pausedThreads[0];
    return this.pausedThreads.find((pt) => pt.threadId === fridaThreadId) || this.pausedThreads[0];
  }

  private formatReadResult(result: ReadMemoryResponse): string {
    if (!result.results || result.results.length === 0) return '<no data>';
    const r = result.results[0];
    if (r.error) return `<error: ${r.error}>`;
    if (r.value !== undefined && r.value !== null) return String(r.value);
    if (r.fields) return JSON.stringify(r.fields);
    return '<no value>';
  }

  private allocVarRef(data: VarRefData): number {
    const ref = this.nextVarRef++;
    this.varRefMap.set(ref, data);
    return ref;
  }

  private resetVarRefs(): void {
    this.nextVarRef = 1;
    this.varRefMap.clear();
  }

  private stopPolling(): void {
    if (this.pollTimer !== undefined) {
      clearTimeout(this.pollTimer);
      this.pollTimer = undefined;
    }
  }

  private async stopSession(): Promise<void> {
    this.stopPolling();
    if (this.client && this.sessionId) {
      try {
        await this.client.stop(this.sessionId);
      } catch {
        // Session may already be gone
      }
      this.sessionId = undefined;
    }
  }

  /** Returns the Strobe session ID, used by extension.ts to sync UI state */
  public getSessionId(): string | undefined {
    return this.sessionId;
  }
}

interface VarRefData {
  type: 'arguments' | 'globals' | 'locals' | 'struct';
  frameId?: number;
  children?: Array<{ name: string; value: string; type?: string; childRef?: number }>;
}
