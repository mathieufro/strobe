# Features by Phase

Each phase builds on the previous. Each has a clear validation criteria: "What can I do now that I couldn't before?"

---

## Phase 1: Passive Tracing + Test Instrumentation

**Goal:** LLM can observe program execution and debug tests without code changes.

### Features

#### Launch with Tracing
- Spawns process via Frida
- Automatically traces user code (source files in project directory)
- Follows fork/exec automatically, tagging events with PID
- Returns session ID for subsequent operations

#### Query Execution History
- Search by function name, source file, return value, duration
- Filter by thread ID or thread group
- Time range filtering
- Order by timestamp or thread-then-timestamp
- Pagination with metadata (total count, showing first N)

When queries return large result sets, the LLM receives pagination info to narrow down with follow-up queries.

#### Dynamic Trace Adjustment
- Add/remove trace patterns while app runs (no restart required)
- Glob syntax: `*` matches within module, `**` matches across modules
- Adjust serialization depth per pattern
- Hot function auto-detection with sampling (LLM is warned when data is sampled)

#### Crash Capture
When app crashes (SIGSEGV, SIGABRT, etc.), Frida intercepts before termination:
- Stack trace at crash point
- Register state
- Local variables in crashing frame
- Last N events leading to crash

Query with `eventType: "crash"` to retrieve full crash context.

#### Stop Session
- Detaches Frida cleanly
- Session data deleted by default
- Optional: retain session for later analysis (auto-purged after 7 days)
- Storage hard limit: 10GB total, oldest sessions purged first

#### Test Instrumentation (TDD Workflow)

First-class support for test-driven debugging. This is a killer feature for autonomous debugging.

**Run full suite** with minimal tracing for fast feedback. On failure, receive structured results with **rule-based hints**:
- Test name, file, line number
- Error message and stack trace
- **Suggested trace patterns** extracted from stack trace (no AI needed)
- **Rerun command** for just this test

**Rerun single test** with targeted tracing using the suggested patterns. Now trace events are captured around the failure point.

**Why this matters:**
- LLMs often forget about lean test scripts, run full suite repeatedly
- Test failures already tell you WHERE to look
- No need to "trace everything" - trace what the failure suggests
- Faster iteration, less noise

### What Gets Captured

| Data | Captured | Notes |
|------|----------|-------|
| Function name | Yes | Demangled (raw name also available) |
| Source file + line | Yes | Via DWARF/PDB |
| Arguments | Yes | JSON serialized, depth-limited, cycle-detected |
| Return value | Yes | JSON serialized |
| Duration | Yes | Nanosecond precision |
| Thread ID + name | Yes | For multi-threaded debugging |
| Process ID | Yes | For fork/exec tracking |
| Call hierarchy | Yes | Parent event tracking |
| Sampling metadata | Yes | When auto-sampling is active |

### Context-Aware Tracing Defaults

Tracing scope depends on context:

| Context | Default | Rationale |
|---------|---------|-----------|
| `debug_launch` | User code | Broad observation for unknown bugs |
| `debug_test` (full suite) | Minimal/none | Fast feedback, wait for failure |
| `debug_test` (rerun failed) | Suggested patterns | Stack trace tells us what to trace |

**User code heuristic:**
- **Traced:** Functions whose source file is in the project directory
- **Not traced:** Standard library, system calls, third-party dependencies

The LLM can broaden or narrow scope at runtime via `debug_trace`.

### Validation Criteria

**Scenario A: General debugging**
1. LLM launches app with tracing
2. User triggers the bug
3. LLM queries execution history
4. LLM identifies suspicious area
5. LLM adjusts traces to focus on that area
6. User triggers bug again
7. LLM finds root cause

**Scenario B: TDD workflow**
1. LLM runs test suite via `debug_test`
2. Test fails, LLM receives structured failure with hints
3. LLM reruns single test with suggested trace patterns
4. LLM queries trace events around the failure
5. LLM finds root cause

**No recompilation. No code changes. No manual log statements. No running full suite repeatedly.**

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
