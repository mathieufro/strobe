# Strobe

Runtime debugging for LLM agents. Launch a program, trace its functions, set breakpoints, read memory and query what actually executed, all over MCP and without recompiling.

```
curl -fsSL https://raw.githubusercontent.com/mathieufro/strobe/main/install.sh | bash
```

## Why

An agent that cannot see a program run has to guess from the source. It reads more files, adds print statements, re-runs the same test and reports the same result. Strobe gives it eyes: it instruments the live process with Frida and exposes the execution timeline through MCP tools, so the agent can find the wrong code path, the unregistered handler or the compile-time guard that no amount of reading reveals.

```
Claude Code --MCP--> strobe daemon --Frida--> your program
                          |
                     SQLite timeline
              (function calls, stdout, crashes)
```

The workflow it enables:

1. Launch the program. stdout and stderr are captured.
2. Read the output first. A crash or an assertion is often enough.
3. If not, add targeted traces on the live process.
4. Query the timeline to see what ran, with what arguments, returning what.
5. Set breakpoints, step, read memory, watch variables. No restart needed.

## Tools

| Tool | What it does |
|---|---|
| `debug_launch` | Spawn a process with Frida attached, capture stdout and stderr |
| `debug_session` | Status, stop, list retained sessions, delete |
| `debug_trace` | Add or remove function trace patterns and variable watches at runtime |
| `debug_query` | Search the execution timeline: functions, output, crashes |
| `debug_breakpoint` | Breakpoints and logpoints, with conditions |
| `debug_continue` | Resume, step over, step into, step out |
| `debug_memory` | Read and write process memory, poll variables over time |
| `debug_test` | Run tests inside Frida (Cargo, Catch2, pytest) with structured results |
| `debug_ui` | Accessibility tree plus AI vision for UI element detection |
| `debug_ui_action` | Click, type, set a value, press a key, scroll, drag |

### Trace patterns

```
foo::bar        exact function
foo::*          direct children of foo
foo::**         all descendants
*::validate     a named function, one level deep
@file:auth.cpp  every function from a source file
```

### Variable watches

Watch globals while specific functions run:

```json
{ "variable": "gTempo", "on": ["audio::process"] }
{ "address": "0x1234", "type": "f64", "label": "tempo" }
{ "expr": "ptr(0x5678).readU32()", "label": "custom" }
```

### Test runner

Tests run inside Frida, so traces can be added mid-test without restarting. Stuck detection catches a deadlock in about eight seconds.

```
debug_test({ projectRoot: "." })                 // everything
debug_test({ projectRoot: ".", test: "auth" })   // matching tests
```

Supports Cargo (Rust), Catch2 (C++), pytest and unittest (Python).

### UI observation (macOS)

The native accessibility tree merged with AI vision (OmniParser v2.0), so custom-drawn widgets get bounding boxes, labels and confidence scores next to the native ones.

```
debug_ui({ sessionId, mode: "both", vision: true })
```

### Active debugging

```
debug_breakpoint({ sessionId, add: [{ function: "parse", condition: "args[0] > 100" }] })
debug_continue({ sessionId, action: "step-over" })
debug_memory({ sessionId, targets: [{ variable: "gCounter" }] })
```

## Install

Prerequisites: macOS arm64 or x86_64 (Linux: tracing works, UI observation is macOS only), a Rust toolchain ([rustup.rs](https://rustup.rs)), Node.js 18 or later for the Frida agent.

Quick install clones the repo, builds from source, installs to `~/.strobe/` and configures MCP for Claude Code:

```bash
curl -fsSL https://raw.githubusercontent.com/mathieufro/strobe/main/install.sh | bash
```

Manual install:

```bash
git clone https://github.com/mathieufro/strobe.git
cd strobe
cd agent && npm install && npm run build && cd ..   # Frida agent first
cargo build --release                               # daemon
./target/release/strobe install                     # MCP configuration
```

On Linux, if bindgen fails with `fatal error: 'time.h' file not found`, pass the include paths explicitly:

```bash
BINDGEN_EXTRA_CLANG_ARGS="-I/usr/include -I/usr/include/$(uname -m)-linux-gnu" cargo build --release
```

Optional AI vision needs Python 3.10 to 3.12, PyTorch and the OmniParser v2.0 models (about 3.5 GB). `strobe setup-vision` creates the venv at `~/.strobe/vision-env/` and downloads the YOLO and Florence-2 models.

## Configuration

`~/.strobe/settings.json`, all keys optional, with `.strobe/settings.json` in a project taking precedence:

```json
{
  "events.maxPerSession": 200000,
  "vision.enabled": false,
  "vision.confidenceThreshold": 0.3,
  "vision.sidecarIdleTimeoutSeconds": 300
}
```

## Architecture

```
MCP client --stdio--> strobe mcp (proxy) --unix socket--> strobe daemon
                                                              |
                                              +---------------+---------------+
                                              |               |               |
                                        SessionManager    FridaWorker     SQLite DB
                                        (DWARF cache,     (spawn, attach,  (events,
                                         hook state)       agent inject)   sessions)
                                              |
                                        +-----+-----+
                                        |           |
                                    TestRunner  VisionSidecar
                                    (Cargo,     (Python,
                                     Catch2)     OmniParser)
```

- **Daemon.** One per user on `~/.strobe/strobe.sock`, started on the first MCP call, stopped after 30 minutes idle.
- **Frida agent.** TypeScript injected into the target. A CModule tracer makes native hooks 10 to 50 times faster than JavaScript hooks.
- **DWARF parser.** Compilation units parsed in parallel with rayon. Tells user code from library code and resolves variables.
- **Event store.** SQLite in WAL mode, a 200k-event FIFO per session, configurable up to 10M.

| Language | Tracing | Tests | Symbols |
|---|---|---|---|
| C | yes | Catch2 | DWARF |
| C++ | yes | Catch2 | DWARF, demangled |
| Rust | yes | Cargo | DWARF, demangled |
| Swift | yes | | DWARF |
| Python | yes (CPython 3.11+) | pytest, unittest | source level (`sys.settrace`) |

| Operation | Time |
|---|---|
| DWARF parse, 100k functions | 0.27 s |
| Spawn and attach | about 1 s |
| Trace overhead per call | 1 to 5 us (CModule) |
| Query over 200k events | under 10 ms |
| UI observation, accessibility only | under 50 ms |
| UI observation with vision | about 2 s |

## Layout

```
src/
  daemon/            server, session management, tool dispatch
  frida_collector/   Frida FFI, spawn and attach, agent injection
  dwarf/             DWARF parsing (gimli + rayon)
  mcp/               JSON-RPC protocol, stdio proxy
  db/                SQLite schema, event storage
  test/              test runner, adapters, stuck detection
  ui/                accessibility, screenshots, vision merge
agent/               TypeScript Frida agent (CModule tracer)
vision-sidecar/      Python OmniParser v2.0 wrapper
skills/              Claude Code debugging skills
```

## Related

- [atelier](https://github.com/mathieufro/atelier) and [atelier-cc](https://github.com/mathieufro/atelier-cc): autonomous coding pipelines whose implement and e2e stages run on Strobe.

## License

MIT
