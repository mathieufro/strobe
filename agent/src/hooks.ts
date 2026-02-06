import { CModuleTracer, HookMode } from './cmodule-tracer.js';

interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

export { HookMode } from './cmodule-tracer.js';

export class HookInstaller {
  private tracer: CModuleTracer;

  constructor(onEvents: (events: any[]) => void) {
    this.tracer = new CModuleTracer(onEvents);
  }

  setImageBase(imageBase: string): void {
    this.tracer.setImageBase(imageBase);
  }

  setSessionId(sessionId: string): void {
    this.tracer.setSessionId(sessionId);
  }

  installHook(func: FunctionTarget, mode: HookMode = 'full'): boolean {
    return this.tracer.installHook(func, mode);
  }

  removeHook(address: string): void {
    this.tracer.removeHook(address);
  }

  activeHookCount(): number {
    return this.tracer.activeHookCount();
  }

  removeAll(): void {
    this.tracer.removeAll();
  }

  updateWatches(watches: Parameters<CModuleTracer['updateWatches']>[0]): void {
    this.tracer.updateWatches(watches);
  }

  updateExprWatches(exprs: Parameters<CModuleTracer['updateExprWatches']>[0]): void {
    this.tracer.updateExprWatches(exprs);
  }
}
