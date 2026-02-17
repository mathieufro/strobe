# Phase 4: UI Observation

**Date:** 2026-02-11
**Status:** Spec
**Goal:** LLM can see the current state of any GUI application — native widgets via accessibility APIs, custom-painted widgets via AI vision — with a single tool call.

---

## Overview

Phase 4 adds UI observation to Strobe. A single MCP tool (`debug_ui`) returns a structured tree of all visible UI elements for a running application. Native widgets are captured via platform accessibility APIs (AXUIElement on macOS). Custom-painted widgets (JUCE knobs, game UIs, canvas elements) are detected via an AI vision pipeline (OmniParser v2). The two sources are merged into a unified tree with stable IDs that the LLM can reference across calls.

**Three milestones:**
- **M1:** AX tree capture + `debug_ui(mode="tree")` (AX-only)
- **M2:** Vision sidecar (OmniParser v2) + screenshot capture
- **M3:** Merge pipeline + `debug_ui(mode="both")` + comprehensive testing

---

## Architecture

```
debug_ui(sessionId, mode, vision?)
     │
     ├─ mode="tree" ──► AX Query ──► [AX Tree]
     │                                    │
     │                  Vision Sidecar ◄──┤ (if vision=true)
     │                  (OmniParser v2)   │
     │                       │            │
     │                  [Vision Nodes] ───┤
     │                                    │
     │                  Merge Engine ◄────┘
     │                       │
     │                  [Unified Tree] ──► Compact text / JSON
     │
     ├─ mode="screenshot" ──► ScreenCapture backend ──► Base64 PNG
     │
     └─ mode="both" ──► tree + screenshot in one response
```

### New Source Files

| File | Purpose |
|------|---------|
| `src/ui/mod.rs` | Module root |
| `src/ui/accessibility.rs` | macOS AXUIElement queries, Linux stubs |
| `src/ui/vision.rs` | Sidecar process management, communication protocol |
| `src/ui/capture.rs` | Pluggable screenshot backends (`ScreenCapture` trait) |
| `src/ui/merge.rs` | IoU matching, tree merging, stable ID generation |
| `src/ui/tree.rs` | Unified tree data model, compact text + JSON formatters |
| `vision-sidecar/` | Python package: OmniParser v2 wrapper |

---

## MCP Tool: `debug_ui`

### Request

```typescript
debug_ui({
  sessionId: string,          // Required — resolves PID from session
  mode: "tree" | "screenshot" | "both",  // Required
  vision?: boolean,           // Enable AI vision pass (default: false)
  verbose?: boolean,          // JSON output instead of compact text (default: false)
})
```

### Response

```typescript
{
  tree?: string,              // Compact text (default) or JSON (if verbose=true)
  screenshot?: string,        // Base64 PNG (only if mode includes screenshot)
  stats?: {
    axNodes: number,          // AX nodes found
    visionNodes: number,      // Vision-only nodes added
    mergedNodes: number,      // Nodes matched by both AX and vision
    latencyMs: number,        // Total processing time
  }
}
```

### Compact Text Format (default)

```
[window "ERAE MK2 Simulator" id=w1 bounds=0,0,1200,800]
  [toolbar id=tb1]
    [button "Play" id=btn_play enabled focused]
    [button "Stop" id=btn_stop disabled]
    [slider "Volume" value=0.75 id=sld_vol]
  [panel "Main" id=p1]
    [knob "Filter Cutoff" value≈0.6 id=vk_3 source=vision]
    [knob "Resonance" value≈0.3 id=vk_4 source=vision]
```

Notes:
- `source=vision` tag only appears on vision-detected or merged nodes
- `value≈` prefix for approximate values from vision (vs exact `value=` from AX)
- `bounds=x,y,w,h` included for all nodes that have spatial info
- Indentation reflects parent-child hierarchy (2 spaces per level)

### JSON Format (verbose=true)

```json
{
  "nodes": [{
    "id": "w1",
    "role": "window",
    "title": "ERAE MK2 Simulator",
    "bounds": { "x": 0, "y": 0, "w": 1200, "h": 800 },
    "enabled": true,
    "focused": false,
    "source": "ax",
    "actions": ["AXRaise"],
    "children": [{
      "id": "btn_play",
      "role": "button",
      "title": "Play",
      "bounds": { "x": 10, "y": 5, "w": 80, "h": 30 },
      "enabled": true,
      "focused": true,
      "source": "ax",
      "actions": ["AXPress"],
      "children": []
    }]
  }]
}
```

### Latency Targets

| Mode | Expected Latency |
|------|-----------------|
| `mode="tree"` (AX-only) | <50ms |
| `mode="tree"` + `vision=true` | 0.5–2s |
| `mode="screenshot"` | <50ms |
| `mode="both"` + `vision=true` | 0.5–2s |

### Error Cases

| Condition | Response |
|-----------|----------|
| Session not found | Error: "Session '{id}' not found" |
| Process exited | Error: "Process not running (PID {pid} exited)" |
| No AX permissions | Error: "Accessibility permission required. Grant in System Settings > Privacy & Security > Accessibility" |
| Vision not configured | Warning in stats: "Vision pipeline not enabled. Set vision=true and ensure models are installed." |
| Sidecar crash | Auto-restart. Return AX-only tree with warning: "Vision sidecar restarted, returning AX-only tree." |

---

## Component 1: Accessibility Tree (macOS AXUIElement)

### Implementation

Uses macOS Accessibility framework via `core-foundation` and `accessibility-sys` Rust crates (FFI to `ApplicationServices.framework`).

### Query Flow

1. Get PID from session's `FridaSpawner` state
2. Create `AXUIElementCreateApplication(pid)` reference
3. Walk tree recursively via `AXUIElementCopyAttributeValue`, collecting per node:
   - `kAXRoleAttribute` → role (button, slider, window, etc.)
   - `kAXTitleAttribute` / `kAXDescriptionAttribute` → title/label
   - `kAXValueAttribute` → current value
   - `kAXEnabledAttribute` → enabled state
   - `kAXFocusedAttribute` → focused state
   - `kAXPositionAttribute` + `kAXSizeAttribute` → bounding box (x, y, w, h)
   - `kAXActionsAttribute` → available actions list (prep for Phase 5)
   - `kAXChildrenAttribute` → recursive walk
4. Generate stable IDs (see Stable ID Generation below)

### Permission Handling

- On first call: `AXIsProcessTrustedWithOptions({ kAXTrustedCheckOptionPrompt: true })` — triggers system permission dialog automatically
- If denied: return actionable error message with path to System Settings
- Cache trusted status for session lifetime (re-check on new sessions)

### Platform Abstraction

```rust
pub trait AccessibilityProvider: Send + Sync {
    fn query_tree(&self, pid: u32) -> Result<UiNode>;
    fn is_available(&self) -> bool;
}

// macOS: full implementation via AXUIElement API
pub struct MacOSAccessibility;

// Linux: stub returning UnsupportedPlatform error
pub struct LinuxAccessibility;
impl AccessibilityProvider for LinuxAccessibility {
    fn query_tree(&self, _pid: u32) -> Result<UiNode> {
        Err(Error::UnsupportedPlatform("AT-SPI2 not yet implemented"))
    }
    fn is_available(&self) -> bool { false }
}
```

---

## Component 2: Vision Sidecar (OmniParser v2)

### Architecture

A long-running Python process managed by the Rust daemon. OmniParser v2 (YOLOv8 + Florence-2) detects and classifies UI elements from screenshots. Communicates via JSON over stdin/stdout.

### Directory Structure

```
vision-sidecar/
├── pyproject.toml              # Dependencies: torch, ultralytics, transformers
├── strobe_vision/
│   ├── __init__.py
│   ├── server.py               # Main loop: read JSON from stdin, write JSON to stdout
│   ├── omniparser.py           # OmniParser v2 wrapper (YOLO + Florence-2)
│   ├── models.py               # Model loading, MPS device selection
│   └── protocol.py             # Request/response type definitions
└── models/                     # Bundled with install (~1.5GB total)
    ├── icon_detect/            # YOLOv8 weights (~25MB)
    └── icon_caption/           # Florence-2-base weights (~1.5GB)
```

### Communication Protocol

Newline-delimited JSON over stdin/stdout.

**Detection request (daemon → sidecar):**
```json
{
  "id": "req_001",
  "type": "detect",
  "image": "<base64 PNG>",
  "options": {
    "confidence_threshold": 0.3,
    "iou_threshold": 0.5
  }
}
```

**Detection response (sidecar → daemon):**
```json
{
  "id": "req_001",
  "type": "result",
  "elements": [
    {
      "label": "knob",
      "description": "Filter Cutoff control",
      "confidence": 0.87,
      "bounds": { "x": 340, "y": 200, "w": 60, "h": 60 }
    }
  ],
  "latency_ms": 850
}
```

**Health check:**
```json
{ "id": "health", "type": "ping" }
→ { "id": "health", "type": "pong", "models_loaded": true, "device": "mps" }
```

**Error response:**
```json
{
  "id": "req_001",
  "type": "error",
  "message": "Model loading failed: out of memory"
}
```

### Daemon-Side Management

```rust
pub struct VisionSidecar {
    process: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout_reader: Option<BufReader<ChildStdout>>,
    last_used: Instant,
    idle_timeout: Duration,     // Default: 5 minutes
}

impl VisionSidecar {
    pub async fn detect(&mut self, screenshot: &[u8]) -> Result<Vec<VisionElement>>;
    fn ensure_running(&mut self) -> Result<()>;      // Lazy start
    fn check_idle_timeout(&mut self);                // 5-min idle → SIGTERM
    fn restart_after_crash(&mut self) -> Result<()>; // Auto-recovery
}
```

### Lifecycle

1. **Lazy start:** First `detect()` call spawns `python -m strobe_vision.server`
2. **Health check:** Wait for `pong` response (confirms models loaded, reports device)
3. **Model loading:** ~3-5s on first start (models loaded into MPS GPU memory)
4. **Steady state:** Subsequent calls ~0.5-1.5s (inference only, no model loading)
5. **Idle timeout:** 5 minutes of no requests → `SIGTERM` → frees GPU memory (~1-2GB)
6. **Crash recovery:** Process exit detected → next `detect()` call spawns new process
7. **Shutdown:** Daemon exit → `SIGTERM` to sidecar

### Device Selection

Auto-detect in order: MPS (Apple Silicon GPU) → CUDA → CPU.

MPS is the default for Apple Silicon Macs. PyTorch `device="mps"` provides GPU acceleration without CoreML model conversion. CoreML optimization can be added later for Neural Engine access.

### Feature Flag

Vision is **disabled by default**. Enable via:
- `~/.strobe/settings.json`: `{ "vision": { "enabled": true } }`
- Per-call: `debug_ui(vision=true)` parameter

When disabled, `debug_ui(vision=true)` returns an error: "Vision pipeline not configured. Enable in settings.json."

---

## Component 3: Screenshot Capture

### Pluggable Backend Architecture

```rust
pub trait ScreenCapture: Send + Sync {
    fn capture_window(&self, pid: u32) -> Result<Vec<u8>>; // Returns PNG bytes
    fn is_available(&self) -> bool;
}
```

### Phase 4 Implementation: macOS Window Capture

```rust
pub struct MacOSWindowCapture;
```

Uses `CGWindowListCreateImage` targeting a specific window:
1. `CGWindowListCopyWindowInfo(kCGWindowListOptionAll)` → find windows matching PID
2. Select main window (largest area, on-screen)
3. `CGWindowListCreateImage(windowBounds, kCGWindowListOptionIncludingWindow, windowID)` → capture
4. Convert `CGImage` → PNG bytes via `CGBitmapContext`
5. Expected latency: ~10ms

### Future Backends (not Phase 4)

| Backend | Use Case | API |
|---------|----------|-----|
| `SimulatorCapture` | iOS Simulator | `xcrun simctl io screenshot` |
| `BrowserCapture` | Web apps | Chrome DevTools Protocol (CDP) |
| `LinuxCapture` | Linux desktop | XGetImage / PipeWire |

---

## Component 4: Merge Pipeline

### Merge Algorithm

Combines AX tree (structured, hierarchical) with vision detections (flat, bounding boxes + labels) into a unified tree.

```
AX Tree (structured, hierarchical)     Vision Detections (flat, bbox + label)
         │                                          │
         └──────────── IoU Matching ────────────────┘
                           │
                    ┌──────┴──────┐
                    │             │
              Matched nodes   Unmatched
              (source=merged) ┌────┴────┐
                              │         │
                        AX-only    Vision-only
                        (keep)     (add to tree)
```

### IoU Matching

1. For each vision detection, compute IoU (Intersection over Union) against all AX **leaf** nodes that have bounding boxes
2. IoU ≥ 0.5 → **match**. AX node gains `source=Merged`, vision confidence score, and vision-estimated value if AX value is missing
3. IoU < 0.5 for all AX nodes → **unmatched vision node**. Inserted into tree as child of nearest AX container by spatial containment (vision bbox center inside container bbox)
4. AX nodes with no vision match → kept as `source=Ax` (native widgets with good AX support don't need vision)

### Data Model

```rust
pub struct UiNode {
    pub id: String,              // Stable hash-based ID (e.g., "btn_a3f2")
    pub role: String,            // button, slider, window, knob, etc.
    pub title: Option<String>,   // Label/title text
    pub value: Option<String>,   // Current value (for inputs)
    pub enabled: bool,
    pub focused: bool,
    pub bounds: Option<Rect>,    // x, y, width, height
    pub actions: Vec<String>,    // Available AX actions (prep for Phase 5)
    pub source: NodeSource,      // Ax, Vision, Merged
    pub children: Vec<UiNode>,
}

pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

pub enum NodeSource {
    Ax,
    Vision { confidence: f32 },
    Merged { confidence: f32 },
}
```

### Stable ID Generation

```rust
fn generate_id(node: &UiNode, sibling_index: usize) -> String {
    let prefix = role_prefix(&node.role); // btn, sld, w, knb, txt, lst, itm, pnl, etc.
    let hash_input = format!("{}:{}:{}:{}",
        node.source_type(), node.role,
        node.title.as_deref().unwrap_or(""),
        sibling_index
    );
    let hash = xxhash64(&hash_input);
    format!("{}_{:04x}", prefix, hash & 0xFFFF) // e.g., btn_a3f2
}

fn role_prefix(role: &str) -> &str {
    match role {
        "window" => "w",
        "button" => "btn",
        "slider" => "sld",
        "textField" | "textArea" => "txt",
        "knob" => "knb",
        "list" => "lst",
        "item" | "row" => "itm",
        "toolbar" => "tb",
        "panel" | "group" => "pnl",
        "label" | "staticText" => "lbl",
        "image" => "img",
        "menu" => "mnu",
        "menuItem" => "mi",
        "tab" => "tab",
        "checkbox" | "toggle" => "chk",
        _ => "el",
    }
}
```

IDs are deterministic: same tree structure + same content → same IDs. IDs only change when the widget tree itself changes (widget added/removed/reordered/renamed).

---

## Milestones

### M1: AX Tree + debug_ui (AX-only)

**Scope:**
- `src/ui/accessibility.rs` — macOS AXUIElement implementation + Linux stub
- `src/ui/tree.rs` — UiNode data model, compact text + JSON formatters
- `src/ui/capture.rs` — MacOSWindowCapture (for `mode="screenshot"`)
- `src/ui/mod.rs` — Module wiring
- `debug_ui` MCP tool handler in `server.rs` (tree + screenshot modes, no vision)
- SwiftUI test app (`tests/fixtures/ui-test-app/`)
- Permission handling (auto-prompt + error messages)

**Validation:**
- `debug_ui(mode="tree")` returns correct AX tree for SwiftUI test app
- `debug_ui(mode="screenshot")` returns valid PNG
- Stable IDs unchanged across 10 consecutive calls
- AX-only latency <50ms
- Clear error on permission denied / process exited

### M2: Vision Sidecar + OmniParser

**Scope:**
- `vision-sidecar/` — Python package with OmniParser v2 wrapper
- `src/ui/vision.rs` — Sidecar process management (lazy start, idle timeout, crash recovery)
- Sidecar protocol (JSON stdin/stdout)
- Golden screenshot test fixtures
- Feature flag in settings.json

**Validation:**
- Sidecar starts on first `detect()` call, loads models into MPS
- Detection on golden screenshots matches expected elements (≥80% recall at IoU ≥ 0.5)
- 5-minute idle timeout → graceful shutdown
- Crash → auto-restart → next call succeeds
- `vision=true` without configuration → clear error

### M3: Merge Pipeline + Comprehensive Testing

**Scope:**
- `src/ui/merge.rs` — IoU matching, spatial containment, tree merging
- `debug_ui(vision=true)` end-to-end integration
- `debug_ui(mode="both")` returns tree + screenshot
- All test layers (unit, integration, E2E, field)
- Documentation

**Validation:**
- Merged tree contains both AX and vision-detected nodes
- Vision-only nodes correctly placed in tree hierarchy
- All 10 validation criteria pass (see Testing Strategy)
- Field tests pass on 3 real apps

---

## Testing Strategy

### Test Target: SwiftUI Test App

Located at `tests/fixtures/ui-test-app/`. Minimal macOS SwiftUI app with deterministic layout:

```
┌─ Window "Strobe UI Test" ──────────────────────┐
│ ┌─ Toolbar ─────────────────────────────────┐  │
│ │  [Button "Action"]  [Toggle "Enable"]     │  │
│ └───────────────────────────────────────────┘  │
│ ┌─ Panel ───────────────────────────────────┐  │
│ │  Label: "Volume"                          │  │
│ │  [Slider value=0.5]                       │  │
│ │  [TextField "Name" text="test"]           │  │
│ │  [List]                                   │  │
│ │    [Item "Alpha"]                         │  │
│ │    [Item "Beta"]                          │  │
│ │    [Item "Gamma"]                         │  │
│ └───────────────────────────────────────────┘  │
│ ┌─ Custom Canvas (no AX) ──────────────────┐  │
│ │  (drawn shapes — vision detection only)   │  │
│ └───────────────────────────────────────────┘  │
└────────────────────────────────────────────────┘
```

The custom canvas area deliberately has no accessibility support, testing that the vision pipeline detects painted elements.

### Layer 1: Unit Tests (Rust, no platform deps)

All run on any platform, no AX permissions or GPU required.

| Test | What it validates |
|------|-------------------|
| Tree compact text formatting | Known UiNode tree → expected compact text output |
| Tree JSON formatting | Known UiNode tree → expected JSON output |
| Stable ID generation | Same input → same ID; different input → different ID |
| ID stability over 10 calls | 10x `generate_id` with same input → identical results |
| IoU calculation | Overlapping, non-overlapping, contained, identical boxes |
| Merge: matched nodes | AX + vision with IoU ≥ 0.5 → source=Merged |
| Merge: AX-only nodes | AX node with no vision match → source=Ax, unchanged |
| Merge: vision-only nodes | Vision node with no AX match → source=Vision, placed in nearest container |
| Merge: spatial containment | Vision orphan → correct parent by bbox containment |
| Sidecar protocol serialization | Request/response JSON round-trip |
| Role prefix mapping | All known roles → correct prefix |

### Layer 2: Integration Tests (macOS, requires AX permission)

| Test | What it validates |
|------|-------------------|
| AX tree from test app | Launch SwiftUI app → AX query → verify known widget tree structure |
| AX attribute accuracy | Button titles, slider values, enabled/disabled match expected |
| AX bounds accuracy | Bounding boxes non-zero, reasonable sizes, correct relative positions |
| AX permission check | `AXIsProcessTrusted` returns correct state |
| Screenshot capture | Capture test app window → valid PNG, reasonable dimensions, non-black |
| Screenshot window targeting | Captures only the target window (not full screen) |
| Vision sidecar lifecycle | Start → detect → idle timeout → auto-shutdown → restart |
| Vision on golden images | OmniParser on stored screenshots → expected elements detected |
| Vision accuracy | Detected bboxes vs ground truth: IoU ≥ 0.5 for ≥80% of elements |
| Vision device selection | Sidecar reports MPS device on Apple Silicon |

### Layer 3: End-to-End Tests (full pipeline via MCP)

| Test | What it validates |
|------|-------------------|
| debug_ui tree-only | Launch test app via Frida → `debug_ui(mode="tree")` → compact text output correct |
| debug_ui verbose JSON | `verbose=true` → valid JSON with all attributes |
| debug_ui with vision | `vision=true` → merged tree has both AX and vision nodes |
| debug_ui screenshot | `mode="screenshot"` → valid base64 PNG |
| debug_ui both | `mode="both"` → tree + screenshot in single response |
| Stable IDs across calls | 10 consecutive `debug_ui` calls → zero ID changes |
| Stats accuracy | `stats.axNodes` + `stats.visionNodes` counts match tree contents |
| Concurrent sessions | Two apps running → each gets correct UI tree for its PID |
| Process exit handling | Process exits → `debug_ui` returns clear error |
| Sidecar crash mid-request | Kill sidecar → next call auto-restarts → succeeds |

### Layer 4: Field Tests (manual, documented procedure)

| Test | App | Framework | What it validates |
|------|-----|-----------|-------------------|
| JUCE custom widgets | ERAE MK2 Simulator | JUCE | Vision detects knobs, sliders, custom-painted widgets |
| Native Cocoa | Calculator.app | AppKit | AX tree captures all buttons, display, layout |
| Electron/Web | VS Code | Electron | Mixed AX + web content, deep tree, many nodes |
| No AX | Custom canvas app | None | Vision-only mode, all visible elements detected |

Field test procedures documented in `docs/field-tests/2026-02-XX-ui-observation-*.md`.

### Validation Criteria (all must pass)

1. **AX correctness:** Tree captures all native widgets with correct title, role, value, enabled, focused, bounds
2. **Vision accuracy:** ≥80% of custom widgets detected on golden screenshots (IoU ≥ 0.5)
3. **ID stability:** Stable IDs unchanged across 10 consecutive calls (zero changes)
4. **Latency:** AX-only <50ms, AX+vision <2s
5. **Field tests:** Pass on 3 real apps (ERAE Simulator, Calculator.app, Electron app)
6. **Error handling:** Permission denied, process exit, sidecar crash → all return actionable error messages
7. **Sidecar resilience:** Crash → auto-restart → next call succeeds with no intervention
8. **Memory stability:** No leaks after 100 consecutive `debug_ui` calls
9. **Concurrent sessions:** Two simultaneous apps → correct tree for each PID
10. **Documentation:** Permissions setup, vision configuration, troubleshooting all documented

---

## Configuration

### settings.json

```json
{
  "vision": {
    "enabled": false,
    "confidence_threshold": 0.3,
    "iou_merge_threshold": 0.5,
    "sidecar_idle_timeout_seconds": 300,
    "device": "auto"
  }
}
```

### Permissions (macOS)

System Settings > Privacy & Security > Accessibility — must grant to the terminal or IDE running Strobe.

Auto-prompted on first `debug_ui` call via `AXIsProcessTrustedWithOptions`. If denied, `debug_ui` returns an actionable error with the exact path to the setting.

---

## Dependencies

### Rust crates (new)

| Crate | Purpose |
|-------|---------|
| `core-foundation` | macOS CF types (CFString, CFArray, etc.) |
| `core-graphics` | CGWindowListCreateImage for screenshots |
| `accessibility-sys` or raw FFI | AXUIElement API bindings |
| `xxhash-rust` | Fast non-crypto hashing for stable IDs |

### Python packages (vision-sidecar)

| Package | Purpose |
|---------|---------|
| `torch` | ML framework (MPS backend for Apple Silicon) |
| `ultralytics` | YOLOv8 inference |
| `transformers` | Florence-2 inference |
| `Pillow` | Image processing |

### Model weights (~1.5GB total, bundled)

| Model | Size | Source |
|-------|------|--------|
| YOLOv8 (OmniParser icon detect) | ~25MB | `microsoft/OmniParser-v2.0` |
| Florence-2-base (icon caption) | ~1.5GB | `microsoft/OmniParser-v2.0` |

---

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| AX permission not granted | debug_ui tree returns empty | Auto-prompt + clear error message with system settings path |
| OmniParser accuracy on JUCE widgets | Vision-only nodes may be wrong | Confidence threshold filtering, ≥80% target not 100% |
| Florence-2 slow on CPU (no MPS) | >5s latency | Device auto-detection, warn if falling back to CPU |
| Model weights bloat install size | +1.5GB distribution | Vision is optional feature, weights only needed when enabled |
| AX tree very large (Electron apps) | Token-heavy output | Full tree as designed; LLM can use mode="screenshot" for overview |
| Sidecar Python version conflicts | Import errors | Isolate in venv, pin versions in pyproject.toml |

---

## Non-Goals (Phase 4)

- **UI interaction** (clicking, typing, dragging) — Phase 5
- **Linux AT-SPI2 implementation** — stubs only, full implementation future
- **iOS Simulator capture** — future `SimulatorCapture` backend
- **Web app CDP capture** — future `BrowserCapture` backend
- **Real-time UI diffing** — no change detection between calls
- **CoreML model conversion** — MPS is sufficient for v1; CoreML optimization later

---

## Sources

- [OmniParser v2 (Microsoft Research)](https://www.microsoft.com/en-us/research/articles/omniparser-v2-turning-any-llm-into-a-computer-use-agent/)
- [OmniParser GitHub](https://github.com/microsoft/OmniParser)
- [OmniParser v2 HuggingFace](https://huggingface.co/microsoft/OmniParser-v2.0)
- [YOLOv8 on Apple Silicon benchmarks](https://blog.roboflow.com/putting-the-new-m4-macs-to-the-test/)
- [CoreML YOLOv8 integration](https://github.com/hoangtheanhhp/CodeProject.AI-ObjectDetectionYOLOv8-coreml-apple-silicon-gpu)
