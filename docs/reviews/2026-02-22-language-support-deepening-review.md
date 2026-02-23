# Review: Language Support Deepening

**Plan:** `docs/plans/2026-02-22-language-support-deepening.md`
**Spec:** `docs/specs/2026-02-22-language-support-deepening.md`
**Reviewed:** 2026-02-22
**Commits:** `59acbe9..77f3ae9` (3 commits)
**Branch:** `feature/language-support-deepening`
**Method:** 6-agent parallel review (Completeness, Correctness, Security, Integration, Test Coverage, Code Quality)

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Correctness | 2 | 6 | 2 |
| Integration | 1 | 1 | 0 |
| Security | 0 | 4 | 1 |
| Tests | 1 | 6 | 2 |
| Completeness | 0 | 2 | 1 |
| Code Quality | 0 | 1 | 4 |
| **Total** | **4** | **20** | **10** |

**Ready to merge:** No — 4 critical issues must be fixed first.

---

## Blocking Issues

### B1. GTest `command_for_binary` / `single_test_for_binary` not wired as trait overrides
**Severity:** Critical | **Category:** Integration
**Location:** `src/test/gtest_adapter.rs:132-152`
**Found by:** Pass 1 (Completeness), Pass 4 (Integration), Pass 6 (Code Quality)

The methods are defined as associated functions in `impl GTestAdapter`, not as overrides of the `TestAdapter` trait. When `mod.rs:244` calls `adapter.command_for_binary(cmd, level)?` through a `&dyn TestAdapter`, it dispatches to the default impl in `adapter.rs` which returns `Err(ValidationError("gtest does not support direct binary execution"))`. The GTest-specific implementation is unreachable via dynamic dispatch.

Also: the signatures differ — inherent methods take `cmd: &str` (no `&self`) and return `TestCommand` (not `Result<TestCommand>`).

**Impact:** Any user passing `command` to `debug_test` with `framework: "gtest"` always gets an error. The core GTest use case is completely broken.

**Fix:** Move into `impl TestAdapter for GTestAdapter` with correct signatures:
```rust
impl TestAdapter for GTestAdapter {
    fn command_for_binary(&self, cmd: &str, _level: Option<TestLevel>) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: cmd.to_string(),
            args: vec!["--gtest_output=json:/dev/stdout".to_string()], // see B2
            env: HashMap::new(),
        })
    }
    fn single_test_for_binary(&self, cmd: &str, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: cmd.to_string(),
            args: vec![
                "--gtest_output=json:/dev/stdout".to_string(),
                format!("--gtest_filter={}", test_name),
            ],
            env: HashMap::new(),
        })
    }
}
```

### B2. GTest `--gtest_output=json` writes to file, not stdout
**Severity:** Critical | **Category:** Correctness
**Location:** `src/test/gtest_adapter.rs:136`
**Found by:** Pass 2 (Correctness)

`--gtest_output=json` without a path writes to `test_detail.json` in the CWD. `parse_output(stdout, ...)` receives only human-readable text, so `parse_gtest_json` always fails and falls back to `parse_gtest_text_fallback`, losing detailed failure info. The spec explicitly says `--gtest_output=json:/dev/stdout`.

**Fix:** Change to `"--gtest_output=json:/dev/stdout".to_string()` in both `command_for_binary` and `single_test_for_binary`.

### B3. Mocha fallback `summary.failed` always 0
**Severity:** Critical | **Category:** Correctness
**Location:** `src/test/mocha_adapter.rs:161-172`
**Found by:** Pass 5 (Test Coverage)

When JSON parsing fails and `exit_code != 0`, the `failures` vec contains one entry but `summary.failed` is hardcoded to `0`. The MCP response surface reports `failed: 0` to the LLM even when Mocha crashed.

**Fix:** Change line 164 from `failed: 0` to `failed: if exit_code != 0 { 1 } else { 0 }`.

### B4. GTest `update_progress` — `SuitesFinished` phase never fires
**Severity:** Critical | **Category:** Correctness
**Location:** `src/test/gtest_adapter.rs:434`
**Found by:** Pass 2 (Correctness)

Checks for `"tests ran"` but actual GTest output is `"N tests from M test suite ran."` — the substring `"tests ran"` never appears. The stuck detector will see stale `running_tests` entries and `SuitesFinished` is never reached.

**Fix:** Change check to `trimmed.contains("ran.")` which matches the actual GTest output format.

---

## Important Issues

### I1. ESM source transformation: unescaped `name`/`url` in single-quoted strings
**Severity:** Important | **Category:** Correctness + Security
**Location:** `src/daemon/session_manager.rs:122-124`, `agent/src/tracers/v8-tracer.ts:210-213`
**Found by:** Pass 2 (Correctness), Pass 3 (Security)

The `url` value (file path) is interpolated directly into a single-quoted JS string. A path containing `'` (e.g., `/home/o'brian/app.mjs`) produces invalid JavaScript. Node.js module loading crashes for that module.

**Fix:** Escape single quotes and backslashes in both `name` and `url` before interpolation:
```typescript
const safeName = name.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
const safeUrl = url.replace(/\\/g, '\\\\').replace(/'/g, "\\'");
```
Apply in both `v8-tracer.ts:210` and `session_manager.rs:122`.

### I2. Go adapter `.last()` for error message picks up `--- FAIL:` line
**Severity:** Important | **Category:** Correctness
**Location:** `src/test/go_adapter.rs:179-183`
**Found by:** Pass 2 (Correctness)

For a failing test, `.last()` on non-empty output lines returns `"--- FAIL: TestName (0.00s)"` instead of the actual assertion message. The LLM sees a useless error message.

**Fix:** Filter out `=== RUN` and `--- FAIL/PASS/SKIP` lines, take first remaining:
```rust
let message = output_text.lines()
    .filter(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with("=== RUN") &&
        !t.starts_with("--- FAIL") && !t.starts_with("--- PASS")
    })
    .next()
    .unwrap_or("Test failed")
    .trim()
    .to_string();
```

### I3. Mocha `parse_output` ignores `exit_code` when JSON parse succeeds
**Severity:** Important | **Category:** Correctness
**Location:** `src/test/mocha_adapter.rs:141-143`
**Found by:** Pass 2 (Correctness)

When JSON parsing succeeds but `exit_code != 0` and `summary.failed == 0` (partial JSON from crash), the adapter reports success. Should cross-check exit code after `build_result_from_report`.

### I4. Python `_strobe_on_start` may fail for decorated functions
**Severity:** Important | **Category:** Correctness
**Location:** `agent/src/tracers/python-tracer.ts:344-350`
**Found by:** Pass 2 (Correctness)

`co_firstlineno` on CPython 3.12+ points to the decorator line, not the `def` line. If `PythonResolver` emits the `def` line, decorated functions never match. Consider also matching on `code.co_name` (function name).

### I5. Python `removeAllHooks`/`dispose` only call `sys.settrace(None)`, not `threading.settrace_all_threads(None)`
**Severity:** Important | **Category:** Correctness
**Location:** `agent/src/tracers/python-tracer.ts:527-541`
**Found by:** Pass 2 (Correctness)

Other threads retain trace callbacks after cleanup, causing unnecessary overhead. Should call `threading.settrace_all_threads(None)` (3.12+) or `threading.settrace(None)` (fallback).

### I6. "No framework detected" error only lists Cargo/Catch2
**Severity:** Important | **Category:** Completeness
**Location:** `src/test/mod.rs:210-215`
**Found by:** Pass 1, 4, 6

The `"Unknown framework"` error at line 189 was correctly updated with all 11 frameworks, but the `"No framework detected"` message still only mentions Cargo and Catch2. Misleading for users of Go, Deno, Mocha, etc.

### I7. `python_e2e.rs` function tracing assertion not hardened
**Severity:** Important | **Category:** Completeness
**Location:** `tests/python_e2e.rs:170-175`
**Found by:** Pass 1 (Completeness)

Plan Task 2 explicitly requires replacing the `eprintln!` with a hard `assert!`. Still uses a soft warning.

### I8. Temp file for ESM hook script has predictable name
**Severity:** Important | **Category:** Security
**Location:** `src/daemon/session_manager.rs:107-153`
**Found by:** Pass 3 (Security)

Written to `/tmp/strobe-esm-hooks-{session_id}.mjs` with predictable session IDs. `std::fs::write` performs no exclusive creation check. On shared systems, symlink attacks are possible.

**Fix:** Write to `~/.strobe/` instead of `/tmp`, or use `tempfile::Builder` with random suffix.

### I9. Logpoint `.format()` template with unsanitized message — SSTI-like risk
**Severity:** Important | **Category:** Security
**Location:** `agent/src/tracers/python-tracer.ts:303`
**Found by:** Pass 3 (Security)

Python's `str.format()` supports `{__class__.__init__.__globals__[...]}` attribute traversal. Logpoint messages are embedded with only `'` and `\` escaping, then `.format(**{**frame.f_globals, **frame.f_locals})` processes them.

**Fix:** Replace `.format()` with a safe template engine that only allows simple `{variable_name}` lookups.

### I10. `writeVariable` uses unsanitized `expr` in both Python and V8 tracers
**Severity:** Important | **Category:** Security
**Location:** `python-tracer.ts:719-728`, `v8-tracer.ts:172-178`
**Found by:** Pass 3 (Security)

Both tracers embed `expr` directly into code strings (`${expr} = ...` in Python, `new Function('__v', '${expr} = __v')` in JS). Semicolons or newlines in `expr` allow code injection.

**Fix:** Validate `expr` against `/^[a-zA-Z_][\w.]*(\[[\w'"]+\])*$/` before embedding.

### I11. ESM `registerHooks` load callback doesn't handle async `nextLoad`
**Severity:** Important | **Category:** Integration
**Location:** `agent/src/tracers/v8-tracer.ts:195-219`
**Found by:** Pass 4 (Integration)

`nextLoad()` can return a Promise for non-file sources. The code accesses `result.format` synchronously, which would be `undefined` on a Promise. Transform silently skipped for those modules.

### I12. No tests for mocha/gtest `update_progress`
**Severity:** Important | **Category:** Tests
**Location:** `src/test/mocha_adapter.rs`, `src/test/gtest_adapter.rs`
**Found by:** Pass 5 (Test Coverage)

Both `update_progress` functions feed directly into the `StuckDetector`. No unit tests verify their parsing logic.

### I13. No integration test for new adapter registration
**Severity:** Important | **Category:** Tests
**Location:** `tests/test_runner.rs`
**Found by:** Pass 5 (Test Coverage)

`test_adapter_detection()` only tests Cargo and Catch2. Plan Task 13 Step 2 requires adding Deno, Go, Mocha, and GTest detection tests.

### I14. No automated ESM `function_enter` test
**Severity:** Important | **Category:** Tests
**Location:** Missing from `tests/frida_e2e.rs`
**Found by:** Pass 5 (Test Coverage)

The ESM fixture exists but has no automated test verifying `function_enter` events. The core Stream B deliverable has no CI enforcement.

### I15. Mocha missing tests: skipped/pending, `single_test_command`
**Severity:** Important | **Category:** Tests
**Location:** `src/test/mocha_adapter.rs`
**Found by:** Pass 5 (Test Coverage)

The `pending` (skip) parsing path and `single_test_command()` have zero test coverage. A field rename or logic inversion would go undetected.

### I16. GTest text fallback doesn't handle `[SKIPPED]` lines
**Severity:** Important | **Category:** Tests
**Location:** `src/test/gtest_adapter.rs:285-349`
**Found by:** Pass 5 (Test Coverage)

`parse_gtest_text_fallback` never increments `skipped` for `[  SKIPPED ]` lines. Skipped tests silently vanish from results in text-fallback mode.

### I17. GTest detect tests use fixed temp dir paths — race in parallel
**Severity:** Important | **Category:** Tests
**Location:** `src/test/gtest_adapter.rs:447-473`
**Found by:** Pass 5 (Test Coverage)

Uses `std::env::temp_dir()` with fixed names instead of `tempfile::tempdir()`. Parallel `cargo test` runs will race. Other adapters use `tempfile::tempdir()`.

### I18. `extract_test_name_from_text` dead code + duplication in `update_progress`
**Severity:** Important | **Category:** Code Quality
**Location:** `src/test/gtest_adapter.rs:352-431`
**Found by:** Pass 6 (Code Quality)

The function's first 3 lines compute `after` but it's only used if `line.find(']')` is `None`, which never happens for valid GTest output. `update_progress` duplicates the same name extraction logic 3 times inline.

---

## Minor Issues

### M1. Deno `detect()` returns 92, spec says 90
`src/test/deno_adapter.rs:14`

### M2. Go location regex matches vendor/stdlib paths before test file
`src/test/go_adapter.rs:128`

### M3. `settraceInstalled` not reset in `removeAllHooks()`/`dispose()`
`agent/src/tracers/python-tracer.ts:540` — stale flag, but doesn't cause functional issues due to `traceInstalled` gating.

### M4. Monitoring teardown duplicated verbatim in `dispose()` and `removeAllHooks()`
`agent/src/tracers/python-tracer.ts` — extract to private helper.

### M5. `file.replace('file://', '')` called 3x per event in hot-path `__strobe_trace` bridge
`agent/src/tracers/v8-tracer.ts:70-86` — hoist once before loop.

### M6. `#[allow(dead_code)]` on `MochaTest.file` — field never used
`src/test/mocha_adapter.rs:43-46` — either populate or remove.

### M7. No `module.register()` fallback for Node 20.6-22.14 in ESM hook script
`src/daemon/session_manager.rs:109-150` — spec version matrix item, not a functional bug for primary target.

### M8. `gtest_filter` argument not sanitized for GTest metacharacters
`src/test/gtest_adapter.rs:143-152` — wildcards in test name could run all tests.

### M9. Deno/Go adapters missing edge case tests
`src/test/deno_adapter.rs` (`deno.lock` detection), `src/test/go_adapter.rs` (`go.sum`-only, crash exit codes).

### M10. `progress_fn` dispatch missing `"pytest"` entry (pre-existing)
`src/test/mod.rs:306-314` — `pytest_adapter` exports `update_progress` but isn't wired.

---

## Approved

- [x] Stream A: Python 3.12+ `sys.monitoring` tracer — correctly implemented with dual-mode tracing
- [x] Stream A: CPython version detection via `Py_GetVersion` — correct
- [x] Stream A: `dispose()` / `removeAllHooks()` monitoring teardown — present (minor duplication)
- [x] Stream A: `PyEval_SaveThread/RestoreThread` removed from breakpoints — correct
- [x] Stream B: `globalThis.__strobe_trace` bridge — correctly installed with name+file matching
- [x] Stream B: `globalThis.__strobe_hooks` pattern sharing — synced on `installHook()`
- [x] Stream B: `generate_esm_hook_script()` — correctly generated and injected via `NODE_OPTIONS`
- [x] Stream B: Node.js vs Bun detection for `NODE_OPTIONS` — correct basename check
- [x] Stream B: Temp file cleanup on session stop — present in both `stop_session()` and `stop_session_retain()`
- [x] Stream B: `registerEsmHooks()` called from `initialize()` — correct ordering (after bridge install)
- [x] Stream B: `node_esm_target.mjs` fixture — correct with keep-alive interval
- [x] Stream C: V8 runtime flag conditioned on Bun — correctly implemented via `is_bun` flag
- [x] Stream C: Validation script with multi-hook attribution, async URL parsing, response sync — correct
- [x] Stream D: All 4 adapters registered in `TestRunner::new()` — verified
- [x] Stream D: `parse_junit_xml` made `pub(crate)` — verified
- [x] Stream D: `progress_fn` dispatch updated for 4 new adapters — verified
- [x] Stream D: MCP schema `framework` enum updated — verified
- [x] Stream D: Unknown framework error message updated — verified
- [x] Stream D: `command_for_binary` / `single_test_for_binary` added to `TestAdapter` trait — verified
- [x] Stream D: Deno adapter — JUnit XML with preamble stripping, `rerun` field, `update_progress`
- [x] Stream D: Go adapter — streaming JSON, `build-fail` handling, regex escaping, UTF-8 safe truncation
- [ ] Stream D: GTest adapter — blocked by B1 (trait wiring), B2 (`--gtest_output`), B4 (progress)
- [ ] Stream D: Mocha adapter — blocked by B3 (fallback `failed` count)

## Recommendations

1. **GTest adapter needs the most work** — 3 of 4 critical issues are here. Fix trait wiring, `:/dev/stdout` path, and progress string match.
2. **Add `update_progress` tests for all new adapters** — this is the stuck detector's feed path and currently has zero coverage for mocha/gtest.
3. **Consider using `tempfile::NamedTempFile` for ESM hook scripts** — avoids both the predictable name issue and the cleanup problem.
4. **Extract Python monitoring teardown to a helper** — duplicated verbatim in `dispose()` and `removeAllHooks()`.
5. **The `writeVariable` input validation should be added before the next release** — both Python and V8 paths accept arbitrary expressions.
