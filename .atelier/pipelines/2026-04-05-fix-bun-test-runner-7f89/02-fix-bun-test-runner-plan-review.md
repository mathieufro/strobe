# Plan Review: Fix Bun Test Runner

## Pass 1: Goal Compliance

The plan addresses all user requirements:
- Full error reporting (Task 2: native output parser with stack traces, file:line extraction)
- Full pass/fail (Task 2 + 3: parsing + progress tracking)
- stderr/stdout (Task 2: stderr-first with stdout fallback)
- Individual test files (Task 6: file path support + workspace stripping)
- Full suite and level-based runs (Task 5: orchestrator parsing for unit/integration/e2e)
- Verified against `/Users/alex/liaison/` (Task 7)

`test:full` (composite sequential runner) is correctly scoped out — the user can invoke levels individually via `debug_test`. No gold-plating detected.

No issues.

## Pass 2: Codebase Alignment

### Issue 1 — **Critical**: `parse_suites_from_ts` brace-depth tracking exits after 2 suites

**Location:** Task 5, section 5.3, the `parse_suites_from_ts` function.

The SUITES detection branch sets `brace_depth = 0` and calls `continue`, skipping brace counting on the detection line itself (`const SUITES: Record<...> = {`). This means the outer opening `{` is never counted.

When parsing suites, each inner suite entry's closing `},` decrements brace_depth to 0. The exit condition is:
```rust
if brace_depth <= 0 && in_suites_block && suites.len() > 0 {
    // flush and break
}
```

Trace through the real `test-run.ts`:
1. `all: {` → brace_depth = 1, suite name detected
2. Inner lines → brace_depth stays 1
3. `},` → brace_depth = 0 → `suites.len() == 0` (all not yet flushed) → no break
4. `unit: {` → brace_depth = 1, flushes "all" → `suites.len() = 1`
5. Inner lines → brace_depth stays 1
6. `},` → brace_depth = 0 → **`suites.len() == 1 > 0` → BREAKS**

Only `all` and `unit` are parsed. `integration`, `e2e`, `playwright`, `worker-env` — all lost.

**Fix:** Initialize `brace_depth = 1` instead of `0` to account for the outer `{` on the skipped SUITES declaration line. This way, inner suite `},` brings depth to 1 (not 0), and only the final `};` of the whole SUITES object brings it to 0.

### Issue 2 — **Medium**: Detection root cause #3 is misdescribed for liaison

**Location:** Plan "Current State" section, root cause #3.

The plan says: *"if `"vitest"` appears in root `package.json`, adapter returns 0."*

Verified: liaison's root `package.json` does NOT contain `"vitest"` — it's only in `apps/web/package.json`. Current detection returns 85 (bun.lock exists) for liaison, so detection already works. The monorepo detection improvements are still valuable for robustness, but the root cause framing is misleading — it implies this is why liaison detection fails today, when it doesn't.

**Fix:** Reframe root cause #3 as a robustness improvement: *"Detection lacks monorepo awareness — a root package.json with vitest would incorrectly bail even when bun:test lives in a separate workspace. Liaison works today (returns 85 via bun.lock) but lacks the higher-confidence 90 from bunfig.toml scanning."*

## Pass 3: Task Coherence

Task ordering is correct: Task 1 (TestCommand fields) is a prerequisite for Tasks 4-6 (which produce cwd/remove_env). Task 2 (parser) is independent. Task 3 (progress) depends on Task 2's helper functions. Task 7 (E2E) depends on all prior tasks.

No circular dependencies. Final wiring is covered: Task 3 updates the progress routing in mod.rs (line 339), and Task 5 updates suite_command to use orchestrator + workspace cwd.

No issues.

## Pass 4: TDD Feasibility

### Issue 3 — **High**: Skip marker parsing may misfire on error output lines

**Location:** Task 2, section 2.3, the main parse loop's skip detection:

```rust
if (trimmed.starts_with('-') || trimmed.starts_with('»')) && trimmed.len() > 2 {
    let (name, _) = parse_test_marker_line(trimmed);
    if !name.is_empty() && name != "-" {
        flush_failure(...);
        skipped += 1;
        ...
    }
}
```

This runs before the failure-message collection block. If a failure's error output contains a line starting with `-` (e.g., a diff-style assertion message like `- Expected: 401` or `- snapshot line`), it would:
1. Call `flush_failure` prematurely (truncating the error message)
2. Count it as a skipped test with name `Expected: 401`

While Bun's standard assertion output (`error: expect(...).toBe(...)`, `Expected:`, `Received:`) doesn't typically start with `-`, diff-style matchers (`toMatchSnapshot`, `toEqual` for objects) can produce `-`-prefixed lines.

No test covers this case — the test constants all have clean error formats.

**Fix:** Only check for skip markers when NOT inside a failure context. Restructure the main loop to skip the `-` check when `failure_ctx.is_some()`:
```rust
// Skip markers — only outside failure context
if failure_ctx.is_none()
    && (trimmed.starts_with('-') || trimmed.starts_with('»'))
    && trimmed.len() > 2
{
    ...
}
```
Add a test with a diff-style failure containing `-`-prefixed lines to verify.

### Issue 4 — **Medium**: `update_progress` calls `finish_test` without `start_test`

**Location:** Task 3, section 3.3.

The progress tracker calls `p.finish_test(&name)` for each `✓`/`✗`/`-` marker, but never calls `p.start_test(name)`. Since `finish_test` removes from `running_tests` (which was never populated), it's a no-op — no per-test durations are recorded, and the stuck detector's `running_tests`-based individual test tracking won't see Bun tests.

This is a design limitation (Bun only emits result lines, not start lines), but it should be explicitly documented and handled:

**Fix:** Add a comment explaining the limitation. For stuck detection, rely on process-level monitoring (which already works). Optionally, call `p.start_test(name.clone())` immediately before `p.finish_test(&name)` so the `test_durations` map is at least populated (even if duration = 0).

## Pass 5: Edge Case Coverage

### Issue 5 — **Medium**: Hardcoded `remove_env: vec!["DATABASE_URL"]`

**Location:** Tasks 5 and 6, every `TestCommand` construction in the Bun adapter.

`DATABASE_URL` is unconditionally stripped for all Bun test runs. This is correct for liaison (Bun auto-loads `.env.test`, and inherited env overrides it), but could break projects that intentionally pass DATABASE_URL to tests.

**Fix:** Only strip `DATABASE_URL` when a `.env.test` file exists in the test cwd:
```rust
let remove_env = if cwd.as_deref()
    .map(|c| Path::new(c).join(".env.test").exists())
    .unwrap_or_else(|| project_root.join(".env.test").exists())
{
    vec!["DATABASE_URL".to_string()]
} else {
    vec![]
};
```

### Issue 6 — **Low**: `e2e-parallel` suite script path leaks into dirs

**Location:** Task 5, section 5.3, `extract_quoted_strings` filter.

For `cmd: ["bun", "run", "scripts/test-e2e-parallel.ts", "--workers=4"]`:
- `"bun"` and `"run"` are filtered
- `"--workers=4"` is filtered
- `"scripts/test-e2e-parallel.ts"` passes both filters (ends with `.ts` BUT contains `/`)

So `e2e-parallel.dirs = vec!["scripts/test-e2e-parallel.ts"]`. If someone invoked this via `debug_test(level: "e2e")` and it fell through to `e2e-parallel`, the resulting `bun test scripts/test-e2e-parallel.ts` would be wrong.

Since `e2e-parallel` is out of scope and no `TestLevel` maps to it, this is non-blocking.

**Fix:** Filter the first element after "bun" that looks like a script path (contains no test-path indicators). Or explicitly handle `cmd[1] == "run"` vs `cmd[1] == "test"` to emit empty `dirs` for non-test commands.

### Issue 7 — **Low**: Sub-millisecond durations truncate to 0

**Location:** Task 2, section 2.3, `parse_duration_bracket`.

`"0.45ms"` → `0.45f64 as u64` → `0`. Tests with sub-ms durations report `duration_ms: 0`. Not functionally harmful (duration is informational) but slightly lossy.

**Fix:** Use `.round()` before casting: `ms_str.parse::<f64>().unwrap_or(0.0).round() as u64`. Or accept and document.

## Pass 6: Scope Discipline

All 7 tasks earn their place — each addresses a distinct root cause. No unnecessary abstractions. The orchestrator parser (Task 5) is the most complex piece but it's justified by liaison's actual `test-run.ts` architecture.

The helper functions (`is_file_header`, `parse_test_marker_line`, `parse_duration_bracket`, `flush_failure`, `extract_location_from_stack`, `extract_quoted_strings`, `extract_cwd_path`, `find_workspace_dirs`, `resolve_workspace_path`) are all used by multiple callers or complex enough to warrant extraction.

No issues.

---

## Summary

| # | Severity | Pass | Issue |
|---|----------|------|-------|
| 1 | **Critical** | Codebase Alignment | `parse_suites_from_ts` brace-depth starts at 0, exits after 2 suites |
| 2 | Medium | Codebase Alignment | Detection root cause #3 misdescribed — vitest not in liaison root |
| 3 | **High** | TDD Feasibility | Skip marker (`-`) parsing can misfire inside failure error output |
| 4 | Medium | TDD Feasibility | `finish_test` without `start_test` — stuck detector blind to individual Bun tests |
| 5 | Medium | Edge Cases | Hardcoded `remove_env: ["DATABASE_URL"]` may break non-liaison projects |
| 6 | Low | Edge Cases | `e2e-parallel` script path leaks into dirs (out of scope, non-blocking) |
| 7 | Low | Edge Cases | Sub-ms durations truncate to 0 |

## Verdict: `has_issues`

Issue #1 is critical — it would cause `debug_test(level: "integration")` and `debug_test(level: "e2e")` to silently fail (suites not found, falls back to running all tests). Issue #3 is high — diff-style assertion failures would corrupt skip counts and truncate error messages.
