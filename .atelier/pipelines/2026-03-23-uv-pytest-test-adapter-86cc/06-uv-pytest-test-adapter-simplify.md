# Simplification Review: uv pytest test adapter

- **Date:** 2026-03-24
- **Files in scope:** `src/test/pytest_adapter.rs` (92 insertions, 21 deletions)
- **Spec:** `.atelier/pipelines/2026-03-23-uv-pytest-test-adapter-86cc/03-uv-pytest-test-adapter-task-spec.md`

---

## Verdict: No changes needed

The implementation is already at minimum complexity for the problem it solves.

---

## Pass Results

### Pass 1: Necessity

No unnecessary mechanisms. The change adds:
- One 1-line detection helper (`use_uv`) — checks `uv.lock` existence
- Conditional prefix in two command-building methods
- 5 unit tests

All proportional. No retry logic, caching, state machines, or defensive mechanisms.

### Pass 2: Dead Surface

Everything added is reachable:
- `use_uv()` called from both `suite_command` and `single_test_command`
- No unused parameters, types, or fields introduced
- All 5 tests exercise real code paths

### Pass 3: Spec Hygiene

N/A — no spec files in the diff.

### Pass 4: Code Consistency

- String conversion uses `.into()` matching the file's existing convention (other adapters use `.to_string()` but within-file consistency is correct)
- Conditional `if uv { ... } else { ... }` pattern is clear and idiomatic
- No naming drift or mixed idioms

### Pass 5: Code Clarity

- No deep nesting (max 2 levels)
- Longest new method body is ~15 lines
- No negated conditions or unnecessary type assertions

### Pass 6: Code Compression

- The program/args switching (3 lines) is duplicated across `suite_command` and `single_test_command`. Extracting a `base_command()` helper would save ~3 lines but add an abstraction for only 2 call sites. Per "three similar lines of code is better than a premature abstraction" — leave as-is.
- No redundant variables, wrapper functions, identity transforms, or debug artifacts.

---

## Considered and Rejected

| Idea | Why rejected |
|------|-------------|
| Extract `base_command()` helper | Only 2 call sites, each 3 lines. Premature abstraction. |
| Remove `/// Check whether...` doc comment on `use_uv` | Subtraction pass should not remove informational comments that explain *why* (uv.lock detection). |
| Merge test cases | Each test targets a distinct code path (suite/single x uv/non-uv + level). No redundancy. |
