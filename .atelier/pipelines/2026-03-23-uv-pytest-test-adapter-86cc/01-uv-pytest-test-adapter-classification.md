# Classification: uv run pytest Test Adapter

**Pipeline type:** Task
**Execution mode:** In-tree

## Rationale

Adding `uv run pytest tests/` support is a small, pattern-following addition to the existing test adapter system. The pytest adapter already handles output parsing, JSON reports, and trace suggestions — the uv variant mirrors it with `uv run` as the command prefix and uv-specific detection (uv.lock, [tool.uv] in pyproject.toml).

**Scope:** 2-3 files in a single component (src/test/).

**Why Task, not Feature:** Follows an established adapter pattern with no multi-component design needed. A senior engineer could hold this in their head.

**Why in-tree:** Single-pipeline work, no parallel branches or PR review needed.
