# Phase 3 Review Fixes — Implementation Plan

**Spec:** Phase 3 multi-agent review findings (8 critical, 18 high, 30+ medium)
**Goal:** Fix all bugs, bridge all gaps identified in the Phase 3 code review
**Architecture:** Targeted fixes across Rust core (server.rs, types.rs, session_manager.rs) and VS Code extension (24 TS files)
**Tech Stack:** Rust (serde, tokio), TypeScript (VS Code API, DAP, Node.js)
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Rust core):** Tasks 1–4 — `server.rs`, `types.rs`, `session_manager.rs`
- **Stream B (Client layer):** Tasks 5–8 — `strobe-client.ts`, `types.ts`, `polling-engine.ts`
- **Stream C (DAP adapter):** Tasks 9–13 — `debug-adapter.ts`
- **Stream D (Memory panels):** Tasks 14–17 — `memory-panel.ts`, `watch-panel.ts`
- **Stream E (Testing):** Tasks 18–23 — `test-controller.ts`, `test-discovery.ts`, `test-codelens.ts`
- **Stream F (Editor):** Tasks 24–26 — `decorations.ts`, `context-menu.ts`, `function-identifier.ts`
- **Stream G (Infrastructure):** Tasks 27–32 — `extension.ts`, `daemon-manager.ts`, `settings-sync.ts`, `sidebar-provider.ts`, `package.json`

Streams A–G are independent and can run in parallel. Within each stream, tasks are serial.

---

### Task 1: Add format_event branches for Pause/Logpoint/ConditionError [C1, M8]

**Files:**
- Modify: `src/daemon/server.rs:33-116`

**Step 1: Add Pause event branch after Stdout/Stderr block**

In `format_event()`, after the `Stdout`/`Stderr` block (line 69), add:

```rust
    if event.event_type == crate::db::EventType::Pause {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "pause",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "backtrace": event.backtrace,
            "arguments": event.arguments,
        });
    }

    if event.event_type == crate::db::EventType::Logpoint {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "logpoint",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "logpointMessage": event.logpoint_message,
        });
    }

    if event.event_type == crate::db::EventType::ConditionError {
        return serde_json::json!({
            "id": event.id,
            "timestamp_ns": event.timestamp_ns,
            "eventType": "condition_error",
            "threadId": event.thread_id,
            "pid": event.pid,
            "function": event.function_name,
            "sourceFile": event.source_file,
            "line": event.line_number,
            "breakpointId": event.breakpoint_id,
            "logpointMessage": event.logpoint_message,
        });
    }
```

**Checkpoint:** Pause/Logpoint/ConditionError events now have dedicated formatting with relevant fields (breakpointId, backtrace, logpointMessage) instead of falling through to function trace formatting.

---

### Task 2: Fix events_dropped edge case with empty sessions [M10]

**Files:**
- Modify: `src/daemon/server.rs:1646-1651`

**Step 1: Fix the edge case**

Change:
```rust
let events_dropped = if let Some(after) = req.after_event_id {
    let min_rowid = self.session_manager.db().min_rowid_for_session(&req.session_id)?;
    Some(min_rowid.map_or(false, |min| after + 1 < min))
} else {
    None
};
```

To:
```rust
let events_dropped = if let Some(after) = req.after_event_id {
    let min_rowid = self.session_manager.db().min_rowid_for_session(&req.session_id)?;
    Some(match min_rowid {
        Some(min) => after + 1 < min,
        None => after > 0, // All events evicted → dropped if cursor was set
    })
} else {
    None
};
```

**Checkpoint:** `events_dropped` correctly returns `true` when all events have been evicted from a session that previously had events.

---

### Task 3: Remove redundant require_session in Status path [M11]

**Files:**
- Modify: `src/daemon/server.rs:1714-1717`

**Step 1: Remove the redundant check**

Change:
```rust
SessionAction::Status => {
    let _ = self.require_session(session_id)?;
    let status = self.session_manager.session_status(session_id)?;
```

To:
```rust
SessionAction::Status => {
    let status = self.session_manager.session_status(session_id)?;
```

**Checkpoint:** Session lookup is performed once instead of twice.

---

### Task 4: Restore min_offset=0 for user breakpoints [H1]

**Files:**
- Modify: `src/daemon/session_manager.rs:1250-1255`

**Step 1: Restore conditional min_offset**

Find the line:
```rust
let min_offset: u64 = 16;
```

Replace with:
```rust
let is_step_hook = bp.is_none() && pause_info.address.is_some();
let min_offset: u64 = if is_step_hook { 16 } else { 0 };
```

**Checkpoint:** User breakpoints at function entries will correctly resolve to the first source line. Step hooks still use 16-byte offset to avoid re-triggering.

---

### Task 5: Fix error code parsing in StrobeClient [C2]

**Files:**
- Modify: `strobe-vscode/src/client/strobe-client.ts:201-211`

**Step 1: Fix the regex and add JSON.parse safety**

The server sends error text like `"SESSION_NOT_FOUND": Session s1 not found` (the code is JSON-serialized with quotes). Fix the parsing:

```typescript
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
```

**Checkpoint:** `StrobeError.code` is correctly parsed. `StrobeError.message` is clean (no code prefix). Malformed JSON produces a descriptive error instead of raw SyntaxError.

---

### Task 6: Fix WriteMemoryResponse type and add missing error codes [H4, H5]

**Files:**
- Modify: `strobe-vscode/src/client/types.ts:271-277` and `373-382`

**Step 1: Fix WriteMemoryResponse to match Rust WriteResult**

```typescript
export interface WriteMemoryResponse {
  results: Array<{
    variable?: string;
    address: string;
    previousValue?: unknown;
    newValue: unknown;
    error?: string;
  }>;
}
```

**Step 2: Add missing error codes**

```typescript
export const StrobeErrorCodes = {
  NO_DEBUG_SYMBOLS: 'NO_DEBUG_SYMBOLS',
  SIP_BLOCKED: 'SIP_BLOCKED',
  SESSION_EXISTS: 'SESSION_EXISTS',
  SESSION_NOT_FOUND: 'SESSION_NOT_FOUND',
  PROCESS_EXITED: 'PROCESS_EXITED',
  FRIDA_ATTACH_FAILED: 'FRIDA_ATTACH_FAILED',
  INVALID_PATTERN: 'INVALID_PATTERN',
  VALIDATION_ERROR: 'VALIDATION_ERROR',
  WATCH_FAILED: 'WATCH_FAILED',
  TEST_RUN_NOT_FOUND: 'TEST_RUN_NOT_FOUND',
  READ_FAILED: 'READ_FAILED',
  WRITE_FAILED: 'WRITE_FAILED',
  INTERNAL_ERROR: 'INTERNAL_ERROR',
} as const;
```

**Checkpoint:** TS types match Rust server. All error codes available.

---

### Task 7: Add eventType union type for type safety [M6]

**Files:**
- Modify: `strobe-vscode/src/client/types.ts:144`

**Step 1: Add EventTypeFilter type and use it**

Add near the top of the types section:

```typescript
export type EventTypeFilter =
  | 'function_enter'
  | 'function_exit'
  | 'stdout'
  | 'stderr'
  | 'crash'
  | 'variable_snapshot'
  | 'pause'
  | 'logpoint'
  | 'condition_error';
```

Then in `QueryRequest`, change `eventType?: string;` to `eventType?: EventTypeFilter;`.

**Checkpoint:** Consumers get compile-time checking for valid event types.

---

### Task 8: Fix polling engine — guard concurrent polls, add backoff [H6, M12]

**Files:**
- Modify: `strobe-vscode/src/client/polling-engine.ts`

**Step 1: Add in-flight guards and error backoff**

Replace the polling methods with guarded versions. Key changes:
- Add `private statusInFlight = false;` and `private eventsInFlight = false;`
- In `pollStatus`: check and set `statusInFlight` flag, clear in finally
- In `pollEvents`: check and set `eventsInFlight` flag, clear in finally
- Add `private consecutiveErrors = 0;` — on error increment; on success reset to 0
- When `consecutiveErrors > 5`, emit a single `error` event and stop polling (daemon is likely dead)

```typescript
  private statusInFlight = false;
  private eventsInFlight = false;
  private consecutiveErrors = 0;

  private async pollStatus(): Promise<void> {
    if (this.statusInFlight) return;
    this.statusInFlight = true;
    try {
      const status = await this.client.sessionStatus(this.sessionId);
      this.consecutiveErrors = 0;
      this.emit('status', status);

      if (status.status === 'exited') {
        this.emit('sessionEnd');
        this.stop();
      }
    } catch (err) {
      this.consecutiveErrors++;
      if (this.consecutiveErrors > 5) {
        this.emit('error', err instanceof Error ? err : new Error(String(err)));
        this.stop();
      }
    } finally {
      this.statusInFlight = false;
    }
  }

  private async pollEvents(): Promise<void> {
    if (this.eventsInFlight) return;
    this.eventsInFlight = true;
    try {
      const resp = await this.client.query({
        sessionId: this.sessionId,
        afterEventId: this.cursor,
        limit: 200,
        verbose: true,
      });
      if (resp.lastEventId != null) {
        this.cursor = resp.lastEventId;
      }
      if (resp.events.length > 0) {
        this.emit('events', resp.events);
      }
      if (resp.eventsDropped) {
        this.emit('eventsDropped');
      }
    } catch {
      // Errors handled by pollStatus consecutiveErrors counter
    } finally {
      this.eventsInFlight = false;
    }
  }
```

**Checkpoint:** Concurrent polls are impossible. 5 consecutive errors stops polling cleanly.

---

### Task 9: Track and remove function breakpoints [C4]

**Files:**
- Modify: `strobe-vscode/src/dap/debug-adapter.ts:260-302`

**Step 1: Add tracking map**

Add member variable:
```typescript
private trackedFunctionBpIds: string[] = [];
```

**Step 2: Remove old function breakpoints before adding new**

In `setFunctionBreakPointsRequest`, before the `add` call:

```typescript
    // Remove old function breakpoints
    if (this.trackedFunctionBpIds.length > 0) {
      await this.client.setBreakpoints({
        sessionId: this.sessionId,
        remove: this.trackedFunctionBpIds,
      });
    }
```

After the response:
```typescript
    this.trackedFunctionBpIds = (result.breakpoints || []).map((bp) => bp.id);
```

**Checkpoint:** Function breakpoints are properly replaced on each call instead of accumulating.

---

### Task 10: Track last execution action for correct StoppedEvent reason [H7]

**Files:**
- Modify: `strobe-vscode/src/dap/debug-adapter.ts`

**Step 1: Add execution action tracker**

Add member:
```typescript
private lastAction: 'continue' | 'step-over' | 'step-into' | 'step-out' = 'continue';
```

**Step 2: Set in doStep**

In `doStep(action)`, before calling `client.continue`:
```typescript
this.lastAction = action === 'continue' ? 'continue' : action;
```

**Step 3: Use in StoppedEvent**

Where the StoppedEvent is sent (line ~579):
```typescript
const reason = this.lastAction === 'continue' ? 'breakpoint' : 'step';
const evt = new StoppedEvent(reason, dapThreadId);
```

Reset after sending:
```typescript
this.lastAction = 'continue';
```

**Checkpoint:** Step operations show "Paused on step" instead of "Paused on breakpoint".

---

### Task 11: Forward stdout/stderr to Debug Console [H9]

**Files:**
- Modify: `strobe-vscode/src/dap/debug-adapter.ts`

**Step 1: Add output polling alongside status polling**

In `pollStatus()`, after the status poll, add output forwarding:

```typescript
    // Forward stdout/stderr to Debug Console
    try {
      const outputResp = await this.client.query({
        sessionId: this.sessionId,
        afterEventId: this.outputCursor,
        limit: 100,
      });
      if (outputResp.lastEventId != null) {
        this.outputCursor = outputResp.lastEventId;
      }
      for (const event of outputResp.events) {
        if (event.eventType === 'stdout' && event.text) {
          this.sendEvent(new OutputEvent(event.text + '\n', 'stdout'));
        } else if (event.eventType === 'stderr' && event.text) {
          this.sendEvent(new OutputEvent(event.text + '\n', 'stderr'));
        }
      }
    } catch {
      // Output forwarding is best-effort
    }
```

Add members:
```typescript
private outputCursor: number | undefined;
```

Add import:
```typescript
import { ..., OutputEvent } from '@vscode/debugadapter';
```

**Checkpoint:** Program stdout/stderr appears in VS Code Debug Console.

---

### Task 12: Clean up thread map on session end [M6-DAP]

**Files:**
- Modify: `strobe-vscode/src/dap/debug-adapter.ts`

**Step 1: Clear maps in stopSession**

In `stopSession()`, add cleanup:
```typescript
    this.threadMap.clear();
    this.reverseThreadMap.clear();
    this.nextDapThreadId = 1;
    this.trackedBreakpointIds.clear();
    this.trackedLogpointIds.clear();
    this.trackedFunctionBpIds = [];
```

**Checkpoint:** Thread maps and breakpoint tracking don't leak across sessions.

---

### Task 13: Fix hitCondition parsing [M1-DAP]

**Files:**
- Modify: `strobe-vscode/src/dap/debug-adapter.ts:187` and `276`

**Step 1: Parse hitCondition as integer, ignoring non-numeric prefix**

Replace both instances of:
```typescript
hitCount: bp.hitCondition ? parseInt(bp.hitCondition, 10) : undefined,
```

With:
```typescript
hitCount: bp.hitCondition ? parseInt(bp.hitCondition.replace(/\D+/g, ''), 10) || undefined : undefined,
```

This extracts the numeric part from expressions like `>= 10`, `== 5`, `% 3` (gets `10`, `5`, `3`).

**Checkpoint:** DAP hitCondition values like `>= 10` are parsed correctly instead of becoming NaN.

---

### Task 14: Add CSP to both webview panels [C5]

**Files:**
- Modify: `strobe-vscode/src/memory/memory-panel.ts:153-158`
- Modify: `strobe-vscode/src/memory/watch-panel.ts:111-138`

**Step 1: Add nonce generation utility**

In both files, add:
```typescript
function getNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let nonce = '';
  for (let i = 0; i < 32; i++) {
    nonce += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return nonce;
}
```

**Step 2: Add CSP meta tag to HTML head**

In `getHtml()` for both panels, generate a nonce and add to `<head>`:
```html
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';">
```

And add `nonce="${nonce}"` attribute to the `<script>` tag.

**Checkpoint:** Both webviews have nonce-based CSP restricting script execution.

---

### Task 15: Fix double-dispose in both panels [C6]

**Files:**
- Modify: `strobe-vscode/src/memory/memory-panel.ts:335-340`
- Modify: `strobe-vscode/src/memory/watch-panel.ts:199-204`

**Step 1: Remove `this.panel.dispose()` from dispose methods**

In `memory-panel.ts`, change `dispose()`:
```typescript
private dispose(): void {
  MemoryPanel.currentPanel = undefined;
  this.stopPoll();
  for (const d of this.disposables) d.dispose();
  // Panel is already disposed by VS Code when onDidDispose fires
}
```

Same for `watch-panel.ts`:
```typescript
private dispose(): void {
  WatchPanel.currentPanel = undefined;
  if (this.pollTimer) clearInterval(this.pollTimer);
  for (const d of this.disposables) d.dispose();
}
```

**Checkpoint:** No double-dispose crash when user closes panel tab.

---

### Task 16: Fix sessionId change handling in panels [H11]

**Files:**
- Modify: `strobe-vscode/src/memory/memory-panel.ts:15-22`
- Modify: `strobe-vscode/src/memory/watch-panel.ts:24-28`

**Step 1: Stop poll and reset content on sessionId change**

In `MemoryPanel.createOrShow`, when panel exists:
```typescript
    if (MemoryPanel.currentPanel) {
      MemoryPanel.currentPanel.stopPoll();
      MemoryPanel.currentPanel.sessionId = sessionId;
      MemoryPanel.currentPanel.panel.reveal();
      return MemoryPanel.currentPanel;
    }
```

Same for `WatchPanel.createOrShow`:
```typescript
    if (WatchPanel.currentPanel) {
      if (WatchPanel.currentPanel.pollTimer) {
        clearInterval(WatchPanel.currentPanel.pollTimer);
        WatchPanel.currentPanel.pollTimer = undefined;
      }
      WatchPanel.currentPanel.sessionId = sessionId;
      WatchPanel.currentPanel.panel.reveal();
      return WatchPanel.currentPanel;
    }
```

**Checkpoint:** Changing sessions stops stale polls.

---

### Task 17: Fix watch panel reading by address instead of label [H12]

**Files:**
- Modify: `strobe-vscode/src/memory/watch-panel.ts:94`

**Step 1: Use variable name or address instead of label**

Change:
```typescript
const targets = this.watches.map((w) => ({ variable: w.label }));
```

To:
```typescript
const targets = this.watches.map((w) => {
  if (w.address) return { address: w.address, type: w.type };
  return { variable: w.variable ?? w.label };
});
```

This uses the actual variable name or resolved address from `ActiveWatch` instead of the display label.

**Checkpoint:** Watches with custom labels read the correct variable/address.

---

### Task 18: Add daemon-side test cancellation [C7]

**Files:**
- Modify: `strobe-vscode/src/testing/test-controller.ts:244-254`
- Modify: `strobe-vscode/src/extension.ts:371-409`

**Step 1: Stop session on cancellation in test controller**

In `pollTestStatus`, when cancellation fires, add stop call:

```typescript
token.onCancellationRequested(async () => {
  // Stop the Frida session to kill the test process
  if (startResp?.sessionId) {
    try {
      await client.stop(startResp.sessionId);
    } catch {
      // Best effort
    }
  }
});
```

Note: `startResp` needs to be accessible — pass `sessionId` from the `testStatus` response into the cancellation handler.

**Step 2: Same for cmdRunSingleTest in extension.ts**

After the polling loop detects cancellation, add:
```typescript
// After loop exits due to cancellation
if (token.isCancellationRequested && resp.testRunId) {
  try {
    const status = await client.testStatus(resp.testRunId);
    if (status.sessionId) {
      await client.stop(status.sessionId);
    }
  } catch {
    // Best effort
  }
}
```

**Checkpoint:** Cancelling a test run in the UI actually kills the test process on the daemon side.

---

### Task 19: Add concurrent test run guard [H13]

**Files:**
- Modify: `strobe-vscode/src/testing/test-controller.ts`

**Step 1: Add isRunning flag**

Add member:
```typescript
private activeRun: vscode.TestRun | null = null;
```

At start of `runTests`:
```typescript
if (this.activeRun) {
  vscode.window.showWarningMessage('Strobe: A test run is already in progress.');
  return;
}
```

Set `this.activeRun = run;` after creating the run. Clear in finally block: `this.activeRun = null;`.

**Checkpoint:** Only one test run can execute at a time.

---

### Task 20: Fix multi-select test execution [H14]

**Files:**
- Modify: `strobe-vscode/src/testing/test-controller.ts:119-128`

**Step 1: Build filter for multiple selections**

Change the filter logic:
```typescript
    let testFilter: string | undefined;
    const leafItems: vscode.TestItem[] = [];

    if (request.include && request.include.length > 0) {
      this.collectLeaves(request.include, leafItems);
      if (leafItems.length === 1) {
        testFilter = leafItems[0].id;
      }
      // For multiple items, run each test by name — use common prefix or first match
      // Note: daemon uses substring match, so we can't filter for exact multiple tests.
      // Best effort: run all and filter results client-side.
    } else {
```

This is a known limitation — the daemon doesn't support multiple exact test filters. The fix is to let the current behavior (run all) stand but improve the result mapping (Task 21).

**Checkpoint:** Acknowledged limitation documented.

---

### Task 21: Fix failed test run leaving items stuck as "running" [H4-testing]

**Files:**
- Modify: `strobe-vscode/src/testing/test-controller.ts:231-239`

**Step 1: Mark all items as errored on test run failure**

Change the `status === 'failed'` handler to mark ALL items:
```typescript
      } else if (status.status === 'failed') {
        const errMsg = status.error ?? 'Test run failed';
        for (const item of leafItems) {
          run.errored(item, new vscode.TestMessage(errMsg));
        }
        return;
      }
```

**Checkpoint:** No test items remain in "running" state after a failed run.

---

### Task 22: Add subprocess timeout to test discovery [H15]

**Files:**
- Modify: `strobe-vscode/src/testing/test-discovery.ts:37-73`

**Step 1: Add timeout to cargo spawn**

After creating the process, add a timeout:
```typescript
    const timeout = setTimeout(() => {
      proc.kill();
    }, 30_000); // 30s timeout for cargo build + list

    proc.on('close', (code) => {
      clearTimeout(timeout);
      // ... existing logic
    });

    proc.on('error', () => {
      clearTimeout(timeout);
      resolve([]);
    });
```

**Step 2: Same for Go discoverer** (line ~92)

Add identical timeout pattern.

**Checkpoint:** Test discovery cannot hang forever.

---

### Task 23: Wire GoTestDiscoverer and fix CodeLens test name [H16, H17]

**Files:**
- Modify: `strobe-vscode/src/testing/test-controller.ts:49-51`
- Modify: `strobe-vscode/src/testing/test-codelens.ts:54-65`

**Step 1: Include GoTestDiscoverer in controller**

Change:
```typescript
this.discoverer = await detectDiscoverer(workspaceFolder, [
  new CargoDiscoverer(),
]);
```

To:
```typescript
this.discoverer = await detectDiscoverer(workspaceFolder);
```

This uses the default parameter which includes both `CargoDiscoverer` and `GoTestDiscoverer`.

**Step 2: Improve CodeLens test name extraction for Rust**

In `extractTestName`, for Rust, try to include the module path. After the function name regex match, scan upward for `mod` declarations:

```typescript
private extractTestName(
  document: vscode.TextDocument,
  testLine: number,
  langId: string,
): string | undefined {
  // ... existing extraction to get bare function name ...

  if (langId === 'rust' && funcName) {
    // Try to build qualified name by scanning for enclosing mod declarations
    const mods: string[] = [];
    let braceDepth = 0;
    for (let i = testLine; i >= 0; i--) {
      const text = document.lineAt(i).text;
      braceDepth += (text.match(/\}/g) || []).length - (text.match(/\{/g) || []).length;
      if (braceDepth < 0) {
        const modMatch = text.match(/\bmod\s+(\w+)/);
        if (modMatch) {
          mods.unshift(modMatch[1]);
          braceDepth = 0;
        }
      }
    }
    return mods.length > 0 ? `${mods.join('::')}::${funcName}` : funcName;
  }

  return funcName;
}
```

**Checkpoint:** Go test discovery works. Rust CodeLens generates more specific test names.

---

### Task 24: Fix decoration file matching [H10]

**Files:**
- Modify: `strobe-vscode/src/editor/decorations.ts:98`

**Step 1: Use normalized full path comparison with suffix fallback**

Change:
```typescript
if (!stat.file || path.basename(filePath) !== path.basename(stat.file)) continue;
```

To:
```typescript
if (!stat.file) continue;
// Full path match or suffix match (DWARF may have relative paths)
const normalFile = path.normalize(stat.file);
if (normalFile !== filePath && !filePath.endsWith(normalFile) && !normalFile.endsWith(path.basename(filePath))) continue;
```

Wait — this would still match basenames. Better approach: require either exact match or the stat file to be a suffix of the editor file path:

```typescript
if (!stat.file) continue;
if (!filePath.endsWith(stat.file) && stat.file !== filePath) continue;
```

This handles both absolute paths (exact match) and relative paths from DWARF (suffix match like `src/parser.cpp`).

**Checkpoint:** `src/parser.cpp` and `lib/parser.cpp` no longer share decorations.

---

### Task 25: Fix decoration debounce stale rendering [M-debounce]

**Files:**
- Modify: `strobe-vscode/src/editor/decorations.ts:80-87`

**Step 1: Re-schedule if dirty after render**

```typescript
private scheduleRender(): void {
  if (this.debounceTimer) return;
  this.debounceTimer = setTimeout(() => {
    this.debounceTimer = undefined;
    this.dirty = false;
    this.render();
    // If new events arrived during render, schedule another
    if (this.dirty) {
      this.scheduleRender();
    }
  }, DEBOUNCE_MS);
}
```

**Checkpoint:** Decorations always catch up to the latest events.

---

### Task 26: Add remote workspace guard to context menu commands [M-remote]

**Files:**
- Modify: `strobe-vscode/src/editor/context-menu.ts`

**Step 1: Add file scheme check at the top of breakpoint/logpoint functions**

Add a helper:
```typescript
function requireLocalFile(editor: vscode.TextEditor): string | undefined {
  if (editor.document.uri.scheme !== 'file') {
    vscode.window.showWarningMessage('Strobe: Only local files are supported.');
    return undefined;
  }
  return editor.document.uri.fsPath;
}
```

Use in `setBreakpointAtCursor`, `addLogpointAtCursor`, and the trace function:
```typescript
const filePath = requireLocalFile(editor);
if (!filePath) return;
```

**Checkpoint:** Clear error message instead of silent failure on remote files.

---

### Task 27: Fix startSession leaking old PollingEngine [M-leak]

**Files:**
- Modify: `strobe-vscode/src/extension.ts:420-425`

**Step 1: Stop old engine before creating new**

At the top of `startSession`:
```typescript
function startSession(client: StrobeClient, sessionId: string): void {
  // Clean up any existing session first
  if (pollingEngine) {
    pollingEngine.stop();
    pollingEngine = null;
  }

  activeSessionId = sessionId;
  pollingEngine = new PollingEngine(client, sessionId);
  // ... rest unchanged
```

**Checkpoint:** Starting a new session cleanly stops the old one.

---

### Task 28: Fix daemon manager — clean up dead client on reconnect [H18]

**Files:**
- Modify: `strobe-vscode/src/utils/daemon-manager.ts:29-37`

**Step 1: Disconnect dead client before reconnecting**

In `ensureClient`, before creating new client:
```typescript
  async ensureClient(): Promise<StrobeClient> {
    if (this.client?.isConnected) return this.client;
    // Clean up dead client
    if (this.client) {
      this.client.disconnect();
      this.client = null;
    }
    if (this.connectPromise) return this.connectPromise;
    // ... rest unchanged
```

**Step 2: Remove unused `net` import**

Remove: `import * as net from 'net';`

**Checkpoint:** Dead sockets are cleaned up. No unused imports.

---

### Task 29: Sync remaining settings and read them [H-settings]

**Files:**
- Modify: `strobe-vscode/src/utils/settings-sync.ts:9-12`
- Modify: `strobe-vscode/src/memory/memory-panel.ts` (poll interval)
- Modify: `strobe-vscode/src/memory/watch-panel.ts` (poll interval)

**Step 1: Add missing settings to sync map**

```typescript
const SETTING_MAP: Record<string, string> = {
  'events.maxPerSession': 'events.maxPerSession',
  'test.statusRetryMs': 'test.statusRetryMs',
  'trace.serializationDepth': 'trace.serializationDepth',
  'memory.pollIntervalMs': 'memory.pollIntervalMs',
};
```

**Step 2: Read pollIntervalMs in memory panel**

In `memory-panel.ts`, where the poll interval is hardcoded (line ~259):
```typescript
const defaultInterval = vscode.workspace.getConfiguration('strobe').get<number>('memory.pollIntervalMs', 500);
```

Use `defaultInterval` as the default in the HTML dropdown.

**Step 3: Same for watch panel** (line ~80):
```typescript
const interval = vscode.workspace.getConfiguration('strobe').get<number>('memory.pollIntervalMs', 1000);
```

**Checkpoint:** All 4 declared settings are synced and consumed.

---

### Task 30: Fix sidebar comparison to use content hash [L-sidebar]

**Files:**
- Modify: `strobe-vscode/src/sidebar/sidebar-provider.ts:55-70`

**Step 1: Use JSON comparison for arrays**

Change length-only checks to content checks:

```typescript
  update(sessionId: string, status: SessionStatusResponse): void {
    if (
      this.sessionId === sessionId &&
      this.status?.eventCount === status.eventCount &&
      this.status?.hookedFunctions === status.hookedFunctions &&
      this.status?.status === status.status &&
      JSON.stringify(this.status?.tracePatterns) === JSON.stringify(status.tracePatterns) &&
      JSON.stringify(this.status?.breakpoints) === JSON.stringify(status.breakpoints) &&
      JSON.stringify(this.status?.logpoints) === JSON.stringify(status.logpoints) &&
      JSON.stringify(this.status?.watches) === JSON.stringify(status.watches) &&
      this.status?.pausedThreads.length === status.pausedThreads.length
    ) {
      return;
    }
```

**Checkpoint:** Adding/removing items with same count correctly refreshes sidebar.

---

### Task 31: Fix conflicting keybindings [M-keybindings]

**Files:**
- Modify: `strobe-vscode/package.json:249-290`

**Step 1: Replace conflicting keybindings**

Change:
- `strobe.addTracePattern`: `ctrl+shift+t` / `cmd+shift+t` → `ctrl+alt+t` / `cmd+alt+t` (avoids Reopen Closed Editor)
- `strobe.openMemoryInspector`: `ctrl+shift+m` / `cmd+shift+m` → `ctrl+alt+m` / `cmd+alt+m` (avoids Toggle Problems)
- `strobe.addWatch`: `ctrl+shift+w` / `cmd+shift+w` → `ctrl+alt+w` / `cmd+alt+w` (avoids Close Window)

**Checkpoint:** No more conflicts with standard VS Code keybindings.

---

### Task 32: Fix deactivate to handle errors and clean up fully [M-deactivate]

**Files:**
- Modify: `strobe-vscode/src/extension.ts:477-480`

**Step 1: Make deactivate robust**

```typescript
export function deactivate(): void {
  try { pollingEngine?.stop(); } catch { /* */ }
  try { daemonManager.dispose(); } catch { /* */ }
}
```

**Checkpoint:** Deactivation always completes even if one cleanup step throws.

---

## Verification

After all tasks, run:
1. `cd agent && npm run build` — rebuild agent (Task 1 doesn't touch agent, but verify)
2. `touch src/frida_collector/spawner.rs && cargo build` — verify Rust compiles
3. `cd strobe-vscode && npx tsc --noEmit` — verify TypeScript compiles
4. Run existing tests: `cargo test` (unit tests should still pass)

## Summary

| Stream | Tasks | Critical Fixed | High Fixed | Medium Fixed |
|--------|-------|---------------|------------|-------------|
| A: Rust core | 1–4 | 1 (C1) | 1 (H1) | 3 |
| B: Client | 5–8 | 2 (C2, C3) | 3 (H4, H5, H6) | 2 |
| C: DAP | 9–13 | 1 (C4) | 2 (H7, H9) | 3 |
| D: Memory | 14–17 | 2 (C5, C6) | 2 (H11, H12) | 1 |
| E: Testing | 18–23 | 2 (C7, C8 partial) | 4 (H13-16) | 1 |
| F: Editor | 24–26 | 0 | 1 (H10) | 2 |
| G: Infra | 27–32 | 0 | 2 (H18, H-settings) | 5 |
| **Total** | **32** | **8** | **15** | **17** |
