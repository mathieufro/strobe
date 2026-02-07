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
LLM: *launches app — no tracing needed, stdout/stderr captured automatically*
You: *click the button that causes the bug*
LLM: *reads stderr — sees ASAN crash at lv_obj_style.c:632*
LLM: *adds tracing on the suspicious module*
You: *click the button again*
LLM: *queries execution timeline, correlates traces with crash output*
LLM: "Found it - memory pool exhaustion in ViewManager::setView(). Here's the fix."
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
│  - Captures process stdout/stderr automatically              │
│  - Intercepts function calls (no code changes needed)        │
│  - Captures arguments, return values, timing                 │
│  - Stores execution history in SQLite                        │
│  - LLM adjusts tracing scope at runtime                      │
└─────────────────────────────────────────────────────────────┘
                           │
                           ▼ MCP (Model Context Protocol)
┌─────────────────────────────────────────────────────────────┐
│  LLM                                                         │
│  - debug_launch: start app (captures stdout/stderr)          │
│  - debug_query: read output, search traces                   │
│  - debug_trace: add patterns when you need deeper insight    │
│  - debug_stop: end session and clean up                      │
│  - debug_ui_tree: see the UI (Phase 4)                       │
│  - debug_ui_action: interact with the UI (Phase 5)           │
│  - debug_test_scenario: autonomous runtime tests (Phase 6)   │
└─────────────────────────────────────────────────────────────┘
```

## Key Capabilities

### Automatic Output Capture

Process stdout/stderr are captured automatically on every launch — no configuration needed. This alone is often enough to diagnose crashes: ASAN output, assertion messages, error logs all appear in the unified timeline. Start here before adding any trace patterns.

### Incremental Tracing (No Restart Required)

The LLM adjusts observation scope while your app runs. Start with zero trace patterns (just stdout/stderr). When output alone isn't enough, add targeted patterns for suspicious modules. Increase serialization depth for complex structs. Remove patterns when done. All without restarting. Uses glob syntax (`*` and `**`) familiar from shell and .gitignore.

### Crash Capture

When your app crashes, Strobe captures the stderr output (ASAN reports, stack traces, assertion messages) into the event timeline. The LLM reads the crash output, then adds targeted tracing to understand the root cause.

### Searchable Execution History

Query what happened, don't just observe current state. Find all null returns, slow functions, specific error patterns. Filter by thread, time range, function pattern. Process stdout/stderr are captured into the same timeline, so the LLM can correlate output with function calls. Pagination with metadata helps LLM narrow down large result sets.

### Watch Variables

Read global and static variable values at the exact moment traced functions execute. No manual logging or printf debugging. The LLM specifies which variables to watch and optionally which functions to watch them in:

```typescript
debug_trace({
  sessionId: "...",
  watches: {
    add: [
      { variable: "gCounter" },                        // Always captured
      { variable: "gTempo", on: ["audio::process"] },  // Only during audio::process
      { variable: "gClock->member", on: ["midi::*"] }  // Pointer dereferencing with wildcard
    ]
  }
})
```

Supports DWARF-based variable resolution (`gVar`, `gPtr->member`), raw memory addresses, and JavaScript expressions. Pattern matching with `on` field enables contextual filtering—capture a variable only during specific functions to reduce noise.

### Conditional Breakpoints

Pause only when it matters. Set conditions on field values, hit counts. The LLM sets a breakpoint, inspects state when it triggers, and continues - all programmatically.

### Test Instrumentation (TDD Workflow)

First-class support for test-driven debugging. Run full suite with minimal tracing for fast feedback. On failure, receive structured results with rule-based hints: suggested trace patterns extracted from stack traces, single-test rerun commands.

LLM reruns just the failing test with targeted tracing, queries the captured events, finds root cause. No more running full suite repeatedly. No more guessing what to trace.

## What Gets Captured

**Always:** Process stdout and stderr (captured at the Frida Device level, works with ASAN/sanitizer binaries).

**On demand:** Function enter/exit events when trace patterns are added. Patterns match demangled function names using glob syntax, or source files using `@file:` prefix.

**Not traced by default:** Nothing. Tracing is opt-in. Start with output capture, add patterns incrementally as needed.

The LLM can adjust tracing at runtime — broaden to include a dependency, narrow to focus on one module, or use `@usercode` to trace all project functions.

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

### Phase 1a: Tracing Foundation
- Daemon architecture (lazy start, Unix socket, 30min idle shutdown)
- Launch binary with Frida (Linux + macOS)
- DWARF parsing to identify user code
- Dynamic trace patterns (add/remove at runtime)
- Capture function enter/exit, arguments, return values
- Capture process stdout/stderr into unified event timeline
- Store in SQLite with FTS, query with summary/verbose modes
- MCP tools: `debug_launch`, `debug_trace`, `debug_query`, `debug_stop`

**Validation:** Launch binary, add targeted traces, query events (including stdout/stderr), find bug—no code changes to target.

### Phase 1b: Advanced Runtime Control
- Configurable serialization depth per pattern
- Multi-threading support (thread name, thread-aware queries)
- Hot function auto-detection with sampling
- Storage limits and retention policies

**Validation:** High-throughput functions auto-sample. Deep inspection when needed.

### Phase 1c: Crash & Multi-Process
- Crash capture (signal interception, stack, registers, locals)
- Fork/exec following with PID tagging
- Enhanced queries (time range, duration filters)

**Validation:** App crashes → LLM gets full context. App forks → events tracked across processes.

### Phase 1d: Test Instrumentation
- `debug_test` tool for TDD workflow
- Test adapter trait for extensibility
- cargo test adapter (Rust)
- Structured failures with rule-based trace hints
- Single-test rerun with targeted tracing

**Validation:** Test fails → rerun with suggested traces → find root cause. No full suite reruns.

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
- Unified UI tree: native accessibility (AXUIElement / AT-SPI2) + AI vision (YOLO + SigLIP for custom widgets)
- On-demand computation (~30-60ms), compact tree format with stable element IDs
- MCP tools: `debug_ui_tree`, `debug_ui_screenshot`

**Validation:** LLM sees native and custom-painted UI elements in one tree, describes the UI, correlates with traces.

### Phase 5: UI Interaction
- Intent-based actions: `click`, `set_value`, `type`, `select`, `drag`, `key`
- VLM-powered motor layer: classifies unknown widgets, learns interaction model, caches profiles
- MCP tools: `debug_ui_action`

**Validation:** LLM sets a JUCE knob value via intent — motor layer figures out the drag mechanics autonomously.

### Phase 6: I/O Channels + Scenario Runner
- Universal I/O channel abstraction: `InputChannel` / `OutputChannel` traits
- Wraps existing capabilities (stdout/stderr, traces, UI) as channels
- Scenario runner (`debug_test_scenario`): flat action list, on failure → process stays alive, LLM debugs
- MCP tools: `debug_channel_send`, `debug_channel_query`, `debug_test_scenario`

**Validation:** Autonomous synth test — UI knob + MIDI input + trace assertion in one scenario, no human in the loop.

### Phase 7: Concrete I/O Channels
- MIDI (CoreMIDI / ALSA), Audio (CoreAudio tap / JACK), Network (Frida socket intercept), File (FSEvents / inotify)
- Each implements channel traits — automatically works with scenario runner

### Future Phases
- **Phase 8: Advanced Threading Tools** - Lock tracing, deadlock detection, race condition hints
- **Phase 9: Additional Languages & Runtimes** - JavaScript/TypeScript (CDP), Python, Go, Java/Kotlin
- **Phase 10+:** Windows support, distributed tracing
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

## Setup

### macOS

Strobe uses Frida for dynamic instrumentation, which requires `task_for_pid` permissions. On macOS, you need to enable Developer Mode:

```bash
sudo DevToolsSecurity -enable
```

This is a one-time setup that allows debugging tools to attach to processes. You'll be prompted for your password.

Additionally, binaries must be signed with the `get-task-allow` entitlement to be debugged. Debug builds typically have this, but if you encounter issues, you can sign manually:

```bash
# Create entitlements file
cat > debug.entitlements << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.get-task-allow</key>
    <true/>
</dict>
</plist>
EOF

# Sign your binary
codesign -f -s - --entitlements debug.entitlements /path/to/your/binary
```

### Linux

No special setup required. Frida works out of the box on Linux.

### Debug Symbols

Strobe requires debug symbols (DWARF) to identify functions and source locations. Build your code with debug info:

```bash
# C/C++ with clang/gcc
clang -g -o myapp myapp.c

# Rust (debug builds include symbols by default)
cargo build

# macOS: Generate .dSYM bundle for release builds
dsymutil /path/to/binary
```

## Extensibility

The architecture is designed so **anyone can add support for new languages, I/O channels, or platform backends** without understanding the whole system.

Three extension points:
- **Collectors** (`Collector` trait) - Add language support
- **I/O Channels** (`InputChannel`/`OutputChannel` traits) - Add app I/O (MIDI, serial, custom protocols)
- **Platform Backends** (`UIObserver`, `UIInput`, `VisionPipeline` traits) - Add OS support

All emit to unified schemas. Contributors don't touch storage, queries, MCP, scenario runner, or VS Code.

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
