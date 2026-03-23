# Task Spec: uv pytest test adapter

## Design

### Purpose and Success Criteria

Add `uv run pytest` support to Strobe's test runner so that Python projects managed by uv are tested correctly without manual `framework` overrides.

**Success criteria:**
1. When `uv.lock` exists in `project_root`, `PytestAdapter` emits `uv run pytest ...` instead of `python3 -m pytest ...`
2. When `uv.lock` does not exist, behavior is unchanged (`python3 -m pytest ...`)
3. All existing pytest tests continue to pass
4. Output parsing, progress updates, trace suggestions, and `name()` are unaffected

### Architecture and Approach

**Approach:** Modify the existing `PytestAdapter` in-place. The adapter's `suite_command()` and `single_test_command()` methods gain a `uv.lock` existence check that switches the program and arg prefix.

**Rejected alternative:** A separate `UvPytestAdapter` would require a new file, new module declaration, new registration in `TestRunner::new()`, and a new progress dispatch match arm — all for logic that differs only in the program/args tuple. The existing adapter already has `project_root` available in both command methods.

**Why this works cleanly:**
- `name()` stays `"pytest"` → progress dispatch (`mod.rs:327`) unchanged
- `parse_output()` is identical — `uv run pytest --json-report` produces the same JSON as `python3 -m pytest --json-report`
- `detect()` is unchanged — it answers "is this a pytest project?", not "how to invoke it"
- `suggest_traces()` and `capture_stacks()` are language-level, not invocation-level
- `resolve_program()` in `mod.rs:560` handles PATH lookup for `uv` the same way it does for `python3`

### Components and Data Flow

**Single file modified:** `src/test/pytest_adapter.rs`

1. New helper: `fn use_uv(project_root: &Path) -> bool` — checks `project_root.join("uv.lock").exists()`
2. `suite_command()`: signature changes `_project_root` → `project_root`. If `use_uv(project_root)`, program is `"uv"` and args start with `["run", "pytest", ...]` instead of `["-m", "pytest", ...]`
3. `single_test_command()`: same conditional. `_root` → `root`, check `use_uv(root)`

**Data flow unchanged:** `TestRunner::run()` calls `adapter.suite_command()` → gets `TestCommand` → `resolve_program()` resolves `uv` or `python3` → Frida spawns it → stdout captured → `parse_output()` parses JSON report. The only difference is which binary gets spawned.

### Integration

No new integration points. The feature is reachable via the existing `debug_test` MCP tool with any Python project that has a `uv.lock` file. Auto-detection and `framework: "pytest"` override both work.

**Error path:** If `uv.lock` exists but `uv` is not on PATH, `resolve_program("uv")` returns `"uv"` (unresolved), and Frida's spawn fails with a clear error. This matches the existing behavior when `python3` is not on PATH.

---

## Implementation Plan

### Scope

**In scope:**
- Modify `PytestAdapter.suite_command()` and `single_test_command()` to use `uv run pytest` when `uv.lock` exists
- Unit tests for the command-switching logic

**Out of scope:**
- `uv run python -m unittest` support (separate adapter, separate task)
- Detection confidence changes based on uv presence
- `[tool.uv]` pyproject.toml detection
- uv installation or version checking

### Current State

`PytestAdapter` hardcodes `python3` as the program and `-m pytest` as the first args in both `suite_command()` (line 70) and `single_test_command()` (line 78). The `project_root` parameter is available but unused (prefixed with `_`).

### Task 1: uv detection and command switching

**Files:**
- Modify: `src/test/pytest_adapter.rs` (lines 43-90)
- Test: `src/test/pytest_adapter.rs` (existing `#[cfg(test)]` module, line 333+)

#### 1.1 Write failing tests

Add to the existing `mod tests` block in `pytest_adapter.rs`:

```rust
#[test]
fn test_suite_command_uses_uv_when_uv_lock_exists() {
    let adapter = PytestAdapter;
    let dir = tempfile::tempdir().unwrap();
    // Create uv.lock to signal uv project
    std::fs::write(dir.path().join("uv.lock"), "").unwrap();

    let cmd = adapter.suite_command(dir.path(), None, &HashMap::new()).unwrap();
    assert_eq!(cmd.program, "uv");
    assert_eq!(cmd.args[0], "run");
    assert_eq!(cmd.args[1], "pytest");
    assert!(cmd.args.contains(&"--json-report".to_string()));
}

#[test]
fn test_suite_command_uses_python3_without_uv_lock() {
    let adapter = PytestAdapter;
    let dir = tempfile::tempdir().unwrap();
    // No uv.lock

    let cmd = adapter.suite_command(dir.path(), None, &HashMap::new()).unwrap();
    assert_eq!(cmd.program, "python3");
    assert_eq!(cmd.args[0], "-m");
    assert_eq!(cmd.args[1], "pytest");
}

#[test]
fn test_single_test_command_uses_uv_when_uv_lock_exists() {
    let adapter = PytestAdapter;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uv.lock"), "").unwrap();

    let cmd = adapter.single_test_command(dir.path(), "test_foo").unwrap();
    assert_eq!(cmd.program, "uv");
    assert_eq!(cmd.args[0], "run");
    assert_eq!(cmd.args[1], "pytest");
    assert!(cmd.args.contains(&"-k".to_string()));
    assert!(cmd.args.contains(&"test_foo".to_string()));
}

#[test]
fn test_single_test_command_uses_python3_without_uv_lock() {
    let adapter = PytestAdapter;
    let dir = tempfile::tempdir().unwrap();

    let cmd = adapter.single_test_command(dir.path(), "test_foo").unwrap();
    assert_eq!(cmd.program, "python3");
    assert_eq!(cmd.args[0], "-m");
}

#[test]
fn test_suite_command_uv_with_level() {
    let adapter = PytestAdapter;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("uv.lock"), "").unwrap();

    let cmd = adapter.suite_command(dir.path(), Some(TestLevel::Unit), &HashMap::new()).unwrap();
    assert_eq!(cmd.program, "uv");
    assert_eq!(cmd.args[0], "run");
    assert_eq!(cmd.args[1], "pytest");
    // Level markers should still be appended
    assert!(cmd.args.contains(&"-m".to_string()));
    assert!(cmd.args.contains(&"not integration and not e2e".to_string()));
}
```

#### 1.2 Run test — verify failure

```
debug_test({ projectRoot: "/Users/alex/strobe", test: "pytest_adapter::tests::test_suite_command_uses_uv_when_uv_lock_exists" })
```

Expected failure: `assertion failed: cmd.program == "uv"` — program is still `"python3"`.

#### 1.3 Implementation

Add helper function and modify `suite_command` and `single_test_command`:

```rust
/// Check whether the project uses uv (presence of uv.lock).
fn use_uv(project_root: &Path) -> bool {
    project_root.join("uv.lock").exists()
}
```

In `suite_command`, change `_project_root` → `project_root` and build args conditionally:

```rust
fn suite_command(
    &self,
    project_root: &Path,
    level: Option<TestLevel>,
    _env: &HashMap<String, String>,
) -> crate::Result<TestCommand> {
    let uv = use_uv(project_root);
    let mut args: Vec<String> = if uv {
        vec!["run".into(), "pytest".into()]
    } else {
        vec!["-m".into(), "pytest".into()]
    };
    args.extend(["--tb=short".into(), "-q".into(), "--json-report".into(), "--json-report-file=-".into()]);
    match level {
        Some(TestLevel::Unit) => {
            args.extend(["-m".into(), "not integration and not e2e".into()]);
        }
        Some(TestLevel::Integration) => {
            args.extend(["-m".into(), "integration".into()]);
        }
        Some(TestLevel::E2e) => {
            args.extend(["-m".into(), "e2e".into()]);
        }
        None => {}
    }
    Ok(TestCommand {
        program: if uv { "uv".into() } else { "python3".into() },
        args,
        env: HashMap::new(),
    })
}
```

In `single_test_command`, change `_root` → `root`:

```rust
fn single_test_command(&self, root: &Path, test_name: &str) -> crate::Result<TestCommand> {
    let uv = use_uv(root);
    let mut args: Vec<String> = if uv {
        vec!["run".into(), "pytest".into()]
    } else {
        vec!["-m".into(), "pytest".into()]
    };
    args.extend([
        "-k".into(),
        test_name.into(),
        "--json-report".into(),
        "--json-report-file=-".into(),
        "--tb=short".into(),
    ]);
    Ok(TestCommand {
        program: if uv { "uv".into() } else { "python3".into() },
        args,
        env: HashMap::new(),
    })
}
```

#### 1.4 Run test — verify passes

```
debug_test({ projectRoot: "/Users/alex/strobe", test: "pytest_adapter" })
```

All 8 tests green (3 existing + 5 new).

#### 1.5 Checkpoint

PytestAdapter now emits `uv run pytest` commands when `uv.lock` is present, and unchanged `python3 -m pytest` commands otherwise.

**Edge cases covered:**
- No uv.lock → original behavior preserved (regression guard)
- uv.lock present with test level → markers still appended correctly after `run pytest`
- uv.lock present with single test → `-k` filter works with uv prefix
- Empty uv.lock file → still detected (file existence is the signal, not content)

---

### Execution Order

Sequential single task — no parallelism needed.

```
[Task 1: command switching + tests] ─── done
```

### Files Modified Summary

| File | Action | Change |
|------|--------|--------|
| `src/test/pytest_adapter.rs` | Modify | Add `use_uv()` helper; update `suite_command()` and `single_test_command()` to conditionally use `uv run pytest`; add 5 unit tests |

### Edge Case Coverage Matrix

| Requirement | Test | Assertion |
|-------------|------|-----------|
| uv.lock → `uv run pytest` (suite) | `test_suite_command_uses_uv_when_uv_lock_exists` | `program == "uv"`, `args[0..2] == ["run", "pytest"]` |
| No uv.lock → `python3 -m pytest` (suite) | `test_suite_command_uses_python3_without_uv_lock` | `program == "python3"`, `args[0..2] == ["-m", "pytest"]` |
| uv.lock → `uv run pytest` (single) | `test_single_test_command_uses_uv_when_uv_lock_exists` | `program == "uv"`, args contain `-k` and test name |
| No uv.lock → `python3 -m pytest` (single) | `test_single_test_command_uses_python3_without_uv_lock` | `program == "python3"` |
| uv + test level markers | `test_suite_command_uv_with_level` | args contain `-m` and level filter after `run pytest` |
| Existing detection unchanged | `test_detect_pytest_config` (existing) | confidence >= 80 for fixture dir |
| Existing JSON parsing unchanged | `test_parse_pytest_json_report` (existing) | parsed counts match |
