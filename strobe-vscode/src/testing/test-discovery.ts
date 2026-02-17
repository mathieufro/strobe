import * as cp from 'child_process';
import * as fs from 'fs';
import * as path from 'path';

export interface DiscoveredTest {
  name: string;
  file?: string;
  line?: number;
}

export interface TestDiscoverer {
  /** Framework name (matches daemon's adapter name) */
  readonly framework: string;
  /** Confidence 0-100 that this framework applies to the workspace */
  detect(workspaceFolder: string): Promise<number>;
  /** List all tests in the workspace */
  listTests(workspaceFolder: string): Promise<DiscoveredTest[]>;
}

export class CargoDiscoverer implements TestDiscoverer {
  readonly framework = 'cargo';

  async detect(workspaceFolder: string): Promise<number> {
    const cargoPath = path.join(workspaceFolder, 'Cargo.toml');
    try {
      await fs.promises.access(cargoPath);
      return 90;
    } catch {
      return 0;
    }
  }

  async listTests(workspaceFolder: string): Promise<DiscoveredTest[]> {
    // Run: cargo test --tests -- --list
    // Output format: "test_name: test\n" per test
    return new Promise((resolve) => {
      const proc = cp.spawn('cargo', ['test', '--tests', '--', '--list'], {
        cwd: workspaceFolder,
        env: process.env,
      });

      let stdout = '';
      proc.stdout.on('data', (d: Buffer) => {
        stdout += d;
      });
      proc.stderr.on('data', () => {
        /* discard compilation output */
      });

      const timeout = setTimeout(() => {
        proc.kill();
      }, 30_000); // 30s timeout for cargo build + list

      proc.on('close', (code) => {
        clearTimeout(timeout);
        if (code !== 0) {
          // Compilation may fail — return empty list, not an error.
          // The actual failure will surface when tests are run.
          resolve([]);
          return;
        }

        const tests: DiscoveredTest[] = [];
        for (const line of stdout.split('\n')) {
          // Format: "module::test_name: test"
          const match = line.match(/^(.+):\s+test$/);
          if (match) {
            tests.push({ name: match[1] });
          }
        }
        resolve(tests);
      });

      proc.on('error', () => {
        clearTimeout(timeout);
        // cargo not in PATH — return empty
        resolve([]);
      });
    });
  }
}

export class GoTestDiscoverer implements TestDiscoverer {
  readonly framework = 'go';

  async detect(workspaceFolder: string): Promise<number> {
    const goModPath = path.join(workspaceFolder, 'go.mod');
    try {
      await fs.promises.access(goModPath);
      return 85;
    } catch {
      return 0;
    }
  }

  async listTests(workspaceFolder: string): Promise<DiscoveredTest[]> {
    return new Promise((resolve) => {
      const proc = cp.spawn('go', ['test', '-list', '.*', './...'], {
        cwd: workspaceFolder,
        env: process.env,
      });

      let stdout = '';
      proc.stdout.on('data', (d: Buffer) => { stdout += d; });
      proc.stderr.on('data', () => { /* discard */ });

      const timeout = setTimeout(() => {
        proc.kill();
      }, 30_000); // 30s timeout for go test -list

      proc.on('close', (code) => {
        clearTimeout(timeout);
        if (code !== 0) {
          resolve([]);
          return;
        }
        const tests: DiscoveredTest[] = [];
        for (const line of stdout.split('\n')) {
          const trimmed = line.trim();
          // go test -list outputs test names, one per line
          // Skip "ok" lines, "?" lines, and empty lines
          if (trimmed && !trimmed.startsWith('ok ') && !trimmed.startsWith('?')) {
            tests.push({ name: trimmed });
          }
        }
        resolve(tests);
      });

      proc.on('error', () => {
        clearTimeout(timeout);
        resolve([]);
      });
    });
  }
}

/**
 * Auto-detect the best discoverer for a workspace.
 * Returns the highest-confidence discoverer, or undefined if none match.
 */
export async function detectDiscoverer(
  workspaceFolder: string,
  discoverers: TestDiscoverer[] = [new CargoDiscoverer(), new GoTestDiscoverer()],
): Promise<TestDiscoverer | undefined> {
  let best: TestDiscoverer | undefined;
  let bestScore = 0;
  for (const d of discoverers) {
    const score = await d.detect(workspaceFolder);
    if (score > bestScore) {
      bestScore = score;
      best = d;
    }
  }
  return bestScore > 0 ? best : undefined;
}
