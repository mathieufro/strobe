# Strobe

LLM-native debugging infrastructure. Launch programs, trace functions at runtime, observe execution — no recompilation needed.

```
curl -fsSL https://raw.githubusercontent.com/Primitive78/strobe/main/install.sh | bash
```

## What It Does

Strobe gives LLMs (and humans) runtime visibility into compiled programs through an MCP interface. Instead of print-debugging or reading code to guess what's happening, Strobe instruments live processes and lets you observe actual execution.

```
Claude Code ──MCP──> strobe daemon ──Frida──> your program
                         │
                    SQLite timeline
              (function calls, stdout, crashes)
```

**Core workflow:**
1. Launch a program — stdout/stderr captured automatically
2. Read output first — crashes and errors are often enough
3. Add targeted traces on the live process if needed
4. Query the execution timeline to understand what happened
5. Set breakpoints, read memory, watch variables — all without restarting

## Features

### MCP Tools

| Tool | What it does |
|------|-------------|
| `debug_launch` | Spawn process with Frida attached, capture stdout/stderr |
| `debug_session` | Get status, stop, list retained, or delete sessions |
| `debug_trace` | Add/remove function trace patterns and variable watches at runtime |
| `debug_query` | Search the execution timeline (functions, output, crashes) |
| `debug_breakpoint` | Set breakpoints and logpoints with conditions |
| `debug_continue` | Resume execution, step over/into/out |
| `debug_memory` | Read/write process memory, poll variables over time |
| `debug_test` | Run tests inside Frida (Cargo, Catch2), structured results |
| `debug_ui` | Query accessibility tree + AI vision for UI element detection |

### Trace Patterns

```
foo::bar       exact function
foo::*         direct children of foo
foo::**        all descendants
*::validate    named function, one level deep
@file:auth.cpp functions from a source file
```

### Variable Watches

Watch globals during specific function execution:
```json
{ "variable": "gTempo", "on": ["audio::process"] }
{ "address": "0x1234", "type": "f64", "label": "tempo" }
{ "expr": "ptr(0x5678).readU32()", "label": "custom" }
```

### Test Runner

Runs tests inside Frida — add traces mid-test without restarting. Smart stuck detection catches deadlocks in ~8 seconds.

```
debug_test({ projectRoot: "." })           // run all tests
debug_test({ projectRoot: ".", test: "auth" })  // run matching test
```

Supports **Cargo** (Rust) and **Catch2** (C++).

### UI Observation (macOS)

Combines native accessibility tree with AI-powered vision (OmniParser v2.0) to detect UI elements:

```
debug_ui({ sessionId, mode: "both", vision: true })
```

Returns merged tree: AX nodes for native widgets + vision-detected custom elements with bounding boxes, labels, and confidence scores.

### Active Debugging

Set breakpoints with conditions, step through code, read/write memory:

```
debug_breakpoint({ sessionId, add: [{ function: "parse", condition: "args[0] > 100" }] })
debug_continue({ sessionId, action: "step-over" })
debug_memory({ sessionId, targets: [{ variable: "gCounter" }] })
```

## Installation

### Prerequisites

- **macOS** arm64 or x86_64 (Linux: core tracing works, UI observation is macOS-only)
- **Rust** toolchain ([rustup.rs](https://rustup.rs))
- **Node.js** 18+ (for building the Frida agent)

### Quick Install

```bash
curl -fsSL https://raw.githubusercontent.com/Primitive78/strobe/main/install.sh | bash
```

This clones the repo, builds from source, installs to `~/.strobe/`, and configures MCP for Claude Code.

### Manual Install

```bash
git clone https://github.com/Primitive78/strobe.git
cd strobe

# Build agent (TypeScript, must be first)
cd agent && npm install && npm run build && cd ..

# Build daemon (Rust)
cargo build --release

# Configure MCP
./target/release/strobe install
```

### Vision Setup (Optional)

AI vision requires Python 3.10-3.12, PyTorch, and OmniParser v2.0 models (~3.5 GB total):

```bash
strobe setup-vision
```

This creates a Python venv at `~/.strobe/vision-env/`, installs ML dependencies, and downloads the fine-tuned YOLO + Florence-2 models.

## Configuration

Settings in `~/.strobe/settings.json` (all optional):

```json
{
  "events.maxPerSession": 200000,
  "vision.enabled": false,
  "vision.confidenceThreshold": 0.3,
  "vision.sidecarIdleTimeoutSeconds": 300
}
```

Project-level overrides in `.strobe/settings.json` take precedence.

## Architecture

```
MCP Client ──stdio──> strobe mcp (proxy) ──unix socket──> strobe daemon
                                                              │
                                              ┌───────────────┼───────────────┐
                                              │               │               │
                                        SessionManager    FridaWorker     SQLite DB
                                        (DWARF cache,     (spawn, attach,  (events,
                                         hook state)       agent inject)   sessions)
                                              │
                                        ┌─────┴─────┐
                                        │           │
                                    TestRunner  VisionSidecar
                                    (Cargo,     (Python,
                                     Catch2)     OmniParser)
```

- **Daemon**: Long-running process on `~/.strobe/strobe.sock`. One per user, auto-starts on first MCP call, shuts down after 30 min idle.
- **Frida Agent**: TypeScript injected into target process. CModule tracer for 10-50x faster native hooks.
- **DWARF Parser**: Parallel compilation unit parsing via rayon. Identifies user code, resolves variables.
- **Event Store**: SQLite WAL mode. 200k event FIFO buffer per session (configurable up to 10M).

## Language Support

| Language | Tracing | Tests | Debug Symbols |
|----------|---------|-------|---------------|
| C | Yes | Catch2 | DWARF |
| C++ | Yes | Catch2 | DWARF + demangling |
| Rust | Yes | Cargo | DWARF + demangling |
| Swift | Yes | — | DWARF |

## Performance

| Operation | Time |
|-----------|------|
| DWARF parse (100k functions) | 0.27s |
| Process spawn + attach | ~1s |
| Function trace overhead | ~1-5 us/call (CModule) |
| Event query (200k events) | <10ms |
| UI observation (AX only) | <50ms |
| UI observation (AX + vision) | ~2s |

## Project Structure

```
src/
  daemon/         Server, session management, tool dispatch
  frida_collector/ Frida FFI, process spawn/attach, agent injection
  dwarf/          DWARF symbol parsing (gimli + rayon)
  mcp/            JSON-RPC protocol, stdio proxy
  db/             SQLite schema, event storage
  test/           Test runner, adapters, stuck detection
  ui/             Accessibility, screenshot capture, vision merge
agent/            TypeScript Frida agent (CModule tracer)
vision-sidecar/   Python OmniParser v2.0 wrapper
skills/           Claude Code debugging skill
```

## License

MIT
