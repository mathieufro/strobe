---
name: runtime-tracing-gotchas
description: "Non-obvious facts about tracing Bun, Node ESM and Python targets with Frida"
metadata:
  type: reference
---

- Bun release binaries strip all JSC symbols (only `__mh_execute_header` is exported), so JSC function tracing is impossible. Hooks report `installed=0` and the agent falls back to NativeTracer, which crashes on interpreted targets.
- Bun needs re-signing with the `get-task-allow` entitlement before Frida can attach on macOS.
- Frida 17.x removed the static `Module.getExportByName()`; use `Process.getModuleByName(lib).getExportByName(sym)`.
- Python 3.12+ tracing uses `sys.monitoring` (PEP 669); older versions use `sys.settrace`. coverage.py / pytest-cov can steal a `sys.monitoring` tool ID, so the tracer retries IDs 0-5.
- Node ESM: only output capture works; function tracing via patterns is not connected to `registerHooks`. Bun E2E likewise covers output capture only.
- The agent TypeScript is embedded with `include_str!` in `src/frida_collector/spawner.rs`; `agent/dist/` is gitignored. After changing the agent: `cd agent && npm run build`, then `touch src/frida_collector/spawner.rs` so cargo re-embeds it.
