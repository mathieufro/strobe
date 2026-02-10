import * as net from 'net';
import * as path from 'path';
import * as os from 'os';
import { EventEmitter } from 'events';
import {
  JsonRpcRequest,
  JsonRpcResponse,
  McpToolCallResponse,
  LaunchOptions,
  LaunchResponse,
  SessionStatusResponse,
  TraceRequest,
  TraceResponse,
  QueryRequest,
  QueryResponse,
  BreakpointRequest,
  BreakpointResponse,
  StepAction,
  ContinueResponse,
  TestRunRequest,
  TestStartResponse,
  TestStatusResponse,
  ReadMemoryResponse,
  MemoryWriteRequest,
  WriteMemoryResponse,
  ListSessionsResponse,
} from './types';

const SOCKET_PATH = path.join(os.homedir(), '.strobe', 'strobe.sock');
const PROTOCOL_VERSION = '2024-11-05';

export class StrobeClient extends EventEmitter {
  private socket: net.Socket | null = null;
  private buffer = '';
  private requestId = 0;
  private pending = new Map<
    string | number,
    {
      resolve: (value: unknown) => void;
      reject: (err: Error) => void;
    }
  >();
  private _connected = false;

  get isConnected(): boolean {
    return this._connected;
  }

  async connect(): Promise<void> {
    if (this._connected) return;

    this.socket = net.createConnection(SOCKET_PATH);

    await new Promise<void>((resolve, reject) => {
      const onConnect = (): void => {
        cleanup();
        resolve();
      };
      const onError = (err: Error): void => {
        cleanup();
        reject(err);
      };
      const cleanup = (): void => {
        this.socket!.removeListener('connect', onConnect);
        this.socket!.removeListener('error', onError);
      };
      this.socket!.once('connect', onConnect);
      this.socket!.once('error', onError);
    });

    this.socket.on('data', (data) => this.onData(data));
    this.socket.on('close', () => this.onClose());
    this.socket.on('error', (err) => this.emit('error', err));

    this._connected = true;

    // MCP handshake: initialize
    await this.sendRequest('initialize', {
      protocolVersion: PROTOCOL_VERSION,
      capabilities: {},
      clientInfo: { name: 'strobe-vscode', version: '0.1.0' },
    });

    // Send initialized notification (no response expected)
    this.sendNotification('notifications/initialized', {});
  }

  disconnect(): void {
    if (this.socket) {
      this.socket.destroy();
      this.socket = null;
    }
    this._connected = false;
    // Reject all pending requests
    for (const [, p] of this.pending) {
      p.reject(new Error('Disconnected'));
    }
    this.pending.clear();
  }

  // ---- Tool methods (map to 8 consolidated MCP tools) ----

  async launch(opts: LaunchOptions): Promise<LaunchResponse> {
    return this.callTool('debug_launch', opts) as Promise<LaunchResponse>;
  }

  async sessionStatus(
    sessionId: string,
  ): Promise<SessionStatusResponse> {
    return this.callTool('debug_session', {
      action: 'status',
      sessionId,
    }) as Promise<SessionStatusResponse>;
  }

  async stop(sessionId: string, retain = false): Promise<unknown> {
    return this.callTool('debug_session', {
      action: 'stop',
      sessionId,
      retain,
    });
  }

  async listSessions(): Promise<ListSessionsResponse> {
    return this.callTool('debug_session', { action: 'list' }) as Promise<ListSessionsResponse>;
  }

  async deleteSession(sessionId: string): Promise<unknown> {
    return this.callTool('debug_session', {
      action: 'delete',
      sessionId,
    });
  }

  async trace(req: TraceRequest): Promise<TraceResponse> {
    return this.callTool('debug_trace', req) as Promise<TraceResponse>;
  }

  async query(req: QueryRequest): Promise<QueryResponse> {
    return this.callTool('debug_query', req) as Promise<QueryResponse>;
  }

  async setBreakpoints(
    req: BreakpointRequest,
  ): Promise<BreakpointResponse> {
    return this.callTool(
      'debug_breakpoint',
      req,
    ) as Promise<BreakpointResponse>;
  }

  async continue(
    sessionId: string,
    action?: StepAction,
  ): Promise<ContinueResponse> {
    return this.callTool('debug_continue', {
      sessionId,
      action,
    }) as Promise<ContinueResponse>;
  }

  async writeMemory(req: MemoryWriteRequest): Promise<WriteMemoryResponse> {
    return this.callTool('debug_memory', req) as Promise<WriteMemoryResponse>;
  }

  async readMemory(req: {
    sessionId: string;
    targets: Array<{
      variable?: string;
      address?: string;
      size?: number;
      type?: string;
    }>;
    depth?: number;
  }): Promise<ReadMemoryResponse> {
    return this.callTool('debug_memory', { ...req, action: 'read' }) as Promise<ReadMemoryResponse>;
  }

  async runTest(req: TestRunRequest): Promise<TestStartResponse> {
    return this.callTool('debug_test', {
      ...req,
      action: 'run',
    }) as Promise<TestStartResponse>;
  }

  async testStatus(testRunId: string): Promise<TestStatusResponse> {
    return this.callTool('debug_test', {
      action: 'status',
      testRunId,
    }) as Promise<TestStatusResponse>;
  }

  // ---- Protocol layer ----

  private async callTool(name: string, args: unknown): Promise<unknown> {
    const response = (await this.sendRequest('tools/call', {
      name,
      arguments: args,
    })) as McpToolCallResponse;

    if (response.isError) {
      const text = response.content?.[0]?.text ?? 'Unknown error';
      // Server format: "ERROR_CODE": message  (code is JSON-quoted)
      const codeMatch = text.match(/^"([A-Z_]+)":\s*/);
      const code = codeMatch?.[1];
      const message = codeMatch ? text.slice(codeMatch[0].length) : text;
      throw new StrobeError(message, code);
    }

    // Tool responses wrap the actual JSON in a text content block
    const text = response.content?.[0]?.text;
    if (!text) return {};
    try {
      return JSON.parse(text);
    } catch {
      throw new StrobeError(`Invalid JSON response from daemon: ${text.slice(0, 200)}`);
    }
  }

  private sendRequest(
    method: string,
    params: unknown,
  ): Promise<unknown> {
    return new Promise((resolve, reject) => {
      const id = ++this.requestId;
      this.pending.set(id, { resolve, reject });

      const msg: JsonRpcRequest = {
        jsonrpc: '2.0',
        id,
        method,
        params,
      };

      this.socket!.write(JSON.stringify(msg) + '\n');
    });
  }

  private sendNotification(method: string, params: unknown): void {
    const msg = { jsonrpc: '2.0', method, params };
    this.socket!.write(JSON.stringify(msg) + '\n');
  }

  private onData(data: Buffer): void {
    this.buffer += data.toString();
    let newlineIdx: number;
    while ((newlineIdx = this.buffer.indexOf('\n')) !== -1) {
      const line = this.buffer.slice(0, newlineIdx);
      this.buffer = this.buffer.slice(newlineIdx + 1);
      if (line.trim()) {
        this.handleMessage(line);
      }
    }
  }

  private handleMessage(line: string): void {
    try {
      const msg = JSON.parse(line) as JsonRpcResponse;
      const p = this.pending.get(msg.id);
      if (p) {
        this.pending.delete(msg.id);
        if (msg.error) {
          p.reject(
            new StrobeError(
              msg.error.message,
              msg.error.data as string | undefined,
            ),
          );
        } else {
          p.resolve(msg.result);
        }
      }
    } catch {
      // Ignore malformed messages
    }
  }

  private onClose(): void {
    this._connected = false;
    for (const [, p] of this.pending) {
      p.reject(new Error('Connection closed'));
    }
    this.pending.clear();
    this.emit('disconnected');
  }
}

export class StrobeError extends Error {
  public readonly code?: string;
  constructor(message: string, code?: string) {
    super(message);
    this.name = 'StrobeError';
    this.code = code;
  }
}
