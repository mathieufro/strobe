---
name: strobe-test-setup
description: Set up and run tests with Strobe's debug_test — framework detection, monorepo support, level filtering, and per-language guidance
---

# Test Infrastructure with Strobe

Strobe's `debug_test` tool auto-detects frameworks, spawns tests inside Frida, streams live progress, and produces structured results with failure details. **Never run test binaries via bash** — always use `debug_test`.

---

## Quick Start

```
debug_test({ projectRoot: "/path/to/project" })
```

That's it. Strobe detects the framework, builds the command, spawns via Frida, and streams results. Poll with `action: "status"` for live progress, or wait for completion.

### Core Parameters

| Parameter | Purpose |
|-----------|---------|
| `projectRoot` | Project root — required for framework detection |
| `test` | Single test: name pattern (`"should validate"`) or file path (`"src/auth.test.ts"`) |
| `level` | `"unit"`, `"integration"`, `"e2e"` — filters test scope |
| `framework` | Override auto-detection: `"cargo"`, `"vitest"`, `"bun"`, `"playwright"`, etc. |
| `command` | Path to test binary (C++/Catch2/GTest only) |

### Polling Pattern

```
// Start
debug_test({ projectRoot: "/project" })
→ { testRunId: "test-abc123", status: "running" }

// Poll (repeat until status: "completed")
debug_test({ action: "status", testRunId: "test-abc123" })
→ { progress: { passed: 42, failed: 1, phase: "running" }, status: "running" }

// Final results include failures with file:line and full stdout/stderr in details file
→ { result: { summary: { passed: 99, failed: 1 }, failures: [...] }, status: "completed" }
```

---

## Framework Reference

### Rust — `cargo` (confidence: 90)

**Detection:** `Cargo.toml` exists.

```
debug_test({ projectRoot: "/my-rust-project" })
debug_test({ projectRoot: "/my-rust-project", test: "test_parse_config" })
```

- Uses `cargo test` with JSON output (`-Zunstable-options --format json`)
- Single test: `test` is a substring filter on test function names
- Integration test binaries: if `tests/<name>.rs` exists, uses `--test <name>` to avoid recompiling all targets

**Gotcha:** macOS Rust builds don't auto-create `.dSYM` — run `dsymutil <binary>` if you need debug symbols for tracing.

---

### C++ / Catch2 (confidence: 85)

**Detection:** Requires `command` parameter pointing to a compiled test binary.

```
debug_test({ command: "./build/tests", projectRoot: "/my-cpp-project" })
debug_test({ command: "./build/tests", projectRoot: "/my-cpp-project", test: "Vector operations" })
```

- Uses Catch2 XML reporter output
- Level filtering via Catch2 tags: `[unit]`, `[integration]`, `[e2e]`
- Single test: name pattern with wildcard support (`*search*`)

**Setup required:** Compile with debug symbols (`-g`). Strobe reads DWARF for function tracing.

---

### C++ / GTest (confidence: 85)

**Detection:** Requires `command` parameter; binary must be linked with GoogleTest.

```
debug_test({ command: "./build/test_suite", projectRoot: "/my-cpp-project" })
debug_test({ command: "./build/test_suite", projectRoot: "/my-cpp-project", test: "MyTest.HandlesEdgeCase" })
```

- Uses `--gtest_output=json` for structured results
- Single test: `--gtest_filter=<pattern>` (supports `*` wildcards)

---

### Python / pytest (confidence: 90)

**Detection:** `pyproject.toml` with `[tool.pytest]`, or `pytest.ini`, `setup.cfg` with pytest config, or `conftest.py`.

```
debug_test({ projectRoot: "/my-python-project" })
debug_test({ projectRoot: "/my-python-project", test: "test_login_flow" })
debug_test({ projectRoot: "/my-python-project", level: "e2e" })
```

- Uses `pytest --json-report` for structured results
- Auto-detects `uv` — uses `uv run pytest` when `uv.lock` exists
- Level filtering: `-m unit`, `-m integration`, `-m e2e` (requires pytest markers in test code)
- Single test: `-k <pattern>` substring match

**Markers for levels:** Tests need `@pytest.mark.unit`, `@pytest.mark.integration`, `@pytest.mark.e2e` decorators for level filtering to work.

---

### Python / unittest (confidence: 70)

**Detection:** Falls back when pytest is not configured but Python test files exist.

```
debug_test({ projectRoot: "/my-python-project", framework: "unittest" })
debug_test({ projectRoot: "/my-python-project", framework: "unittest", test: "test_module.TestClass.test_method" })
```

- Uses `python3 -m unittest discover -v`
- Single test: full dotted path (`test_module.TestClass.test_method`)

---

### JavaScript / Vitest (confidence: 95)

**Detection:** `vitest.config.ts/js` or `vite.config.ts/js` with vitest in `package.json`.

```
debug_test({ projectRoot: "/my-js-project" })
debug_test({ projectRoot: "/my-js-project", test: "should validate email" })
```

- Uses JSON reporter + custom Strobe stderr reporter for live progress
- Forces `--pool=threads` (fork/exec deadlocks with Frida's spawn gating)
- Custom command support: `debug_test({ command: "npm run test", projectRoot: "..." })`

**Gotcha:** Vitest's `--pool=forks` is incompatible with Frida. Strobe forces `--pool=threads`.

---

### JavaScript / Jest (confidence: 92)

**Detection:** `jest.config.js/ts/cjs/mjs` or `"jest"` in `package.json`.

```
debug_test({ projectRoot: "/my-js-project" })
debug_test({ projectRoot: "/my-js-project", test: "handles edge case" })
```

- Uses `npx jest --json` for structured results
- Single test: `-t <pattern>` name filter

---

### JavaScript / Mocha (confidence: 90)

**Detection:** `.mocharc.yml/json/js/cjs` or `"mocha"` in `package.json` (when vitest/jest not present).

```
debug_test({ projectRoot: "/my-js-project" })
debug_test({ projectRoot: "/my-js-project", test: "connects to database" })
```

- Uses `npx mocha --reporter json`
- Single test: `--grep <pattern>` regex filter

---

### TypeScript / Bun (confidence: 85–90)

**Detection:** `bunfig.toml` (90), `bun.lock`/`bun.lockb` (85), or `"bun test"` in package.json scripts.

```
debug_test({ projectRoot: "/my-bun-project" })
debug_test({ projectRoot: "/my-bun-project", test: "should validate token" })
debug_test({ projectRoot: "/my-bun-project", test: "src/auth.test.ts" })
debug_test({ projectRoot: "/my-bun-project", level: "unit" })
```

- Parses Bun's native stderr output — both `✓`/`✗` (older) and `(pass)`/`(fail)` (v1.3+) markers
- File paths passed directly (`bun test src/auth.test.ts`); name patterns use `--test-name-pattern`
- Level support via test orchestrator scripts (`scripts/test-run.ts` with SUITES object)

**Monorepo support:** Scans workspaces for `bunfig.toml`. Sets `cwd` to the workspace dir. Strips `DATABASE_URL` when `.env.test` exists (Bun auto-loads it).

**Monorepo setup checklist:**
1. Root `package.json` has `"workspaces": ["apps/*", "packages/*"]`
2. Test workspace has `bunfig.toml` (e.g., `apps/api/bunfig.toml`)
3. Optional: `scripts/test-run.ts` with `SUITES` object mapping level names to `{ cmd, cwd }` entries
4. Optional: `.env.test` in workspace for test-specific database URL

---

### TypeScript / Deno (confidence: 90)

**Detection:** `deno.json` or `deno.jsonc` exists.

```
debug_test({ projectRoot: "/my-deno-project" })
debug_test({ projectRoot: "/my-deno-project", test: "handles timeout" })
```

- Uses `deno test --reporter=junit`
- Single test: `--filter=<pattern>`

---

### Go (confidence: 90)

**Detection:** `go.mod` exists.

```
debug_test({ projectRoot: "/my-go-project" })
debug_test({ projectRoot: "/my-go-project", test: "TestParseConfig" })
```

- Uses `go test -v -json ./...`
- Single test: `-run ^TestName$` (regex-escaped exact match)

---

### Playwright (confidence: 80–95)

**Detection:** `playwright.config.ts/js/mts`. Returns 80 when vitest/jest also present (explicit `framework: "playwright"` recommended).

```
debug_test({ projectRoot: "/my-project", framework: "playwright" })
debug_test({ projectRoot: "/my-project", framework: "playwright", test: "login flow" })
```

- E2E only — rejects `level: "unit"` and `level: "integration"`
- Uses custom Strobe reporter for live progress (file-based, handles Bun exec-replacement)
- JUnit XML output for final results, falls back to progress file events

**Monorepo support:** Scans workspaces for `playwright.config.ts`. Sets `cwd` to that workspace (e.g., `apps/web/`).

**Setup checklist:**
1. `playwright.config.ts` in the project or workspace dir
2. For Strobe's custom reporter: config should check for `/tmp/.strobe-playwright-reporter.mjs` and register it
3. In monorepos: Strobe auto-detects the workspace — no extra configuration needed

---

## Monorepo Detection (General)

Strobe reads `"workspaces"` from root `package.json` and expands glob patterns (`"apps/*"` → list subdirs). Each adapter then looks for its own marker files within workspaces:

| Framework | Workspace Marker |
|-----------|-----------------|
| Bun | `bunfig.toml` |
| Playwright | `playwright.config.ts/js/mts` |
| Vitest | `vitest.config.ts/js` or `vite.config.ts/js` |

When a workspace is found, `cwd` is set to that directory automatically. The test binary runs from there, finding local `node_modules`, config files, and `.env.test`.

---

## Test Levels

Use `level` to filter test scope. How levels map depends on the framework:

| Framework | Unit | Integration | E2E |
|-----------|------|-------------|-----|
| Cargo | `--` filter | `--` filter | `--` filter |
| Catch2 | `[unit]` tag | `[integration]` tag | `[e2e]` tag |
| pytest | `-m unit` | `-m integration` | `-m e2e` |
| Bun | Orchestrator dirs or `test:unit` script | Orchestrator dirs or `test:integration` script | Orchestrator dirs or `test:e2e` script |
| Playwright | ❌ (E2E only) | ❌ (E2E only) | Default |
| Go/Deno/Jest/Vitest/Mocha | Not yet mapped | Not yet mapped | Not yet mapped |

---

## Troubleshooting

### "No tests found"
- Check `projectRoot` points to the directory with `Cargo.toml`, `package.json`, `go.mod`, etc.
- For C++: `command` parameter is required — Strobe can't auto-detect compiled binaries
- For monorepos: ensure the framework's marker file exists in a workspace

### "FRIDA_ATTACH_FAILED"
- macOS: Bun needs re-signing with `get-task-allow` entitlement for Frida attach
- Check if the binary is ASAN-instrumented (known Frida conflict)

### Framework detected wrong
- Use `framework` parameter to override: `debug_test({ framework: "bun", projectRoot: "..." })`
- In monorepos with mixed frameworks (vitest + bun), explicit `framework` is often needed

### Progress stays at 0/0/0
- Some frameworks only report results at the end (JUnit-based: Deno, Catch2)
- Bun and Vitest stream per-test progress; Playwright uses a file-based reporter
- If truly stuck: check `action: "status"` for stuck warnings and test stall diagnostics
