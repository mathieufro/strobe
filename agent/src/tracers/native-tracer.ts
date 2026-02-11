// agent/src/tracers/native-tracer.ts
import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage, ReadMemoryMessage, WriteMemoryMessage } from './tracer.js';

export class NativeTracer implements Tracer {
  private agent: any; // Reference to StrobeAgent for delegation

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    // CModuleTracer is initialized in StrobeAgent constructor (existing behavior)
  }

  dispose(): void {
    if (this.agent.cmoduleTracer) {
      this.agent.cmoduleTracer.dispose();
    }
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    // Delegate to existing CModuleTracer hook installation
    return this.agent.installNativeHook(target, mode);
  }

  removeHook(id: number): void {
    this.agent.removeNativeHook(id);
  }

  removeAllHooks(): void {
    this.agent.removeAllNativeHooks();
  }

  activeHookCount(): number {
    return this.agent.cmoduleTracer?.activeHookCount() ?? 0;
  }

  installBreakpoint(msg: BreakpointMessage): void {
    // Delegate to existing setBreakpoint logic
    this.agent.setNativeBreakpoint(msg);
  }

  removeBreakpoint(id: string): void {
    this.agent.removeNativeBreakpoint(id);
  }

  installStepHooks(msg: StepHooksMessage): void {
    this.agent.installNativeStepHooks(msg);
  }

  installLogpoint(msg: LogpointMessage): void {
    this.agent.setNativeLogpoint(msg);
  }

  removeLogpoint(id: string): void {
    this.agent.removeNativeLogpoint(id);
  }

  readVariable(expr: string): any {
    // Native variables go through DWARF → read_memory path, not eval
    throw new Error('Native variables use handleReadMemory, not readVariable');
  }

  writeVariable(expr: string, value: any): void {
    throw new Error('Native variables use handleWriteMemory, not writeVariable');
  }

  handleReadMemory(msg: ReadMemoryMessage): void {
    this.agent.handleNativeReadMemory(msg);
  }

  handleWriteMemory(msg: WriteMemoryMessage): void {
    this.agent.handleNativeWriteMemory(msg);
  }

  setImageBase(imageBase: string): void {
    // Delegate entirely to CModuleTracer to avoid duplicate slide calculation
    if (this.agent.cmoduleTracer) {
      this.agent.cmoduleTracer.setImageBase(imageBase);
    }
  }

  getSlide(): NativePointer {
    // Get slide from CModuleTracer (single source of truth)
    return this.agent.cmoduleTracer?.getSlide() ?? ptr(0);
  }
}
