# Classification: uv run pytest Test Adapter

## Pipeline: Task

## Rationale

Single-subsystem change following an established, well-documented adapter pattern. The existing `PytestAdapter` already implements JSON output parsing, progress tracking, trace suggestions, and timeout logic — all of which are reused verbatim for `uv run pytest`. The delta is detection logic (presence of `uv.lock` or `uv` tooling) and command construction (`uv run pytest` instead of `python3 -m pytest`). Touches 2-3 files in `src/test/`, no cross-cutting concerns, no new abstractions.

## Scope

- **New file:** `src/test/uv_pytest_adapter.rs` — new adapter struct implementing `TestAdapter` trait
- **Modified:** `src/test/mod.rs` — register `UvPytestAdapter` in the adapter list, wire up progress function
- **Detection signals:** `uv.lock` file presence, `pyproject.toml` with `[tool.uv]` section, `uv` binary on PATH
- **Confidence:** ~95 (higher than plain pytest's 90, so uv projects prefer this adapter when both match)
- **Command:** `uv run pytest --tb=short -q --json-report --json-report-file=-`
- **Reuse:** Output parsing, progress tracking, trace suggestions, and stack capture all delegate to existing `pytest_adapter` functions

## Why Not Smaller / Bigger

- **Not a Quick Fix:** Requires a new file, adapter registration, detection heuristics, and test-level command variants — more than a one-line change.
- **Not a Feature:** No new abstractions, no API surface changes, no cross-component coordination. Follows an existing pattern exactly. A single spec+plan document is sufficient.
