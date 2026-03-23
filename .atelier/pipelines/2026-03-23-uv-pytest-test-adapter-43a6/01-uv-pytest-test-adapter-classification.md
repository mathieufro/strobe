# Classification: uv pytest test adapter

## Pipeline Type: Task

Small scope — adds `uv run pytest` support to the existing test adapter system. Touches one component (`src/test/`). The pattern is well-established with 11 existing adapters. Likely involves a new adapter file (or extending the existing pytest adapter), registration in `mod.rs`, and detection logic for `uv` projects (e.g., `uv.lock`, `[tool.uv]` in `pyproject.toml`).

## Execution Mode: In-tree

Single focused change, no parallel work risk, straightforward to implement and test.
