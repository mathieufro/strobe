import * as net from 'net';
import * as path from 'path';
import * as os from 'os';
import * as fs from 'fs';
import * as cp from 'child_process';
import { StrobeClient } from '../client/strobe-client';

const STROBE_DIR = path.join(os.homedir(), '.strobe');
const SOCKET_PATH = path.join(STROBE_DIR, 'strobe.sock');
const PID_PATH = path.join(STROBE_DIR, 'strobe.pid');

export class DaemonManager {
  private binaryPath: string;
  private client: StrobeClient | null = null;
  private connectPromise: Promise<StrobeClient> | null = null;

  constructor(extensionPath: string) {
    // Binary bundled at <extensionPath>/bin/strobe
    // During development, fall back to PATH or target/debug
    const bundledPath = path.join(extensionPath, 'bin', 'strobe');
    if (fs.existsSync(bundledPath)) {
      this.binaryPath = bundledPath;
    } else {
      // Development fallback: use strobe from PATH
      this.binaryPath = 'strobe';
    }
  }

  async ensureClient(): Promise<StrobeClient> {
    if (this.client?.isConnected) return this.client;
    if (this.connectPromise) return this.connectPromise;
    this.connectPromise = this.doEnsureClient();
    try {
      return await this.connectPromise;
    } finally {
      this.connectPromise = null;
    }
  }

  private async doEnsureClient(): Promise<StrobeClient> {
    // Try connecting to existing daemon
    if (await this.tryConnect()) {
      return this.client!;
    }

    // Start daemon and connect
    await this.startDaemon();
    if (await this.tryConnect(50, 100)) {
      // 50 attempts x 100ms = 5s
      return this.client!;
    }

    throw new Error(
      'Daemon failed to start within 5 seconds. Check ~/.strobe/daemon.log',
    );
  }

  private async tryConnect(
    attempts = 1,
    delayMs = 0,
  ): Promise<boolean> {
    for (let i = 0; i < attempts; i++) {
      if (i > 0 && delayMs > 0) {
        await new Promise((r) => setTimeout(r, delayMs));
      }
      try {
        this.client = new StrobeClient();
        await this.client.connect();
        return true;
      } catch {
        this.client = null;
      }
    }
    return false;
  }

  private async startDaemon(): Promise<void> {
    // Ensure ~/.strobe/ exists
    fs.mkdirSync(STROBE_DIR, { recursive: true });

    // Clean stale files (mirrors proxy.rs:177-191)
    this.cleanupStaleFiles();

    // Spawn daemon
    const logPath = path.join(STROBE_DIR, 'daemon.log');
    const logFd = fs.openSync(logPath, 'a');

    const child = cp.spawn(this.binaryPath, ['daemon'], {
      detached: true,
      stdio: ['ignore', 'ignore', logFd],
      env: {
        ...process.env,
        RUST_LOG: process.env.RUST_LOG ?? 'info',
      },
    });

    child.unref();
    fs.closeSync(logFd);
  }

  private cleanupStaleFiles(): void {
    try {
      const pidStr = fs.readFileSync(PID_PATH, 'utf-8').trim();
      const pid = parseInt(pidStr, 10);
      if (isNaN(pid)) return;

      try {
        // Check if process is alive (signal 0 doesn't kill, just checks)
        process.kill(pid, 0);
        // Process is alive — don't clean up
      } catch {
        // Process is dead — clean stale files
        try {
          fs.unlinkSync(SOCKET_PATH);
        } catch {}
        try {
          fs.unlinkSync(PID_PATH);
        } catch {}
      }
    } catch {
      // No PID file — nothing to clean
    }
  }

  getClient(): StrobeClient | null {
    return this.client?.isConnected ? this.client : null;
  }

  dispose(): void {
    this.client?.disconnect();
    this.client = null;
  }
}
