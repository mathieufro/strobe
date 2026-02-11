# Features by Phase

Each phase builds on the previous. Each has a clear validation criteria: "What can I do now that I couldn't before?"

## Implementation Status

| Phase | Name | Status |
|-------|------|--------|
| 1a | Tracing Foundation | **Shipped** |
| 1b | Advanced Runtime Control | **Shipped** |
| 1c | Crash & Multi-Process | **Shipped** |
| 1d | Test Instrumentation | **Shipped** |
| 1e | Live Memory Access | **Shipped** |
| 2 | Active Debugging (breakpoints, stepping) | **Shipped** |
| 3 | VS Code Extension | **Shipped** |
| 4 | UI Observation (AX tree + AI vision) | **Shipped** |
| 5 | UI Interaction | Planned |
| 6 | I/O Channels | Planned |
| 7 | Autonomous Test Scenarios | Planned |

---

## Phase 1a: Tracing Foundation

**Goal:** Prove the core concept works. LLM can launch a program, add targeted traces, observe execution, and query what happened.

### Features

#### Daemon Architecture
- Single global daemon per user
- Lazy start on first MCP call
- Unix socket at `~/.strobe/strobe.sock`
- Auto-shutdown after 30 minutes idle

#### Launch Process
- Spawns process via Frida
- Reads DWARF debug info to identify user code
- Returns human-readable session ID

#### Dynamic Trace Patterns
- Add/remove patterns at runtime via `debug_trace`
- Glob syntax: `*` matches within module, `**` matches across
- Special pattern `@usercode` for all project functions
- Hooks injected live, no restart required

#### Basic Event Capture
- Function enter events (name, arguments)
- Function exit events (return value, duration)
- Nanosecond timestamps for ordering
- Parent event tracking for call hierarchy

#### Process Output Capture
- stdout/stderr captured automatically via Frida's Device "output" signal (`FRIDA_STDIO_PIPE`)
- Works reliably with ASAN/sanitizer-instrumented binaries (no agent-side hooks needed)
- Output events interleaved chronologically in the unified event timeline
- Queryable via `debug_query` with `eventType: "stdout"` or `"stderr"`
- This is the primary debugging tool — often sufficient to diagnose crashes without any trace patterns

#### Serialization
- Primitives serialized directly
- Structs serialized recursively (default depth 3, max 10 via `serializationDepth`)
- Arrays truncated to first 100 elements
- Strings truncated at 1KB
- Pointers as hex address

#### Storage
- SQLite with WAL mode
- Events table with indexes for common queries
- FTS5 for function name search

#### Query Execution History
- Search by function name (equals, contains, regex)
- Search by source file
- Filter by return value (equals, isNull)
- Pagination (default limit: 50)
- Summary mode (default) vs verbose mode

#### Stop Session
- Detaches Frida cleanly
- Deletes session data
- Session stays queryable after process exits until stop

#### MCP Tools
- `debug_launch` - Launch binary with Frida (applies pending patterns, captures stdout/stderr)
- `debug_trace` - Add/remove trace patterns (call before launch to set pending, or with sessionId for live)
- `debug_query` - Query unified timeline (function traces + stdout/stderr, chronologically ordered)
- `debug_stop` - End session and cleanup

### What Gets Captured (Phase 1a)

| Data | Captured | Notes |
|------|----------|-------|
| Function name | Yes | Demangled (raw name also available) |
| Source file + line | Yes | Via DWARF |
| Arguments | Yes | JSON serialized, depth 1 |
| Return value | Yes | JSON serialized |
| Duration | Yes | Nanosecond precision |
| Timestamp | Yes | Nanoseconds since session start |
| Thread ID | Yes | Basic support |
| Call hierarchy | Yes | Parent event tracking |
| Process stdout | Yes | Via Frida Device "output" signal |
| Process stderr | Yes | Via Frida Device "output" signal |

### Platform Support (Phase 1a)

| Platform | Status |
|----------|--------|
| Linux (x86_64) | Supported |
| macOS (arm64, x86_64) | Supported |
| Windows | Future phase |

### Language Support (Phase 1a)

| Language | Status | Debug Info |
|----------|--------|------------|
| C | Supported | DWARF |
| C++ | Supported | DWARF + demangling |
| Rust | Supported | DWARF + demangling |

### Error Handling

| Error | LLM Action |
|-------|------------|
| `NO_DEBUG_SYMBOLS` | Ask user to rebuild with `-g` |
| `SIP_BLOCKED` | Offer: copy to /tmp, codesign, or disable SIP |
| `SESSION_EXISTS` | Call `debug_stop` first |

### Recommended Workflow

The most effective approach is **incremental observation** — start with nothing and add only what you need:

1. **Launch with no patterns** — stdout/stderr are always captured
2. **Read output first** — crash messages, ASAN reports, and error logs are often enough
3. **Add targeted traces** — only when output alone doesn't explain the issue
4. **Narrow or widen** — adjust patterns based on what you learn, no restart needed

This is much faster than trying to guess the right trace patterns upfront, and avoids overwhelming the system with unnecessary events.

### Validation Criteria

**Scenario: Crash investigation (output-first)**
1. LLM calls `debug_launch` with **no** trace patterns
2. User triggers the crash
3. LLM calls `debug_query({ eventType: "stderr" })` — sees ASAN crash report
4. Crash report points to `lv_obj_style.c:632` via `KeyboardMappingSubView` constructor
5. LLM reads the relevant source, identifies memory pool exhaustion
6. LLM proposes fix — **no tracing was needed at all**

**Scenario: Targeted tracing (when output isn't enough)**
1. LLM launches with no patterns, reads output — no crash, but wrong behavior
2. LLM calls `debug_trace({ sessionId, add: ["submit::*", "form::validate"] })`
3. User reproduces the bug
4. LLM calls `debug_query` to find suspicious return values
5. LLM narrows further or queries with `verbose: true`
6. LLM identifies root cause

**Success:** LLM can observe what happened — starting from process output and escalating to function traces only when needed. No code changes. No restarts. No guesswork.

---

## Phase 1b: Advanced Runtime Control

**Goal:** Production-ready tracing with performance safeguards and deeper inspection.

### Features

#### Configurable Serialization Depth
- `serializationDepth` parameter in `debug_trace` (1-10, default: 3)
- Recursive object inspection via `ObjectSerializer` — follows pointers, serializes structs/arrays
- Circular reference detection with `<circular ref to 0x...>` markers
- Depth limiting with `<max depth N reached>` markers
- Arrays capped at 100 elements
- Flow: API → daemon → spawner → agent (via hooks message)

#### Multi-Threading Support
- Thread name capture (when available)
- Thread-aware queries (filter by thread)
- Order by thread-then-timestamp for per-thread analysis

#### Hot Function Handling
- Auto-detect functions called >100k/sec
- Auto-sample to 1% (configurable)
- Sampling indicator in query results
- LLM can disable sampling or narrow patterns

#### Storage Management
- Configurable retention (default: delete on stop)
- Optional retain for later analysis (`debug_stop({ retain: true })`)
- Auto-purge retained sessions after 7 days
- Hard limit: 10GB total, oldest purged first

#### Enhanced debug_trace
- `depth` parameter for serialization depth
- Returns sampling warnings if active

### Validation Criteria

**Scenario: Deep inspection with safeguards**
1. LLM launches app, adds trace on `process_data::*`
2. Function called 500k times/sec — auto-sampling kicks in
3. LLM receives warning: "sampling at 1%"
4. LLM narrows pattern to `process_data::validate` only
5. Full capture resumes
6. LLM requests `depth: 2` for nested config struct
7. LLM finds bug in nested field

**Success:** High-throughput functions don't crash the system. LLM can inspect deeper when needed.

---

## Phase 1c: Crash & Multi-Process

**Goal:** Handle crashes gracefully and track execution across fork/exec.

### Features

#### Crash Capture
When app crashes (SIGSEGV, SIGABRT, etc.), Frida intercepts before termination:
- Signal type and faulting address
- Stack trace at crash point
- Register state
- Local variables in crashing frame (via DWARF)
- Last N events leading to crash

Query with `eventType: "crash"` to retrieve full crash context.

#### Fork/Exec Following
- Automatically attach to child processes
- Tag events with process ID
- Unified view across all spawned processes
- Session includes all PIDs

#### Enhanced Queries
- Time range filtering (`-5s`, absolute timestamps)
- Duration filtering (find slow functions)
- Process ID filtering
- Combined filters

### Validation Criteria

**Scenario A: Crash debugging**
1. LLM launches app with tracing
2. User triggers a crash (null pointer, etc.)
3. Frida intercepts signal, captures state
4. LLM queries `eventType: "crash"`
5. LLM sees stack trace, registers, locals, and events leading to crash
6. LLM identifies root cause

**Scenario B: Multi-process tracking**
1. LLM launches app that forks worker processes
2. Events captured from parent and all children
3. LLM queries with PID filter to focus on specific process
4. LLM correlates events across processes

**Success:** Crashes don't lose information. Fork/exec doesn't break tracing.

---

## Phase 1d: Test Instrumentation

**Goal:** Universal, machine-readable test output for any language/framework. First-class TDD workflow where the LLM never wastes turns re-running tests to understand failures. Smart stuck detection catches deadlocks and infinite loops in ~8 seconds.

**Full spec:** [specs/2026-02-07-phase-1d-test-instrumentation.md](specs/2026-02-07-phase-1d-test-instrumentation.md)

### Features

#### Backend-Agnostic Test Adapter Architecture

Pluggable adapter system where each adapter owns the full lifecycle: detection, command construction, output parsing, rerun commands, trace suggestions, and language-aware stack capture.

```rust
pub trait TestAdapter: Send + Sync {
    fn detect(&self, project_root: &Path) -> u8;  // 0-100 confidence
    fn name(&self) -> &str;
    fn suite_command(&self, project_root: &Path, level: Option<TestLevel>, env: &HashMap<String, String>) -> TestCommand;
    fn single_test_command(&self, project_root: &Path, test_name: &str) -> TestCommand;
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult;
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack>;
}
```

Auto-detection from `projectRoot` (highest confidence wins), with explicit `framework` override as escape hatch. Falls back to `GenericAdapter` (runs command as-is, returns raw output).

#### Smart Execution Path Switching

Single `debug_test` tool, two paths chosen automatically:

| Condition | Path | Overhead |
|-----------|------|----------|
| No `tracePatterns` or `watches` | Direct subprocess | None |
| `tracePatterns` or `watches` present | Frida (via `debug_launch`) | ~1s |

The LLM doesn't need to know which path is used — the tool returns the same structured format either way (plus `sessionId` on the Frida path for `debug_query`).

#### Test Levels

Explicit `level` parameter filters which tests to run and calibrates hard timeouts:

| Level | Cargo | Catch2 | Hard Timeout |
|-------|-------|--------|-------------|
| `unit` | `cargo test --lib` | `--tag [unit]` | 30s |
| `integration` | `cargo test --test '*'` | `--tag [integration]` | 120s |
| `e2e` | `cargo test --test 'e2e*'` | `--tag [e2e]` | 300s |
| omitted | all tests | all tags | 120s |

#### Universal Structured Output

Every adapter normalizes output into the same format — the LLM never parses raw terminal output:

```json
{
  "framework": "cargo",
  "summary": { "passed": 58, "failed": 2, "skipped": 1, "duration_ms": 2280 },
  "failures": [{
    "name": "parser::tests::test_empty_input",
    "file": "src/parser.rs",
    "line": 142,
    "message": "assertion `left == right` failed...",
    "suggested_traces": ["parser::parse", "parser::handle_empty"]
  }],
  "details": "/tmp/strobe/tests/abc123-2026-02-07.json"
}
```

Minimal response for the context window. Full details (all test names, per-test stdout/stderr, raw framework output) written to temp file — LLM reads it only when needed.

#### Smart Stuck Detection

Multi-signal detector catches deadlocks and infinite loops in ~8 seconds, regardless of test level:

| Signal | Method | Interval |
|--------|--------|----------|
| Output silence | Track last stdout/stderr timestamp | Continuous |
| CPU delta | Sample process CPU time | Every 2s |
| Stack comparison | Compare thread stacks across samples | Triggered at ~6s |

**Decision matrix:**

| Output | CPU | Stacks | Verdict |
|--------|-----|--------|---------|
| Silent | 0% | Same | **Deadlock** — capture stacks, kill, report |
| Silent | 100% | Same | **Infinite loop** — capture stacks, kill, report |
| Silent | 100% | Different | Legit work — wait for hard timeout |
| Active | Any | — | Not stuck |

Stack capture is language-aware (part of adapter trait): native languages use OS-level sampling, VM languages use runtime-specific tools (jstack, py-spy, etc.).

Before killing, captures full thread backtraces so the LLM sees the deadlock graph directly:

```json
{
  "stuck": [{
    "name": "test_concurrent_access",
    "diagnosis": "Deadlock: 0% CPU, identical stacks across 3 samples",
    "threads": [
      { "name": "thread-1", "stack": ["Mutex::lock (mutex.rs:45)", "db::connect (db.rs:12)"] },
      { "name": "thread-2", "stack": ["Mutex::lock (mutex.rs:45)", "db::migrate (db.rs:88)"] }
    ]
  }]
}
```

#### Built-in Adapters

**CargoTestAdapter** — Auto-detected via `Cargo.toml`. Uses `--format json` for structured output. Extracts module paths from test names for trace suggestions.

**Catch2Adapter** — Detected by probing binary with `--list-tests`. Uses `--reporter xml`. Requires `command` parameter (compiled binary path).

**GenericAdapter** — Always available as fallback. Runs command as-is, applies regex heuristics for common patterns.

#### Proactive TDD Onboarding

When no tests exist, the tool returns project info instead of failing:

```json
{
  "no_tests": true,
  "project": { "language": "rust", "build_system": "cargo" },
  "hint": "No tests found. Cargo projects support inline #[test] functions and a tests/ directory."
}
```

Ships with a TDD skill that instructs the LLM to guide users toward test-first debugging when they report bugs without existing tests.

#### Auto-Installation

`strobe install` detects the user's coding agent (Claude Code, OpenCode, Codex) and installs MCP config + skills automatically.

#### Async Test Execution

`debug_test` returns immediately with a `testRunId`. Poll `debug_test_status` for progress and results. The server blocks up to 15s per poll, throttling LLM calls while providing timely completion. Progress includes `currentTest`, `currentTestElapsedMs`, and `currentTestBaselineMs` (historical average from last 10 runs).

#### File-Based Settings System

Three-layer configuration with shallow merge:

```
Built-in defaults (Rust)
  ↓ overridden by
~/.strobe/settings.json (user global)
  ↓ overridden by
<projectRoot>/.strobe/settings.json (project-local)
```

**Current settings:**
- `events.maxPerSession` — Event limit per session (default: 200,000)
- `test.statusRetryMs` — Base polling delay for test status (default: 5,000ms)

Settings are re-read on every tool call (no caching). Replaces previous `STROBE_MAX_EVENTS_PER_SESSION` env var.

**Full spec:** [specs/2026-02-08-settings-system.md](specs/2026-02-08-settings-system.md)

#### Session Management

- `debug_stop({ retain: true })` preserves session data for post-mortem analysis
- `debug_list_sessions` — list all retained sessions with metadata
- `debug_delete_session` — manually delete a retained session

#### Contextual Watch Filtering

Watch variables only during specific functions using the `on` field with pattern matching:

```json
{ "variable": "gTempo", "on": ["audio::process"] }
{ "variable": "gClock", "on": ["midi::*"] }
{ "variable": "gState", "on": ["juce::**"] }
```

Pattern syntax: `*` stops at `::` (shallow), `**` crosses `::` (deep). Patterns resolved at runtime against installed hooks.

**Full docs:** [features/2026-02-06-contextual-watch-filtering.md](features/2026-02-06-contextual-watch-filtering.md)

#### Event Storage Limits

Per-session FIFO buffer (configurable via settings):
- Default: 200,000 events (~56MB DB, fast queries <10ms)
- Audio/DSP: 500,000 events (~140MB DB, moderate queries ~28ms)
- Avoid 1M+ unless necessary (slow queries >300ms)

Oldest events auto-deleted when limit reached. Async cleanup never blocks tracing.

#### MCP Tools
- `debug_test` — Start async test run, returns `testRunId`
- `debug_test_status` — Poll test progress and results
- `debug_list_sessions` — List retained sessions
- `debug_delete_session` — Delete a retained session

### Context-Aware Tracing Defaults

| Context | Default Tracing | Rationale |
|---------|-----------------|-----------|
| `debug_launch` | None (stdout/stderr only) | Output is often enough; add patterns incrementally |
| `debug_test` (full suite) | None | Fast feedback via direct subprocess |
| `debug_test` (rerun) | Suggested patterns | Stack trace tells us what to trace; uses Frida path |

### Validation Criteria

**Scenario A: Fast structured feedback**
1. LLM calls `debug_test({ projectRoot: "/path/to/project" })`
2. Cargo adapter auto-detected, runs `cargo test --format json`
3. Response: structured summary with failures, file:line, messages, suggested traces
4. LLM fixes code, reruns — no turns wasted parsing output

**Scenario B: Stuck test detection**
1. LLM runs integration tests, one test deadlocks
2. Stuck detector: silence + 0% CPU + identical stacks → confirmed in ~8s
3. Thread backtrace captured before kill
4. LLM sees deadlock graph, identifies lock ordering issue immediately

**Scenario C: Instrumented rerun**
1. LLM gets failure with `suggested_traces`
2. LLM calls `debug_test({ test: "test_name", tracePatterns: suggested_traces })`
3. Frida path activates, response includes `sessionId`
4. LLM calls `debug_query` to inspect trace events, finds root cause

**Scenario D: Vibe coder onboarding**
1. User says "I have a bug in the parser"
2. LLM calls `debug_test` → `no_tests: true`
3. LLM suggests creating a test that reproduces the bug first
4. User now has a regression test they didn't know they needed

**Success:** Universal structured output. No test framework lock-in. Stuck tests caught in seconds, not minutes. LLM never re-runs tests just to understand what failed.

---

## Phase 1e: Live Memory Reads

**Goal:** On-demand memory snapshots from running processes without breakpoints or tracing. Point-in-time reads of global/static variables, with polling mode for observing state changes over time.

**Full spec:** [specs/2026-02-08-phase-1e-live-memory-reads.md](specs/2026-02-08-phase-1e-live-memory-reads.md)

### Features

#### Non-Blocking Memory Reads
- Read variables by name (DWARF-resolved) or raw memory addresses
- No breakpoints, no function hooks required
- Multiple targets in a single call (up to 16)
- Struct traversal with configurable depth (1-5)
- Per-target error handling (one bad variable doesn't kill the whole read)

#### Polling Mode
- Sample variables at regular intervals (50-5000ms)
- Events stored as `variable_snapshot` in timeline
- Interleaved with function traces for causal analysis
- Auto-stops after duration (max 30s)

#### Timeline Integration

Poll samples appear as `variable_snapshot` events in the unified timeline:

```
t=0ms     variable_snapshot  { "gTempo": 120.0, "gBufferSize": 0 }
t=12ms    function_enter     midi::processBlock
t=13ms    function_exit      midi::processBlock (ret: 3)
t=100ms   variable_snapshot  { "gTempo": 120.0, "gBufferSize": 3 }
```

Query with `debug_query({ eventType: "variable_snapshot" })`.

#### Buffer Dumps
- `bytes` type writes raw data to file (not in-chat)
- Response includes file path + hex preview of first 32 bytes

#### MCP Tools
- `debug_read` — Read memory on-demand (one-shot or poll mode)

### Validation Criteria

**Scenario A: One-shot memory inspection**
1. LLM launches app, sees suspicious behavior in output
2. LLM calls `debug_read({ sessionId, targets: [{ variable: "gTempo" }] })`
3. Response shows current value without pausing execution
4. LLM identifies wrong value, traces root cause

**Scenario B: Polling for state changes**
1. LLM suspects variable changes incorrectly during audio processing
2. LLM calls `debug_read({ ..., poll: { intervalMs: 100, durationMs: 2000 } })`
3. Returns immediately, samples appear in timeline
4. LLM calls `debug_query({ eventType: "variable_snapshot" })`
5. Timeline shows variable changes interleaved with function calls — causal chain visible

**Success:** LLM can inspect live memory without stopping execution. Polling mode reveals state changes in context of function calls.

---

## Phase 2: Active Debugging

**Goal:** LLM can pause execution at precise points, inspect and modify state, and step through code.

**Supported languages:** C, C++, Rust, Swift (native binaries with standard DWARF). Go deferred to future phase.

**Full spec:** [docs/specs/2026-02-09-active-debugging.md](specs/2026-02-09-active-debugging.md)

### Features

#### Breakpoints (`debug_breakpoint`)
- **Line-level granularity**: Break at any source line via DWARF `.debug_line` → instruction address
- **Function-level**: Break at function entry via pattern matching
- Conditional breakpoints (JS expression evaluation)
- Hit count support (break on Nth occurrence)
- Pause via Frida's `recv().wait()` — blocks calling thread, JS event loop stays alive

#### Stepping (`debug_continue`)
- **step-over**: Next line in same function (one-shot breakpoint at next DWARF line entry)
- **step-into**: Follow function calls (one-shot hooks on callee entries)
- **step-out**: Run until current function returns (hook at return address)

#### State Inspection & Injection
- Inspect variables while paused via existing `debug_read`
- **Write globals/statics** (`debug_write`) — while paused or running
- **Write locals** (`debug_write`) — at breakpoint, via DWARF location lists (Phase 2c)
- Navigate struct fields, array elements

#### Logpoints (`debug_logpoint`)
- Log without stopping execution
- Template substitution: `"tempo={args[0]}, rate={args[1].sampleRate}"`
- Events appear in timeline alongside traces, queryable via `debug_query`

### Implementation Phases

- **Phase 2a**: Core breakpoints + continue + global writes + DWARF line tables
- **Phase 2b**: Stepping (step-over/into/out) + logpoints
- **Phase 2c**: Local variable writes (DWARF location lists, register mapping)

### Validation Criteria

Find a bug that traces alone couldn't catch:
1. LLM sees suspicious pattern in traces
2. LLM sets conditional breakpoint at specific source line
3. App pauses at exact moment of interest
4. LLM inspects local variables, finds wrong value
5. LLM writes corrected value to test hypothesis
6. LLM continues, adds logpoint to verify fix persists
7. LLM identifies root cause

---

## Phase 3: VS Code Integration

**Goal:** Humans can see what the LLM sees. Frictionless onboarding.

### Features

#### One-Click Install
- Available on VS Code marketplace
- No CLI setup required
- Extension manages daemon lifecycle

#### Debug Panel
- Standard VS Code debugging UI
- Breakpoints, call stack, variables
- Integrates with DAP (Debug Adapter Protocol)

#### Execution History Viewer
- Timeline of traced events
- Click to navigate to any point
- Filter by function, module, time

#### Query Panel
- Write structured queries
- Results in sortable table
- Click events to inspect state

### Validation Criteria

Non-technical user can:
1. Install extension from marketplace
2. Open their project
3. Click "Debug with Strobe"
4. See execution traces in UI
5. Click on events to see details

---

## Phase 4: UI Observation

**Goal:** LLM can see the current state of any GUI application — native widgets via accessibility APIs, custom-painted widgets via AI vision — with a single tool call.

**Full spec:** [specs/2026-02-07-ui-observation-interaction-io-channels.md](specs/2026-02-07-ui-observation-interaction-io-channels.md)

### How It Works

The tree is computed on every `debug_ui_tree` call (~30-60ms). LLM token latency dominates — 60ms is invisible.

1. Screenshot (platform API) — ~10ms
2. Perceptual hash — same as last? Reuse cached vision. Changed? Run YOLO + SigLIP — ~10ms
3. Accessibility tree query (platform API) — ~5ms
4. Merge AX nodes with vision boxes (IoU 0.5 matching) — ~1ms
5. Assign stable IDs, project to compact format — ~2ms

### Compact Tree Format

```
[window "ERAE MK2 Simulator" id=w1]
  [toolbar id=tb1]
    [button "Play" id=btn_play enabled]
    [button "Stop" id=btn_stop disabled]
    [slider "Volume" value=0.75 id=sld_vol]
  [panel "Main" id=p1]
    [knob "Filter Cutoff" value≈0.6 id=vk_3 source=vision]
    [knob "Resonance" value≈0.3 id=vk_4 source=vision]
    [list "Presets" id=lst_presets loading]
      [item "Default" id=pr_1]
      [item "Bass Heavy" id=pr_2]
```

### AI Vision Pipeline

For apps with no accessibility support (JUCE, OpenGL, game engines):

| Role | Model | Latency | Hardware |
|------|-------|---------|----------|
| Detection | YOLOv8 (OmniParser weights) | ~5ms | CoreML / Neural Engine |
| Classification | SigLIP 2 | ~2ms/crop | CoreML / GPU |
| Captioning | FastVLM 0.5B | ~30-50ms | CoreML / Neural Engine |

### Platform Support

| Platform | Screenshot | Accessibility | AI Vision |
|----------|-----------|---------------|-----------|
| macOS | CGWindowListCreateImage | AXUIElement | CoreML |
| Linux | XGetImage | AT-SPI2 | ONNX Runtime |

### MCP Tools

- `debug_ui_tree(sessionId)` → compact unified tree (~30-60ms)
- `debug_ui_screenshot(sessionId)` → screenshot as PNG

### Validation Criteria

1. LLM launches ERAE simulator, calls `debug_ui_tree` → receives tree in ~60ms
2. Tree contains native AX elements AND vision-detected knobs/custom widgets
3. Stable IDs persist across consecutive calls
4. LLM describes UI from tree alone

---

## Phase 5: UI Interaction

**Goal:** LLM can control GUI applications through intent-based actions, with a VLM-powered motor layer that learns how to interact with unknown widgets.

**Full spec:** [specs/2026-02-07-ui-observation-interaction-io-channels.md](specs/2026-02-07-ui-observation-interaction-io-channels.md)

### Intent-Based Actions

The LLM expresses **what** it wants. The motor layer figures out **how**.

```
debug_ui_action(sessionId, action)

  click(id="btn_play")
  set_value(id="sld_vol", value=0.5)
  type(id="txt_name", text="hello")
  select(id="lst_presets", item="Bass Heavy")
  scroll(id="lst_presets", direction="down", amount=3)
  drag(from="track_1", to="slot_3")
  key(key="s", modifiers=["cmd"])
```

### Motor Layer Strategy

1. **Native AX action** — if accessibility exposes increment/decrement, use it
2. **VLM classification** — vision model looks at widget crop, predicts interaction type ("vertical-drag knob")
3. **Execute + verify** — perform motor plan, re-read tree, check value changed
4. **Cache profile** — learned interaction cached for instant reuse: `(app, role, label) → motor strategy`

First interaction with unknown widget: ~300ms (VLM + execute + verify). Cached: ~50ms.

### Platform Support

| Platform | Input | Vision Motor |
|----------|-------|-------------|
| macOS | CGEvent | CoreML (FastVLM) |
| Linux | XTest | ONNX Runtime |

### Validation Criteria

1. `set_value` on a JUCE knob → VLM classifies → drags → verifies → cached
2. Second call to same knob type uses cached profile (no VLM)
3. Clicks, text input, list selection all work via intent API

---

## Phase 6: I/O Channel Abstraction + Scenario Runner

**Goal:** Unify all app I/O under a common channel model. Introduce the scenario runner for autonomous runtime testing.

**Full spec:** [specs/2026-02-07-ui-observation-interaction-io-channels.md](specs/2026-02-07-ui-observation-interaction-io-channels.md)

### Core Insight

UI is just one I/O channel. MIDI, audio, network, files, stdout/stderr, and function traces are all I/O channels. Each can send stimuli and/or capture observations. A JUCE synth test and a headless Rust API test use the same scenario format — just different channels.

### Channel Traits

```rust
trait InputChannel: Send + Sync {
    fn name(&self) -> &str;                                    // "ui", "midi", "net:8080"
    fn send(&self, action: ChannelAction) -> Result<ActionResult>;
}

trait OutputChannel: Send + Sync {
    fn name(&self) -> &str;
    fn start_capture(&self) -> Result<()>;
    fn stop_capture(&self) -> Result<()>;
    fn query(&self, filter: OutputFilter) -> Result<Vec<ChannelEvent>>;
}
```

### Channel Registry

Channels are registered explicitly on launch or mid-session:

```
debug_launch(command, channels: ["ui", "midi"])
debug_channel_add(sessionId, "net:8080")
```

Existing capabilities (stdout/stderr, function traces) are wrapped as channels automatically — always present.

### Scenario Runner

Flat action list. Executes sequentially. On failure: **stops, returns error, process stays alive**. The LLM takes over as debugger with full tool access.

```json
{
  "channels": ["ui", "midi"],
  "steps": [
    {"do": "ui.set_value", "id": "knob_release", "value": 0.0},
    {"do": "midi.send", "type": "noteOn", "note": 60, "velocity": 100},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOff", "note": 60},
    {"wait": 500},
    {"assert": "trace", "fn": "Voice::free", "called": true}
  ]
}
```

Failure returns minimal context (step number, expected vs actual, session ID). The LLM pulls what it needs via `debug_ui_tree`, `debug_query`, etc.

### MCP Tools

- `debug_channel_add(sessionId, channel)` → register channel on running session
- `debug_channel_list(sessionId)` → list active channels
- `debug_channel_send(sessionId, channel, action)` → send stimulus to any non-UI channel
- `debug_channel_query(sessionId, channel, filter?)` → query captured output
- `debug_test_scenario(sessionId, scenario)` → execute scenario, return pass/fail

### Validation Criteria

1. ERAE synth scenario: UI knob + MIDI input + trace assertion — all in one scenario
2. Headless API scenario: HTTP request + trace assertion — same format, no UI
3. Failure mid-scenario → LLM receives error → investigates with existing tools → process still alive

---

## Phase 7: Concrete I/O Channels

**Goal:** Implement the most important non-UI I/O channels. Each is a self-contained implementation of the channel traits.

**Full spec:** [specs/2026-02-07-ui-observation-interaction-io-channels.md](specs/2026-02-07-ui-observation-interaction-io-channels.md)

### MIDI Channel

Send MIDI to the target app, capture MIDI output. Virtual MIDI port strategy.

| Platform | API |
|----------|-----|
| macOS | CoreMIDI virtual port |
| Linux | ALSA sequencer virtual port |

### Audio Channel

Inject audio, capture output, compute metrics (RMS, peak, FFT).

| Platform | Capture | Injection |
|----------|---------|-----------|
| macOS | CoreAudio process tap (macOS 14.2+) | Virtual audio device |
| Linux | PipeWire / JACK | JACK client connection |

### Network Channel

Send packets/requests, capture outgoing traffic via Frida socket intercept (cross-platform).

### File Channel

Write/delete files, watch for app file changes via FSEvents (macOS) / inotify (Linux).

### Channel Summary

| Channel | Input | Output | Complexity |
|---------|-------|--------|-----------|
| `ui` | CGEvent / XTest | AX + Vision | High (Phase 4-5) |
| `midi` | CoreMIDI / ALSA | CoreMIDI / ALSA | Medium |
| `audio` | Virtual device | CoreAudio / JACK | Medium |
| `net` | Socket from daemon | Frida intercept | Medium |
| `file` | Filesystem ops | FSEvents / inotify | Low |
| `trace` | *(existing)* | *(existing)* | Already done |
| `stdio` | stdin injection | *(existing)* | Already done |

### Validation Criteria

1. MIDI: send noteOn to ERAE → capture MIDI output → assert correct response
2. Audio: inject tone → capture output → assert RMS above threshold
3. Full scenario: UI + MIDI + audio + trace in one test

---

## Future Phases

### Phase 8: Advanced Threading Tools
- Lock acquisition tracing
- Deadlock detection
- Spinlock detection
- Thread timeline visualization
- Race condition hints

### Phase 9: Additional Languages & Runtimes
- JavaScript/TypeScript via Chrome DevTools Protocol (Node.js, browser, Electron)
- Python (via sys.settrace or Frida)
- Go (enhanced DWARF support, goroutine awareness)
- Java/Kotlin (via ART hooks on Android)

### Phase 10: Windows Support
- Frida works on Windows
- PDB parsing for symbols
- Named pipes for daemon communication
- Windows-specific UI capture (DXGI + UI Automation)

### Phase 11: Distributed Tracing
- Follow requests across services
- Correlate traces from multiple processes
- Network request interception

### Commercial Features (Strobe Cloud)
- CI/CD integration
- Automatic test generation from traces
- Regression detection across commits

---

## Contributor Extensibility

The architecture is designed so **anyone can add support for new languages, I/O channels, or platform backends** without understanding the whole codebase.

### Adding Language Support

Implement the `Collector` trait:
- `attach` - Connect to target process
- `detach` - Clean disconnect
- `set_trace_patterns` - Update what gets traced
- `poll_events` - Receive trace events

Emit events conforming to the unified `TraceEvent` schema, and the rest of the system (storage, queries, MCP) works automatically.

### Adding I/O Channels

Implement `InputChannel`, `OutputChannel`, or both:
- `InputChannel::send(action)` - Send stimulus to target app
- `OutputChannel::start_capture()` / `stop_capture()` - Control recording
- `OutputChannel::query(filter)` - Query captured events

The channel automatically works with `debug_channel_send`, `debug_channel_query`, and the scenario runner (`debug_test_scenario`). Examples: MIDI, audio, serial, custom protocol.

### Adding Platform Backends

Implement platform traits for a new OS:
- `UIObserver` - Screenshot + accessibility tree (e.g., UI Automation for Windows)
- `UIInput` - Mouse/keyboard injection (e.g., SendInput for Windows)
- `VisionPipeline` - AI model inference (e.g., DirectML for Windows)

### What Contributors Don't Touch

- SQLite storage layer
- Query engine
- MCP protocol handling
- Scenario runner logic
- VS Code extension
- Frida agent (unless adding native support)

Clean interfaces = more contributors = more capabilities.

---

## Performance Characteristics

### Overhead

| Scenario | Overhead |
|----------|----------|
| User code tracing (default) | 5-15% CPU |
| Full tracing (all functions) | 20-40% CPU |
| Breakpoints only (no tracing) | < 1% CPU |
| UI tree (on-demand) | ~30-60ms per call |

### Throughput

| Metric | Target |
|--------|--------|
| Events per second | 100k+ |
| Query latency (simple) | < 10ms |
| Query latency (complex) | < 100ms |
| Storage per event | ~200 bytes |

### Scalability

- SQLite handles millions of events
- WAL mode for concurrent read/write
- Configurable retention (auto-delete old events)
- Ring buffer under memory pressure

---

## Security & Privacy

### What We Can't Do

- Debug processes owned by other users
- Debug setuid binaries
- Elevate privileges
- Access kernel memory

### Data Handling

- All data stored locally
- No telemetry in open source version
- Session data deleted with `debug_stop`
- No network calls unless explicitly configured
