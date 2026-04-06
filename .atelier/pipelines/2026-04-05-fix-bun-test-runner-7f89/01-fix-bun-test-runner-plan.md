# Fix Bun Test Runner — Implementation Plan

## Goal

Make Strobe's Bun test adapter fully functional: real-time progress tracking, complete error reporting, monorepo workspace support, individual test file execution, and level-based test selection — verified against the liaison project at `/Users/alex/liaison/`.

## Scope

**In scope:**
- Replace JUnit XML parsing with native Bun output parsing (progress + final results)
- Dedicated `bun_adapter::update_progress()` for streaming progress
- Add `cwd` and `remove_env` fields to `TestCommand` for monorepo + env handling
- Monorepo detection: scan workspaces for `bunfig.toml`
- Parse test orchestrator scripts (`scripts/test-run.ts`) for level → directory mapping
- Support test file paths (not just name patterns) in `single_test_command`
- Fix detection for monorepo projects with mixed frameworks (vitest + bun:test)
- Env handling: strip `DATABASE_URL`, set `NO_COLOR=1`

**Out of scope:**
- Vitest / Playwright adapter changes (already functional)
- Composite suites (`test:full` — user invokes levels individually via `debug_test`)
- `e2e-parallel` (custom parallel runner with worker processes)
- Bun function-level tracing (JSC symbols stripped in release builds)

## Current State

Five root causes prevent Bun tests from working:

1. **No progress tracking** — Bun routes to `vitest_adapter::update_progress` (line 339 of `mod.rs`), whose fallback looks for `"status":"passed"` (Vitest JSON) or `✓ (5 tests) 120ms` (Vitest suite format). Bun outputs `✓ test name [1.23ms]` — never matched. Progress stays 0/0/0 throughout.

2. **JUnit-only parsing kills useful output** — `bun_adapter.rs:36-40` builds `bun test --reporter=junit --reporter-outfile=/dev/stdout`, which replaces Bun's default reporter. Stack traces, file/line info, error context — all lost. JUnit only delivers at process exit — zero streaming.

3. **Detection lacks monorepo awareness** — `bun_adapter.rs:11-16`: if `"vitest"` appears in root `package.json`, adapter returns 0. Liaison works today (returns 85 via bun.lock) because vitest is only in `apps/web/package.json`, not the root. But a root-level vitest dependency would incorrectly bail even when bun:test lives in a separate workspace. Also never checks for `bunfig.toml`, missing the higher-confidence 90 signal.

4. **No test file path support** — `bun_adapter.rs:75-87`: `single_test_command` always uses `--test-name-pattern`. No way to run `bun test src/middleware/auth.test.ts`.

5. **No cwd / workspace support** — `TestCommand` (adapter.rs:14-18) has no `cwd` field. Tests always run from `project_root` (mod.rs:295). In monorepos, bun:test needs the workspace dir where `bunfig.toml` lives (e.g., `apps/api/`).

## Architecture Approach

Drop JUnit XML for Bun. Parse Bun's native stderr output directly — it streams per-test `✓`/`✗` markers with durations, and includes stack traces with file:line for failures. Add `cwd` and `remove_env` to `TestCommand` so the adapter can target monorepo workspaces and strip env vars (`DATABASE_URL`) that block Bun's `.env.test` auto-loading. Parse the project's test orchestrator script (`test-run.ts`) to map `TestLevel` values to the correct `bun test <dirs>` invocation and workspace `cwd`.

---

## Task 1: Add `cwd` and `remove_env` to TestCommand

**Files:**
- Modify: `src/test/adapter.rs` (lines 13-18) — add fields to `TestCommand`
- Modify: `src/test/mod.rs` (lines 274-299) — use `cwd` in spawn, apply `remove_env`
- Modify: every adapter returning `TestCommand` — add default field values
- Test: `src/test/bun_adapter.rs` (inline `#[cfg(test)]`)

### 1.1 Write failing test

```rust
// In src/test/bun_adapter.rs — add to existing #[cfg(test)] mod tests
#[test]
fn test_command_cwd_and_remove_env() {
    let cmd = TestCommand {
        program: "bun".to_string(),
        args: vec!["test".to_string()],
        env: HashMap::new(),
        cwd: Some("/project/apps/api".to_string()),
        remove_env: vec!["DATABASE_URL".to_string()],
    };
    assert_eq!(cmd.cwd.as_deref(), Some("/project/apps/api"));
    assert_eq!(cmd.remove_env, vec!["DATABASE_URL".to_string()]);
}

#[test]
fn test_command_defaults_backward_compatible() {
    let cmd = TestCommand {
        program: "cargo".to_string(),
        args: vec!["test".to_string()],
        env: HashMap::new(),
        cwd: None,
        remove_env: vec![],
    };
    assert!(cmd.cwd.is_none());
    assert!(cmd.remove_env.is_empty());
}
```

### 1.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_command_cwd" })
```
Expected: compilation error — `no field named 'cwd' on type 'TestCommand'`.

### 1.3 Implementation

**adapter.rs** — add fields to `TestCommand`:
```rust
#[derive(Debug, Clone)]
pub struct TestCommand {
    pub program: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    /// Override working directory. None = use project_root.
    pub cwd: Option<String>,
    /// Env vars to remove from the inherited environment.
    pub remove_env: Vec<String>,
}
```

**mod.rs** — wire `cwd` into spawn and apply `remove_env` (around lines 274-299):
```rust
// After building combined_env (line 277):
for key in &test_cmd.remove_env {
    combined_env.remove(key);
}

// For spawn_with_frida cwd parameter (line 295):
let spawn_cwd = test_cmd.cwd.as_deref()
    .unwrap_or(project_root.to_str().unwrap_or("."));
// ...
let pid = session_manager.spawn_with_frida(
    session_id,
    &program,
    &test_cmd.args,
    Some(spawn_cwd),          // was: Some(project_root.to_str()...)
    project_root.to_str().unwrap_or("."),
    Some(&combined_env),
    has_trace_patterns,
    None,
).await?;
```

**All existing adapters** — add `cwd: None, remove_env: vec![]` to every `TestCommand` construction. Files to update:
- `cargo_adapter.rs`
- `catch2_adapter.rs`
- `pytest_adapter.rs`
- `unittest_adapter.rs`
- `vitest_adapter.rs`
- `jest_adapter.rs`
- `deno_adapter.rs`
- `go_adapter.rs`
- `gtest_adapter.rs`
- `mocha_adapter.rs`
- `playwright_adapter.rs`

Each adapter has 1-3 `TestCommand { ... }` constructions. Add the two new fields with defaults.

### 1.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_command_cwd" })
```
Both tests pass. Existing adapter tests also pass (backward compatible defaults).

### 1.5 Checkpoint

TestCommand now supports `cwd` and `remove_env`. All existing adapters compile with default values. The test runner uses `cwd` when set and removes specified env vars.

**Edge cases covered:**
- `cwd: None` → falls back to `project_root` (all existing adapters)
- `remove_env: vec![]` → no-op (existing behavior preserved)
- `cwd` set to nonexistent path → Frida spawn returns error (existing error handling)

---

## Task 2: Parse Bun's native test output

**Files:**
- Modify: `src/test/bun_adapter.rs` — add `parse_bun_output()`, update `parse_output()` to use it
- Test: `src/test/bun_adapter.rs` (inline `#[cfg(test)]`)

### 2.1 Write failing tests

```rust
// In src/test/bun_adapter.rs — replace or augment existing test constants

const BUN_NATIVE_PASS: &str = "\
bun test v1.2.0 (abc123def)

src/math.test.ts:
✓ Math > adds two numbers [1.23ms]
✓ Math > subtracts [0.45ms]

 2 pass
 0 fail
 2 expect() calls

Ran 2 tests across 1 files. [10.00ms]
";

const BUN_NATIVE_FAIL: &str = "\
bun test v1.2.0 (abc123def)

src/auth.test.ts:
✓ Auth > validates token [0.50ms]
✗ Auth > rejects expired token [0.12ms]

error: expect(received).toBe(expected)

Expected: 401
Received: 200

      at /project/src/auth.test.ts:45:12
      at processTicksAndRejections (node:internal/process/task_queues:95:5)

 1 pass
 1 fail
 2 expect() calls

Ran 2 tests across 1 files. [5.00ms]
";

const BUN_NATIVE_MIXED: &str = "\
bun test v1.2.0 (abc123def)

src/math.test.ts:
✓ Math > adds [1.00ms]
✗ Math > divides [0.10ms]

error: expect(received).toBe(expected)

Expected: 2
Received: NaN

      at /project/src/math.test.ts:20:5

src/todo.test.ts:
✓ Todo > exists [0.30ms]
- Todo > not yet [skip]

 2 pass
 1 fail
 1 skip
 4 expect() calls

Ran 4 tests across 2 files. [15.00ms]
";

const BUN_NATIVE_MULTI_FAILURE: &str = "\
bun test v1.2.0 (abc123def)

src/routes/users.test.ts:
✓ GET /users > returns list [2.00ms]
✗ POST /users > validates email [0.50ms]

error: expect(received).toContain(expected)

Expected string to contain: \"@\"
Received: \"invalid\"

      at /project/src/routes/users.test.ts:30:10

✗ DELETE /users > requires auth [0.10ms]

error: expect(received).toBe(expected)

Expected: 401
Received: 200

      at /project/src/routes/users.test.ts:55:8

 1 pass
 2 fail
 3 expect() calls

Ran 3 tests across 1 files. [8.00ms]
";

#[test]
fn test_parse_bun_native_passing() {
    let result = parse_bun_output(BUN_NATIVE_PASS);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.failed, 0);
    assert_eq!(result.summary.skipped, 0);
    assert!(result.failures.is_empty());
    assert_eq!(result.all_tests.len(), 2);
    assert_eq!(result.all_tests[0].name, "Math > adds two numbers");
    assert_eq!(result.all_tests[0].status, TestStatus::Pass);
    assert!(result.all_tests[0].duration_ms > 0);
}

#[test]
fn test_parse_bun_native_failure() {
    let result = parse_bun_output(BUN_NATIVE_FAIL);
    assert_eq!(result.summary.passed, 1);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.failures.len(), 1);

    let f = &result.failures[0];
    assert_eq!(f.name, "Auth > rejects expired token");
    assert!(f.message.contains("Expected: 401"), "message should contain expected value, got: {}", f.message);
    assert!(f.message.contains("Received: 200"), "message should contain received value");
    assert_eq!(f.file.as_deref(), Some("src/auth.test.ts"));
    assert_eq!(f.line, Some(45));
}

#[test]
fn test_parse_bun_native_mixed() {
    let result = parse_bun_output(BUN_NATIVE_MIXED);
    assert_eq!(result.summary.passed, 2);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.summary.skipped, 1);
    assert_eq!(result.all_tests.len(), 4);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].name, "Math > divides");
    assert_eq!(result.failures[0].file.as_deref(), Some("src/math.test.ts"));
}

#[test]
fn test_parse_bun_native_multi_failure() {
    let result = parse_bun_output(BUN_NATIVE_MULTI_FAILURE);
    assert_eq!(result.summary.passed, 1);
    assert_eq!(result.summary.failed, 2);
    assert_eq!(result.failures.len(), 2);
    assert_eq!(result.failures[0].name, "POST /users > validates email");
    assert_eq!(result.failures[0].line, Some(30));
    assert_eq!(result.failures[1].name, "DELETE /users > requires auth");
    assert_eq!(result.failures[1].line, Some(55));
}

#[test]
fn test_parse_bun_native_empty() {
    let result = parse_bun_output("");
    assert_eq!(result.summary.passed, 0);
    assert_eq!(result.summary.failed, 0);
    assert!(result.all_tests.is_empty());
}

#[test]
fn test_parse_output_prefers_stderr() {
    // Bun writes test output to stderr; stdout may have user console.log
    let adapter = BunAdapter;
    let result = adapter.parse_output("console.log output", BUN_NATIVE_PASS, 0);
    assert_eq!(result.summary.passed, 2, "should parse stderr, not stdout");
}

#[test]
fn test_parse_output_falls_back_to_stdout() {
    // Wrappers may redirect child stderr to stdout
    let adapter = BunAdapter;
    let result = adapter.parse_output(BUN_NATIVE_PASS, "", 0);
    assert_eq!(result.summary.passed, 2, "should fall back to stdout when stderr empty");
}

// Test constant with diff-style assertion output containing '-'-prefixed lines
const BUN_NATIVE_DIFF_FAILURE: &str = "\
bun test v1.2.0 (abc123def)

src/snapshot.test.ts:
✓ Snapshot > matches basic [0.50ms]
✗ Snapshot > matches complex [0.20ms]

error: expect(received).toEqual(expected)

- Expected
+ Received

  Object {
-   \"status\": 401,
+   \"status\": 200,
  }

      at /project/src/snapshot.test.ts:25:8

 1 pass
 1 fail
 2 expect() calls

Ran 2 tests across 1 files. [5.00ms]
";

#[test]
fn test_parse_bun_native_diff_failure_no_false_skip() {
    // Diff-style assertion output with '-'-prefixed lines must NOT be counted as skipped tests
    let result = parse_bun_output(BUN_NATIVE_DIFF_FAILURE);
    assert_eq!(result.summary.passed, 1);
    assert_eq!(result.summary.failed, 1);
    assert_eq!(result.summary.skipped, 0, "diff lines starting with '-' must not count as skipped");
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].name, "Snapshot > matches complex");
    assert!(result.failures[0].message.contains("Expected"), "full diff should be in message");
    assert!(result.failures[0].message.contains("401"), "diff content should be preserved");
}
```

### 2.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_parse_bun_native" })
```
Expected: compilation error — `cannot find function 'parse_bun_output'`.

### 2.3 Implementation

```rust
/// Parse Bun's default test output into TestResult.
/// Bun writes per-test markers (✓/✗/-) to stderr with durations and failure details.
pub fn parse_bun_output(output: &str) -> TestResult {
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures: Vec<TestFailure> = Vec::new();
    let mut all_tests: Vec<TestDetail> = Vec::new();

    // Current file header (e.g., "src/auth.test.ts")
    let mut current_file: Option<String> = None;

    // Failure collection state: (test_name, file, message_lines)
    let mut failure_ctx: Option<(String, Option<String>, Vec<String>)> = None;

    for line in output.lines() {
        let trimmed = line.trim();

        // File header: "path/to/file.test.ts:" (line ending with colon, looks like a test file)
        if is_file_header(trimmed) {
            flush_failure(&mut failure_ctx, &mut failures);
            current_file = Some(trimmed.trim_end_matches(':').to_string());
            continue;
        }

        // Pass: ✓ Test Name [1.23ms]
        if trimmed.starts_with('✓') || trimmed.starts_with('\u{2713}') {
            flush_failure(&mut failure_ctx, &mut failures);
            let (name, dur) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                passed += 1;
                all_tests.push(TestDetail {
                    name, status: TestStatus::Pass, duration_ms: dur,
                    stdout: None, stderr: None, message: None,
                });
            }
            continue;
        }

        // Fail: ✗ Test Name [0.12ms]
        if trimmed.starts_with('✗') || trimmed.starts_with('\u{2717}')
            || trimmed.starts_with('\u{2718}')
        {
            flush_failure(&mut failure_ctx, &mut failures);
            let (name, dur) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                failed += 1;
                failure_ctx = Some((name.clone(), current_file.clone(), vec![]));
                all_tests.push(TestDetail {
                    name, status: TestStatus::Fail, duration_ms: dur,
                    stdout: None, stderr: None, message: None,
                });
            }
            continue;
        }

        // Skip: - Test Name [skip] or » Test Name
        // Only check outside failure context — diff-style assertion output
        // (e.g., "- Expected: 401") starts with '-' and would misfire.
        if failure_ctx.is_none()
            && (trimmed.starts_with('-') || trimmed.starts_with('»')
                || trimmed.starts_with('\u{00bb}'))
            && trimmed.len() > 2
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() && name != "-" {
                flush_failure(&mut failure_ctx, &mut failures);
                skipped += 1;
                all_tests.push(TestDetail {
                    name, status: TestStatus::Skip, duration_ms: 0,
                    stdout: None, stderr: None, message: None,
                });
            }
            continue;
        }

        // Summary lines (e.g., " 3 pass") — skip, we count from markers
        // Failure message collection
        if let Some((_, _, ref mut msg_lines)) = failure_ctx {
            if !trimmed.is_empty() {
                msg_lines.push(trimmed.to_string());
            }
        }
    }

    flush_failure(&mut failure_ctx, &mut failures);

    let total_duration = all_tests.iter().map(|t| t.duration_ms).sum();
    TestResult {
        summary: TestSummary { passed, failed, skipped, stuck: None, duration_ms: total_duration },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Check if a line is a file header like "src/auth.test.ts:"
fn is_file_header(line: &str) -> bool {
    line.ends_with(':')
        && !line.starts_with(' ')
        && (line.contains(".test.") || line.contains(".spec.") || line.contains(".ts:") || line.contains(".js:"))
}

/// Extract test name and duration from a marker line like "✓ Test Name [1.23ms]"
fn parse_test_marker_line(line: &str) -> (String, u64) {
    // Strip leading marker character(s) and whitespace
    let after_marker = line.trim_start_matches(|c: char| !c.is_alphanumeric() && c != '[')
        .trim_start();

    // Extract duration from trailing [Xms] or [Xs]
    let (name_part, dur) = if let Some(bracket_start) = after_marker.rfind('[') {
        let bracket_content = &after_marker[bracket_start + 1..].trim_end_matches(']');
        let duration = parse_duration_bracket(bracket_content);
        (after_marker[..bracket_start].trim(), duration)
    } else {
        (after_marker.trim(), 0u64)
    };

    (name_part.to_string(), dur)
}

/// Parse duration from bracket content: "1.23ms" → 1, "0.45ms" → 0 (rounded), "1.50s" → 1500
fn parse_duration_bracket(s: &str) -> u64 {
    let s = s.trim();
    if let Some(ms_str) = s.strip_suffix("ms") {
        ms_str.parse::<f64>().unwrap_or(0.0).round() as u64
    } else if let Some(s_str) = s.strip_suffix('s') {
        (s_str.parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u64
    } else {
        0
    }
}

/// Flush accumulated failure context into the failures vec.
/// Extracts file path and line number from stack trace lines.
fn flush_failure(
    ctx: &mut Option<(String, Option<String>, Vec<String>)>,
    failures: &mut Vec<TestFailure>,
) {
    if let Some((name, file_from_header, msg_lines)) = ctx.take() {
        if msg_lines.is_empty() {
            failures.push(TestFailure {
                name, file: file_from_header, line: None,
                message: String::new(), rerun: None, suggested_traces: vec![],
            });
            return;
        }

        let message = msg_lines.join("\n");

        // Extract file:line from first "at" line that isn't node_modules
        let (file, line) = extract_location_from_stack(&msg_lines, &file_from_header);

        failures.push(TestFailure {
            name, file, line,
            message, rerun: None, suggested_traces: vec![],
        });
    }
}

/// Extract file path and line number from stack trace lines.
/// Prefers the first non-node_modules frame. Falls back to file header.
fn extract_location_from_stack(
    lines: &[String],
    file_from_header: &Option<String>,
) -> (Option<String>, Option<u32>) {
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("at ") { continue; }
        // Patterns: "at /abs/path:line:col" or "at func (path:line:col)"
        let path_part = if let Some(paren_start) = trimmed.rfind('(') {
            &trimmed[paren_start + 1..trimmed.len() - trimmed.ends_with(')') as usize]
        } else {
            &trimmed[3..] // skip "at "
        };
        if path_part.contains("node_modules") || path_part.contains("node:internal") {
            continue;
        }
        // Parse "path:line:col"
        let parts: Vec<&str> = path_part.rsplitn(3, ':').collect();
        if parts.len() >= 3 {
            let file_path = parts[2];
            let line_num = parts[1].parse::<u32>().ok();
            // Make path relative if absolute
            let relative = if file_path.starts_with('/') {
                // Try stripping common prefixes
                file_path.rsplit_once("/src/")
                    .map(|(_, rest)| format!("src/{}", rest))
                    .unwrap_or_else(|| file_path.to_string())
            } else {
                file_path.to_string()
            };
            return (Some(relative), line_num);
        }
    }
    (file_from_header.clone(), None)
}
```

Update `parse_output` to use `parse_bun_output`:

```rust
fn parse_output(&self, stdout: &str, stderr: &str, _exit_code: i32) -> TestResult {
    // Bun writes test output to stderr (default reporter).
    // Wrappers may redirect child stderr to stdout.
    // Try stderr first, fall back to stdout.
    let result = parse_bun_output(stderr);
    if !result.all_tests.is_empty() {
        return result;
    }
    parse_bun_output(stdout)
}
```

Keep `parse_junit_xml` as `pub(crate)` — still used by Deno and Playwright adapters.

### 2.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_parse_bun_native" })
```
All 7 tests pass.

### 2.5 Checkpoint

Bun output parser handles pass/fail/skip markers, extracts failure messages with file:line from stack traces, and works with both stderr (direct) and stdout (wrapper) output.

**Edge cases covered:**
- Empty output → 0 tests, no crash
- Multiple failures in same file → each attributed correctly
- Stack traces with node_modules frames → skipped, first user-code frame extracted
- Duration parsing: both `ms` and `s` suffixes
- stderr vs stdout fallback → prefers stderr, falls back to stdout
- Nested describe names → `Parent > Child > test` preserved as-is
- Diff-style failure output with `-`-prefixed lines → not miscounted as skipped tests

---

## Task 3: Bun-specific progress tracker

**Files:**
- Modify: `src/test/bun_adapter.rs` — add `pub fn update_progress()`
- Modify: `src/test/mod.rs` (line 339) — route `"bun"` to `bun_adapter::update_progress`
- Test: `src/test/bun_adapter.rs`

### 3.1 Write failing test

```rust
#[test]
fn test_bun_update_progress_pass() {
    use std::sync::{Arc, Mutex};
    use super::super::{TestProgress, TestPhase};

    let progress = Arc::new(Mutex::new(TestProgress::new()));

    update_progress("src/auth.test.ts:\n✓ Auth > passes [1.00ms]\n", &progress);

    let p = progress.lock().unwrap();
    assert_eq!(p.passed, 1, "should count 1 pass");
    assert_eq!(p.failed, 0);
    assert_eq!(p.phase, TestPhase::Running, "file header should transition to Running");
}

#[test]
fn test_bun_update_progress_mixed() {
    use std::sync::{Arc, Mutex};
    use super::super::{TestProgress, TestPhase};

    let progress = Arc::new(Mutex::new(TestProgress::new()));

    // First chunk: file header + some results
    update_progress("src/math.test.ts:\n✓ adds [1.00ms]\n✗ divides [0.10ms]\n", &progress);

    // Second chunk: more results from another file
    update_progress("src/todo.test.ts:\n✓ exists [0.30ms]\n- skipped [skip]\n", &progress);

    let p = progress.lock().unwrap();
    assert_eq!(p.passed, 2);
    assert_eq!(p.failed, 1);
    assert_eq!(p.skipped, 1);
}

#[test]
fn test_bun_update_progress_ignores_non_test_lines() {
    use std::sync::{Arc, Mutex};
    use super::super::TestProgress;

    let progress = Arc::new(Mutex::new(TestProgress::new()));

    update_progress("bun test v1.2.0 (abc123def)\n\n 2 pass\n 1 fail\n\nRan 3 tests across 1 files. [10.00ms]\n", &progress);

    let p = progress.lock().unwrap();
    assert_eq!(p.passed, 0, "summary lines should not increment counters");
    assert_eq!(p.failed, 0);
}
```

### 3.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_bun_update_progress" })
```
Expected: compilation error — `cannot find function 'update_progress'` in `bun_adapter`.

### 3.3 Implementation

```rust
// In src/test/bun_adapter.rs

use std::sync::{Arc, Mutex};
use super::TestProgress;

/// Incremental progress tracker for Bun's default test output.
/// Called from the DB event polling loop in mod.rs with text chunks.
///
/// Note: Bun only emits result markers (✓/✗/-), not start markers, so we call
/// start_test immediately before finish_test. This means per-test durations are
/// ~0, but it populates test_durations for reporting. Stuck detection relies on
/// process-level monitoring (already functional) rather than individual test tracking.
pub fn update_progress(text: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();

    for line in text.lines() {
        let trimmed = line.trim();

        // File header → transition to Running
        if is_file_header(trimmed) {
            if p.phase == super::TestPhase::Compiling {
                p.phase = super::TestPhase::Running;
            }
            continue;
        }

        // Pass
        if trimmed.starts_with('✓') || trimmed.starts_with('\u{2713}') {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                p.passed += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
            continue;
        }

        // Fail
        if trimmed.starts_with('✗') || trimmed.starts_with('\u{2717}')
            || trimmed.starts_with('\u{2718}')
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                p.failed += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
            continue;
        }

        // Skip
        if (trimmed.starts_with('-') || trimmed.starts_with('»')
            || trimmed.starts_with('\u{00bb}'))
            && trimmed.len() > 2
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() && name != "-" {
                p.skipped += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
        }
    }
}
```

**mod.rs** — update routing (line 339):

```rust
// Change from:
"vitest" | "jest" | "bun" => Some(vitest_adapter::update_progress),
// To:
"vitest" | "jest" => Some(vitest_adapter::update_progress),
"bun" => Some(bun_adapter::update_progress),
```

### 3.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_bun_update_progress" })
```
All 3 tests pass.

### 3.5 Checkpoint

Bun now has dedicated progress tracking. The `✓`/`✗`/`-` markers are counted in real-time as DB events stream in. File headers transition from Compiling → Running. Summary lines and error messages are ignored (no double-counting).

**Edge cases covered:**
- Multiple chunks → counts accumulate correctly across calls
- Summary lines (`N pass`, `N fail`) → not counted (only markers)
- Empty text or header-only lines → no-op
- File header as first content → phase transitions immediately

---

## Task 4: Monorepo detection and workspace support

**Files:**
- Modify: `src/test/bun_adapter.rs` — rewrite `detect()`, add workspace scanning
- Test: `src/test/bun_adapter.rs`

### 4.1 Write failing test

```rust
#[test]
fn test_detect_bunfig_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bunfig.toml"), "[test]\ntimeout = 30000").unwrap();
    let adapter = BunAdapter;
    assert!(adapter.detect(dir.path(), None) >= 90,
        "bunfig.toml should give high confidence");
}

#[test]
fn test_detect_monorepo_bun_workspace() {
    let dir = tempfile::tempdir().unwrap();
    // Root has vitest in deps (from web app) + workspaces
    std::fs::write(dir.path().join("package.json"),
        r#"{"workspaces": ["apps/*"], "devDependencies": {"vitest": "^3"}}"#).unwrap();
    std::fs::write(dir.path().join("bun.lock"), "").unwrap();
    // API workspace has bunfig.toml
    let api = dir.path().join("apps/api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("bunfig.toml"), "[test]\ntimeout = 30000").unwrap();

    let adapter = BunAdapter;
    let conf = adapter.detect(dir.path(), None);
    assert!(conf >= 85,
        "monorepo with bun:test workspace should detect despite vitest in root, got {}", conf);
}

#[test]
fn test_detect_monorepo_no_bun_workspace() {
    let dir = tempfile::tempdir().unwrap();
    // Root has vitest + workspaces but NO bun:test workspace
    std::fs::write(dir.path().join("package.json"),
        r#"{"workspaces": ["apps/*"], "devDependencies": {"vitest": "^3"}}"#).unwrap();
    let web = dir.path().join("apps/web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("vitest.config.ts"), "export default {}").unwrap();

    let adapter = BunAdapter;
    let conf = adapter.detect(dir.path(), None);
    assert_eq!(conf, 0, "monorepo with only vitest workspaces should return 0");
}
```

### 4.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_detect_bunfig" })
```
Expected: `test_detect_bunfig_toml` fails — current `detect()` doesn't check for `bunfig.toml`.
`test_detect_monorepo_bun_workspace` fails — returns 0 because vitest found in root package.json.

### 4.3 Implementation

```rust
fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
    // Direct bunfig.toml — strong signal for bun:test
    if project_root.join("bunfig.toml").exists() {
        return 90;
    }

    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        // Monorepo: check if any workspace has bunfig.toml
        if pkg.contains("\"workspaces\"") {
            if has_bun_test_workspace(project_root, &pkg) {
                return 90;
            }
            // Monorepo with workspaces but no bun:test workspace — don't
            // claim based on bun.lock alone (it's the package manager, not test runner)
            if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                return 0;
            }
        } else {
            // Non-monorepo: defer to vitest/jest if present (existing behavior)
            if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") {
                    return 90;
                }
                return 0;
            }
        }
    }

    if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
        return 85;
    }

    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") { return 90; }
        if pkg.contains("\"bun\"") { return 75; }
    }
    0
}

/// Check if any workspace directory contains bunfig.toml (bun:test).
fn has_bun_test_workspace(project_root: &Path, pkg_json: &str) -> bool {
    let workspace_dirs = find_workspace_dirs(project_root, pkg_json);
    workspace_dirs.iter().any(|ws| ws.join("bunfig.toml").exists())
}

/// Resolve workspace glob patterns to concrete directory paths.
/// Supports simple patterns like "apps/*" and "packages/*".
fn find_workspace_dirs(project_root: &Path, pkg_json: &str) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    // Extract workspaces array — simple string matching
    let ws_start = match pkg_json.find("\"workspaces\"") {
        Some(i) => i,
        None => return dirs,
    };
    let bracket_start = match pkg_json[ws_start..].find('[') {
        Some(i) => ws_start + i,
        None => return dirs,
    };
    let bracket_end = match pkg_json[bracket_start..].find(']') {
        Some(i) => bracket_start + i,
        None => return dirs,
    };
    let array_content = &pkg_json[bracket_start + 1..bracket_end];

    for item in array_content.split(',') {
        let pattern = item.trim().trim_matches('"').trim_matches('\'');
        if pattern.is_empty() { continue; }

        if pattern.ends_with("/*") {
            // Glob: "apps/*" → list subdirs of apps/
            let parent = project_root.join(pattern.trim_end_matches("/*"));
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        dirs.push(entry.path());
                    }
                }
            }
        } else {
            // Direct: "packages/shared"
            let ws = project_root.join(pattern);
            if ws.is_dir() { dirs.push(ws); }
        }
    }
    dirs
}

/// Find the primary bun:test workspace directory in a monorepo.
/// Returns the first workspace with bunfig.toml, or None.
pub(crate) fn find_bun_workspace(project_root: &Path) -> Option<std::path::PathBuf> {
    let pkg = std::fs::read_to_string(project_root.join("package.json")).ok()?;
    if !pkg.contains("\"workspaces\"") { return None; }
    let dirs = find_workspace_dirs(project_root, &pkg);
    dirs.into_iter().find(|ws| ws.join("bunfig.toml").exists())
}

/// Build remove_env list: only strip DATABASE_URL when a .env.test file exists
/// in the test cwd (Bun auto-loads it, and inherited env would override).
/// Projects without .env.test intentionally pass DATABASE_URL — don't strip it.
fn bun_remove_env(cwd: &Option<String>, project_root: &Path) -> Vec<String> {
    let has_env_test = cwd.as_deref()
        .map(|c| Path::new(c).join(".env.test").exists())
        .unwrap_or_else(|| project_root.join(".env.test").exists());
    if has_env_test {
        vec!["DATABASE_URL".to_string()]
    } else {
        vec![]
    }
}
```

### 4.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_detect" })
```
All detection tests pass (new + existing).

### 4.5 Checkpoint

Bun adapter now detects `bunfig.toml` directly, scans monorepo workspaces, and doesn't bail on vitest when bun:test exists in a separate workspace.

**Edge cases covered:**
- `bunfig.toml` at project root → 90 confidence (direct bun:test project)
- Monorepo with bun:test + vitest in separate workspaces → 90 confidence
- Monorepo with vitest only → 0 (correctly defers)
- Non-monorepo with vitest → 0 (existing behavior preserved)
- Workspace glob `apps/*` → resolves to actual subdirs

---

## Task 5: Test orchestrator parsing + level mapping

**Files:**
- Modify: `src/test/bun_adapter.rs` — add orchestrator parsing, update `suite_command()`
- Test: `src/test/bun_adapter.rs`

### 5.1 Write failing test

```rust
#[test]
fn test_parse_orchestrator_suites() {
    let content = r#"
const ROOT = path.resolve(import.meta.dir, "..")

const SUITES: Record<string, { cmd: string[]; cwd: string }> = {
  all: {
    cmd: ["bun", "test"],
    cwd: path.join(ROOT, "apps/api"),
  },
  unit: {
    cmd: ["bun", "test", "src/services", "src/middleware", "src/lib", "src/db"],
    cwd: path.join(ROOT, "apps/api"),
  },
  integration: {
    cmd: ["bun", "test", "src/routes", "src/tests/auth-service.test.ts", "src/tests/app-contract.test.ts", "src/tests/full-integration.test.ts"],
    cwd: path.join(ROOT, "apps/api"),
  },
  e2e: {
    cmd: ["bun", "test", "src/tests/e2e"],
    cwd: path.join(ROOT, "apps/api"),
  },
  "e2e-parallel": {
    cmd: ["bun", "run", "scripts/test-e2e-parallel.ts", "--workers=4"],
    cwd: ROOT,
  },
};
"#;
    let suites = parse_suites_from_ts(content).unwrap();
    assert!(suites.len() >= 4, "should parse at least 4 suites, got {}", suites.len());

    let unit = &suites["unit"];
    assert_eq!(unit.dirs, vec!["src/services", "src/middleware", "src/lib", "src/db"]);
    assert_eq!(unit.cwd, "apps/api");

    let integration = &suites["integration"];
    assert!(integration.dirs.contains(&"src/routes".to_string()));
    assert!(integration.dirs.contains(&"src/tests/auth-service.test.ts".to_string()));

    let e2e = &suites["e2e"];
    assert_eq!(e2e.dirs, vec!["src/tests/e2e"]);
    assert_eq!(e2e.cwd, "apps/api");

    let all = &suites["all"];
    assert!(all.dirs.is_empty(), "all suite should have no dirs (runs everything)");
    assert_eq!(all.cwd, "apps/api");
}

#[test]
fn test_parse_orchestrator_empty() {
    assert!(parse_suites_from_ts("").is_none());
    assert!(parse_suites_from_ts("const x = 5;").is_none());
}

#[test]
fn test_suite_command_with_orchestrator() {
    let dir = tempfile::tempdir().unwrap();

    // Set up monorepo with orchestrator
    let scripts = dir.path().join("scripts");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(scripts.join("test-run.ts"), r#"
const SUITES = {
  unit: {
    cmd: ["bun", "test", "src/services", "src/lib"],
    cwd: path.join(ROOT, "apps/api"),
  },
};
"#).unwrap();

    std::fs::write(dir.path().join("package.json"),
        r#"{"workspaces": ["apps/*"], "scripts": {"test:unit": "bun run scripts/test-run.ts unit"}}"#
    ).unwrap();

    let api = dir.path().join("apps/api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("bunfig.toml"), "[test]").unwrap();

    let cmd = BunAdapter.suite_command(dir.path(), Some(TestLevel::Unit), &Default::default()).unwrap();
    assert_eq!(cmd.program, "bun");
    assert!(cmd.args.contains(&"src/services".to_string()), "should include dirs from orchestrator, got: {:?}", cmd.args);
    assert!(cmd.args.contains(&"src/lib".to_string()));
    assert!(cmd.cwd.is_some(), "should set cwd");
    assert!(cmd.cwd.as_ref().unwrap().ends_with("apps/api"), "cwd should be workspace dir");
}
```

### 5.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_parse_orchestrator" })
```
Expected: compilation error — `cannot find function 'parse_suites_from_ts'`.

### 5.3 Implementation

```rust
/// Parsed suite config from a test orchestrator script.
#[derive(Debug, Clone)]
pub(crate) struct OrchestratorSuite {
    /// Directories/files to pass to `bun test` (empty = all tests)
    pub dirs: Vec<String>,
    /// Workspace-relative cwd (e.g., "apps/api")
    pub cwd: String,
}

/// Parse suite configs from a test orchestrator TypeScript file.
/// Looks for a SUITES-like object mapping suite names to {cmd, cwd} entries.
pub(crate) fn parse_suites_from_ts(content: &str) -> Option<HashMap<String, OrchestratorSuite>> {
    let mut suites = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_dirs: Vec<String> = Vec::new();
    let mut current_cwd: Option<String> = None;
    let mut brace_depth = 0i32;
    let mut in_suites_block = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect start of SUITES object
        if !in_suites_block {
            if (trimmed.contains("SUITES") || trimmed.contains("suites"))
                && (trimmed.contains("Record<") || trimmed.contains(": {") || trimmed.contains("= {"))
            {
                in_suites_block = true;
                // Start at 1 to account for the outer '{' on this line (which we skip via continue).
                // Without this, inner suite '},' brings depth to 0 and triggers early exit.
                brace_depth = 1;
            }
            continue;
        }

        // Track brace depth
        brace_depth += trimmed.chars().filter(|&c| c == '{').count() as i32;
        brace_depth -= trimmed.chars().filter(|&c| c == '}').count() as i32;

        if brace_depth <= 0 && in_suites_block && suites.len() > 0 {
            // Flush last suite
            if let Some(name) = current_name.take() {
                suites.insert(name, OrchestratorSuite {
                    dirs: std::mem::take(&mut current_dirs),
                    cwd: current_cwd.take().unwrap_or_default(),
                });
            }
            break; // End of SUITES object
        }

        // Suite name: `name: {` or `"name": {`
        if trimmed.ends_with(": {") || trimmed.ends_with(":{") || trimmed.ends_with(": {,") {
            // Flush previous suite
            if let Some(name) = current_name.take() {
                suites.insert(name, OrchestratorSuite {
                    dirs: std::mem::take(&mut current_dirs),
                    cwd: current_cwd.take().unwrap_or_default(),
                });
            }
            let name_part = trimmed.split(':').next().unwrap_or("").trim();
            let name = name_part.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
            if !name.is_empty() {
                current_name = Some(name.to_string());
            }
            continue;
        }

        // cmd array: `cmd: ["bun", "test", "src/services", ...]`
        if trimmed.starts_with("cmd:") || trimmed.starts_with("cmd :") {
            let all_strings = extract_quoted_strings(trimmed);
            // "bun run <script>" commands are custom runners, not `bun test` — no dirs to extract
            let is_bun_test = all_strings.get(0).map(|s| s == "bun").unwrap_or(false)
                && all_strings.get(1).map(|s| s == "test").unwrap_or(false);
            current_dirs = if is_bun_test {
                all_strings.into_iter()
                    .filter(|s| s != "bun" && s != "test" && !s.starts_with("--"))
                    .collect()
            } else {
                vec![] // non-test command (e.g., "bun run scripts/...") → no test dirs
            };
            continue;
        }

        // cwd: `cwd: path.join(ROOT, "apps/api")` or `cwd: ROOT`
        if trimmed.starts_with("cwd:") || trimmed.starts_with("cwd :") {
            current_cwd = extract_cwd_path(trimmed);
            continue;
        }
    }

    // Flush last
    if let Some(name) = current_name.take() {
        suites.insert(name, OrchestratorSuite {
            dirs: std::mem::take(&mut current_dirs),
            cwd: current_cwd.take().unwrap_or_default(),
        });
    }

    if suites.is_empty() { None } else { Some(suites) }
}

/// Extract all double/single-quoted strings from a line.
fn extract_quoted_strings(line: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == quote { break; }
                s.push(c);
            }
            if !s.is_empty() { strings.push(s); }
        }
    }
    strings
}

/// Extract cwd path from a line like `cwd: path.join(ROOT, "apps/api")`
fn extract_cwd_path(line: &str) -> Option<String> {
    // Pattern: path.join(ROOT, "relative/path") → "relative/path"
    if let Some(start) = line.find("path.join") {
        let after = &line[start..];
        // Find the last quoted string (the relative path)
        let strings = extract_quoted_strings(after);
        return strings.last().cloned();
    }
    // Pattern: `cwd: ROOT` → "" (project root)
    if line.contains("ROOT") {
        return Some(String::new());
    }
    // Direct string: `cwd: "apps/api"`
    let strings = extract_quoted_strings(line);
    strings.first().cloned()
}

/// Find and parse the test orchestrator script referenced in package.json.
pub(crate) fn load_orchestrator(project_root: &Path) -> Option<HashMap<String, OrchestratorSuite>> {
    let pkg = std::fs::read_to_string(project_root.join("package.json")).ok()?;

    // Find script path from any test:* script that references a .ts file
    for key in &["test:unit", "test:integration", "test:e2e", "test"] {
        if let Some(script) = extract_script_value(&pkg, key) {
            let parts: Vec<&str> = script.split_whitespace().collect();
            // Pattern: "bun run scripts/test-run.ts <args>"
            if parts.len() >= 3 && parts[0] == "bun" && parts[1] == "run" {
                let script_path = parts[2];
                if script_path.ends_with(".ts") || script_path.ends_with(".js") {
                    if let Ok(content) = std::fs::read_to_string(project_root.join(script_path)) {
                        return parse_suites_from_ts(&content);
                    }
                }
            }
        }
    }
    None
}
```

**Update `suite_command()` to use orchestrator + workspace cwd:**

```rust
fn suite_command(
    &self,
    project_root: &Path,
    level: Option<TestLevel>,
    _env: &HashMap<String, String>,
) -> crate::Result<TestCommand> {
    let mut args = vec!["test".to_string()];
    let mut cwd: Option<String> = None;

    // Try orchestrator script first (handles monorepo suite configs)
    let orchestrator = load_orchestrator(project_root);
    if let Some(ref suites) = orchestrator {
        let suite_key = match level {
            Some(TestLevel::Unit) => "unit",
            Some(TestLevel::Integration) => "integration",
            Some(TestLevel::E2e) => "e2e",
            None => "all",
        };
        if let Some(suite) = suites.get(suite_key) {
            args.extend(suite.dirs.iter().cloned());
            if !suite.cwd.is_empty() {
                cwd = Some(project_root.join(&suite.cwd).to_string_lossy().into_owned());
            }
        }
    }

    // If no orchestrator or no matching suite, try workspace detection
    if cwd.is_none() {
        if let Some(ws) = find_bun_workspace(project_root) {
            cwd = Some(ws.to_string_lossy().into_owned());
        }
    }

    // If no orchestrator, fall back to old level→dir extraction from package.json
    if orchestrator.is_none() {
        if let Some(level) = level {
            let level_key = match level {
                TestLevel::Unit => "test:unit",
                TestLevel::Integration => "test:integration",
                TestLevel::E2e => "test:e2e",
            };
            let pkg_path = cwd.as_deref()
                .map(|c| Path::new(c).join("package.json"))
                .unwrap_or_else(|| project_root.join("package.json"));
            if let Ok(pkg) = std::fs::read_to_string(&pkg_path) {
                if let Some(script) = extract_script_value(&pkg, level_key) {
                    let parts: Vec<&str> = script.split_whitespace().collect();
                    if parts.len() >= 2 && parts[0] == "bun" && parts[1] == "test" {
                        for part in &parts[2..] {
                            if !part.starts_with('-') {
                                args.push(part.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    let remove_env = bun_remove_env(&cwd, project_root);
    Ok(TestCommand {
        program: "bun".to_string(),
        args,
        env: HashMap::new(),
        cwd,
        remove_env,
    })
}
```

### 5.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_parse_orchestrator" })
debug_test({ project_root: "/Users/alex/strobe", test: "test_suite_command" })
```
All tests pass including the orchestrator-based suite_command test.

### 5.5 Checkpoint

The adapter now reads test orchestrator scripts to map levels to directories and workspace cwds. `suite_command` produces the correct `bun test <dirs>` with the right cwd for each level. Falls back gracefully to workspace detection or package.json extraction.

**Edge cases covered:**
- No orchestrator script → falls back to workspace detection + package.json
- Empty/malformed script → `parse_suites_from_ts` returns None, graceful fallback
- Suite key not found → falls back (e.g., "e2e-parallel" not mapped from TestLevel)
- `cwd: ROOT` (no join) → uses project root
- Multi-line cmd arrays → handled by extracting quoted strings from the line
- Non-bun commands (e.g., `"bun", "run", "scripts/..."`) → dirs empty, only cwd used

---

## Task 6: File path support + workspace mapping

**Files:**
- Modify: `src/test/bun_adapter.rs` — rewrite `single_test_command()` with path detection
- Test: `src/test/bun_adapter.rs`

### 6.1 Write failing test

```rust
#[test]
fn test_single_test_file_path() {
    let dir = tempfile::tempdir().unwrap();
    let cmd = BunAdapter.single_test_command(dir.path(), "src/middleware/auth.test.ts").unwrap();
    assert!(!cmd.args.contains(&"--test-name-pattern".to_string()),
        "file path should not use --test-name-pattern");
    assert!(cmd.args.contains(&"src/middleware/auth.test.ts".to_string()),
        "file path should be passed directly to bun test");
}

#[test]
fn test_single_test_name_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let cmd = BunAdapter.single_test_command(dir.path(), "should validate token").unwrap();
    assert!(cmd.args.contains(&"--test-name-pattern".to_string()),
        "name pattern should use --test-name-pattern");
    assert!(cmd.args.contains(&"should validate token".to_string()));
}

#[test]
fn test_single_test_workspace_path() {
    let dir = tempfile::tempdir().unwrap();
    // Set up monorepo
    let api = dir.path().join("apps/api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("bunfig.toml"), "[test]").unwrap();
    std::fs::write(dir.path().join("package.json"),
        r#"{"workspaces": ["apps/*"]}"#).unwrap();

    // Path includes workspace prefix
    let cmd = BunAdapter.single_test_command(
        dir.path(), "apps/api/src/middleware/auth.test.ts"
    ).unwrap();
    assert!(cmd.args.contains(&"src/middleware/auth.test.ts".to_string()),
        "should strip workspace prefix, got: {:?}", cmd.args);
    assert!(cmd.cwd.as_ref().unwrap().ends_with("apps/api"),
        "should set cwd to workspace dir, got: {:?}", cmd.cwd);
}

#[test]
fn test_single_test_path_no_workspace() {
    let dir = tempfile::tempdir().unwrap();
    // Non-monorepo with bunfig.toml
    std::fs::write(dir.path().join("bunfig.toml"), "[test]").unwrap();

    let cmd = BunAdapter.single_test_command(
        dir.path(), "src/middleware/auth.test.ts"
    ).unwrap();
    assert!(cmd.args.contains(&"src/middleware/auth.test.ts".to_string()));
    // cwd not set (or set to project root) — no workspace to detect
}
```

### 6.2 Run test — verify failure

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_single_test_file_path" })
```
Expected: `test_single_test_file_path` fails — current implementation always uses `--test-name-pattern`.

### 6.3 Implementation

```rust
fn single_test_command(
    &self,
    project_root: &Path,
    test_name: &str,
) -> crate::Result<TestCommand> {
    let is_file_path = test_name.contains('/')
        || test_name.ends_with(".ts") || test_name.ends_with(".tsx")
        || test_name.ends_with(".js") || test_name.ends_with(".jsx")
        || test_name.contains(".test.") || test_name.contains(".spec.");

    if is_file_path {
        let (cwd, relative_path) = resolve_workspace_path(project_root, test_name);
        let remove_env = bun_remove_env(&cwd, project_root);
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec!["test".to_string(), relative_path],
            env: HashMap::new(),
            cwd,
            remove_env,
        })
    } else {
        // Name pattern — need workspace cwd for bunfig.toml discovery
        let cwd = find_bun_workspace(project_root)
            .map(|ws| ws.to_string_lossy().into_owned());
        let remove_env = bun_remove_env(&cwd, project_root);
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "test".to_string(),
                "--test-name-pattern".to_string(),
                test_name.to_string(),
            ],
            env: HashMap::new(),
            cwd,
            remove_env,
        })
    }
}

/// Resolve a test file path to a workspace-relative path + cwd.
/// If the path starts with a known workspace prefix, strips it and sets cwd.
fn resolve_workspace_path(project_root: &Path, test_path: &str) -> (Option<String>, String) {
    // Try to find workspace from path prefix
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if pkg.contains("\"workspaces\"") {
            let ws_dirs = find_workspace_dirs(project_root, &pkg);
            for ws in &ws_dirs {
                if let Ok(relative_ws) = ws.strip_prefix(project_root) {
                    let prefix = relative_ws.to_string_lossy();
                    let prefix_with_slash = format!("{}/", prefix);
                    if test_path.starts_with(&*prefix_with_slash) {
                        let stripped = &test_path[prefix_with_slash.len()..];
                        return (
                            Some(ws.to_string_lossy().into_owned()),
                            stripped.to_string(),
                        );
                    }
                }
            }
        }
    }

    // No workspace match — check if file exists relative to bun workspace
    if let Some(ws) = find_bun_workspace(project_root) {
        if ws.join(test_path).exists() {
            return (
                Some(ws.to_string_lossy().into_owned()),
                test_path.to_string(),
            );
        }
    }

    // No workspace context — pass path as-is
    (None, test_path.to_string())
}
```

### 6.4 Run test — verify passes

```
debug_test({ project_root: "/Users/alex/strobe", test: "test_single_test" })
```
All 4 tests pass.

### 6.5 Checkpoint

`single_test_command` now distinguishes file paths from name patterns. File paths are passed directly to `bun test`. Workspace prefixes are stripped and cwd is set to the workspace directory. Name patterns still use `--test-name-pattern`.

**Edge cases covered:**
- File path with workspace prefix → stripped, cwd set to workspace
- File path without workspace prefix → checked relative to bun workspace
- Name pattern (no path indicators) → uses `--test-name-pattern`
- Non-monorepo project → no workspace stripping, path passed as-is
- Path with `.test.` but no `/` → treated as file path (e.g., `auth.test.ts`)

---

## Task 7: End-to-end verification against liaison

**Files:**
- No code changes — manual verification
- Test: live `debug_test` calls against `/Users/alex/liaison/`

### 7.1 Verify detection

```
debug_test({ project_root: "/Users/alex/liaison" })
```
Expected: detects `bun` framework (not vitest). Progress shows incrementing pass/fail counts. Final result includes all tests from `apps/api`.

### 7.2 Verify unit tests

```
debug_test({ project_root: "/Users/alex/liaison", level: "unit" })
```
Expected: runs only `src/services`, `src/middleware`, `src/lib`, `src/db` from `apps/api/`. Progress tracks correctly. Failures include file paths and line numbers.

### 7.3 Verify integration tests

```
debug_test({ project_root: "/Users/alex/liaison", level: "integration" })
```
Expected: runs `src/routes` + specific test files from `apps/api/`. Progress tracks correctly.

### 7.4 Verify individual test file

```
debug_test({ project_root: "/Users/alex/liaison", test: "apps/api/src/middleware/auth.test.ts" })
```
Expected: runs only `auth.test.ts` from `apps/api/`. Workspace prefix stripped. `bunfig.toml` timeout respected.

### 7.5 Verify individual test by name

```
debug_test({ project_root: "/Users/alex/liaison", test: "should require authentication" })
```
Expected: runs with `--test-name-pattern`, finds matching test(s).

### 7.6 Verify error reporting

If any tests fail, verify:
- Failure message includes the assertion error text
- File path is relative (e.g., `src/middleware/auth.test.ts`)
- Line number points to the actual assertion
- `suggested_traces` includes `@file:auth.test` pattern

### 7.7 Verify progress tracking

During a long test run (`level: "integration"`), poll with:
```
debug_test({ action: "status", testRunId: "<id>" })
```
Expected: `progress.passed` and `progress.failed` increment in real-time, not all-at-once. `phase` transitions from `Compiling` to `Running`.

### 7.8 Checkpoint

All liaison test scenarios work: full suite, level-based, individual file, individual name pattern. Real-time progress, full error reporting with file:line, stdout/stderr captured.

**Edge cases to verify manually:**
- Test with `beforeAll` failure → reported as test file failure
- Test with timeout → Bun shows `(timed out)` marker, correctly parsed as failure
- Empty test level (no matching tests) → 0 tests, hint about no tests found

---

## Execution Order

```
Task 1: TestCommand cwd/remove_env ─────┐
                                         ├─→ Task 4: Monorepo detection ──→ Task 5: Orchestrator parsing
Task 2: Bun output parser ──→ Task 3: Progress tracker                  └─→ Task 6: File path support
                                                                                         │
                                                    All tasks ──────────────────────────→ Task 7: Integration
```

- **Parallel block 1:** Tasks 1 + 2 (independent)
- **Sequential:** Task 3 after Task 2 (shares helper functions)
- **Sequential:** Task 4 after Task 1 (uses cwd)
- **Parallel block 2:** Tasks 5 + 6 after Task 4 (both use workspace detection)
- **Sequential:** Task 7 after all (integration verification)

## Files Modified Summary

| File | Action | Description |
|------|--------|-------------|
| `src/test/adapter.rs` | Modify | Add `cwd: Option<String>` and `remove_env: Vec<String>` to `TestCommand` |
| `src/test/mod.rs` | Modify | Use `test_cmd.cwd` for spawn cwd, apply `remove_env`, route `"bun"` to `bun_adapter::update_progress` |
| `src/test/bun_adapter.rs` | Modify | Rewrite: `detect()`, `suite_command()`, `single_test_command()`, `parse_output()`. Add: `parse_bun_output()`, `update_progress()`, `parse_suites_from_ts()`, workspace helpers |
| `src/test/cargo_adapter.rs` | Modify | Add `cwd: None, remove_env: vec![]` to TestCommand constructions |
| `src/test/catch2_adapter.rs` | Modify | Same — add default fields |
| `src/test/pytest_adapter.rs` | Modify | Same |
| `src/test/unittest_adapter.rs` | Modify | Same |
| `src/test/vitest_adapter.rs` | Modify | Same |
| `src/test/jest_adapter.rs` | Modify | Same |
| `src/test/deno_adapter.rs` | Modify | Same |
| `src/test/go_adapter.rs` | Modify | Same |
| `src/test/gtest_adapter.rs` | Modify | Same |
| `src/test/mocha_adapter.rs` | Modify | Same |
| `src/test/playwright_adapter.rs` | Modify | Same |

## Edge Case Coverage Matrix

| Requirement | Test Location | Key Assertion |
|------------|---------------|---------------|
| Pass counting | Task 2: `test_parse_bun_native_passing` | `passed == 2`, `all_tests.len() == 2` |
| Fail with message | Task 2: `test_parse_bun_native_failure` | `message.contains("Expected: 401")` |
| File:line extraction | Task 2: `test_parse_bun_native_failure` | `file == "src/auth.test.ts"`, `line == 45` |
| Skip counting | Task 2: `test_parse_bun_native_mixed` | `skipped == 1` |
| Multiple failures | Task 2: `test_parse_bun_native_multi_failure` | `failures.len() == 2`, correct names |
| Empty output | Task 2: `test_parse_bun_native_empty` | `all_tests.is_empty()` |
| stderr preference | Task 2: `test_parse_output_prefers_stderr` | Parses stderr when both present |
| stdout fallback | Task 2: `test_parse_output_falls_back_to_stdout` | Parses stdout when stderr empty |
| Diff-style failure | Task 2: `test_parse_bun_native_diff_failure_no_false_skip` | `skipped == 0`, diff lines not miscounted |
| Progress pass | Task 3: `test_bun_update_progress_pass` | `passed == 1`, phase == Running |
| Progress mixed | Task 3: `test_bun_update_progress_mixed` | Correct counts across chunks |
| Summary not counted | Task 3: `test_bun_update_progress_ignores_non_test_lines` | `passed == 0` |
| bunfig.toml detection | Task 4: `test_detect_bunfig_toml` | `confidence >= 90` |
| Monorepo + vitest | Task 4: `test_detect_monorepo_bun_workspace` | `confidence >= 85` |
| Monorepo vitest-only | Task 4: `test_detect_monorepo_no_bun_workspace` | `confidence == 0` |
| Orchestrator parsing | Task 5: `test_parse_orchestrator_suites` | Correct dirs + cwd per suite |
| Empty orchestrator | Task 5: `test_parse_orchestrator_empty` | Returns `None` |
| Level → dirs | Task 5: `test_suite_command_with_orchestrator` | Args contain dirs, cwd set |
| File path detection | Task 6: `test_single_test_file_path` | No `--test-name-pattern` |
| Name pattern detection | Task 6: `test_single_test_name_pattern` | Has `--test-name-pattern` |
| Workspace prefix strip | Task 6: `test_single_test_workspace_path` | Prefix stripped, cwd set |
| DATABASE_URL removed | Tasks 5-6: all TestCommand constructions | `remove_env` contains "DATABASE_URL" only when `.env.test` exists |
