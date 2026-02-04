# Strobe

**LLM-Native Debugging Infrastructure**

Strobe gives LLMs complete visibility into program execution. No code changes, no log statements, no recompilation. The LLM controls what to observe, when, and how deep.

## The Problem

LLMs can read your code. They can't watch it run.

Current debugging workflow with an LLM:
```
LLM: "Add a log at line 47"
You: *edit, recompile, run*
LLM: "Hmm, add another log at line 92"
You: *edit, recompile, run*
LLM: "Now add one at line 156"
You: *edit, recompile, run*
... repeat until you hate everything ...
```

Strobe workflow:
```
LLM: *launches app with tracing*
You: *click the button that causes the bug*
LLM: *queries execution history, finds suspicious function*
LLM: *adds deeper tracing on that module*
You: *click the button again*
LLM: "Found it - null pointer in buffer.data at render.rs:247. Here's the fix."
```

**No recompilation. No code changes. No manual log archaeology.**

## How It Works

```
┌─────────────────────────────────────────────────────────────┐
│  Your App (running normally)                                 │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼ Frida (dynamic instrumentation)
┌─────────────────────────────────────────────────────────────┐
│  Strobe Daemon                                               │
│  - Intercepts function calls (no code changes needed)        │
│  - Captures arguments, return values, timing                 │
│  - Stores execution history in SQLite                        │
│  - LLM adjusts tracing scope at runtime                      │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼ MCP (Model Context Protocol)
┌─────────────────────────────────────────────────────────────┐
│  LLM                                                         │
│  - debug_launch: start app with tracing                      │
│  - debug_query: search execution history                     │
│  - debug_trace: adjust what gets captured (live!)            │
│  - debug_breakpoint: pause on conditions                     │
│  - debug_inspect: examine state                              │
└─────────────────────────────────────────────────────────────┘
```

## Key Capabilities

### Dynamic Tracing (No Restart Required)

The LLM adjusts observation scope while your app runs. Add trace patterns for suspicious modules, increase serialization depth for complex structs, remove patterns when done - all without restarting. Uses glob syntax (`*` and `**`) familiar from shell and .gitignore.

Traditional debuggers require restart to change what you observe. Strobe doesn't.

### Crash Capture

When your app crashes, Strobe intercepts the signal and captures:
- Stack trace at crash point
- Register state
- Local variables in the crashing frame
- Last N events leading to the crash

The LLM gets a "black box recording" of exactly what happened.

### Searchable Execution History

Query what happened, don't just observe current state. Find all null returns, slow functions, specific error patterns. Filter by thread, time range, function pattern. Pagination with metadata helps LLM narrow down large result sets.

### Conditional Breakpoints

Pause only when it matters. Set conditions on field values, hit counts. The LLM sets a breakpoint, inspects state when it triggers, and continues - all programmatically.

### Test Instrumentation (TDD Workflow)

First-class support for test-driven debugging. Run full suite with minimal tracing for fast feedback. On failure, receive structured results with rule-based hints: suggested trace patterns extracted from stack traces, single-test rerun commands.

LLM reruns just the failing test with targeted tracing, queries the captured events, finds root cause. No more running full suite repeatedly. No more guessing what to trace.

## What Gets Traced

**By default:** All functions in your code (source files in project directory).

**Not traced:** Standard library, system calls, third-party dependencies.

This heuristic uses debug info (DWARF/PDB) to determine source file location. Functions from files outside your project are skipped.

The LLM can adjust this at runtime - broaden to include a dependency, narrow to focus on one module.

## Target Use Case

Strobe is for **developers debugging their own code during development**.

- You control the build (debug symbols available)
- You're in a dev environment (no code signing restrictions)
- You can reproduce the bug (or Strobe catches it on first occurrence)

This is not for:
- Reverse engineering release builds
- Production debugging (yet)
- Debugging code you don't have source for

## Development Phases

### Phase 1: Passive Tracing + Test Instrumentation
- Launch binary with Frida tracing (Linux + macOS)
- Capture function enter/exit, arguments, return values
- Full multi-threading support (thread ID, name, ordering)
- Auto-follow fork/exec with PID tagging
- Store in SQLite with auto-retention
- **Test instrumentation with smart hints** - run tests, get structured failures with suggested trace patterns
- Crash capture with state snapshot
- Hot function auto-detection with sampling
- MCP tools: `debug_launch`, `debug_query`, `debug_trace`, `debug_test`, `debug_stop`

**Validation A:** Debug a real bug by running once, querying history, adjusting traces, re-triggering. No recompilation.

**Validation B:** Run test suite, test fails, LLM reruns single test with suggested tracing, finds root cause. No full suite reruns.

### Phase 2: Active Debugging
- Conditional breakpoints
- State inspection when paused
- Resume execution
- MCP tools: `debug_breakpoint`, `debug_inspect`, `debug_continue`

**Validation:** Set a breakpoint on a condition, inspect variables when it hits, find a bug you couldn't find with traces alone.

### Phase 3: VS Code Integration
- Extension manages daemon lifecycle
- Debug panel shows breakpoints, call stack, variables
- Execution history viewer
- One-click install from marketplace

**Validation:** Same debugging power, but humans can see what the LLM sees.

### Phase 4: UI Observation
- Screenshots on demand
- Accessibility tree for structured UI state
- MCP tools: `debug_ui_state`

**Validation:** LLM can see the current state of a GUI app and correlate visual state with execution traces.

### Phase 5: UI Interaction
- Click, type, scroll, drag
- Target elements by accessible name or coordinates
- MCP tools: `debug_ui_action`

**Validation:** LLM can autonomously reproduce a bug by navigating the UI, without human assistance.

### Future Phases
- **Phase 6: Advanced Threading Tools** - Lock tracing, deadlock detection, race condition hints
- **Phase 7: Smart Test Integration** - Language-specific test setup skills, framework adapters (Google Test, Catch2, pytest, Jest)
- **Phase 8: JavaScript/TypeScript** - Chrome DevTools Protocol collector for Node.js, browser apps, Electron
- **Phase 9+:** Additional languages (Python, Go), Windows support, distributed tracing
- **Commercial features:** CI/CD integration, auto-test generation, regression detection

## Architecture

Built on [Frida](https://frida.re/) for dynamic binary instrumentation. Frida can intercept any function in a running process without code changes.

**Key characteristics:**
- **Global daemon** - Single daemon per user, auto-starts on first use, auto-shuts down after idle
- **MCP transport** - stdio proxy to persistent daemon (maximum MCP client compatibility)
- **Storage** - SQLite with auto-retention (7-day purge, 10GB limit)
- **Multi-process** - Automatically follows fork/exec, tags events with PID
- **Multi-threaded** - Thread ID and name on every event, thread-aware queries
- **Symbol demangling** - Full C++/Rust demangling, raw names also available

Future phases add [Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/) for JavaScript/TypeScript debugging.

See [ARCHITECTURE.md](ARCHITECTURE.md) for technical details.

## Extensibility

The architecture is designed so **anyone can add support for obscure languages or test frameworks** without understanding the whole system.

Two extension points:
- **Collectors** (`Collector` trait) - Add language support
- **Test Adapters** (`TestAdapter` trait) - Add test framework support

Both emit to unified schemas. Contributors don't touch storage, queries, MCP, or VS Code.

See [FEATURES.md](FEATURES.md#contributor-extensibility) for details.

## Project Structure

```
strobe/
├── core/                    # Rust daemon + MCP server
├── frida-scripts/           # TypeScript injection scripts
├── vscode-extension/        # VS Code integration (Phase 3)
└── docs/                    # Documentation
```

## License

MIT License (our code) + LGPL-2.0 with wxWindows exception (Frida components)
