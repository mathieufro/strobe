# M2: Test Runner Integration — Implementation Plan

**Spec:** `docs/specs/2026-02-10-vscode-extension.md` (M2 section)
**Goal:** Integrate Strobe's test runner into VS Code's native Test Explorer with live progress, stuck detection warnings, failure details with file:line links, suggested traces, and CodeLens on test functions.
**Architecture:** The extension uses VS Code's `TestController` API. Test **discovery** runs extension-side (e.g., `cargo test -- --list`), building a `TestItem` tree. Test **execution** delegates to the daemon via `debug_test({ action: "run" })`, polling `debug_test({ action: "status" })` every 1s for progress. The daemon handles Frida instrumentation, framework detection, and stuck detection — the extension just maps results to VS Code's Testing API.
**Tech Stack:** VS Code Testing API (`TestController`, `TestRunProfile`, `TestItem`, `TestMessage`), existing `StrobeClient`
**Commit strategy:** Single commit at end

## Workstreams

Two independent streams that converge in Task 5:

- **Stream A (Discovery):** Tasks 1, 2 — `TestDiscoverer` interface + `CargoDiscoverer`
- **Stream B (Execution + UI):** Tasks 3, 4 — `TestController` + status polling + CodeLens
- **Serial:** Task 5 (wire into extension.ts), Task 6 (verify build)

---

### Task 1: TestDiscoverer Interface + CargoDiscoverer

**Files:**
- Create: `strobe-vscode/src/testing/test-discovery.ts`

**Step 1: Write `src/testing/test-discovery.ts`**

The discoverer interface + Cargo implementation. Discovery runs extension-side (no daemon needed) — it shells out to `cargo test -- --list` and parses the output.

```typescript
import * as cp from 'child_process';
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
    // Check for Cargo.toml
    const fs = await import('fs');
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
    return new Promise((resolve, reject) => {
      const proc = cp.spawn('cargo', ['test', '--tests', '--', '--list'], {
        cwd: workspaceFolder,
        env: { ...process.env, RUSTC_BOOTSTRAP: '1' },
      });

      let stdout = '';
      let stderr = '';
      proc.stdout.on('data', (d) => { stdout += d; });
      proc.stderr.on('data', (d) => { stderr += d; });

      proc.on('close', (code) => {
        if (code !== 0) {
          // Compilation may fail — return empty list, not an error
          // The actual failure will surface when tests are run
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

      proc.on('error', (err) => {
        // cargo not in PATH — return empty
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
  discoverers: TestDiscoverer[] = [new CargoDiscoverer()],
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
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** `CargoDiscoverer` can detect Rust projects and list all test names by running `cargo test --tests -- --list`.

---

### Task 2: Add `running_tests` to TestStatusResponse

**Files:**
- Modify: `strobe-vscode/src/client/types.ts`

The daemon's `TestProgressSnapshot` includes a `running_tests` field (parallel test execution in Cargo) and `current_test_baseline_ms`. The extension's type is missing these. Also add `suggestedTraces` to stuck warnings.

**Step 1: Update `TestStatusResponse.progress` in types.ts**

Add after `currentTestElapsedMs`:

```typescript
    currentTestBaselineMs?: number;
    runningTests?: Array<{
      name: string;
      elapsedMs: number;
      baselineMs?: number;
    }>;
```

Update stuck warning type to include `suggestedTraces`:

```typescript
    warnings?: Array<{
      testName?: string;
      idleMs: number;
      diagnosis: string;
      suggestedTraces?: string[];
    }>;
```

Add to `result`:

```typescript
    stuck: Array<{
      name: string;
      elapsedMs: number;
      diagnosis: string;
      threads?: Array<{ name: string; stack: string[] }>;
      suggestedTraces?: string[];
    }>;
```

Also add `noTests`, `hint`, and `rerun` fields to match daemon:

```typescript
    noTests?: boolean;
    hint?: string;
    // each failure:
    // add rerun?: string; to failure type
```

**Step 2: Expand the failure type**

Change failures array element to include `rerun`:

```typescript
    failures: Array<{
      name: string;
      file?: string;
      line?: number;
      message: string;
      rerun?: string;
      suggestedTraces: string[];
    }>;
```

**Step 3: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS (adding optional fields is backward-compatible)

**Checkpoint:** Extension types fully mirror the daemon's `DebugTestStatusResponse` / `TestProgressSnapshot` / `TestStuckWarning` / `TestFailure` structs.

---

### Task 3: TestController — VS Code Testing API Integration

**Files:**
- Create: `strobe-vscode/src/testing/test-controller.ts`

This is the core file. It creates a VS Code `TestController`, populates it from discovery, and runs tests via the daemon.

**Step 1: Write `src/testing/test-controller.ts`**

```typescript
import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';
import { TestStatusResponse } from '../client/types';
import { TestDiscoverer, DiscoveredTest, detectDiscoverer, CargoDiscoverer } from './test-discovery';

export class StrobeTestController {
  private controller: vscode.TestController;
  private discoverer: TestDiscoverer | undefined;
  private activeTestRunId: string | undefined;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private testItemMap = new Map<string, vscode.TestItem>();
  // Disposables for cleanup
  private disposables: vscode.Disposable[] = [];

  constructor(
    private getClient: () => Promise<StrobeClient>,
    private outputChannel: { appendLine(text: string): void; show(): void },
  ) {
    this.controller = vscode.tests.createTestController(
      'strobe.testController',
      'Strobe Tests',
    );

    // Run profile: runs tests with Strobe's daemon
    const runProfile = this.controller.createRunProfile(
      'Run',
      vscode.TestRunProfileKind.Run,
      (request, token) => this.runTests(request, token),
    );

    // Debug profile: runs with pre-loaded trace patterns
    const debugProfile = this.controller.createRunProfile(
      'Debug with Strobe',
      vscode.TestRunProfileKind.Debug,
      (request, token) => this.runTests(request, token, true),
    );

    // Resolve handler: called when user expands the test tree
    this.controller.resolveHandler = async () => {
      await this.discoverTests();
    };

    this.disposables.push(runProfile, debugProfile);
  }

  async discoverTests(): Promise<void> {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!workspaceFolder) return;

    this.discoverer = await detectDiscoverer(workspaceFolder);
    if (!this.discoverer) return;

    const tests = await this.discoverer.listTests(workspaceFolder);

    // Clear existing items
    this.testItemMap.clear();
    this.controller.items.replace([]);

    // Build test tree: group by module path
    // e.g., "audio::midi::test_parse" → audio > midi > test_parse
    const rootItems: vscode.TestItem[] = [];
    const groups = new Map<string, vscode.TestItem>();

    for (const test of tests) {
      const parts = test.name.split('::');
      const testName = parts[parts.length - 1];
      const modulePath = parts.slice(0, -1);

      // Create or find parent groups
      let parent: vscode.TestItem | undefined;
      let parentKey = '';
      for (const part of modulePath) {
        parentKey = parentKey ? `${parentKey}::${part}` : part;
        if (!groups.has(parentKey)) {
          const groupItem = this.controller.createTestItem(parentKey, part);
          groups.set(parentKey, groupItem);
          if (parent) {
            parent.children.add(groupItem);
          } else {
            rootItems.push(groupItem);
          }
        }
        parent = groups.get(parentKey)!;
      }

      // Create test item
      const item = this.controller.createTestItem(test.name, testName);
      if (test.file) {
        item.uri = vscode.Uri.file(test.file);
        if (test.line) {
          item.range = new vscode.Range(test.line - 1, 0, test.line - 1, 0);
        }
      }
      this.testItemMap.set(test.name, item);

      if (parent) {
        parent.children.add(item);
      } else {
        rootItems.push(item);
      }
    }

    this.controller.items.replace(rootItems);
  }

  private async runTests(
    request: vscode.TestRunRequest,
    token: vscode.CancellationToken,
    debug = false,
  ): Promise<void> {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!workspaceFolder) return;

    const run = this.controller.createTestRun(request);

    // Determine which tests to run
    let testFilter: string | undefined;
    const requestedItems: vscode.TestItem[] = [];

    if (request.include && request.include.length > 0) {
      // Specific tests requested
      if (request.include.length === 1) {
        const item = request.include[0];
        // If it's a leaf test (no children), filter to that test
        if (item.children.size === 0) {
          testFilter = item.id;
        } else {
          // It's a module group — run all children
          // Don't set testFilter; let daemon run all, we'll filter display
        }
      }
      this.collectTestItems(request.include, requestedItems);
    } else {
      // Run all
      this.controller.items.forEach((item) =>
        this.collectTestItems([item], requestedItems),
      );
    }

    // Mark all as queued
    for (const item of requestedItems) {
      if (item.children.size === 0) {
        run.enqueued(item);
      }
    }

    try {
      const client = await this.getClient();

      // Build trace patterns for debug mode
      const tracePatterns = debug ? this.getSuggestedTraces(request.include) : undefined;

      // Start test run via daemon
      const startResp = await client.runTest({
        projectRoot: workspaceFolder,
        test: testFilter,
        tracePatterns: tracePatterns?.length ? tracePatterns : undefined,
        framework: this.discoverer?.framework,
      });

      this.activeTestRunId = startResp.testRunId;

      if (debug) {
        this.outputChannel.appendLine(
          `Strobe: Running tests with Frida instrumentation (${startResp.framework})`,
        );
        this.outputChannel.show();
      }

      // Poll until complete
      await this.pollTestStatus(client, startResp.testRunId, run, token);
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      // Mark all as errored
      for (const item of requestedItems) {
        if (item.children.size === 0) {
          run.errored(item, new vscode.TestMessage(msg));
        }
      }
    } finally {
      this.activeTestRunId = undefined;
      run.end();
    }
  }

  private async pollTestStatus(
    client: StrobeClient,
    testRunId: string,
    run: vscode.TestRun,
    token: vscode.CancellationToken,
  ): Promise<void> {
    const startedTests = new Set<string>();

    while (!token.isCancellationRequested) {
      const status = await client.testStatus(testRunId);

      if (status.progress) {
        const p = status.progress;

        // Mark running tests as started
        if (p.runningTests) {
          for (const rt of p.runningTests) {
            if (!startedTests.has(rt.name)) {
              startedTests.add(rt.name);
              const item = this.testItemMap.get(rt.name);
              if (item) run.started(item);
            }
          }
        } else if (p.currentTest && !startedTests.has(p.currentTest)) {
          startedTests.add(p.currentTest);
          const item = this.testItemMap.get(p.currentTest);
          if (item) run.started(item);
        }

        // Surface stuck warnings as test messages
        if (p.warnings) {
          for (const w of p.warnings) {
            const msg = `\u26A0 ${w.diagnosis} (idle ${Math.round(w.idleMs / 1000)}s)`;
            this.outputChannel.appendLine(`Strobe Test: ${w.testName ?? 'unknown'}: ${msg}`);
            if (w.testName) {
              const item = this.testItemMap.get(w.testName);
              if (item) {
                run.appendOutput(`\u26A0 STUCK: ${w.diagnosis}\r\n`, undefined, item);
              }
            }
          }
        }
      }

      // Terminal states
      if (status.status === 'completed' && status.result) {
        this.applyResults(run, status);
        return;
      }

      if (status.status === 'failed') {
        // Test infrastructure failure (not a test failure)
        const errMsg = status.error ?? 'Test run failed';
        this.outputChannel.appendLine(`Strobe Test: ${errMsg}`);
        // Mark remaining queued tests as errored
        for (const [, item] of this.testItemMap) {
          if (!startedTests.has(item.id)) {
            run.errored(item, new vscode.TestMessage(errMsg));
          }
        }
        return;
      }

      // Server blocks up to 15s, so 1s interval avoids busy-waiting
      await new Promise((r) => setTimeout(r, 1000));
    }

    // Cancelled
    this.outputChannel.appendLine('Strobe Test: Run cancelled');
  }

  private applyResults(run: vscode.TestRun, status: TestStatusResponse): void {
    const result = status.result!;

    // Build a set of failed/stuck test names for quick lookup
    const failedNames = new Set(result.failures.map((f) => f.name));
    const stuckNames = new Set(
      (result.stuck as Array<{ name: string }>).map((s) => s.name),
    );

    // Process failures
    for (const failure of result.failures) {
      const item = this.testItemMap.get(failure.name);
      if (!item) continue;

      const msg = new vscode.TestMessage(failure.message);

      // Set location if available
      if (failure.file) {
        const uri = vscode.Uri.file(failure.file);
        const line = (failure.line ?? 1) - 1;
        msg.location = new vscode.Location(uri, new vscode.Position(line, 0));
      }

      run.failed(item, msg);

      // Append suggested traces as output
      if (failure.suggestedTraces.length > 0) {
        run.appendOutput(
          `Suggested traces: ${failure.suggestedTraces.join(', ')}\r\n`,
          undefined,
          item,
        );
      }
    }

    // Process stuck tests
    for (const stuck of result.stuck as Array<{
      name: string;
      elapsedMs: number;
      diagnosis: string;
      suggestedTraces?: string[];
    }>) {
      const item = this.testItemMap.get(stuck.name);
      if (item) {
        run.failed(
          item,
          new vscode.TestMessage(`STUCK: ${stuck.diagnosis} (${Math.round(stuck.elapsedMs / 1000)}s)`),
        );
      }
    }

    // Mark passed tests (anything not failed or stuck)
    for (const [name, item] of this.testItemMap) {
      if (!failedNames.has(name) && !stuckNames.has(name)) {
        // Could be passed or skipped — check if it was in the run
        // The daemon doesn't return individual pass events, only summary counts
        // So mark as passed if not in failure/stuck sets
        run.passed(item);
      }
    }

    // Summary in output
    const s = result.summary;
    this.outputChannel.appendLine(
      `Strobe Test: ${s.passed} passed, ${s.failed} failed, ${s.skipped} skipped (${(s.durationMs / 1000).toFixed(1)}s)`,
    );
  }

  private collectTestItems(
    items: readonly vscode.TestItem[],
    out: vscode.TestItem[],
  ): void {
    for (const item of items) {
      out.push(item);
      item.children.forEach((child) => this.collectTestItems([child], out));
    }
  }

  private getSuggestedTraces(
    include: readonly vscode.TestItem[] | undefined,
  ): string[] | undefined {
    // For debug mode, if a specific test was selected and it has prior failure data
    // with suggested traces, use those. Otherwise return undefined.
    // This is a placeholder — real implementation would cache failure data.
    return undefined;
  }

  /** Refresh test list (called from command palette) */
  async refresh(): Promise<void> {
    await this.discoverTests();
  }

  dispose(): void {
    this.pollTimer && clearInterval(this.pollTimer);
    for (const d of this.disposables) d.dispose();
    this.controller.dispose();
  }
}
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** `StrobeTestController` provides VS Code Test Explorer with test tree from Cargo discovery, runs tests via daemon, polls for live progress with running test tracking and stuck warnings, and maps results (pass/fail/stuck) to `TestRun` events.

---

### Task 4: CodeLens Provider for Test Functions

**Files:**
- Create: `strobe-vscode/src/testing/test-codelens.ts`

Shows "Run Test | Debug Test" above `#[test]` functions in Rust files (and `TEST_CASE` in Catch2 files).

**Step 1: Write `src/testing/test-codelens.ts`**

```typescript
import * as vscode from 'vscode';

// Patterns that identify test functions per language
const TEST_PATTERNS: Array<{ languageIds: string[]; pattern: RegExp }> = [
  // Rust: #[test] or #[tokio::test] on line before fn
  { languageIds: ['rust'], pattern: /^\s*#\[(?:tokio::)?test\b/ },
  // C++ Catch2: TEST_CASE("name"
  { languageIds: ['cpp', 'c'], pattern: /^\s*TEST_CASE\s*\(/ },
  // C++ GoogleTest: TEST(Suite, Name) or TEST_F(Suite, Name)
  { languageIds: ['cpp', 'c'], pattern: /^\s*TEST(?:_F)?\s*\(/ },
];

// Extract function name from the line after the test attribute
const FN_NAME_PATTERNS: Array<{ languageIds: string[]; pattern: RegExp }> = [
  // Rust: fn test_name(
  { languageIds: ['rust'], pattern: /^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/ },
  // Catch2: TEST_CASE("test name"
  { languageIds: ['cpp', 'c'], pattern: /TEST_CASE\s*\(\s*"([^"]+)"/ },
  // GoogleTest: TEST(Suite, Name) → Suite::Name
  { languageIds: ['cpp', 'c'], pattern: /TEST(?:_F)?\s*\(\s*(\w+)\s*,\s*(\w+)/ },
];

export class TestCodeLensProvider implements vscode.CodeLensProvider {
  private _onDidChangeCodeLenses = new vscode.EventEmitter<void>();
  readonly onDidChangeCodeLenses = this._onDidChangeCodeLenses.event;

  provideCodeLenses(
    document: vscode.TextDocument,
    _token: vscode.CancellationToken,
  ): vscode.CodeLens[] {
    const lenses: vscode.CodeLens[] = [];
    const langId = document.languageId;

    // Find applicable patterns for this language
    const testPatterns = TEST_PATTERNS.filter((p) =>
      p.languageIds.includes(langId),
    );
    if (testPatterns.length === 0) return [];

    for (let i = 0; i < document.lineCount; i++) {
      const line = document.lineAt(i).text;

      for (const tp of testPatterns) {
        if (!tp.pattern.test(line)) continue;

        // Found a test marker — find the function name
        const testName = this.extractTestName(document, i, langId);
        if (!testName) continue;

        const range = new vscode.Range(i, 0, i, line.length);

        // "Run Test" lens
        lenses.push(
          new vscode.CodeLens(range, {
            title: 'Run Test',
            command: 'strobe.runSingleTest',
            arguments: [testName],
          }),
        );

        // "Debug Test" lens
        lenses.push(
          new vscode.CodeLens(range, {
            title: 'Debug Test',
            command: 'strobe.debugSingleTest',
            arguments: [testName],
          }),
        );

        break; // Don't match multiple patterns on same line
      }
    }

    return lenses;
  }

  private extractTestName(
    document: vscode.TextDocument,
    markerLine: number,
    langId: string,
  ): string | undefined {
    const fnPatterns = FN_NAME_PATTERNS.filter((p) =>
      p.languageIds.includes(langId),
    );

    // For Rust, the test attribute is on a line ABOVE the fn declaration
    // For C++ (Catch2/GTest), the test macro IS the declaration
    if (langId === 'rust') {
      // Scan up to 3 lines below for the fn declaration
      for (let j = markerLine + 1; j < Math.min(markerLine + 4, document.lineCount); j++) {
        const text = document.lineAt(j).text;
        for (const fp of fnPatterns) {
          const m = fp.pattern.exec(text);
          if (m) return m[1];
        }
      }
    } else {
      // C++ — the test name is on the marker line itself
      const text = document.lineAt(markerLine).text;
      for (const fp of fnPatterns) {
        const m = fp.pattern.exec(text);
        if (m) {
          // GoogleTest: "Suite::Name", Catch2: just the name
          return m[2] ? `${m[1]}::${m[2]}` : m[1];
        }
      }
    }
    return undefined;
  }

  /** Call this after test discovery to refresh lenses */
  refresh(): void {
    this._onDidChangeCodeLenses.fire();
  }
}
```

**Step 2: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Checkpoint:** CodeLens shows "Run Test | Debug Test" above Rust `#[test]` functions and C++ `TEST_CASE`/`TEST` macros.

---

### Task 5: Wire Everything into extension.ts + package.json

**Files:**
- Modify: `strobe-vscode/src/extension.ts`
- Modify: `strobe-vscode/package.json`

**Step 1: Update `package.json`**

Add new commands and CodeLens activation:

```jsonc
// Add to "commands" array:
{
  "command": "strobe.refreshTests",
  "title": "Strobe: Refresh Test List"
},
{
  "command": "strobe.runSingleTest",
  "title": "Strobe: Run Test"
},
{
  "command": "strobe.debugSingleTest",
  "title": "Strobe: Debug Test with Strobe"
}
```

No `activationEvents` change needed — the extension already activates on `*` (empty array = always active). CodeLens triggers from the provider registration.

**Step 2: Update `extension.ts`**

Add imports and initialization:

```typescript
import { StrobeTestController } from './testing/test-controller';
import { TestCodeLensProvider } from './testing/test-codelens';
```

In `activate()`, after the existing sidebar/command registration:

```typescript
// Test Explorer
const testController = new StrobeTestController(
  () => daemonManager.ensureClient(),
  outputChannel,
);

// CodeLens for test functions
const codeLensProvider = new TestCodeLensProvider();
const codeLensRegistration = vscode.languages.registerCodeLensProvider(
  [
    { language: 'rust' },
    { language: 'cpp' },
    { language: 'c' },
  ],
  codeLensProvider,
);

// Register test commands
context.subscriptions.push(
  testController,
  codeLensRegistration,

  vscode.commands.registerCommand('strobe.refreshTests', () =>
    testController.refresh(),
  ),

  vscode.commands.registerCommand(
    'strobe.runSingleTest',
    async (testName: string) => {
      const workspaceFolder =
        vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      if (!workspaceFolder) return;
      try {
        const client = await daemonManager.ensureClient();
        const resp = await client.runTest({
          projectRoot: workspaceFolder,
          test: testName,
        });
        outputChannel.appendLine(
          `Strobe: Running test "${testName}" (${resp.framework})`,
        );
        outputChannel.show();

        // Poll inline (simple single-test flow)
        let done = false;
        while (!done) {
          const status = await client.testStatus(resp.testRunId);
          if (status.status === 'completed') {
            const r = status.result;
            if (r && r.failures.length === 0) {
              vscode.window.showInformationMessage(
                `Strobe: Test "${testName}" passed`,
              );
            } else if (r) {
              const msg = r.failures[0]?.message ?? 'Test failed';
              vscode.window.showErrorMessage(
                `Strobe: Test "${testName}" failed — ${msg}`,
              );
            }
            done = true;
          } else if (status.status === 'failed') {
            vscode.window.showErrorMessage(
              `Strobe: Test run failed — ${status.error ?? 'unknown error'}`,
            );
            done = true;
          } else {
            await new Promise((r) => setTimeout(r, 1000));
          }
        }
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Strobe: ${msg}`);
      }
    },
  ),

  vscode.commands.registerCommand(
    'strobe.debugSingleTest',
    async (testName: string) => {
      const workspaceFolder =
        vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
      if (!workspaceFolder) return;
      try {
        const client = await daemonManager.ensureClient();
        const resp = await client.runTest({
          projectRoot: workspaceFolder,
          test: testName,
          tracePatterns: [`*::${testName}`],
        });
        outputChannel.appendLine(
          `Strobe: Debugging test "${testName}" with trace *::${testName} (${resp.framework})`,
        );
        outputChannel.show();

        // Start session polling for the Frida session
        // (The test has a Frida session attached via sessionId in status)
        let done = false;
        while (!done) {
          const status = await client.testStatus(resp.testRunId);

          // If we get a session ID and don't have an active session, start polling it
          if (status.sessionId && !activeSessionId) {
            startSession(client, status.sessionId);
          }

          if (
            status.status === 'completed' ||
            status.status === 'failed'
          ) {
            done = true;
            if (status.result) {
              const r = status.result;
              if (r.failures.length > 0) {
                const fail = r.failures[0];
                let msg = `Test "${testName}" failed: ${fail.message}`;
                if (fail.suggestedTraces.length > 0) {
                  msg += ` — Suggested traces: ${fail.suggestedTraces.join(', ')}`;
                }
                vscode.window.showErrorMessage(`Strobe: ${msg}`);
              } else {
                vscode.window.showInformationMessage(
                  `Strobe: Test "${testName}" passed`,
                );
              }
            }
          } else {
            await new Promise((r) => setTimeout(r, 1000));
          }
        }
      } catch (err: unknown) {
        const msg = err instanceof Error ? err.message : String(err);
        vscode.window.showErrorMessage(`Strobe: ${msg}`);
      }
    },
  ),
);
```

Note: `startSession` is the existing function in extension.ts that sets up polling for a Frida session. The `debugSingleTest` command reuses it to stream trace events to the Output Channel during the test run.

**Step 3: Verify build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS — produces `dist/extension.js` with all new modules.

**Checkpoint:** Test Explorer shows in VS Code sidebar (beaker icon), discovers Cargo tests, runs them via daemon with live status, shows failures with locations. CodeLens shows Run/Debug above test functions. Debug mode traces the test and streams events to Output Channel.

---

### Task 6: Build Verification + Manual Test Plan

**Step 1: Build**

Run: `cd strobe-vscode && npm run build`
Expected: PASS

**Step 2: Manual end-to-end verification**

In VS Code Extension Development Host with a Rust project open:

1. **Discovery:** Open Test Explorer (beaker icon). Verify test tree loads with module hierarchy.
2. **Run all:** Click "Run All Tests" (play button). Verify:
   - Tests show as queued → running → green/red
   - Summary appears in Output Channel
   - Failed tests show assertion message with file:line link
3. **Run single:** Click "Run Test" CodeLens above a `#[test]` function. Verify:
   - Notification shows pass/fail result
4. **Debug single:** Click "Debug Test" CodeLens. Verify:
   - Output Channel shows trace events from the test function
   - Pass/fail notification includes suggested traces on failure
5. **Stuck detection:** If possible, run a test known to deadlock. Verify:
   - Stuck warning appears in test output
   - Output Channel shows diagnosis
6. **Refresh:** Run "Strobe: Refresh Test List" from command palette. Verify tree updates.

**Checkpoint:** Full M2 feature set verified. Ready for commit.

---

## Risk Mitigations

| Risk | Mitigation |
|------|-----------|
| `cargo test -- --list` compilation slow | Discovery runs async, doesn't block UI. Empty list on failure — real errors surface at run time. |
| Mapping daemon test names to TestItems | Both use `module::test_name` format from Cargo. Direct string match. |
| Parallel test execution (Cargo) | `running_tests` array in progress snapshot tracks all concurrent tests with individual timers. |
| Catch2 discoverer needs pre-built binary | Deferred — M2 ships `CargoDiscoverer` only. Catch2 discoverer can be added in M4 when binary path is part of launch config. |
| CodeLens pattern false positives | Restricted to specific languages + well-known test macros. Regex is conservative. |
| Test run cancellation | `CancellationToken` checked in poll loop. Daemon's Frida session can be stopped via `debug_stop`. |

## File Summary

```
strobe-vscode/src/
├── testing/
│   ├── test-discovery.ts        # TestDiscoverer interface + CargoDiscoverer
│   ├── test-controller.ts       # VS Code TestController, run/poll/results
│   └── test-codelens.ts         # CodeLens above test functions
├── client/
│   └── types.ts                 # (modified) Add runningTests, stuck fields
└── extension.ts                 # (modified) Wire test controller + CodeLens + commands
strobe-vscode/
└── package.json                 # (modified) Add test commands
```

---

## Review Findings

**Reviewed:** 2026-02-10
**Commits:** `main..feature/mcp-consolidation`

### Issues

#### Issue 1: CodeLens missing "Trace" action
**Severity:** Minor
**Location:** `strobe-vscode/src/testing/test-codelens.ts:59-73`
**Requirement:** Spec says "CodeLens on test functions (Run | Debug | Trace)"
**Problem:** Only "Run Test" and "Debug Test" are implemented. No standalone "Trace" CodeLens.
**Suggested fix:** Add a third CodeLens with command `strobe.traceFunction` using the test function name as pattern. Low priority — the "Debug Test" lens already applies trace patterns.

#### Issue 2: Skipped tests marked as passed
**Severity:** Important
**Location:** `strobe-vscode/src/testing/test-controller.ts:298-303`
**Requirement:** Accurate test results in Test Explorer
**Problem:** `applyResults()` marks all items not in `failedNames` or `stuckNames` as `run.passed()`, including skipped tests.
**Suggested fix:** Track skipped test names from the daemon result (if available) and call `run.skipped(item)` for those. If the daemon only provides a count, avoid marking unknown tests as passed when `summary.skipped > 0`.

#### Issue 3: Cancelled test runs leave items in limbo
**Severity:** Important
**Location:** `strobe-vscode/src/testing/test-controller.ts:184-247`
**Problem:** When `token.isCancellationRequested`, the poll loop exits but started/enqueued items are never given a terminal state. They appear frozen in the Test Explorer.
**Suggested fix:** After the loop exits on cancellation, mark all remaining items as `run.skipped()` or `run.errored()` with a "Cancelled" message.

#### Issue 4: CodeLens test run (cmdRunSingleTest) has no cancellation
**Severity:** Important
**Location:** `strobe-vscode/src/extension.ts:267-298`
**Problem:** The `while (!done)` poll loop has no timeout or cancellation token. A stuck test leaves this promise hanging indefinitely.
**Suggested fix:** Use `vscode.window.withProgress` with a cancel button, or add a 10-minute timeout.

#### Issue 5: Duplicated test polling logic
**Severity:** Minor
**Location:** `strobe-vscode/src/extension.ts:266-299` vs `strobe-vscode/src/testing/test-controller.ts:175-247`
**Problem:** Both files implement independent poll-until-done loops with 1s intervals. The `cmdRunSingleTest` variant is simpler and lacks stuck warning surfacing. They will diverge.
**Suggested fix:** Have CodeLens commands route through `StrobeTestController` so results also appear in Test Explorer.

#### Issue 6: `RUSTC_BOOTSTRAP=1` set unnecessarily
**Severity:** Minor
**Location:** `strobe-vscode/src/testing/test-discovery.ts:39`
**Problem:** `RUSTC_BOOTSTRAP: '1'` enables nightly compiler features on stable toolchains. Not needed for `cargo test -- --list`.
**Suggested fix:** Remove unless there's a documented reason.

#### Issue 7: Zero TypeScript tests for the extension
**Severity:** Critical
**Location:** `strobe-vscode/` (entire directory)
**Problem:** No `.test.ts` files exist. No test infrastructure (no vitest/jest config, no test script in package.json). ~1,500 lines of M2 code have zero automated test coverage.
**Suggested fix:** Add vitest + `@vscode/test-electron` mock. Priority tests: `CargoDiscoverer` parsing edge cases, `TestCodeLensProvider` regex patterns, `TestController.applyResults` state mapping.

### Approved
- [x] TestDiscoverer interface + CargoDiscoverer
- [x] Live red/green test status with running_tests display
- [x] Stuck detection warnings surfaced as test messages
- [x] Failure details with file:line links and suggested traces
- [x] "Debug with Strobe" run profile
- [x] CodeLens Run Test + Debug Test
- [ ] CodeLens "Trace" action — Issue 1
- [ ] Skipped test handling — Issue 2
- [ ] Cancellation handling — Issues 3, 4
- [ ] Test coverage — Issue 7

### Summary
- Critical: 1 (no TS tests)
- Important: 3 (skipped-as-passed, cancellation gaps)
- Minor: 3 (missing Trace lens, duplicated polling, RUSTC_BOOTSTRAP)
- Ready to merge: **No** — fix Issues 2, 3, 4 first; Issue 7 before next milestone
