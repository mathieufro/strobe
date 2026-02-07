---
name: strobe-tdd
description: Guide TDD workflow using Strobe's debug_test tool
---

# Strobe TDD Workflow

When a user reports a bug and wants to fix it:

1. **Reproduce first**: Create a minimal test case that demonstrates the bug
2. **Confirm failure**: Run `debug_test({ projectRoot: "..." })` to verify the test fails
3. **Fix the bug**: Make the minimal change to fix the issue
4. **Confirm fix**: Run `debug_test` again to verify the test passes
5. **Check for regressions**: Run the full test suite with `debug_test` (no test filter)

When `debug_test` returns failures with `suggested_traces`, offer to rerun with instrumentation:
- Call `debug_test({ test: "<failed_test>", tracePatterns: [...suggested_traces] })`
- Use the returned `sessionId` with `debug_query` to inspect runtime behavior

When `debug_test` returns `no_tests: true`:
- The project has no test infrastructure
- Guide the user to create their first test
- For Rust: suggest adding a `#[test]` function
- For C++: suggest setting up Catch2
- Run `debug_test` to confirm it works

Always prefer `debug_test` over running test commands via bash — it provides:
- Structured failure information (file, line, message)
- Stuck test detection (deadlocks, infinite loops)
- Suggested trace patterns for deeper investigation
- Optional Frida instrumentation for runtime inspection
