# Phases 4-7: UI Observation, Interaction, I/O Channels & Scenario Runner

**Status:** Spec Complete
**Goal:** LLM can observe and interact with any application — GUI or headless — through a unified I/O channel model, and run autonomous test scenarios that combine UI actions, MIDI, audio, network, and runtime assertions.

---

## Design Principles

1. **I/O channels are the universal abstraction.** UI, MIDI, audio, network, file, stdout/stderr, and function traces are all I/O channels. Each can send stimuli and/or capture observations.
2. **On-demand, not always-running.** The UI tree computes on every `debug_ui_tree` call (~30-60ms). LLM token latency dominates — 60ms is invisible. No background threads, no wasted resources.
3. **Intent over mechanics.** The LLM says `set_value(id="knob_freq", value=0.8)`, not "drag from (200,300) by (0,-40)". The motor layer handles widget-specific physical interaction.
4. **VLM-powered learning.** Unknown widgets are classified by a vision-language model, then the learned interaction profile is cached for instant reuse.
5. **Failure hands control to the LLM.** The scenario runner stops on failure, returns what went wrong, and keeps the process alive. The LLM becomes the debugger with full tool access.
6. **Cross-platform from day one.** All platform-specific code lives behind traits. macOS and Linux implementations ship together.

---

## Architecture Overview

```
┌──────────────────────────────────────────────────────────────┐
│  Scenario Runner (debug_test_scenario)              Phase 6  │
│  - Flat action list: do / wait / assert                      │
│  - Steps reference any I/O channel                           │
│  - On failure: stop, return error, session stays alive       │
└────────────────────────┬─────────────────────────────────────┘
                         │ composes
┌────────────────────────▼─────────────────────────────────────┐
│  I/O Channel Layer                              Phase 6 + 7  │
│                                                               │
│  Each channel implements InputChannel, OutputChannel, or both │
│                                                               │
│  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐           │
│  │   UI    │ │  MIDI   │ │ Network │ │  Audio  │  ...       │
│  │ in+out  │ │ in+out  │ │ in+out  │ │ in+out  │           │
│  │ Phase 4+5│ │ Phase 7 │ │ Phase 7 │ │ Phase 7 │           │
│  └─────────┘ └─────────┘ └─────────┘ └─────────┘           │
│                                                               │
│  Already built (Phase 1a-1b, wrapped as channels in Phase 6):│
│  ┌──────────┐ ┌──────────────┐                               │
│  │ stdout/  │ │ fn traces    │                               │
│  │ stderr   │ │ (debug_trace)│                               │
│  └──────────┘ └──────────────┘                               │
└────────────────────────┬─────────────────────────────────────┘
                         │ uses
┌────────────────────────▼─────────────────────────────────────┐
│  Platform Abstraction                                         │
│                                                               │
│  UI:    macOS (AX/CGEvent/CoreML) │ Linux (AT-SPI2/XTest/ONNX)│
│  MIDI:  macOS (CoreMIDI)          │ Linux (ALSA)              │
│  Audio: macOS (CoreAudio tap)     │ Linux (JACK/PipeWire)     │
│  Net:   cross-platform (Frida socket intercept)               │
│  File:  macOS (FSEvents)          │ Linux (inotify)           │
└──────────────────────────────────────────────────────────────┘
```

---

## Phase 4: UI Observation

**Goal:** LLM can see the current state of any GUI application — native widgets via accessibility APIs, custom-painted widgets via AI vision — with a single tool call.

### How It Works

```
Call debug_ui_tree(sessionId)
  │
  ├─ 1. Screenshot (platform API)                      ~10ms
  ├─ 2. Perceptual hash — same as last screenshot?
  │      yes → reuse cached vision detections
  │      no  → run YOLO detection + SigLIP classify     ~10ms
  ├─ 3. Accessibility tree query (platform API)          ~5ms
  ├─ 4. Merge: match AX nodes to vision boxes (IoU 0.5) ~1ms
  │      - Matched → enrich AX node with visual bbox
  │      - Unmatched vision box → new vision-detected element
  │      - Unmatched AX node → keep as-is
  ├─ 5. Assign stable IDs (track across calls)           ~1ms
  └─ 6. Project to compact text format                   ~1ms
                                                        ─────
                                                  Total: ~30-60ms
```

### Compact Tree Format

The tree must fit in LLM context. Raw accessibility trees have thousands of nodes. Strobe projects to a compact format:

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

Properties per element:
- **role** — button, slider, knob, textfield, list, item, panel, window, etc.
- **label** — human-readable name (from AX or AI caption)
- **value** — current value (exact from AX, approximate from vision: `value≈0.6`)
- **id** — stable identifier, persistent across calls when possible
- **enabled/disabled** — interaction state
- **loading/dynamic** — transitional state
- **source=vision** — element detected by AI, not native accessibility

### AI Vision Pipeline

Native accessibility APIs only work when the application exposes proper accessibility info. Many native C++ apps — JUCE audio software, OpenGL surfaces, game engines — expose partial or empty trees.

**Level 1: YOLO Detection (~5ms GPU, ~30ms CPU)**
- OmniParser V2's fine-tuned YOLOv8 (or YOLO11/YOLO26)
- Detects all interactable elements as bounding boxes
- CoreML export for Apple Silicon Neural Engine inference

**Level 2: Element Classification**
- *Known widgets* → SigLIP/CLIP zero-shot classification (~2ms GPU)
  - Labels: button, slider, knob, text field, dropdown, toggle, checkbox, menu, icon, label
  - Returns: `{ bbox, label: "slider", confidence: 0.94 }`
- *Unknown/ambiguous elements* → FastVLM 0.5B short caption (~30-50ms Neural Engine)
  - Returns: `{ bbox, caption: "circular knob showing value 42%" }`

**Models:**

| Role | Model | Size | Latency | Hardware |
|------|-------|------|---------|----------|
| Detection | YOLOv8 (OmniParser weights) | ~6MB | ~5ms | CoreML / Neural Engine |
| Classification | SigLIP 2 | ~400MB | ~2ms/crop | CoreML / GPU |
| Captioning (fallback) | FastVLM 0.5B | ~500MB | ~30-50ms | CoreML / Neural Engine |

FastVLM (Apple, CVPR 2025) is preferred for captioning — designed for Apple Silicon with CoreML export and 85x faster TTFT than comparable VLMs.

### Dynamic State Detection

- AX role `BusyIndicator` / `ProgressIndicator` → mark parent as `loading`
- Tree region that changed in last N consecutive calls → mark as `dynamic`
- Vision-detected motion between consecutive screenshots (spinning, pulsing)

### Platform Traits

```rust
/// Read the screen — screenshot + accessibility tree
trait UIObserver: Send + Sync {
    fn screenshot(&self, pid: u32) -> Result<Screenshot>;
    fn accessibility_tree(&self, pid: u32) -> Result<Vec<AXNode>>;
}
```

### Platform Implementations

| Platform | Screenshot | Accessibility |
|----------|-----------|---------------|
| macOS | CGWindowListCreateImage | AXUIElement |
| Linux | XGetImage | AT-SPI2 (via atspi crate or D-Bus) |
| Windows | DXGI duplication | UI Automation (future) |

### MCP Tools

```
debug_ui_tree(sessionId)
  → Returns compact unified tree (~30-60ms)
  → Options: verbose (include bounding boxes, raw AX attributes)

debug_ui_screenshot(sessionId)
  → Returns screenshot as PNG
```

### Validation Criteria

1. LLM launches ERAE simulator via `debug_launch`
2. LLM calls `debug_ui_tree` → receives compact tree in ~60ms
3. Tree contains native AX elements AND vision-detected knobs/custom widgets
4. Stable IDs persist across consecutive `debug_ui_tree` calls
5. LLM can describe the UI from the tree ("3 knobs, a preset list, Play/Stop buttons")

---

## Phase 5: UI Interaction

**Goal:** LLM can control GUI applications through intent-based actions, with an intelligent motor layer that uses VLM classification to learn how to interact with unknown widgets.

### Intent-Based Action API

The LLM expresses **what** it wants. The motor layer figures out **how**.

```
debug_ui_action(sessionId, action)

Actions:
  click(id="btn_play")                            → single click
  click(id="btn_play", count=2)                   → double click
  set_value(id="sld_vol", value=0.5)              → slider/knob to value
  type(id="txt_name", text="hello")               → type into field
  type(text="hello")                               → type into focused element
  select(id="lst_presets", item="Bass Heavy")      → select list item
  scroll(id="lst_presets", direction="down", amount=3)
  drag(from="track_1", to="slot_3")                → drag and drop
  key(key="Enter")                                 → press key
  key(key="s", modifiers=["cmd"])                  → keyboard shortcut
```

### Motor Layer

```
Call debug_ui_action(sessionId, {do: "set_value", id: "vk_3", value: 0.8})
  │
  ├─ 1. Look up element vk_3 in current tree
  │
  ├─ 2. Has cached motor profile for this element?
  │      yes → use cached strategy, skip to step 5
  │
  ├─ 3. Try native AX action (AXIncrement/AXDecrement)
  │      worked? → cache "ax_action" profile, done
  │      no AX action available? → continue
  │
  ├─ 4. VLM classifies widget from screenshot crop
  │      → "vertical-drag knob, sensitivity ~200px/full range"
  │      → execute motor plan based on classification
  │
  ├─ 5. Execute motor plan (platform input primitives)
  │
  ├─ 6. Re-read tree, verify value changed
  │      match? → cache learned profile, return success
  │      mismatch? → try VLM's second suggestion, retry (max 3)
  │      still no? → return error to LLM
  │
  └─ Return {success: bool, element: updated_state}
```

**Motor strategy priority:**
1. **Native AX action** — if accessibility says increment/decrement exists, use it
2. **VLM classification** — vision model looks at the widget crop and predicts interaction type
3. **VLM second opinion** — if first classification failed, try alternative
4. **Error** — return to LLM, it can try a different approach

**Widget interaction types the VLM can classify:**

| Widget Type | Motor Strategy |
|-------------|---------------|
| Button | Click center of bbox |
| Slider (horizontal) | Drag handle to proportional X |
| Slider (vertical) | Drag handle to proportional Y |
| Knob (vertical-drag) | Drag up/down from center |
| Knob (circular) | Drag in arc from current angle |
| Text field | Click to focus, then type |
| Dropdown | Click to open, click item |
| Toggle/Checkbox | Click center |

### Motor Profile Cache

- Keyed by `(app_binary, element_role, element_label_pattern)`
- Persists across sessions for the same app
- Example: `(erae_mk2_simulator, knob, "Filter*") → vertical_drag, sensitivity=180px`
- First interaction with unknown widget: ~300ms (VLM inference + execute + verify)
- Cached interaction: ~50ms (execute + verify)

### Platform Traits

```rust
/// Inject input events — mouse, keyboard, scroll
trait UIInput: Send + Sync {
    fn click(&self, x: f64, y: f64, count: u32);
    fn mouse_down(&self, x: f64, y: f64);
    fn mouse_move(&self, x: f64, y: f64);
    fn mouse_up(&self, x: f64, y: f64);
    fn scroll(&self, x: f64, y: f64, dx: f64, dy: f64);
    fn key_press(&self, key: Key, modifiers: &[Modifier]);
    fn key_down(&self, key: Key, modifiers: &[Modifier]);
    fn key_up(&self, key: Key);
    fn type_text(&self, text: &str);
}

/// Classify widget interaction from a screenshot crop
trait VisionPipeline: Send + Sync {
    fn detect(&self, screenshot: &Screenshot) -> Result<Vec<Detection>>;
    fn classify(&self, screenshot: &Screenshot, bbox: &BBox) -> Result<WidgetClass>;
    fn caption(&self, screenshot: &Screenshot, bbox: &BBox) -> Result<String>;
    fn classify_interaction(&self, screenshot: &Screenshot, bbox: &BBox) -> Result<InteractionType>;
}
```

### Platform Implementations

| Platform | Input | Vision |
|----------|-------|--------|
| macOS | CGEvent | CoreML (YOLO + SigLIP + FastVLM) |
| Linux | XTest | ONNX Runtime (same model weights) |
| Windows | SendInput (future) | ONNX Runtime (future) |

### MCP Tools

```
debug_ui_action(sessionId, action)
  → Execute intent-based action
  → Returns: {success: bool, element: updated_state}
```

### Validation Criteria

1. LLM calls `set_value(id="vk_3", value=0.8)` on a JUCE knob
2. Motor layer: no AX action → VLM classifies as "vertical-drag knob" → drags → verifies
3. Tree shows updated value
4. Second call to same knob type uses cached profile (no VLM inference)
5. LLM clicks buttons, types in text fields, selects list items — all via intent API

---

## Phase 6: I/O Channel Abstraction + Scenario Runner

**Goal:** Unify all app I/O under a common channel model. Wrap existing capabilities (stdout/stderr, traces, UI) as channels. Introduce the scenario runner for autonomous testing.

### Channel Traits

```rust
/// Send stimuli into the target app
trait InputChannel: Send + Sync {
    fn name(&self) -> &str;                                    // "ui", "midi", "net:8080"
    fn send(&self, action: ChannelAction) -> Result<ActionResult>;
}

/// Observe output from the target app
trait OutputChannel: Send + Sync {
    fn name(&self) -> &str;
    fn start_capture(&self) -> Result<()>;
    fn stop_capture(&self) -> Result<()>;
    fn query(&self, filter: OutputFilter) -> Result<Vec<ChannelEvent>>;
    fn latest(&self) -> Result<Option<ChannelEvent>>;
}

/// Bidirectional channel
trait IOChannel: InputChannel + OutputChannel {}
```

### Channel Registry

Channels are registered explicitly — on launch or mid-session:

```
debug_launch(command, channels: ["ui", "midi"])
  → Session starts with UI and MIDI channels active

debug_channel_add(sessionId, "net:8080")
  → Adds network channel to running session

debug_channel_list(sessionId)
  → Returns: ["ui", "midi", "net:8080", "trace", "stdio"]
  → (trace and stdio are always present — they're the existing Phase 1a capabilities)
```

### Wrapping Existing Capabilities

Phase 1a already built two output channels — they just weren't called that:

| Existing Capability | Channel Name | Input | Output |
|---------------------|-------------|-------|--------|
| stdout/stderr capture | `stdio` | stdin injection (new) | stdout/stderr events (existing) |
| Function tracing | `trace` | add/remove patterns (existing) | function_enter/exit events (existing) |
| UI observation + interaction | `ui` | click, set_value, etc. (Phase 5) | tree, screenshot (Phase 4) |

These get thin adapter wrappers implementing `InputChannel`/`OutputChannel`. No rewrite — just a uniform interface over what already exists.

### Scenario Runner

The scenario runner executes a flat list of steps. Each step is a `do` (send stimulus), `wait` (pause), or `assert` (check output). Steps reference channels by name.

**Format:**

```json
{
  "channels": ["ui", "midi"],
  "steps": [
    {"do": "ui.set_value", "id": "knob_release", "value": 0.0},
    {"do": "midi.send", "type": "noteOn", "note": 60, "velocity": 100},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOff", "note": 60},
    {"wait": 500},
    {"assert": "trace", "fn": "voiceFree", "called": true},
    {"assert": "ui.value", "id": "label_status", "equals": "Idle"}
  ]
}
```

**Execution model:**

```
Execute steps sequentially:

  step N: {do: ...}
    → dispatch to channel's InputChannel.send()
    → success → continue

  step N: {wait: ms}
    → sleep

  step N: {assert: ...}
    → query channel's OutputChannel
    → matches expected? → continue
    → DOESN'T MATCH →
        STOP execution
        return {
          success: false,
          failed_step: N,
          step: { the failing step },
          actual: "what was actually observed",
          completed_steps: N-1,
          total_steps: total,
          session_id: "..."   ← STILL ALIVE
        }

All steps passed →
  return { success: true, completed_steps: total, total_steps: total }
```

**Key property:** On failure, the process stays alive and the session is hot. The LLM receives the minimal error (step number, expected vs actual, session ID), then uses `debug_ui_tree`, `debug_query`, `debug_trace`, or any other tool to investigate. The LLM becomes the debugger.

### Assert Types

| Assert | Channel | Example |
|--------|---------|---------|
| `trace` | fn traces | `{assert: "trace", fn: "voiceFree", called: true}` |
| `trace` | fn traces | `{assert: "trace", fn: "setFilter", args: [0.9]}` |
| `ui.value` | UI | `{assert: "ui.value", id: "label_status", equals: "Ready"}` |
| `ui.exists` | UI | `{assert: "ui.exists", id: "error_dialog", expected: false}` |
| `midi.output` | MIDI | `{assert: "midi.output", type: "noteOff", note: 60}` |
| `audio.rms` | Audio | `{assert: "audio.rms", below: -60}` |
| `net.response` | Network | `{assert: "net.response", status: 201}` |
| `stdout` | stdio | `{assert: "stdout", contains: "initialized"}` |

### MCP Tools

```
debug_channel_add(sessionId, channel)
  → Register a channel on a running session

debug_channel_list(sessionId)
  → List active channels

debug_channel_send(sessionId, channel, action)
  → Send stimulus to any non-UI channel
  → e.g. debug_channel_send(sid, "midi", {type: "noteOn", note: 60})
  → UI uses debug_ui_action instead (motor layer complexity)

debug_channel_query(sessionId, channel, filter?)
  → Query captured output from any channel
  → e.g. debug_channel_query(sid, "midi", {type: "noteOff"})

debug_test_scenario(sessionId, scenario)
  → Execute scenario (flat action list)
  → On failure: return step + expected/actual, session stays alive
```

### Validation Criteria

**JUCE synth scenario:**
1. LLM generates scenario: set release knob to 0 → send MIDI noteOn → wait → send noteOff → wait → assert voiceFree called
2. Scenario runs, all asserts pass → `{success: true}`
3. LLM modifies: set release to max → same MIDI sequence → assert voiceFree called after longer delay
4. Scenario fails at step 5 (voiceFree not called yet) → LLM investigates with `debug_query`

**Headless API scenario:**
1. LLM generates scenario: send HTTP POST to /users → wait → assert 201 → assert db::insert_user traced
2. Same runner, same format, no UI channel needed

---

## Phase 7: Concrete I/O Channels

**Goal:** Implement the most important non-UI channels. Each is a self-contained implementation of the channel traits.

### MIDI Channel

**Input:** Send MIDI messages to the target app.
**Output:** Capture MIDI messages the app sends.

**Implementation strategy:** Create a virtual MIDI port. Connect it to the target app's MIDI input/output. All messages flow through Strobe.

| Platform | API | Virtual Port |
|----------|-----|-------------|
| macOS | CoreMIDI | `MIDISourceCreate` / `MIDIDestinationCreate` |
| Linux | ALSA | `snd_seq_create_simple_port` |

**Stimulus actions:**
```json
{"do": "midi.send", "type": "noteOn", "channel": 0, "note": 60, "velocity": 100}
{"do": "midi.send", "type": "cc", "channel": 0, "controller": 74, "value": 127}
{"do": "midi.send", "type": "sysex", "data": [0xF0, 0x7E, ...]}
```

**Output query:**
```json
{"assert": "midi.output", "type": "noteOff", "note": 60}
{"assert": "midi.output", "type": "cc", "controller": 1}
```

### Audio Channel

**Input:** Inject audio buffers into the target app's input.
**Output:** Capture audio output, compute metrics (RMS, peak, FFT).

| Platform | Capture API | Injection |
|----------|------------|-----------|
| macOS | CoreAudio audio tap (AudioHardwareCreateProcessTap, macOS 14.2+) | Virtual audio device |
| Linux | PipeWire / JACK | JACK client connection |

**Stimulus actions:**
```json
{"do": "audio.inject", "file": "test_tone_440hz.wav"}
{"do": "audio.inject", "tone": 440, "duration_ms": 1000}
{"do": "audio.silence", "duration_ms": 500}
```

**Output assertions:**
```json
{"assert": "audio.rms", "below": -60}
{"assert": "audio.rms", "above": -6}
{"assert": "audio.peak", "below": 0}
{"assert": "audio.frequency", "fundamental": 440, "tolerance": 5}
```

### Network Channel

**Input:** Send packets/requests to the target app.
**Output:** Capture packets/requests the app sends.

**Implementation strategy:** For output capture, use Frida to intercept socket `send()`/`recv()`/`write()`/`read()` calls — this works cross-platform with no external dependencies. For input, use standard socket APIs from the daemon.

**Stimulus actions:**
```json
{"do": "net.send", "proto": "tcp", "host": "localhost", "port": 8080, "data": "GET / HTTP/1.1\r\n\r\n"}
{"do": "net.send", "proto": "http", "method": "POST", "url": "http://localhost:8080/api", "body": {"key": "value"}}
{"do": "net.send", "proto": "udp", "host": "localhost", "port": 9000, "data": [0x01, 0x02]}
```

**Output assertions:**
```json
{"assert": "net.sent", "proto": "tcp", "contains": "HTTP/1.1 200"}
{"assert": "net.response", "status": 201}
{"assert": "net.sent", "proto": "udp", "port": 9000}
```

### File Channel

**Input:** Create/modify/delete files that the target app watches.
**Output:** Capture file changes the app makes.

| Platform | Watch API |
|----------|----------|
| macOS | FSEvents |
| Linux | inotify |

**Stimulus actions:**
```json
{"do": "file.write", "path": "config.json", "content": "{\"debug\": true}"}
{"do": "file.delete", "path": "cache.db"}
{"do": "file.touch", "path": "trigger.flag"}
```

**Output assertions:**
```json
{"assert": "file.modified", "path": "output.log", "contains": "processed"}
{"assert": "file.created", "path": "results.json"}
```

### Channel Summary

| Channel | Input | Output | macOS | Linux | Complexity |
|---------|-------|--------|-------|-------|-----------|
| `ui` | CGEvent / XTest | AXUIElement / AT-SPI2 + Vision | Phase 4-5 | Phase 4-5 | High (vision + motor) |
| `midi` | CoreMIDI | CoreMIDI | Virtual port | ALSA seq | Medium |
| `audio` | Virtual device | CoreAudio tap / JACK | macOS 14.2+ | PipeWire/JACK | Medium |
| `net` | Socket from daemon | Frida socket intercept | Cross-platform | Cross-platform | Medium |
| `file` | Filesystem ops | FSEvents / inotify | Cross-platform | Cross-platform | Low |
| `trace` | *(existing)* | *(existing)* | Frida | Frida | Already done |
| `stdio` | stdin injection | *(existing)* | Frida | Frida | Already done |

### Validation Criteria

1. MIDI channel: send noteOn to ERAE simulator → capture MIDI output → assert correct response
2. Audio channel: inject test tone → capture output → assert RMS above threshold
3. Full scenario: UI knob + MIDI input + trace assertion in one scenario — all channels working together

---

## Build Sequence

### Phase 4a — UI Observation (AX only, no vision)
1. `UIObserver` trait + macOS AXUIElement implementation
2. Screenshot via CGWindowListCreateImage
3. Tree builder: AX → compact format + stable IDs
4. `debug_ui_tree` + `debug_ui_screenshot` MCP tools
5. Linux AT-SPI2 implementation
6. Validate with ERAE simulator

### Phase 4b — AI Vision Layer
1. `VisionPipeline` trait
2. CoreML integration: YOLO detection + SigLIP classification
3. ONNX runtime integration for Linux
4. Vision → AX merge in tree builder
5. FastVLM for unknown widget captioning

### Phase 5a — UI Interaction
1. `UIInput` trait + macOS CGEvent implementation
2. Linux XTest implementation
3. Motor layer: AX actions → VLM classification → execute → verify → cache
4. `debug_ui_action` MCP tool

### Phase 5b — I/O Channel Abstraction
1. `InputChannel` / `OutputChannel` traits
2. Wrap existing stdout/stderr as `stdio` channel
3. Wrap existing traces as `trace` channel
4. Wrap UI as `ui` channel
5. `debug_channel_add`, `debug_channel_list`, `debug_channel_send`, `debug_channel_query` tools
6. Channel registry in SessionManager

### Phase 6 — Scenario Runner
1. Scenario parser (flat action list)
2. Step executor (do / wait / assert dispatch)
3. Failure handling (stop, return error, keep session alive)
4. `debug_test_scenario` MCP tool
5. Validate: ERAE synth scenario end-to-end (UI + trace asserts)

### Phase 7a — MIDI Channel
1. macOS CoreMIDI virtual port (input + output capture)
2. Linux ALSA sequencer virtual port
3. MIDI message types (noteOn, noteOff, CC, sysex, etc.)

### Phase 7b — Audio Channel
1. macOS CoreAudio process tap (output capture)
2. Linux JACK/PipeWire client (output capture)
3. Audio metrics: RMS, peak, FFT fundamental
4. Audio injection (virtual device or file playback)

### Phase 7c — Network Channel
1. Frida socket intercept (output capture, cross-platform)
2. Socket send from daemon (input, cross-platform)
3. HTTP convenience layer (method, path, body, status)

### Phase 7d — File Channel
1. macOS FSEvents / Linux inotify (output: watch changes)
2. Filesystem ops from daemon (input: write/delete/touch)

---

## Example: Complete Autonomous Synth Test

This is the end-to-end vision — what becomes possible after Phase 7a:

```
User: "Test that the release envelope works correctly"

LLM reads code, understands voice lifecycle, generates scenario:

debug_test_scenario(sessionId, {
  channels: ["ui", "midi"],
  steps: [
    // Test 1: Zero release — voice should free immediately after noteOff
    {"do": "ui.set_value", "id": "knob_release", "value": 0.0},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOn", "note": 60, "velocity": 100},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOff", "note": 60},
    {"wait": 50},
    {"assert": "trace", "fn": "Voice::free", "called": true},

    // Test 2: Max release — voice should stay active longer
    {"do": "ui.set_value", "id": "knob_release", "value": 1.0},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOn", "note": 60, "velocity": 100},
    {"wait": 100},
    {"do": "midi.send", "type": "noteOff", "note": 60},
    {"wait": 50},
    {"assert": "trace", "fn": "Voice::free", "called": false},
    {"wait": 3000},
    {"assert": "trace", "fn": "Voice::free", "called": true}
  ]
})

→ All pass? LLM reports: "Release envelope works correctly."
→ Step 6 fails? LLM: "Voice::free wasn't called after zero-release noteOff.
   Let me investigate..." → uses debug_query, debug_ui_tree, adds traces,
   finds the bug, fixes the code, re-runs scenario.
```

**No human touched the app. The LLM tested real runtime behavior through real I/O.**

---

## Example: Headless API Test

Same runner, no UI:

```
debug_test_scenario(sessionId, {
  channels: ["net:3000"],
  steps: [
    {"do": "net.send", "proto": "http", "method": "POST",
     "url": "http://localhost:3000/users",
     "body": {"name": "alice", "email": "alice@test.com"}},
    {"wait": 200},
    {"assert": "net.response", "status": 201},
    {"assert": "trace", "fn": "db::insert_user", "called": true},
    {"assert": "trace", "fn": "db::insert_user", "args": ["alice"]},

    {"do": "net.send", "proto": "http", "method": "POST",
     "url": "http://localhost:3000/users",
     "body": {"name": "alice", "email": "alice@test.com"}},
    {"wait": 200},
    {"assert": "net.response", "status": 409},
    {"assert": "stdout", "contains": "duplicate key"}
  ]
})
```

---

## Future Considerations (Not In Scope)

- **Scenario library / sharing** — save and re-run scenarios by name
- **Parallel step execution** — run independent steps concurrently
- **Conditional steps** — if/else branching in scenarios
- **Scenario generation from traces** — record a manual session, auto-generate scenario
- **Remote targets** — observe apps on other machines (SSH tunnel for channels)
- **Mobile** — iOS (XCUITest bridge), Android (AccessibilityService + ART hooks)
