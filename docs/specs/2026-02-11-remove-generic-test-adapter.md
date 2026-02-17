# Remove Generic Test Adapter

**Date:** 2026-02-11
**Status:** Approved

## Problem

LLMs frequently pass `framework: "generic"` to `debug_test`, bypassing proper framework detection. The generic adapter provides degraded results: no per-test details, no trace suggestions, no live progress. It's a trap that makes Strobe look broken.

## Solution

Remove the generic adapter entirely. Only Cargo and Catch2 are supported. When neither is detected, return a clear error with guidance. Improve tool descriptions so the LLM gets it right on the first call.

## Changes

### 1. Delete GenericAdapter

- Delete `src/test/generic_adapter.rs`
- Remove `mod generic_adapter` from `src/test/mod.rs`
- Remove `GenericAdapter` from `TestRunner::new()` adapter list
- Remove `use generic_adapter::GenericAdapter` import

### 2. Return error when no adapter matches

Change `detect_adapter` to return `Result<&dyn TestAdapter>` instead of always returning an adapter. When no adapter has confidence > 0, return an error:

```
No test framework detected. Supported frameworks:
- Cargo (Rust): provide projectRoot pointing to a directory with Cargo.toml
- Catch2 (C++): provide command with path to a test binary
```

When the LLM passes an invalid `framework` name (e.g., "generic", "pytest"), return:

```
Unknown framework '{}'. Supported: 'cargo', 'catch2'
```

### 3. Improve tool schema (`debug_test` in server.rs)

**Tool description** — update to:

```
Start a test run asynchronously or poll for results. Returns a testRunId immediately — poll with action: 'status' for progress and results.

Supported frameworks:
- Rust: provide projectRoot (auto-detects Cargo.toml). No command needed.
- C++/Catch2: provide command (path to test binary).

Use this instead of running test commands via bash.
```

**`framework` field** — add enum constraint:

```json
"framework": {
  "type": "string",
  "enum": ["cargo", "catch2"],
  "description": "Override auto-detection. Usually not needed — framework is detected from projectRoot (Cargo) or command (Catch2)."
}
```

**`command` field** — clarify:

```json
"command": {
  "type": "string",
  "description": "Path to test binary. Required for C++/Catch2 projects."
}
```

### 4. Update MCP system prompt (Running Tests section)

Add framework guidance to the system prompt in `server.rs`:

```
### Framework Selection
- **Rust projects**: just provide `projectRoot` — Cargo.toml is auto-detected
- **C++/Catch2**: provide `command` (path to test binary)
- Do NOT pass `framework` unless auto-detection fails — it's usually unnecessary
```

### 5. Update tests

- Remove `test_adapter_detection_explicit_override` test (references generic)
- Remove generic adapter's own tests
- Add test: `detect_adapter` returns error when no adapter matches
- Add test: invalid `framework` name returns error

### 6. Clean up `_ => None` match arm

In `TestRunner::run()`, the progress function match has `_ => None` for non-cargo/catch2. This arm becomes unreachable — can be removed or kept as a compile-time guarantee.
