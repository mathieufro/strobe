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
- stdout/stderr captured automatically via Frida's Device "output" signal (`FRIDA_STDIO_PIPE`)
- Works reliably with ASAN/sanitizer-instrumented binaries (no agent-side hooks needed)
- Output events interleaved chronologically in the unified event timeline
- Queryable via `debug_query` with `eventType: "stdout"` or `"stderr"`
- This is the primary debugging tool — often sufficient to diagnose crashes without any trace patterns

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
| Process stdout | Yes | Via Frida Device "output" signal |
| Process stderr | Yes | Via Frida Device "output" signal |

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

### Recommended Workflow

The most effective approach is **incremental observation** — start with nothing and add only what you need:

1. **Launch with no patterns** — stdout/stderr are always captured
2. **Read output first** — crash messages, ASAN reports, and error logs are often enough
3. **Add targeted traces** — only when output alone doesn't explain the issue
4. **Narrow or widen** — adjust patterns based on what you learn, no restart needed

This is much faster than trying to guess the right trace patterns upfront, and avoids overwhelming the system with unnecessary events.

### Validation Criteria

**Scenario: Crash investigation (output-first)**
1. LLM calls `debug_launch` with **no** trace patterns
2. User triggers the crash
3. LLM calls `debug_query({ eventType: "stderr" })` — sees ASAN crash report
4. Crash report points to `lv_obj_style.c:632` via `KeyboardMappingSubView` constructor
5. LLM reads the relevant source, identifies memory pool exhaustion
6. LLM proposes fix — **no tracing was needed at all**

**Scenario: Targeted tracing (when output isn't enough)**
1. LLM launches with no patterns, reads output — no crash, but wrong behavior
2. LLM calls `debug_trace({ sessionId, add: ["submit::*", "form::validate"] })`
3. User reproduces the bug
4. LLM calls `debug_query` to find suspicious return values
5. LLM narrows further or queries with `verbose: true`
6. LLM identifies root cause

**Success:** LLM can observe what happened — starting from process output and escalating to function traces only when needed. No code changes. No restarts. No guesswork.

---

## Phase 1b: Advanced Runtime Control

**Goal:** Production-ready tracing with performance safeguards and deeper inspection.

### Features

#### Configurable Serialization Depth
- `serializationDepth` parameter in `debug_trace` (1-10, default: 3)
- Recursive object inspection via `ObjectSerializer` — follows pointers, serializes structs/arrays
- Circular reference detection with `<circular ref to 0x...>` markers
- Depth limiting with `<max depth N reached>` markers
- Arrays capped at 100 elements
- Flow: API → daemon → spawner → agent (via hooks message)

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
| `debug_launch` | None (stdout/stderr only) | Output is often enough; add patterns incrementally |
| `debug_test` (full suite) | None | Fast feedback, wait for failure |
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

**Goal:** LLM can see the current state of GUI applications with zero-latency reads.

### Core Insight: Always-Hot Tree

The traditional agent-UI loop is slow: agent requests tree → system computes tree → agent receives → agent decides → agent acts. Every step is a blocking round-trip. Strobe inverts this into a **push model**: the UI tree is continuously maintained in the background, always fresh, always ready. When the LLM calls `debug_ui_tree`, it reads from memory — sub-millisecond response, no computation.

This transforms a 4-step UI interaction from ~8s (4 round-trips with tree computation) to ~2s (4 instant reads + action execution). The difference between "painful" and "usable."

### Architecture

```
┌─────────────────────────────────────────────────────┐
│                  Always-Hot Tree                     │
│                                                     │
│  ┌──────────────┐    ┌──────────────┐               │
│  │ AXUIElement   │    │ AI Vision    │               │
│  │ (structured)  │    │ (screenshot) │               │
│  └──────┬───────┘    └──────┬───────┘               │
│         │                   │                        │
│         └───────┬───────────┘                        │
│                 ▼                                    │
│        ┌────────────────┐                            │
│        │  Unified Tree   │ ← in-memory, always fresh │
│        │  (merged view)  │                            │
│        └────────┬───────┘                            │
│                 │                                    │
│    ┌────────────┼────────────┐                       │
│    ▼            ▼            ▼                       │
│  MCP tool    MCP resource   Instrumentation          │
│  (on-demand) (push/notify)  correlation              │
└─────────────────────────────────────────────────────┘
```

**Background observer** runs on a dedicated thread:
1. Polls native accessibility tree every 100-200ms
2. Captures screenshot at same cadence (or on detected change)
3. Runs AI vision pipeline on screenshot
4. Merges both sources into unified tree
5. Diffs against previous snapshot — marks changed regions
6. Stores current tree in shared memory

**The LLM sees one flat, compact tree** — it doesn't know or care whether an element came from AX or vision.

### Features

#### Screenshot Capture
- Continuous background capture (100-200ms cadence)
- On-demand high-resolution capture (PNG format)
- Capture specific window or full screen
- Change detection: skip AI pipeline if screenshot unchanged (perceptual hash)

#### Native Accessibility Layer

Native accessibility APIs provide structured, labeled elements — fast and reliable when available.

- Structured representation of all UI elements
- Element roles (button, textfield, list, menu, etc.)
- Accessible names and values
- Bounding boxes for element location
- Available actions per element

#### AI Vision Layer

Native accessibility APIs (AXUIElement, AT-SPI2, UI Automation) only work when the application exposes proper accessibility info. Many native C++ apps — especially JUCE-based audio software with custom-painted UIs, OpenGL surfaces, or game engines — expose partial or empty trees.

For these cases, Strobe runs a two-level AI vision pipeline on every captured screenshot:

**Level 1: YOLO Detection (~5ms GPU, ~30ms CPU)**
- OmniParser V2's fine-tuned YOLOv8 icon detection model (or YOLO11/YOLO26)
- Detects all interactable elements as bounding boxes
- Export to CoreML for Apple Silicon Neural Engine inference
- No captioning overhead — detection only

**Level 2: Element Classification**
- *Known widgets* → SigLIP/CLIP zero-shot classification against a fixed label set (~2ms GPU)
  - Labels: button, slider, knob, text field, dropdown, toggle, checkbox, menu, icon, label
  - Returns: `{ bbox: [...], label: "slider", confidence: 0.94 }`
- *Unknown/ambiguous elements* → FastVLM 0.5B short caption (~30-50ms on Neural Engine)
  - Returns: `{ bbox: [...], caption: "circular knob showing value 42%" }`

#### Tree Merge Strategy

1. Query native accessibility tree (roles, names, actions, bounding boxes)
2. Run YOLO detection on screenshot (bounding boxes)
3. Match detected boxes to accessibility nodes by IoU overlap (threshold: 0.5)
4. Unmatched boxes → classify with SigLIP, caption with FastVLM if ambiguous
5. Produce unified tree: native nodes enriched with visual bounding boxes, plus AI-detected nodes for custom widgets
6. Assign stable IDs to all elements (persistent across frames when possible)
7. Mark elements with `source: "native" | "vision" | "merged"` for transparency

**Target latency per frame:** <60ms total (native a11y + YOLO + SigLIP classification)

#### Compact Tree Representation

The tree must be small enough to fit in LLM context. Raw accessibility trees can have thousands of nodes. Strobe projects to a compact format:

```
[window "ERAE MK2 Simulator" id=w1]
  [toolbar id=tb1]
    [button "Play" id=btn_play enabled]
    [button "Stop" id=btn_stop disabled]
    [slider "Volume" value=0.75 id=sld_vol]
  [panel "Main" id=p1]
    [knob "Filter Cutoff" value≈0.6 id=vk_3 source=vision]
    [knob "Resonance" value≈0.3 id=vk_4 source=vision]
    [list "Presets" id=lst_presets loading]
      [item "Default" id=pr_1]
      [item "Bass Heavy" id=pr_2]
```

Key properties per element: **role**, **label**, **value**, **id** (stable), **enabled/disabled**, **loading** (dynamic state), **source** (native/vision). Bounding boxes omitted by default (available via verbose mode).

#### Dynamic State Detection

Some UI elements are in transitional states (loading spinners, animations, progress bars). Strobe detects these via:

- AX role `BusyIndicator` or `ProgressIndicator` → mark parent as `loading`
- Tree region that changed in last N consecutive frames → mark as `dynamic`
- Vision-detected spinning/pulsing elements (motion between consecutive screenshots)
- App-specific hooks via Frida instrumentation (e.g., hook `setLoading(bool)` → annotate tree node)

#### Instrumentation Correlation

Strobe's unique advantage: it knows program internals, not just pixels.

- Vision detects "knob at position (200, 300) with value ≈ 0.6"
- Instrumentation detects `setFilterCutoff(0.6)` was called
- → Strobe **knows** that visual knob = filter cutoff, with exact value from runtime
- This feedback loop provides ground truth that pure vision approaches lack
- Correlation stored in tree: `[knob "Filter Cutoff" value=0.6 linked=setFilterCutoff id=vk_3]`

#### Models

| Role | Model | Size | Latency | Hardware |
|------|-------|------|---------|----------|
| Detection | YOLOv8 (OmniParser weights) | ~6MB | ~5ms | CoreML / Neural Engine |
| Classification | SigLIP 2 | ~400MB | ~2ms/crop | CoreML / GPU |
| Captioning (fallback) | FastVLM 0.5B | ~500MB | ~30-50ms | CoreML / Neural Engine |
| Captioning (full) | Florence-2 / OmniParser V2 | ~1.5GB | ~500ms | GPU (CUDA) |

FastVLM (Apple, CVPR 2025) is the preferred captioning model — designed for Apple Silicon with CoreML export and 85x faster TTFT than comparable VLMs.

### MCP Interface

```
debug_ui_tree(sessionId)
  → Returns current unified tree (instant — reads from memory)
  → Options: verbose (include bounding boxes), depth (max tree depth)

debug_ui_screenshot(sessionId)
  → Returns latest screenshot (PNG, already captured)
  → Options: high_res (trigger fresh high-res capture)

debug_ui_watch(sessionId, elementId)
  → Subscribe to changes on a specific element
  → Returns diff when element value/state changes
```

### Platform Support

| Platform | Screenshot | Accessibility | AI Vision |
|----------|-----------|---------------|-----------|
| Linux (X11) | XGetImage | AT-SPI2 | YOLO + SigLIP (ONNX/TensorRT) |
| macOS | CGWindowListCreateImage | AXUIElement | YOLO + SigLIP + FastVLM (CoreML) |
| Windows | DXGI duplication | UI Automation | YOLO + SigLIP (ONNX/TensorRT) |

### Validation Criteria

LLM can observe GUI state with zero-latency reads:
1. LLM launches GUI app via `debug_launch`
2. LLM calls `debug_ui_tree` → gets unified tree **instantly** (no computation wait)
3. LLM reads tree and describes what it sees ("Audio plugin with 3 knobs, a preset list loading, and Play/Stop buttons")
4. LLM identifies custom-painted widgets missed by native accessibility (e.g. JUCE knobs detected by AI vision)
5. LLM correlates UI element values with runtime function calls (knob value matches `setFilterCutoff(0.6)`)
6. Tree updates in background — next `debug_ui_tree` call reflects new state without delay

---

## Phase 5: UI Interaction

**Goal:** LLM can control GUI applications through intent-based actions, with an intelligent action layer that handles complex motor plans.

This is a killer feature for autonomous debugging. The LLM doesn't just observe — it can reproduce bugs, test UI flows, and interact with complex custom widgets without human help.

### Core Insight: Intent-Based Actions + Intelligent Motor Layer

The LLM should express **what** it wants, not **how** to physically do it. "Set the filter cutoff knob to 0.8" is an intent. The motor layer figures out whether that means a vertical drag, circular drag, double-click-and-type, or scroll — based on the widget type and learned interaction model.

```
┌──────────────────────────────────────────────────┐
│                  LLM Intent                       │
│         "set_value(id=vk_3, value=0.8)"          │
└──────────────────┬───────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────┐
│            Intelligent Action Layer               │
│                                                  │
│  1. Look up element vk_3 in unified tree          │
│  2. Determine widget type: knob (from vision)     │
│  3. Select motor strategy: vertical drag          │
│     (from widget profile or learned behavior)     │
│  4. Calculate drag vector: center, current→target │
│  5. Execute: mouseDown → mouseDrag → mouseUp     │
│  6. Verify: re-read tree, check value changed     │
│  7. Adjust if needed (closed-loop correction)     │
└──────────────────┬───────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────┐
│          Platform Input Layer (CGEvent)           │
└──────────────────────────────────────────────────┘
```

### Features

#### Intent-Based Action API

The LLM uses high-level intents. Element targeting uses stable IDs from the unified tree (Phase 4).

```
debug_ui_action(sessionId, action)

Actions:
  click(id="btn_play")                          → single click on element
  click(id="btn_play", count=2)                 → double click
  set_value(id="sld_vol", value=0.5)            → set slider/knob to value
  type(id="txt_name", text="hello")             → type into field
  type(text="hello")                            → type into focused element
  select(id="lst_presets", item="Bass Heavy")   → select list item
  scroll(id="lst_presets", direction="down", amount=3)
  drag(from="track_1", to="slot_3")             → drag and drop
  key(key="Enter")                              → press key
  key(key="s", modifiers=["cmd"])               → keyboard shortcut
  move_to(id="player", position={x:100, y:200}) → game character / spatial
```

The LLM never needs to know pixel coordinates, drag directions, or hold durations. It just says what it wants.

#### Intelligent Motor Layer

Different widgets require different physical interactions. The motor layer maps intents to motor plans:

**Widget Profiles** — known interaction models for common widget types:

| Widget Type | Motor Strategy | Parameters |
|-------------|---------------|------------|
| Button | Click center of bbox | - |
| Slider (horizontal) | Drag handle to proportional X | min/max position from bbox |
| Slider (vertical) | Drag handle to proportional Y | min/max position from bbox |
| Knob (vertical-drag) | Drag up/down from center | sensitivity from widget profile |
| Knob (circular) | Drag in arc from current angle | center, radius, angle range |
| Text field | Click to focus, then type | - |
| Dropdown | Click to open, click item | - |
| Toggle/Checkbox | Click center | - |
| List item | Click, or double-click to activate | - |

**Profile sources** (in priority order):
1. **Native accessibility actions** — if AX says "increment/decrement" exists, use that
2. **Known toolkit profiles** — JUCE knobs use vertical drag, Unity sliders use horizontal drag, etc.
3. **Learned behavior** — probe the widget: small drag, observe value change, infer interaction model
4. **Fallback** — try vertical drag (most common for knobs), then circular, then click-and-type

**Probing** (for unknown widgets):
1. Record current value from tree
2. Execute small test interaction (5px vertical drag from center)
3. Re-read tree, check if value changed
4. If yes → learned the interaction model (direction, sensitivity)
5. If no → try next strategy (horizontal drag, circular, etc.)
6. Cache learned profile for this widget type

#### Closed-Loop Verification

Every action is verified:
1. Execute motor plan
2. Wait for tree update (background observer captures new state)
3. Compare element value to expected result
4. If mismatch → adjust and retry (up to 3 attempts)
5. Report success/failure to LLM

This is where **instrumentation correlation** (Phase 4) shines: Strobe doesn't just check pixels — it can verify that `setFilterCutoff()` was actually called with the expected value.

#### Complex Interaction Sequences

Some interactions require coordinated multi-step motor plans:

- **Game character movement**: Hold WASD keys for duration, or click-to-move path
- **Drawing/painting**: Bezier curve mouse paths with pressure (if supported)
- **Multi-touch gestures**: Pinch, rotate, swipe (via accessibility actions or simulated touch events)
- **Menu navigation**: Click menu → wait for submenu → click item (with timing)
- **Drag-and-drop with scroll**: Drag to edge → auto-scroll → drop at target

These are composed from primitive motor actions but require timing, sequencing, and state awareness.

#### Platform Input Primitives

Low-level input injection, used by the motor layer (not directly by the LLM):

| Primitive | macOS | Linux (X11) | Windows |
|-----------|-------|-------------|---------|
| Mouse move | CGEvent | XTest | SendInput |
| Mouse click | CGEvent | XTest | SendInput |
| Mouse drag | CGEvent sequence | XTest sequence | SendInput sequence |
| Key press | CGEvent | XTest | SendInput |
| Key hold/release | CGEvent | XTest | SendInput |
| Scroll | CGEvent | XTest | SendInput |

### MCP Interface

```
debug_ui_action(sessionId, action)
  → Executes intent-based action
  → Returns: { success: bool, element: updated_state, verified: bool }

debug_ui_action_batch(sessionId, actions[])
  → Execute sequence of actions with inter-action delays
  → Returns: per-action results

debug_ui_probe(sessionId, elementId)
  → Probe an unknown widget to learn its interaction model
  → Returns: { widgetType, motorStrategy, sensitivity }
```

### Validation Criteria

**Fully autonomous bug reproduction with complex UI:**
1. User: "There's a bug when I set filter cutoff above 0.9 and then switch presets"
2. LLM launches app via `debug_launch`
3. LLM reads `debug_ui_tree` → sees knobs (vision-detected), preset list (native AX)
4. LLM: `set_value(id="vk_3", value=0.95)` → motor layer drags knob, verifies via instrumentation
5. LLM: `select(id="lst_presets", item="Bass Heavy")` → clicks list item
6. LLM reads `debug_ui_tree` → sees the bug (UI glitch, wrong values)
7. LLM correlates with function traces → identifies root cause in preset loading code

**No human touched the app. LLM handled knob rotation, list selection, and bug correlation autonomously.**

**Game/spatial interaction:**
1. LLM launches game, reads tree
2. LLM: `move_to(id="player", position={x:100, y:200})` → motor layer holds WASD keys
3. LLM: `click(id="door")` → opens door
4. LLM reads tree → verifies new room loaded
5. LLM traces `loadLevel()` → confirms correct level transition

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
