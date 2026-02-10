// ---- JSON-RPC protocol ----

export interface JsonRpcRequest {
  jsonrpc: '2.0';
  id: string | number;
  method: string;
  params?: unknown;
}

export interface JsonRpcResponse {
  jsonrpc: '2.0';
  id: string | number;
  result?: unknown;
  error?: { code: number; message: string; data?: unknown };
}

// ---- MCP protocol ----

export interface McpToolCallResponse {
  content: Array<{ type: 'text'; text: string }>;
  isError?: boolean;
}

// ---- debug_launch ----

export interface LaunchOptions {
  command: string;
  args?: string[];
  cwd?: string;
  projectRoot: string;
  env?: Record<string, string>;
}

export interface LaunchResponse {
  sessionId: string;
  pid: number;
  pendingPatternsApplied?: number;
  nextSteps?: string;
}

// ---- debug_session ----

export type SessionAction = 'status' | 'stop' | 'list' | 'delete';

export interface SessionStatusResponse {
  status: 'running' | 'paused' | 'exited';
  pid: number;
  eventCount: number;
  hookedFunctions: number;
  tracePatterns: string[];
  breakpoints: BreakpointInfo[];
  logpoints: LogpointInfo[];
  watches: ActiveWatch[];
  pausedThreads: PausedThreadInfo[];
}

export interface PausedThreadInfo {
  threadId: number;
  breakpointId: string;
  function?: string;
  file?: string;
  line?: number;
}

export interface BreakpointInfo {
  id: string;
  function?: string;
  file?: string;
  line?: number;
  address: string;
}

export interface LogpointInfo {
  id: string;
  message: string;
  function?: string;
  file?: string;
  line?: number;
  address: string;
}

export interface ActiveWatch {
  label: string;
  address: string;
  size: number;
  typeName?: string;
  on?: string[];
}

// ---- debug_trace ----

export interface TraceRequest {
  sessionId?: string;
  add?: string[];
  remove?: string[];
  serializationDepth?: number;
  projectRoot?: string;
  watches?: {
    add?: WatchTarget[];
    remove?: string[];
  };
}

export interface WatchTarget {
  variable?: string;
  address?: string;
  type?: string;
  label?: string;
  expr?: string;
  on?: string[];
}

export interface TraceResponse {
  mode: 'pending' | 'runtime';
  activePatterns: string[];
  hookedFunctions: number;
  matchedFunctions?: number;
  activeWatches: ActiveWatch[];
  warnings: string[];
  eventLimit: number;
  status?: string;
}

// ---- debug_query ----

export interface QueryRequest {
  sessionId: string;
  eventType?: string;
  function?: { equals?: string; contains?: string; matches?: string };
  sourceFile?: { equals?: string; contains?: string };
  returnValue?: { equals?: unknown; isNull?: boolean };
  threadName?: { contains?: string };
  timeFrom?: number | string;
  timeTo?: number | string;
  minDurationNs?: number;
  pid?: number;
  limit?: number;
  offset?: number;
  verbose?: boolean;
  afterEventId?: number;
}

export interface QueryResponse {
  events: StrobeEvent[];
  totalCount: number;
  hasMore: boolean;
  pids?: number[];
  lastEventId?: number;
  eventsDropped?: boolean;
}

export interface StrobeEvent {
  id: string;
  timestamp_ns: number;
  eventType?: string;
  function?: string;
  sourceFile?: string;
  line?: number;
  duration_ns?: number;
  returnType?: string;
  // verbose fields
  threadId?: number;
  threadName?: string;
  pid?: number;
  arguments?: unknown;
  returnValue?: unknown;
  watchValues?: Record<string, unknown>;
  text?: string; // for stdout/stderr
  logpointMessage?: string;
  // crash fields
  signal?: string;
  faultAddress?: string;
  // variable_snapshot
  data?: unknown;
}

// ---- debug_breakpoint ----

export interface BreakpointRequest {
  sessionId: string;
  add?: BreakpointTarget[];
  remove?: string[];
}

export interface BreakpointTarget {
  function?: string;
  file?: string;
  line?: number;
  condition?: string;
  hitCount?: number;
  message?: string; // present = logpoint
}

export interface BreakpointResponse {
  breakpoints: BreakpointInfo[];
  logpoints: LogpointInfo[];
}

// ---- debug_continue ----

export type StepAction = 'continue' | 'step-over' | 'step-into' | 'step-out';

export interface ContinueResponse {
  status: string;
  breakpointId?: string;
  function?: string;
  file?: string;
  line?: number;
}

// ---- debug_memory ----

export interface MemoryReadRequest {
  sessionId: string;
  action?: 'read';
  targets: Array<{
    variable?: string;
    address?: string;
    size?: number;
    type?: string;
  }>;
  depth?: number;
  poll?: { intervalMs: number; durationMs: number };
}

// ---- debug_test ----

export interface TestRunRequest {
  action?: 'run';
  projectRoot: string;
  framework?: string;
  level?: string;
  test?: string;
  command?: string;
  tracePatterns?: string[];
  env?: Record<string, string>;
}

export interface TestStartResponse {
  testRunId: string;
  status: 'running';
  framework: string;
}

export interface TestStatusResponse {
  testRunId: string;
  status: 'running' | 'completed' | 'failed';
  sessionId?: string;
  progress?: {
    elapsedMs: number;
    passed: number;
    failed: number;
    skipped: number;
    currentTest?: string;
    currentTestElapsedMs?: number;
    phase?: string;
    warnings?: Array<{
      testName?: string;
      idleMs: number;
      diagnosis: string;
    }>;
  };
  result?: {
    framework: string;
    summary: {
      passed: number;
      failed: number;
      skipped: number;
      durationMs: number;
    };
    failures: Array<{
      name: string;
      file?: string;
      line?: number;
      message: string;
      suggestedTraces: string[];
    }>;
    stuck: unknown[];
  };
  error?: string;
}

// ---- Error codes ----

export const StrobeErrorCodes = {
  NO_DEBUG_SYMBOLS: 'NO_DEBUG_SYMBOLS',
  SIP_BLOCKED: 'SIP_BLOCKED',
  SESSION_EXISTS: 'SESSION_EXISTS',
  SESSION_NOT_FOUND: 'SESSION_NOT_FOUND',
  PROCESS_EXITED: 'PROCESS_EXITED',
  FRIDA_ATTACH_FAILED: 'FRIDA_ATTACH_FAILED',
  INVALID_PATTERN: 'INVALID_PATTERN',
  VALIDATION_ERROR: 'VALIDATION_ERROR',
} as const;
