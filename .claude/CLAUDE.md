# Strobe

Frida-based dynamic instrumentation tool with an MCP interface for LLMs. Launch programs, trace functions at runtime, query execution history — no recompilation needed.

## Architecture

```
MCP Client → stdio proxy (strobe mcp) → Unix socket → Daemon (strobe daemon)
                                                          ├── SessionManager (DWARF cache, hook tracking)
                                                          ├── FridaSpawner (process spawn, agent injection)
                                                          ├── SQLite (events, sessions)
                                                          └── TestRunner (cargo/catch2 adapters)
```

- **Daemon** (`src/daemon/`): Long-running process on `~/.strobe/strobe.sock`. Exclusive flock, signal handling, idle timeout (30min). One per user.
- **MCP layer** (`src/mcp/`): JSON-RPC 2.0 protocol, tool definitions in `types.rs`, proxy auto-launches daemon.
- **Frida collector** (`src/frida_collector/`): Uses `frida-sys` directly (not `frida-rs` wrappers) due to Script type confusion bug. Unsafe FFI for session/device raw pointer extraction.
- **Agent** (`agent/src/`): TypeScript injected into target process. CModule tracer for 10-50x faster native hooks. Handles ASLR slide computation.
- **DWARF** (`src/dwarf/`): Parallel CU parsing via rayon, lazy struct member resolution. Cached per binary.
- **Test** (`src/test/`): Framework auto-detection via adapter pattern (Cargo 90, Catch2 85, Generic 1). Smart stuck detector with multi-signal analysis.
- **DB** (`src/db/`): SQLite WAL mode. 200k event FIFO buffer per session (configurable via `STROBE_MAX_EVENTS_PER_SESSION`).

## Building

```bash
# Agent (must be built first — Rust embeds dist/agent.js via include_str!)
cd agent && npm install && npm run build && cd ..

# Daemon
cargo build --release

# IMPORTANT: After modifying agent TypeScript, touch the Rust file that embeds it
# because Cargo doesn't track include_str! dependencies:
touch src/frida_collector/spawner.rs
```

## Testing

Use `debug_test` MCP tool for Strobe's own test framework — never run test binaries via bash.

## Key Source Files

| File | Purpose |
|------|---------|
| `src/daemon/server.rs` | MCP tool dispatch, connection handling, daemon lifecycle (~1800 lines) |
| `src/daemon/session_manager.rs` | Session CRUD, DWARF cache, hook/watch state |
| `src/frida_collector/spawner.rs` | Frida FFI, process spawn/attach, agent script management |
| `src/dwarf/parser.rs` | DWARF symbol parsing, function/variable extraction |
| `src/mcp/types.rs` | MCP request/response types, validation constants |
| `src/test/mod.rs` | Test orchestration, async test runs |
| `src/test/stuck_detector.rs` | Deadlock/hang detection via CPU sampling + stack comparison |
| `src/symbols/python_resolver.rs` | Python symbol resolution (AST-based via rustpython-parser) |
| `src/symbols/js_resolver.rs` | JS/TS symbol resolution (regex + source maps) |
| `agent/src/agent.ts` | Main Frida agent — runtime detection, tracer dispatch |
| `agent/src/cmodule-tracer.ts` | High-perf native CModule tracing callbacks |
| `agent/src/tracers/v8-tracer.ts` | Node.js tracer (Module._compile + Proxy wrapping) |
| `agent/src/tracers/jsc-tracer.ts` | Bun tracer (JSObjectCallAsFunction hook) |
| `agent/src/tracers/python-tracer.ts` | Python tracer (sys.settrace via CPython API) |

## Development Patterns

- **Error handling**: Custom `Error` enum in `src/error.rs` via `thiserror`. Propagate with `?`. User-facing errors include actionable guidance (e.g., `NoDebugSymbols` tells user to recompile with `-g`).
- **Async**: Tokio runtime, `Arc<RwLock<T>>` for shared state, `tokio::sync::Notify` for signaling.
- **Frida FFI**: All frida-sys usage is in `spawner.rs`. Session/Device have single non-ZST field so raw ptr extraction from offset 0 is safe.
- **Pattern matching**: `*` stops at `::` (shallow), `**` crosses `::` (deep), `@file:name` matches source file substring.
- **Event limits**: Default 200k events/session. Broad patterns like `juce::*` can generate millions of events — always start narrow.

## Commit Conventions

```
<type>: <description>

Types: feat, fix, docs, refactor, perf, test
```

## Gotchas

- **frida-rs 0.17 Script bug**: Don't use `frida::Script` — use `frida-sys` FFI directly (see `spawner.rs`).
- **include_str! caching**: `touch src/frida_collector/spawner.rs` after agent rebuild.
- **ASAN + Frida**: Agent `write(2)` hook fails silently on ASAN binaries. Device-level output capture is the fallback. Never hook `operator new/delete` in ASAN builds (SEGV).
- **Rust dSYM**: macOS Rust builds don't auto-create `.dSYM` — run `dsymutil <binary>` after build.
- **Frida 17.x API**: `Module.getExportByName()` static removed — use `Process.getModuleByName(lib).getExportByName(sym)`.
- **TinyCC (CModule)**: `g_atomic_int_get()` unavailable — use `g_atomic_int_add(&var, 0)`.
- **Hook limits**: Target <50 hooks. 100+ risks crashes. Hard cap 100 per `debug_trace` call.
