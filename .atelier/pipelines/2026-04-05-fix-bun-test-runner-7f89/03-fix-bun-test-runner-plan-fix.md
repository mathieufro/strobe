# Plan Fix Summary: Fix Bun Test Runner

## Issues Fixed

All 7 issues from the review were applied as localized fixes to the plan.

### Localized Fixes: 7

| # | Severity | Fix Applied |
|---|----------|-------------|
| 1 | **Critical** | `parse_suites_from_ts`: Changed `brace_depth = 0` to `brace_depth = 1` to account for outer `{` on the SUITES declaration line (skipped via `continue`). Without this, inner suite `},` brings depth to 0 and triggers early exit after 2 suites — `integration`, `e2e`, etc. would be lost. |
| 2 | Medium | Reframed root cause #3: liaison works today (returns 85 via bun.lock) because vitest is only in `apps/web/package.json`. Updated description to position monorepo detection as a robustness improvement, not a current-failure explanation. |
| 3 | **High** | Guarded skip marker (`-`/`»`) parsing with `failure_ctx.is_none()` in `parse_bun_output`. Diff-style assertion output (e.g., `- Expected: 401`) would otherwise misfire as skipped tests, truncating error messages. Added `BUN_NATIVE_DIFF_FAILURE` test constant and `test_parse_bun_native_diff_failure_no_false_skip` test. |
| 4 | Medium | Added `p.start_test(name.clone())` before every `p.finish_test(&name)` in `update_progress`. Added doc comment explaining the Bun limitation (no start markers) and that stuck detection relies on process-level monitoring. |
| 5 | Medium | Replaced hardcoded `remove_env: vec!["DATABASE_URL"]` with `bun_remove_env()` helper that checks for `.env.test` in the test cwd (or project root). Projects without `.env.test` won't have DATABASE_URL stripped. Applied to all 3 `TestCommand` constructions in `suite_command` and `single_test_command`. |
| 6 | Low | Replaced fragile string filter in cmd array parsing with explicit `is_bun_test` check. Non-test commands (`"bun", "run", "scripts/..."`) now produce empty `dirs` instead of leaking script paths like `scripts/test-e2e-parallel.ts`. |
| 7 | Low | Added `.round()` before `as u64` cast in `parse_duration_bracket` for both `ms` and `s` suffixes. Sub-millisecond durations like `0.45ms` now round to nearest integer instead of truncating to 0. |

### Architectural Mismatches: 0

### Spec Amendments: 0

No spec amendments needed — all issues were implementation-level fixes within a correct architectural direction.

## Unresolved Issues: 0

All review issues addressed.
