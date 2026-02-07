# Phase 1d: Test Instrumentation

**Date:** 2026-02-07
**Status:** Draft
**Goal:** Universal, machine-readable test output for any language/framework. First-class TDD workflow where the LLM never wastes turns re-running tests to understand failures.

---

## Problem

When an LLM runs tests via bash, it gets unstructured terminal output:

```
test result: ok. 59 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.21s
test result: FAILED. 16 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 47.44s
```

The LLM doesn't know *which* test failed, *where*, or *why*. It re-runs with different flags to get details — burning turns and time. Some frameworks (TS/Jest) use dynamic PTY output that doesn't even survive tool call capture.

Stuck tests are worse: deadlocks and infinite loops cause the LLM to wait indefinitely. The user has to manually intervene or 10 minutes pass before timeout.

The current workaround is hand-building per-project test infrastructure (custom scripts, CLAUDE.md instructions, log files). This is time-consuming and requires expertise most developers don't have.

---

## Solution

A single MCP tool (`debug_test`) that:

1. **Auto-detects** the test framework from `projectRoot`
2. **Runs tests** and parses output into a universal structured format
3. **Detects stuck tests** via multi-signal analysis (CPU + stack sampling)
4. **Captures thread stacks** before killing stuck processes
5. **Suggests trace patterns** from failure context for instrumented reruns
6. **Switches to Frida automatically** when instrumentation is requested

Two execution paths, chosen automatically:

```
debug_test(params)
    │
    ├─ Has tracePatterns/watches? → Frida path (instrumented, ~1s overhead)
    │
    └─ No instrumentation?       → Direct subprocess (fast, no overhead)
```

---

## Architecture

```
┌──────────────────────────────────────────────────┐
│                debug_test MCP tool               │
├──────────────────────────────────────────────────┤
│             TestRunner (orchestrator)             │
│  - Auto-detects framework from projectRoot       │
│  - Delegates to matched adapter                  │
│  - Manages subprocess or Frida session           │
│  - Runs stuck detector in parallel               │
│  - Writes full details to temp file              │
├────────────┬───────────────┬─────────────────────┤
│ CargoTest  │    Catch2     │   GenericAdapter     │
│ Adapter    │    Adapter    │   (raw fallback)     │
└────────────┴───────────────┴─────────────────────┘
```

### Adapter Trait

Each adapter owns the full lifecycle: detection, command construction, output parsing, rerun commands, trace suggestions, and language-aware stack capture.

```rust
pub trait TestAdapter: Send + Sync {
    /// Scan projectRoot for signals. Returns 0-100 confidence. Highest wins.
    fn detect(&self, project_root: &Path) -> u8;

    /// Human-readable name: "cargo", "catch2", "generic"
    fn name(&self) -> &str;

    /// Build command for running tests.
    /// `level` filters to unit/integration/e2e. `None` = run all.
    fn suite_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
        env: &HashMap<String, String>,
    ) -> TestCommand;

    /// Build command for running a single test by name.
    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> TestCommand;

    /// Parse raw stdout + stderr into structured results.
    fn parse_output(
        &self,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> TestResult;

    /// Given a failure, suggest trace patterns for instrumented rerun.
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;

    /// Capture thread stacks for stuck detection. Language-aware.
    /// Native languages: OS-level sampling.
    /// VM languages: runtime-specific (jstack, py-spy, etc.)
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack>;
}

pub enum TestLevel {
    Unit,
    Integration,
    E2e,
}

pub struct TestCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}
```

### Adapter Implementations

**CargoTestAdapter** (detect: `Cargo.toml` → confidence 90):

| Level | Command |
|-------|---------|
| `unit` | `cargo test --lib --format json` |
| `integration` | `cargo test --test '*' --format json` |
| `e2e` | `cargo test --test 'e2e*' --format json` |
| omitted | `cargo test --format json` |
| single test | `cargo test --format json -- {name} --exact` |

- Parse: Rust's `--format json` gives event-per-line JSON (test name, status, stdout, duration)
- Trace suggestions: extract module path from test name → `module::*`
- Stack capture: OS-level (`sample` on macOS, `/proc/task/*/stack` on Linux)

**Catch2Adapter** (detect: probe binary with `--list-tests` → confidence 85):

| Level | Command |
|-------|---------|
| `unit` | `{binary} --reporter xml [unit]` |
| `integration` | `{binary} --reporter xml [integration]` |
| `e2e` | `{binary} --reporter xml [e2e]` |
| omitted | `{binary} --reporter xml` |
| single test | `{binary} --reporter xml "{test_name}"` |

- Requires `command` parameter (compiled binary path)
- Detect: run `command --list-tests`, if it succeeds it's Catch2
- Parse: XML reporter output (test case, section, assertions, file:line, expression vs expanded)
- Trace suggestions: extract function names from section names and assertion locations
- Stack capture: OS-level (same as Cargo — both are native code)

**GenericAdapter** (confidence: always 1, fallback):

- Runs user's command as-is
- Parse: regex heuristics for common patterns (`FAIL`, `PASS`, `assert`, `file:line`)
- Single test: not supported (no `rerun` field in failures)
- Trace suggestions: none
- Stack capture: OS-level best-effort

### Detection Flow

1. If `framework` param provided → use that adapter directly
2. If `command` param provided → probe binary to identify framework
3. Otherwise → scan `projectRoot`:
   - Check for `Cargo.toml` → CargoTestAdapter
   - Check for CMakeLists.txt with Catch2 → Catch2Adapter (still needs `command`)
   - Nothing matches → GenericAdapter
4. If adapter needs info it doesn't have (e.g., Catch2 detected but no binary path):
   ```json
   { "hint": "Catch2 project detected. Provide the test binary path via command parameter." }
   ```

---

## MCP Tool Schema

### Request

```json
{
  "projectRoot": "/Users/alex/strobe",
  "framework": "cargo",
  "level": "unit",
  "test": "parser::tests::test_empty_input",
  "command": "./build/test_runner",
  "tracePatterns": ["parser::parse", "parser::handle_empty"],
  "watches": [{ "variable": "gState", "on": ["parser::*"] }],
  "env": { "RUST_BACKTRACE": "1" },
  "timeout": 60000
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `projectRoot` | yes | Project root for adapter detection |
| `framework` | no | Override auto-detection: `"cargo"`, `"catch2"` |
| `level` | no | Filter: `"unit"`, `"integration"`, `"e2e"`. Omit for all. |
| `test` | no | Run a single test by name |
| `command` | no | Test binary path (required for compiled test frameworks) |
| `tracePatterns` | no | Presence triggers Frida path |
| `watches` | no | Presence triggers Frida path |
| `env` | no | Additional environment variables |
| `timeout` | no | Hard timeout in ms (default per level, see Stuck Detection) |

### Response — Test Results

```json
{
  "framework": "cargo",
  "summary": { "passed": 58, "failed": 2, "skipped": 1, "duration_ms": 2280 },
  "failures": [
    {
      "name": "parser::tests::test_empty_input",
      "file": "src/parser.rs",
      "line": 142,
      "message": "assertion `left == right` failed\n  left: None\n  right: Some(Node { kind: Empty })",
      "suggested_traces": ["parser::parse", "parser::handle_empty"]
    }
  ],
  "details": "/tmp/strobe/tests/abc123-2026-02-07.json"
}
```

### Response — All Passed

```json
{
  "framework": "cargo",
  "summary": { "passed": 82, "failed": 0, "skipped": 0, "duration_ms": 410 },
  "failures": [],
  "details": "/tmp/strobe/tests/abc123-2026-02-07.json"
}
```

### Response — Stuck Test Detected

```json
{
  "framework": "cargo",
  "summary": { "passed": 5, "failed": 0, "stuck": 1, "duration_ms": 8400 },
  "stuck": [
    {
      "name": "test_concurrent_access",
      "elapsed_ms": 8400,
      "diagnosis": "Deadlock: 0% CPU, identical stacks across 3 samples",
      "threads": [
        { "name": "test-thread-1", "stack": ["sync::Mutex::lock (mutex.rs:45)", "db::connect (db.rs:12)"] },
        { "name": "test-thread-2", "stack": ["sync::Mutex::lock (mutex.rs:45)", "db::migrate (db.rs:88)"] }
      ],
      "suggested_traces": ["sync::Mutex::lock", "db::connect", "db::migrate"]
    }
  ],
  "details": "/tmp/strobe/tests/abc123-2026-02-07.json"
}
```

### Response — No Tests Found

```json
{
  "framework": "cargo",
  "summary": null,
  "no_tests": true,
  "project": { "language": "rust", "build_system": "cargo", "test_files": 0 },
  "hint": "No tests found. Cargo projects support inline #[test] functions and a tests/ directory."
}
```

### Response — Instrumented Rerun (Frida path)

When `tracePatterns` or `watches` are present, the response includes `sessionId` for `debug_query`:

```json
{
  "framework": "cargo",
  "summary": { "passed": 0, "failed": 1, "skipped": 0, "duration_ms": 150 },
  "failures": [{ "name": "test_empty_input", "..." : "..." }],
  "sessionId": "strobe-test-2026-02-07-14h32",
  "details": "/tmp/strobe/tests/abc123-2026-02-07.json"
}
```

The LLM can then call `debug_query({ sessionId, ... })` to inspect trace events around the failure.

---

## Stuck Detection

### Multi-Signal Architecture

The stuck detector runs in parallel with the test subprocess. It uses three signals, from cheap to expensive:

**Signal 1: Output monitoring** (continuous)
- Track last stdout/stderr timestamp
- Necessary condition but never sufficient alone
- A silent test doing CPU-bound work is not stuck

**Signal 2: CPU delta sampling** (every 2s)
- Read process CPU time, compute delta between samples
- Three states:
  - `delta == 0` → blocked (all threads sleeping)
  - `delta > 0, constant high` → spinning (infinite loop)
  - `delta > 0, varying` → working

**Signal 3: Stack sampling** (triggered when signals 1+2 are suspicious)
- Take thread stack samples 3 seconds apart
- Compare: identical stacks = definitively stuck, different stacks = making progress

### Decision Matrix

| Output | CPU | Stacks | Verdict |
|--------|-----|--------|---------|
| Silent | 0% | Same | **Deadlock** — kill and report |
| Silent | 100% | Same | **Infinite loop** — kill and report |
| Silent | 100% | Different | Legit work — extend, wait for hard timeout |
| Silent | Variable | Different | Legit work — extend, wait for hard timeout |
| Active | Any | — | Not stuck — don't check stacks |

### Timing

Stuck detection is fast and universal — same thresholds regardless of test level:

| Phase | Timing |
|-------|--------|
| CPU sampling starts | Immediately (every 2s) |
| Stack sampling triggered | After ~6s of suspicious signals |
| Stuck confirmed | ~8s from first suspicious signal |
| Backtrace captured | Before kill (~1s) |

Hard timeout is the only thing that varies per level — it's a safety net for tests that are legitimately slow but not stuck:

| Level | Hard timeout (default) |
|-------|----------------------|
| `unit` | 30s |
| `integration` | 120s |
| `e2e` | 300s |
| custom | `timeout` param |

### Language-Aware Stack Capture

Stack capture is part of the adapter trait. Each adapter uses the best method for its language runtime:

| Language | Method | Stack quality |
|----------|--------|--------------|
| C / C++ | OS-level: `sample` (macOS), `/proc/task/*/stack` (Linux) | Excellent — native frames |
| Rust | OS-level (same as C/C++) | Excellent — native frames |
| Go (future) | SIGABRT → goroutine dump on stderr | Excellent — goroutine stacks |
| Node.js (future) | SIGUSR1 → inspector protocol | Good — JS frames |
| Python (future) | `py-spy dump --pid` or `faulthandler` | Good — Python frames |
| Java (future) | `jstack PID` | Good — Java frames |
| Generic | OS-level best-effort | Varies — at minimum shows syscall/wait state |

For Phase 1d, Cargo and Catch2 both produce native code → OS-level stack sampling works perfectly.

---

## Details File

Every `debug_test` call writes a full details file to `/tmp/strobe/tests/{session-id}.json`. The MCP response includes the path.

Contents:
- All test names with status (pass/fail/skip/stuck)
- Per-test timing
- Full stdout/stderr per test
- Stack traces for failures
- Thread dumps for stuck tests
- Raw framework output (JSON/XML as produced)
- Adapter metadata (framework, command used, level)

The LLM reads this file only when the summary response isn't enough. Keeps the MCP response token-efficient.

---

## Execution Paths

### Fast Path (direct subprocess)

Used when no `tracePatterns` or `watches` are specified.

```
TestRunner::run()
  ├─ Detect adapter
  ├─ adapter.suite_command() or adapter.single_test_command()
  ├─ Spawn subprocess (tokio::process::Command)
  ├─ Capture stdout/stderr via pipes
  ├─ Start StuckDetector in parallel
  │   ├─ CPU sampling loop (2s interval)
  │   ├─ Output silence monitoring
  │   └─ Stack sampling (triggered on suspicious signals)
  ├─ Wait for exit or stuck detection or hard timeout
  ├─ If stuck: adapter.capture_stacks() → kill → return stuck response
  ├─ If normal exit: adapter.parse_output()
  ├─ Write details file
  └─ Return structured response
```

### Frida Path (instrumented)

Used when `tracePatterns` or `watches` are present. Reuses existing `debug_launch` infrastructure.

```
TestRunner::run_instrumented()
  ├─ Detect adapter
  ├─ adapter.single_test_command()  (typically single test rerun)
  ├─ Call SessionManager::spawn_with_frida() with test command
  ├─ Apply tracePatterns and watches via debug_trace
  ├─ Start StuckDetector in parallel
  ├─ Wait for exit or stuck or timeout
  ├─ adapter.parse_output() from captured stdout/stderr
  ├─ Write details file
  └─ Return structured response + sessionId
```

The `sessionId` lets the LLM call `debug_query` to inspect trace events, function arguments, return values, etc.

---

## Test Levels

The `level` parameter filters which tests to run. Each adapter maps levels to framework-specific commands.

### Cargo Mapping

| Level | Command | What it runs |
|-------|---------|-------------|
| `unit` | `cargo test --lib --format json` | `#[test]` functions in `src/` |
| `integration` | `cargo test --test '*' --format json` | Files in `tests/` |
| `e2e` | `cargo test --test 'e2e*' --format json` | Files in `tests/` matching `e2e*` |
| omitted | `cargo test --format json` | Everything |

### Catch2 Mapping

| Level | Command | What it runs |
|-------|---------|-------------|
| `unit` | `{binary} --reporter xml [unit]` | Tests tagged `[unit]` |
| `integration` | `{binary} --reporter xml [integration]` | Tests tagged `[integration]` |
| `e2e` | `{binary} --reporter xml [e2e]` | Tests tagged `[e2e]` |
| omitted | `{binary} --reporter xml` | All tests |

### Generic Mapping

Generic adapter doesn't know how to filter by level. If `level` is provided without `command`, returns an error with hint.

---

## Proactive TDD Onboarding

### MCP Tool Description

The `debug_test` tool description is crafted so LLMs know when to use it:

> Run tests and get structured results. Auto-detects the test framework.
> If no tests exist, returns project info and suggests test setup.
> Use this instead of running test commands via bash — it provides
> machine-readable output, stuck detection, and failure analysis.

### Skill: strobe-tdd

A markdown skill file shipped with Strobe, installed into the user's agent system:

> When a user reports a bug and no test infrastructure exists, suggest
> creating a test that reproduces the bug first. Guide them through:
> 1. Creating a minimal test case for the reported bug
> 2. Running it with `debug_test` to confirm it fails
> 3. Fixing the bug
> 4. Running `debug_test` again to confirm the fix
>
> This is faster than manual reproduction and prevents regressions.

### Auto-Installation

`strobe install` detects the user's coding agent and installs MCP config + skills:

| Agent | Detection | MCP Config | Skill Location |
|-------|-----------|-----------|----------------|
| Claude Code | `~/.claude/` exists | `~/.claude/mcp.json` | `~/.claude/skills/strobe-tdd/` |
| OpenCode | `opencode.json` exists | `opencode.json` | Equivalent location |
| Codex | `.codex/` exists | `.codex/` config | Equivalent location |

---

## File Layout

### New Files

```
src/test/
  mod.rs              — TestRunner orchestrator, auto-detection, path switching
  adapter.rs          — TestAdapter trait, TestCommand, TestResult, TestLevel types
  cargo_adapter.rs    — Cargo test adapter implementation
  catch2_adapter.rs   — Catch2 adapter implementation
  generic_adapter.rs  — Raw fallback adapter
  stuck_detector.rs   — Multi-signal stuck detection engine
  output.rs           — Details file writer (JSON to /tmp/strobe/tests/)

install/
  mod.rs              — strobe install entry point
  detect.rs           — Agent system detection
  install.rs          — MCP config + skill file installation

skills/
  strobe-tdd.md       — TDD guidance skill (markdown)
```

### Modified Files

```
src/mcp/types.rs      — DebugTestRequest, DebugTestResponse schemas
src/daemon/server.rs   — handle_tools_call("debug_test", ...)
src/mcp/tools.rs       — debug_test tool definition + description
src/lib.rs             — pub mod test
```

---

## Scope

### Phase 1d Deliverables

1. `TestAdapter` trait + `TestRunner` orchestrator
2. `CargoTestAdapter` — fully working, validated on strobe's own test suite
3. `Catch2Adapter` — fully working, validated on erae_mk2_simulator
4. `GenericAdapter` — raw fallback for unknown frameworks
5. `StuckDetector` — multi-signal stuck detection with thread backtrace capture
6. `debug_test` MCP tool wired into the daemon
7. Details file writer
8. `strobe install` command with agent detection
9. TDD skill markdown

### Explicitly Deferred

- Jest / Vitest / pytest adapters (future — community can contribute via TestAdapter trait)
- Test coverage tracking
- Watch mode / continuous testing
- CI/CD integration
- Test generation from traces

---

## Validation Criteria

### Scenario A: Fast structured feedback (cargo)

1. LLM calls `debug_test({ projectRoot: "/Users/alex/strobe" })`
2. Cargo adapter auto-detected, runs `cargo test --format json`
3. Response: structured summary with 2 failures, file:line, messages, suggested traces
4. LLM reads failure, fixes code, reruns — all without re-running to get details

### Scenario B: Stuck test detection

1. LLM calls `debug_test({ projectRoot: "...", level: "integration" })`
2. One test deadlocks (two threads waiting on each other's mutex)
3. Stuck detector: silence + 0% CPU + identical stacks → confirmed in ~8s
4. Thread backtrace captured before kill
5. Response includes `stuck` array with thread stacks showing the deadlock
6. LLM sees the lock ordering issue immediately

### Scenario C: Instrumented rerun

1. LLM gets failure from Scenario A with `suggested_traces: ["parser::parse"]`
2. LLM calls `debug_test({ test: "test_empty_input", tracePatterns: ["parser::parse"] })`
3. Frida path activates, runs single test with tracing
4. Response includes `sessionId`
5. LLM calls `debug_query({ sessionId, function: { contains: "parse" }, verbose: true })`
6. LLM sees argument values and return values, identifies root cause

### Scenario D: Vibe coder onboarding

1. User says "I have a bug in the parser"
2. LLM has strobe-tdd skill + debug_test tool
3. LLM calls `debug_test({ projectRoot: "..." })` → `no_tests: true`
4. LLM suggests: "Let's create a test that reproduces this bug first"
5. LLM writes a test, runs `debug_test` → confirms failure
6. LLM fixes bug, runs `debug_test` → passes
7. User now has a regression test they didn't know they needed

### Scenario E: Catch2 (erae simulator)

1. LLM calls `debug_test({ projectRoot: "/Users/alex/erae_touch_mk2_fw", command: "./build/test_runner" })`
2. Catch2 adapter detected via `--list-tests` probe
3. XML output parsed into universal format
4. Same structured response as cargo — the LLM doesn't care which framework
