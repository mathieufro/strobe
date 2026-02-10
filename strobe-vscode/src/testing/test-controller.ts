import * as vscode from 'vscode';
import { StrobeClient } from '../client/strobe-client';
import { TestStatusResponse } from '../client/types';
import {
  TestDiscoverer,
  detectDiscoverer,
  CargoDiscoverer,
} from './test-discovery';

export class StrobeTestController implements vscode.Disposable {
  private controller: vscode.TestController;
  private discoverer: TestDiscoverer | undefined;
  private testItemMap = new Map<string, vscode.TestItem>();
  private disposables: vscode.Disposable[] = [];

  constructor(
    private getClient: () => Promise<StrobeClient>,
    private outputChannel: { appendLine(text: string): void; show(): void },
  ) {
    this.controller = vscode.tests.createTestController(
      'strobe.testController',
      'Strobe Tests',
    );

    const runProfile = this.controller.createRunProfile(
      'Run',
      vscode.TestRunProfileKind.Run,
      (request, token) => this.runTests(request, token),
    );

    const debugProfile = this.controller.createRunProfile(
      'Debug with Strobe',
      vscode.TestRunProfileKind.Debug,
      (request, token) => this.runTests(request, token, true),
    );

    this.controller.resolveHandler = async () => {
      await this.discoverTests();
    };

    this.disposables.push(runProfile, debugProfile);
  }

  async discoverTests(): Promise<void> {
    const workspaceFolder =
      vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!workspaceFolder) return;

    this.discoverer = await detectDiscoverer(workspaceFolder, [
      new CargoDiscoverer(),
    ]);
    if (!this.discoverer) return;

    const tests = await this.discoverer.listTests(workspaceFolder);

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

      // Create leaf test item
      const uri = test.file ? vscode.Uri.file(test.file) : undefined;
      const item = this.controller.createTestItem(test.name, testName, uri);
      if (test.file && test.line) {
        item.range = new vscode.Range(test.line - 1, 0, test.line - 1, 0);
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
    const workspaceFolder =
      vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
    if (!workspaceFolder) return;

    const run = this.controller.createTestRun(request);

    // Determine which tests to run
    let testFilter: string | undefined;
    const leafItems: vscode.TestItem[] = [];

    if (request.include && request.include.length > 0) {
      // Specific tests requested
      if (
        request.include.length === 1 &&
        request.include[0].children.size === 0
      ) {
        // Single leaf test
        testFilter = request.include[0].id;
      }
      this.collectLeaves(request.include, leafItems);
    } else {
      // Run all
      this.controller.items.forEach((item) =>
        this.collectLeaves([item], leafItems),
      );
    }

    // Mark all leaves as queued
    for (const item of leafItems) {
      run.enqueued(item);
    }

    try {
      const client = await this.getClient();

      const startResp = await client.runTest({
        projectRoot: workspaceFolder,
        test: testFilter,
        tracePatterns: debug ? [`*::${testFilter ?? '*'}`] : undefined,
        framework: this.discoverer?.framework,
      });

      if (debug) {
        this.outputChannel.appendLine(
          `Strobe: Running tests with Frida instrumentation (${startResp.framework})`,
        );
        this.outputChannel.show();
      }

      await this.pollTestStatus(
        client,
        startResp.testRunId,
        run,
        leafItems,
        token,
      );
    } catch (err: unknown) {
      const msg = err instanceof Error ? err.message : String(err);
      for (const item of leafItems) {
        run.errored(item, new vscode.TestMessage(msg));
      }
    } finally {
      run.end();
    }
  }

  private async pollTestStatus(
    client: StrobeClient,
    testRunId: string,
    run: vscode.TestRun,
    leafItems: vscode.TestItem[],
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

        // Surface stuck warnings
        if (p.warnings) {
          for (const w of p.warnings) {
            this.outputChannel.appendLine(
              `Strobe Test: ${w.testName ?? 'unknown'}: \u26A0 ${w.diagnosis} (idle ${Math.round(w.idleMs / 1000)}s)`,
            );
            if (w.testName) {
              const item = this.testItemMap.get(w.testName);
              if (item) {
                run.appendOutput(
                  `\u26A0 STUCK: ${w.diagnosis}\r\n`,
                  undefined,
                  item,
                );
              }
            }
          }
        }
      }

      // Terminal states
      if (status.status === 'completed' && status.result) {
        this.applyResults(run, status, leafItems);
        return;
      }

      if (status.status === 'failed') {
        const errMsg = status.error ?? 'Test run failed';
        this.outputChannel.appendLine(`Strobe Test: ${errMsg}`);
        for (const item of leafItems) {
          if (!startedTests.has(item.id)) {
            run.errored(item, new vscode.TestMessage(errMsg));
          }
        }
        return;
      }

      // Server blocks up to 15s, so 1s interval avoids busy-waiting
      await new Promise((r) => setTimeout(r, 1000));
    }

    // Cancelled: mark remaining items as skipped so they don't appear frozen
    this.outputChannel.appendLine('Strobe Test: Run cancelled');
    for (const item of leafItems) {
      if (!startedTests.has(item.id)) {
        run.skipped(item);
      } else {
        run.errored(item, new vscode.TestMessage('Cancelled'));
      }
    }
  }

  private applyResults(
    run: vscode.TestRun,
    status: TestStatusResponse,
    leafItems: vscode.TestItem[],
  ): void {
    const result = status.result!;

    const failedNames = new Set(result.failures.map((f) => f.name));
    const stuckNames = new Set(result.stuck.map((s) => s.name));

    // Process failures
    for (const failure of result.failures) {
      const item = this.testItemMap.get(failure.name);
      if (!item) continue;

      const msg = new vscode.TestMessage(failure.message);
      if (failure.file) {
        const uri = vscode.Uri.file(failure.file);
        const line = (failure.line ?? 1) - 1;
        msg.location = new vscode.Location(
          uri,
          new vscode.Position(line, 0),
        );
      }

      run.failed(item, msg);

      if (failure.suggestedTraces.length > 0) {
        run.appendOutput(
          `Suggested traces: ${failure.suggestedTraces.join(', ')}\r\n`,
          undefined,
          item,
        );
      }
    }

    // Process stuck tests
    for (const stuck of result.stuck) {
      const item = this.testItemMap.get(stuck.name);
      if (item) {
        run.failed(
          item,
          new vscode.TestMessage(
            `STUCK: ${stuck.diagnosis} (${Math.round(stuck.elapsedMs / 1000)}s)`,
          ),
        );
      }
    }

    // Mark remaining leaves: if skipped count > 0 and we can't identify which,
    // mark known non-failed items as passed only up to the expected passed count.
    const expectedPassed = result.summary.passed;
    const expectedSkipped = result.summary.skipped;
    const remainingItems = leafItems.filter(
      (item) => !failedNames.has(item.id) && !stuckNames.has(item.id),
    );

    if (expectedSkipped > 0 && remainingItems.length > expectedPassed) {
      // More remaining items than passed count — some are skipped.
      // Mark as passed up to the expected count, rest as skipped.
      let passedCount = 0;
      for (const item of remainingItems) {
        if (passedCount < expectedPassed) {
          run.passed(item);
          passedCount++;
        } else {
          run.skipped(item);
        }
      }
    } else {
      for (const item of remainingItems) {
        run.passed(item);
      }
    }

    // Summary
    const s = result.summary;
    this.outputChannel.appendLine(
      `Strobe Test: ${s.passed} passed, ${s.failed} failed, ${s.skipped} skipped (${(s.durationMs / 1000).toFixed(1)}s)`,
    );
  }

  private collectLeaves(
    items: readonly vscode.TestItem[],
    out: vscode.TestItem[],
  ): void {
    for (const item of items) {
      if (item.children.size === 0) {
        out.push(item);
      } else {
        item.children.forEach((child) =>
          this.collectLeaves([child], out),
        );
      }
    }
  }

  async refresh(): Promise<void> {
    await this.discoverTests();
  }

  dispose(): void {
    for (const d of this.disposables) d.dispose();
    this.controller.dispose();
  }
}
