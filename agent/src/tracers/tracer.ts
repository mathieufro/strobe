// agent/src/tracers/tracer.ts
// Core tracer interface — all language tracers implement this contract.

export type HookMode = 'full' | 'light';

export interface ResolvedTarget {
  // For native: instruction address (hex)
  address?: string;
  // For interpreted: source location
  file?: string;
  line?: number;
  // Common
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

export interface Tracer {
  // Lifecycle
  initialize(sessionId: string): void;
  dispose(): void;

  // Hook management (resolved targets from daemon)
  installHook(target: ResolvedTarget, mode: HookMode): number | null;
  removeHook(id: number): void;
  removeAllHooks(): void;
  activeHookCount(): number;

  // Breakpoints
  installBreakpoint(msg: BreakpointMessage): void;
  removeBreakpoint(id: string): void;

  // Stepping
  installStepHooks(msg: StepHooksMessage): void;

  // Logpoints
  installLogpoint(msg: LogpointMessage): void;
  removeLogpoint(id: string): void;

  // Variable access
  readVariable(expr: string): any;
  writeVariable(expr: string, value: any): void;

  // Memory access (native only — interpreted languages use readVariable/writeVariable)
  handleReadMemory?(msg: ReadMemoryMessage): void;
  handleWriteMemory?(msg: WriteMemoryMessage): void;

  // ASLR slide (native only, no-op for interpreted)
  setImageBase(imageBase: string): void;
  getSlide(): NativePointer;

  // Runtime resolution fallback (for dynamic functions not in static AST)
  resolvePattern?(pattern: string): ResolvedTarget[];
}

// Message types used by the tracer interface
export interface BreakpointMessage {
  address?: string;
  file?: string;
  line?: number;
  id: string;
  condition?: string;
  hitCount?: number;
  funcName?: string;
  imageBase?: string;
}

export interface StepHooksMessage {
  threadId: number;
  oneShot: Array<{ address: string; noSlide?: boolean }>;
  imageBase?: string;
  returnAddress?: string | null;
}

export interface LogpointMessage {
  address?: string;
  file?: string;
  line?: number;
  id: string;
  message: string;
  condition?: string;
  funcName?: string;
  imageBase?: string;
}

export interface ReadMemoryMessage {
  recipes: any[];
  imageBase?: string;
  poll?: { intervalMs: number; durationMs: number };
}

export interface WriteMemoryMessage {
  recipes: any[];  // Changed from 'targets' to match agent.ts
  imageBase?: string;
}
