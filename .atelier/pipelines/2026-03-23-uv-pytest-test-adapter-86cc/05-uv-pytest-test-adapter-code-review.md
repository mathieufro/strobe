# Code Review: uv run pytest support in PytestAdapter

- **Spec reference:** Original prompt — "I'd like to add support for `uv run pytest tests/` for our tests please."
- **Review date:** 2026-03-24
- **Commits reviewed:** Unstaged working tree changes on `main`
- **Branch:** main (uncommitted)
- **Files changed:** `src/test/pytest_adapter.rs`

---

## Summary Table

| Category      | Critical | Important | Minor |
|---------------|----------|-----------|-------|
| Completeness  | 0        | 0         | 0     |
| Correctness   | 0        | 0         | 0     |
| Code Quality  | 0        | 0         | 1     |
| Security      | 0        | 0         | 0     |
| Coherence     | 0        | 0         | 0     |
| Test Coverage | 0        | 0         | 1     |
| **Total**     | **0**    | **0**     | **2** |

---

## Blocking Issues

None.

---

## All Issues

### 1. Minor / Code Quality — Duplicated uv-detection logic

**File:** `src/test/pytest_adapter.rs:54-56, 81-86`

**Problem:** The args-building pattern (`if uv { vec!["run", "pytest"] } else { vec!["-m", "pytest"] }`) and the program selection (`if uv { "uv" } else { "python3" }`) are duplicated across `suite_command` and `single_test_command`. If a third method needs this (e.g., a future `command_for_binary`), it would be copied again.

**Suggested fix (optional):** Extract a small helper:

```rust
fn base_command(project_root: &Path) -> (String, Vec<String>) {
    if use_uv(project_root) {
        ("uv".into(), vec!["run".into(), "pytest".into()])
    } else {
        ("python3".into(), vec!["-m".into(), "pytest".into()])
    }
}
```

**Verdict:** Non-blocking. Only two call sites, both simple and readable. The duplication is acceptable at this scale.

---

### 2. Minor / Test Coverage — Missing `--json-report` assertion in single_test uv path

**File:** `src/test/pytest_adapter.rs:397-408` (`test_single_test_command_uses_uv_when_uv_lock_exists`)

**Problem:** The test for `single_test_command` with uv verifies `program == "uv"`, `args[0] == "run"`, `args[1] == "pytest"`, and the presence of `-k` / `test_foo`. However, it does not assert that `--json-report` and `--json-report-file=-` are present — these are critical for output parsing. A mutation that removed the `args.extend` for json-report flags would not be caught by this test.

**Suggested fix (optional):** Add:

```rust
assert!(cmd.args.contains(&"--json-report".to_string()));
assert!(cmd.args.contains(&"--json-report-file=-".to_string()));
```

**Verdict:** Non-blocking. The json-report flags are added in a shared code path that's exercised by `test_suite_command_uses_uv_when_uv_lock_exists` (which does check `--json-report`). A mutation would have to be path-specific to escape detection.

---

## Approved Requirements

| # | Requirement | Status |
|---|-------------|--------|
| 1 | Detect uv-managed projects (via `uv.lock` presence) | PASS |
| 2 | Use `uv run pytest` instead of `python3 -m pytest` when uv detected | PASS |
| 3 | Suite command works with uv | PASS |
| 4 | Single test command works with uv | PASS |
| 5 | Test level filtering (`-m unit/integration/e2e`) works with uv | PASS |
| 6 | pytest flags (`--tb=short`, `-q`, `--json-report`, `--json-report-file=-`) preserved | PASS |
| 7 | Non-uv projects unchanged (backward compatible) | PASS |
| 8 | Feature reachable via `debug_test` MCP tool | PASS |
| 9 | Tests cover both uv and non-uv paths | PASS |

---

## Detailed Analysis

### Pass 1: Completeness

The implementation covers both `suite_command` and `single_test_command`, which are the two command-building methods on the `TestAdapter` trait. The `detect()` method correctly does not need changes — it answers "is this a pytest project?" (yes/no), not "how to invoke pytest." The `parse_output()` method also needs no changes since `uv run pytest` produces identical stdout/stderr to `python3 -m pytest`. The `update_progress()` function is unaffected — uv's own output goes to stderr and doesn't match pytest progress patterns.

The feature is reachable: `debug_test` → `detect_adapter()` selects `PytestAdapter` → `suite_command`/`single_test_command` now checks `uv.lock` and dispatches accordingly.

### Pass 2: Correctness

- `uv run pytest <args>` correctly forwards all arguments to pytest — standard uv behavior.
- The `use_uv()` function checks `project_root.join("uv.lock").exists()`, which is the canonical indicator of a uv-managed project.
- When `uv.lock` is absent, behavior is identical to before the change (python3 -m pytest).
- Test level `-m` markers (pytest's `-m`, not Python's `-m`) are appended after the base args, working correctly in both paths.
- Edge case: `uv.lock` present but `uv` not installed → OS error "command not found," which is a reasonable and descriptive failure.

### Pass 3: Code Quality

Clean, minimal change. Follows the existing pattern in the codebase (cf. vitest/jest using `npx` as wrapper). The `use_uv` helper is well-placed as a module-level function with a clear doc comment. Minor duplication noted in Issue #1 but acceptable.

### Pass 4: Security

No concerns. Program names are hardcoded strings (`"uv"` / `"python3"`). No user input flows into command construction beyond `test_name`, which was already present before this change.

### Pass 5: Coherence

Consistent with how other adapters handle wrapper commands (vitest uses `npx`, bun uses `bun` directly). The lock-file-based detection mirrors bun's `bun.lockb` pattern. Fits naturally into the existing architecture.

### Pass 6: Test Coverage

Five new unit tests cover:
- Suite command with uv (program, args prefix, json-report flag)
- Suite command without uv (backward compat)
- Single test command with uv (program, args prefix, -k flag)
- Single test command without uv (backward compat)
- Suite command with uv + test level filtering

Mutation testing analysis:
- `use_uv` always returns `true` → tests 2, 4 fail
- `use_uv` always returns `false` → tests 1, 3, 5 fail
- Swapped branches → tests 1, 2 fail (args mismatch)
- Removed json-report flags → test 1 fails

Minor gap noted in Issue #2 but low risk.

---

## Recommendations (non-blocking)

1. Consider adding a `detect()` boost when `uv.lock` + `pyproject.toml` (with pytest in `[dependency-groups]`) are present, for projects that lack `[tool.pytest]` config. This is a pre-existing gap not introduced by this change.

---

## Verdict: `done`

The implementation is correct, complete, minimal, well-tested, and ready to merge. The two minor issues are non-blocking suggestions for polish.
