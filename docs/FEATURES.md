# Features by Phase

Each phase builds on the previous. Each has a clear validation criteria: "What can I do now that I couldn't before?"

---

## Phase 1a: Tracing Foundation

**Goal:** Prove the core concept works. LLM can launch a program, add targeted traces, observe execution, and query what happened.

### Features

#### Daemon Architecture
- Single global daemon per user
- Lazy start on first MCP call
- Unix socket at `~/.strobe/strobe.sock`
- Auto-shutdown after 30 minutes idle

#### Launch Process
- Spawns process via Frida
- Reads DWARF debug info to identify user code
- Returns human-readable session ID

#### Dynamic Trace Patterns
- Add/remove patterns at runtime via `debug_trace`
- Glob syntax: `*` matches within module, `**` matches across
- Special pattern `@usercode` for all project functions
- Hooks injected live, no restart required

#### Basic Event Capture
- Function enter events (name, arguments)
- Function exit events (return value, duration)
- Nanosecond timestamps for ordering
- Parent event tracking for call hierarchy

#### Process Output Capture
- stdout/stderr captured automatically via `write(2)` syscall interception
- Output events interleaved chronologically in the unified event timeline
- Re-entrancy guard prevents infinite recursion from Frida's own write calls
- Per-session 50MB output capture limit with truncation indicator
- Large writes (>1MB) emit a truncation notice instead of buffering
- Queryable via `debug_query` with `eventType: "stdout"` or `"stderr"`

#### Serialization (Fixed)
- Primitives serialized directly
- Structs serialized to depth 1
- Arrays truncated to first 100 elements
- Strings truncated at 1KB
- Pointers as hex address

#### Storage
- SQLite with WAL mode
- Events table with indexes for common queries
- FTS5 for function name search

#### Query Execution History
- Search by function name (equals, contains, regex)
- Search by source file
- Filter by return value (equals, isNull)
- Pagination (default limit: 50)
- Summary mode (default) vs verbose mode

#### Stop Session
- Detaches Frida cleanly
- Deletes session data
- Session stays queryable after process exits until stop

#### MCP Tools
- `debug_launch` - Launch binary with Frida (applies pending patterns, captures stdout/stderr)
- `debug_trace` - Add/remove trace patterns (call before launch to set pending, or with sessionId for live)
- `debug_query` - Query unified timeline (function traces + stdout/stderr, chronologically ordered)
- `debug_stop` - End session and cleanup

### What Gets Captured (Phase 1a)

| Data | Captured | Notes |
|------|----------|-------|
| Function name | Yes | Demangled (raw name also available) |
| Source file + line | Yes | Via DWARF |
| Arguments | Yes | JSON serialized, depth 1 |
| Return value | Yes | JSON serialized |
| Duration | Yes | Nanosecond precision |
| Timestamp | Yes | Nanoseconds since session start |
| Thread ID | Yes | Basic support |
| Call hierarchy | Yes | Parent event tracking |
| Process stdout | Yes | Via write(2) interception |
| Process stderr | Yes | Via write(2) interception |

### Platform Support (Phase 1a)

| Platform | Status |
|----------|--------|
| Linux (x86_64) | Supported |
| macOS (arm64, x86_64) | Supported |
| Windows | Future phase |

### Language Support (Phase 1a)

| Language | Status | Debug Info |
|----------|--------|------------|
| C | Supported | DWARF |
| C++ | Supported | DWARF + demangling |
| Rust | Supported | DWARF + demangling |

### Error Handling

| Error | LLM Action |
|-------|------------|
| `NO_DEBUG_SYMBOLS` | Ask user to rebuild with `-g` |
| `SIP_BLOCKED` | Offer: copy to /tmp, codesign, or disable SIP |
| `SESSION_EXISTS` | Call `debug_stop` first |

### Validation Criteria

**Scenario: Targeted tracing workflow**
1. LLM calls `debug_launch` with no tracing
2. User reports bug: "crashes when I click submit"
3. LLM calls `debug_trace({ add: ["submit::*", "form::validate"] })`
4. User reproduces bug
5. LLM calls `debug_query` to find suspicious return values
6. LLM queries again with `verbose: true` for full arguments
7. LLM identifies root cause

**Success:** LLM can observe what functions were called, with what arguments, and what they returned—without any code changes to the target. Tracing is targeted, not "trace everything".

---

## Phase 1b: Advanced Runtime Control

**Goal:** Production-ready tracing with performance safeguards and deeper inspection.

### Features

#### Configurable Serialization Depth
- Adjust depth per trace pattern via `debug_trace`
- `depth: 2` for nested struct inspection
- Cycle detection with `<circular ref>` markers
- Per-pattern depth overrides

#### Multi-Threading Support
- Thread name capture (when available)
- Thread-aware queries (filter by thread)
- Order by thread-then-timestamp for per-thread analysis

#### Hot Function Handling
- Auto-detect functions called >100k/sec
- Auto-sample to 1% (configurable)
- Sampling indicator in query results
- LLM can disable sampling or narrow patterns

#### Storage Management
- Configurable retention (default: delete on stop)
- Optional retain for later analysis (`debug_stop({ retain: true })`)
- Auto-purge retained sessions after 7 days
- Hard limit: 10GB total, oldest purged first

#### Enhanced debug_trace
- `depth` parameter for serialization depth
- Returns sampling warnings if active

### Validation Criteria

**Scenario: Deep inspection with safeguards**
1. LLM launches app, adds trace on `process_data::*`
2. Function called 500k times/sec — auto-sampling kicks in
3. LLM receives warning: "sampling at 1%"
4. LLM narrows pattern to `process_data::validate` only
5. Full capture resumes
6. LLM requests `depth: 2` for nested config struct
7. LLM finds bug in nested field

**Success:** High-throughput functions don't crash the system. LLM can inspect deeper when needed.

---

## Phase 1c: Crash & Multi-Process

**Goal:** Handle crashes gracefully and track execution across fork/exec.

### Features

#### Crash Capture
When app crashes (SIGSEGV, SIGABRT, etc.), Frida intercepts before termination:
- Signal type and faulting address
- Stack trace at crash point
- Register state
- Local variables in crashing frame (via DWARF)
- Last N events leading to crash

Query with `eventType: "crash"` to retrieve full crash context.

#### Fork/Exec Following
- Automatically attach to child processes
- Tag events with process ID
- Unified view across all spawned processes
- Session includes all PIDs

#### Enhanced Queries
- Time range filtering (`-5s`, absolute timestamps)
- Duration filtering (find slow functions)
- Process ID filtering
- Combined filters

### Validation Criteria

**Scenario A: Crash debugging**
1. LLM launches app with tracing
2. User triggers a crash (null pointer, etc.)
3. Frida intercepts signal, captures state
4. LLM queries `eventType: "crash"`
5. LLM sees stack trace, registers, locals, and events leading to crash
6. LLM identifies root cause

**Scenario B: Multi-process tracking**
1. LLM launches app that forks worker processes
2. Events captured from parent and all children
3. LLM queries with PID filter to focus on specific process
4. LLM correlates events across processes

**Success:** Crashes don't lose information. Fork/exec doesn't break tracing.

---

## Phase 1d: Test Instrumentation

**Goal:** First-class TDD workflow. Run tests, get structured failures with hints, rerun with targeted tracing.

### Features

#### Run Test Suite
- Execute test command (e.g., `cargo test`)
- Minimal/no tracing for fast feedback
- Parse structured output

#### Structured Failure Output
On test failure, return:
- Test name, file, line number
- Error message
- Stack trace
- **Suggested trace patterns** (extracted from stack, rule-based)
- **Rerun command** for single test

#### Test Adapter Trait
```rust
pub trait TestAdapter {
    fn detect(&self, project: &Path) -> Option<Framework>;
    fn run_command(&self, config: &TestConfig) -> String;
    fn rerun_command(&self, test: &str) -> String;
    fn parse_output(&self, stdout: &str, stderr: &str) -> TestResult;
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String>;
}
```

#### cargo test Adapter (Rust)
- Detect via `Cargo.toml`
- Use `--format json` for structured output
- Parse JSON for failures
- Extract module names from stack for trace hints

#### Rerun with Tracing
- Run single test with trace patterns
- Capture events around failure
- Query to find root cause

#### MCP Tools
- `debug_test` - Run tests, get structured results

### Context-Aware Tracing Defaults

| Context | Default Tracing | Rationale |
|---------|-----------------|-----------|
| `debug_launch` | User code | Broad observation for unknown bugs |
| `debug_test` (full suite) | Minimal/none | Fast feedback, wait for failure |
| `debug_test` (rerun) | Suggested patterns | Stack trace tells us what to trace |

### Validation Criteria

**Scenario: TDD debugging workflow**
1. LLM runs `debug_test({ command: "cargo test" })`
2. Test fails, LLM receives structured failure with hints
3. LLM runs `debug_test({ command: "cargo test", test: "test_name", tracePatterns: hints })`
4. LLM queries trace events around the failure
5. LLM identifies root cause

**Success:** No full suite reruns. No guessing what to trace. Failure tells LLM exactly where to look.

---

## Phase 2: Active Debugging

**Goal:** LLM can pause execution and inspect state.

### Features

#### Conditional Breakpoints
- Break only when condition is met (field comparisons)
- Hit count support (break on Nth occurrence)
- Glob patterns for function matching

#### State Inspection
- Inspect variables at current breakpoint
- Or inspect at historical event (time-travel via event ID)
- Navigate struct fields, array elements
- Returns value and type information

#### Resume Execution
- Continue after breakpoint
- Optionally step to next function call

#### Logpoints (Non-Breaking)
- Log without stopping execution
- Template substitution from local variables

### Validation Criteria

Find a bug that traces alone couldn't catch:
1. LLM sees suspicious pattern in traces
2. LLM sets conditional breakpoint
3. App pauses at exact moment of interest
4. LLM inspects local variables, finds wrong value
5. LLM identifies root cause

---

## Phase 3: VS Code Integration

**Goal:** Humans can see what the LLM sees. Frictionless onboarding.

### Features

#### One-Click Install
- Available on VS Code marketplace
- No CLI setup required
- Extension manages daemon lifecycle

#### Debug Panel
- Standard VS Code debugging UI
- Breakpoints, call stack, variables
- Integrates with DAP (Debug Adapter Protocol)

#### Execution History Viewer
- Timeline of traced events
- Click to navigate to any point
- Filter by function, module, time

#### Query Panel
- Write structured queries
- Results in sortable table
- Click events to inspect state

### Validation Criteria

Non-technical user can:
1. Install extension from marketplace
2. Open their project
3. Click "Debug with Strobe"
4. See execution traces in UI
5. Click on events to see details

---

## Phase 4: UI Observation

**Goal:** LLM can see the current state of GUI applications.

### Features

#### Screenshot Capture
- Capture on demand (PNG format)
- Configurable resolution
- Capture specific window or full screen

#### Accessibility Tree
- Structured representation of all UI elements
- Element roles (button, textfield, list, menu, etc.)
- Accessible names and values
- Bounding boxes for element location
- Available actions per element

The accessibility tree is critical - it gives the LLM a semantic understanding of the UI, not just pixels.

### Platform Support

| Platform | Screenshot | Accessibility |
|----------|-----------|---------------|
| Linux (X11) | XGetImage | AT-SPI2 |
| macOS | CGWindowListCreateImage | AXUIElement |
| Windows | DXGI duplication | UI Automation |

### Validation Criteria

LLM can observe GUI state:
1. LLM launches GUI app
2. LLM captures screenshot + accessibility tree
3. LLM describes what it sees ("Login form with username/password fields and Submit button")
4. LLM correlates UI state with execution traces

---

## Phase 5: UI Interaction

**Goal:** LLM can control GUI applications autonomously.

This is a killer feature for autonomous debugging. The LLM doesn't just observe - it can reproduce bugs without human help.

### Features

#### Click
- Target by accessible name or coordinates
- Single click, double click

#### Type
- Type text into focused element or targeted field
- Supports Unicode

#### Scroll
- Scroll by direction and pixel amount
- Target specific scrollable containers

#### Drag
- Drag from one coordinate to another
- For sliders, reordering, etc.

#### Key Press
- Single keys or modifier combinations (Ctrl+S, Alt+Tab)
- Special keys (Enter, Escape, arrows)

### Platform Support

| Platform | Click | Type | Scroll | Drag | Keys |
|----------|-------|------|--------|------|------|
| Linux (X11) | XTest | XTest | XTest | XTest | XTest |
| macOS | CGEvent | CGEvent | CGEvent | CGEvent | CGEvent |
| Windows | SendInput | SendInput | SendInput | SendInput | SendInput |

### Validation Criteria

Fully autonomous bug reproduction:
1. User: "There's a bug when I log in and go to Settings"
2. LLM launches app
3. LLM captures UI, sees login screen
4. LLM types username/password, clicks Login
5. LLM captures UI, sees dashboard
6. LLM clicks Settings
7. LLM captures UI, sees the bug
8. LLM correlates with traces, identifies root cause

**No human touched the app. LLM did everything.**

---

## Future Phases

### Phase 6: Advanced Threading Tools
- Lock acquisition tracing
- Deadlock detection
- Spinlock detection
- Thread timeline visualization
- Race condition hints

### Phase 7: Smart Test Integration
- Language-specific test setup skills
- Auto-detect project type and configure testing
- Adapters to normalize test framework output
- MCP tool: `debug_setup_tests`

**Supported frameworks:**

| Language | Framework | Output Parsing | Run Command | Rerun Single |
|----------|-----------|----------------|-------------|--------------|
| Rust | cargo test | `--format json` | `cargo test` | `cargo test {name}` |
| C/C++ | Google Test | XML output | `./test_binary` | `./test_binary --gtest_filter={name}` |
| C/C++ | Catch2 | XML/JSON output | `./test_binary` | `./test_binary "{name}"` |
| C/C++ | CTest | JSON output | `ctest` | `ctest -R {name}` |
| Python | pytest | `--json` | `pytest` | `pytest {file}::{name}` |
| JS/TS | Jest | `--json` | `npm test` | `npm test -- -t {name}` |
| Go | go test | `-json` | `go test ./...` | `go test -run {name}` |

**The setup skill provides:**
- Auto-detection of language and framework from project files
- Step-by-step setup instructions
- Required config file changes
- Run command and output format info

**Adapter architecture:** Test frameworks output their native format → Strobe adapter parses it → Unified TestFailure schema for `debug_test`

### Phase 8: JavaScript/TypeScript (CDP)
- Chrome DevTools Protocol collector
- Debug Node.js, browser apps, Electron
- Same MCP interface, different backend

### Phase 9: Additional Languages
- Python (via sys.settrace or Frida)
- Go (enhanced DWARF support, goroutine awareness)
- Java/Kotlin (via ART hooks on Android)

### Phase 10: Windows Support
- Frida works on Windows
- PDB parsing for symbols
- Named pipes for daemon communication
- Windows-specific UI capture

### Phase 11: Distributed Tracing
- Follow requests across services
- Correlate traces from multiple processes
- Network request interception

### Commercial Features (Strobe Cloud)
- CI/CD integration
- Automatic test generation from traces
- Regression detection across commits

---

## Contributor Extensibility

The architecture is designed so **anyone can add support for obscure languages or test frameworks** without understanding the whole codebase.

### Adding Language Support

Implement the `Collector` trait:
- `attach` - Connect to target process
- `detach` - Clean disconnect
- `set_trace_patterns` - Update what gets traced
- `poll_events` - Receive trace events

Emit events conforming to the unified `TraceEvent` schema, and the rest of the system (storage, queries, MCP) works automatically.

### Adding Test Framework Support

Implement the `TestAdapter` trait:
- `detect` - Check if this adapter handles the project
- `run_command` - Get command to run tests
- `rerun_command` - Get command to run single test
- `parse_output` - Parse framework output into unified schema
- `suggest_traces` - Extract trace hints from failures

Parse your framework's output into our unified `TestFailure` schema, and `debug_test` works with it.

### What Contributors Don't Touch

- SQLite storage layer
- Query engine
- MCP protocol handling
- VS Code extension
- Frida agent (unless adding native support)

Clean interfaces = more contributors = more languages supported.

---

## Performance Characteristics

### Overhead

| Scenario | Overhead |
|----------|----------|
| User code tracing (default) | 5-15% CPU |
| Full tracing (all functions) | 20-40% CPU |
| Breakpoints only (no tracing) | < 1% CPU |
| UI capture (on-demand) | ~50ms per capture |

### Throughput

| Metric | Target |
|--------|--------|
| Events per second | 100k+ |
| Query latency (simple) | < 10ms |
| Query latency (complex) | < 100ms |
| Storage per event | ~200 bytes |

### Scalability

- SQLite handles millions of events
- WAL mode for concurrent read/write
- Configurable retention (auto-delete old events)
- Ring buffer under memory pressure

---

## Security & Privacy

### What We Can't Do

- Debug processes owned by other users
- Debug setuid binaries
- Elevate privileges
- Access kernel memory

### Data Handling

- All data stored locally
- No telemetry in open source version
- Session data deleted with `debug_stop`
- No network calls unless explicitly configured
