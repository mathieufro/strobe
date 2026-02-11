# Phase 4: UI Observation — Implementation Plan

**Spec:** `docs/specs/2026-02-11-ui-observation.md`
**Goal:** Add `debug_ui` MCP tool that returns structured UI trees via accessibility APIs and AI vision.
**Architecture:** Daemon-side Rust for AX queries, Python sidecar for OmniParser v2 vision, IoU-based merge into unified tree.
**Tech Stack:** `accessibility-sys` + `core-foundation` + `core-graphics` (macOS FFI), Python + PyTorch + ultralytics + transformers (vision sidecar)
**Commit strategy:** Commit per milestone (M1, M2, M3)

## Workstreams

- **Stream A (M1 — AX tree + MCP tool):** Tasks 1–7
- **Stream B (M2 — Vision sidecar):** Tasks 8–11 (independent of A)
- **Serial (M3 — Merge + E2E):** Tasks 12–16 (depends on A and B)

---

## M1: AX Tree + debug_ui Tool

### Task 1: Add error variants and MCP types

**Files:**
- Modify: `src/error.rs`
- Modify: `src/mcp/types.rs`

**Step 1: Add error variants**

In `src/error.rs`, add after the `WriteFailed` variant (line 36):

```rust
    #[error("UI_QUERY_FAILED: {0}")]
    UiQueryFailed(String),

    #[error("UI_NOT_AVAILABLE: {0}")]
    UiNotAvailable(String),
```

**Step 2: Add ErrorCode variants and mapping**

In `src/mcp/types.rs`, add `UiQueryFailed` to the `ErrorCode` enum (after `WriteFailed`, line 669):

```rust
    UiQueryFailed,
```

Add mapping in `From<crate::Error> for McpError` (after the `WriteFailed` arm, line 693):

```rust
            crate::Error::UiQueryFailed(_) => ErrorCode::UiQueryFailed,
            crate::Error::UiNotAvailable(_) => ErrorCode::UiQueryFailed,
```

**Step 3: Add MCP request/response types**

At the end of `src/mcp/types.rs` (before the `#[cfg(test)]` block at line 2040), add:

```rust
// ============ debug_ui ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiMode {
    Tree,
    Screenshot,
    Both,
}

impl Default for UiMode {
    fn default() -> Self { Self::Tree }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiRequest {
    pub session_id: String,
    #[serde(default)]
    pub mode: UiMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verbose: Option<bool>,
}

impl DebugUiRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string()
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiStats {
    pub ax_nodes: usize,
    pub vision_nodes: usize,
    pub merged_nodes: usize,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<UiStats>,
}
```

**Step 4: Add serde tests for new types**

In the `#[cfg(test)]` module of `src/mcp/types.rs`, add:

```rust
    #[test]
    fn test_debug_ui_request_serde() {
        let req: DebugUiRequest = serde_json::from_str(r#"{"sessionId": "s1", "mode": "tree"}"#).unwrap();
        assert_eq!(req.session_id, "s1");
        assert_eq!(req.mode, UiMode::Tree);
        assert!(req.vision.is_none());
    }

    #[test]
    fn test_debug_ui_request_validation() {
        let req = DebugUiRequest {
            session_id: "".to_string(),
            mode: UiMode::Tree,
            vision: None,
            verbose: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_ui_response_serde() {
        let resp = DebugUiResponse {
            tree: Some("[window \"Test\" id=w1]".to_string()),
            screenshot: None,
            stats: Some(UiStats { ax_nodes: 5, vision_nodes: 0, merged_nodes: 0, latency_ms: 12 }),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("tree").is_some());
        assert!(json.get("screenshot").is_none()); // skip_serializing_if
        assert_eq!(json["stats"]["axNodes"], 5);
    }
```

**Checkpoint:** MCP types compile. `cargo test --lib` passes including new serde tests.

---

### Task 2: Create UI tree data model and formatters

**Files:**
- Create: `src/ui/mod.rs`
- Create: `src/ui/tree.rs`
- Modify: `src/lib.rs` (add `pub mod ui;`)

**Step 1: Write unit tests for tree formatting**

In `src/ui/tree.rs`, add the data model and tests:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeSource {
    Ax,
    Vision { confidence: f32 },
    Merged { confidence: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    pub id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub actions: Vec<String>,
    pub source: NodeSource,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<UiNode>,
}

/// Role to short prefix for stable IDs.
pub fn role_prefix(role: &str) -> &str {
    match role {
        "window" | "AXWindow" => "w",
        "button" | "AXButton" => "btn",
        "slider" | "AXSlider" => "sld",
        "textField" | "AXTextField" | "textArea" | "AXTextArea" => "txt",
        "knob" => "knb",
        "list" | "AXList" | "AXTable" => "lst",
        "row" | "AXRow" | "cell" | "AXCell" => "itm",
        "toolbar" | "AXToolbar" => "tb",
        "group" | "AXGroup" | "AXSplitGroup" => "pnl",
        "staticText" | "AXStaticText" => "lbl",
        "image" | "AXImage" => "img",
        "menu" | "AXMenu" | "menuBar" | "AXMenuBar" => "mnu",
        "menuItem" | "AXMenuItem" => "mi",
        "tabGroup" | "AXTabGroup" => "tab",
        "checkbox" | "AXCheckBox" => "chk",
        "radioButton" | "AXRadioButton" => "rad",
        "popUpButton" | "AXPopUpButton" | "comboBox" | "AXComboBox" => "pop",
        "scrollArea" | "AXScrollArea" => "scr",
        "progressIndicator" | "AXProgressIndicator" => "prg",
        _ => "el",
    }
}

/// Generate a stable ID from role, title, and sibling index.
/// Uses a simple hash to keep IDs short and deterministic.
pub fn generate_id(role: &str, title: Option<&str>, sibling_index: usize) -> String {
    let prefix = role_prefix(role);
    let hash_input = format!("{}:{}:{}", role, title.unwrap_or(""), sibling_index);
    // Simple FNV-1a hash for speed (no crypto needed)
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in hash_input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{}_{:04x}", prefix, hash & 0xFFFF)
}

/// Format a tree as compact indented text.
pub fn format_compact(nodes: &[UiNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        format_node(&mut out, node, 0);
    }
    out
}

fn format_node(out: &mut String, node: &UiNode, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push('[');
    out.push_str(&node.role);

    if let Some(ref title) = node.title {
        out.push_str(&format!(" \"{}\"", title));
    }

    out.push_str(&format!(" id={}", node.id));

    if let Some(ref bounds) = node.bounds {
        out.push_str(&format!(" bounds={},{},{},{}",
            bounds.x as i64, bounds.y as i64, bounds.w as i64, bounds.h as i64));
    }

    if let Some(ref value) = node.value {
        match &node.source {
            NodeSource::Vision { .. } => out.push_str(&format!(" value≈{}", value)),
            _ => out.push_str(&format!(" value={}", value)),
        }
    }

    if node.enabled {
        out.push_str(" enabled");
    }
    if node.focused {
        out.push_str(" focused");
    }

    match &node.source {
        NodeSource::Vision { .. } => out.push_str(" source=vision"),
        NodeSource::Merged { .. } => out.push_str(" source=merged"),
        NodeSource::Ax => {} // default, no tag
    }

    out.push_str("]\n");

    for child in &node.children {
        format_node(out, child, depth + 1);
    }
}

/// Format a tree as JSON string.
pub fn format_json(nodes: &[UiNode]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&serde_json::json!({ "nodes": nodes }))
}

/// Count nodes recursively.
pub fn count_nodes(nodes: &[UiNode]) -> usize {
    nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> Vec<UiNode> {
        vec![UiNode {
            id: "w_0001".to_string(),
            role: "window".to_string(),
            title: Some("Test App".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![
                UiNode {
                    id: "btn_a1b2".to_string(),
                    role: "button".to_string(),
                    title: Some("Play".to_string()),
                    value: None,
                    enabled: true,
                    focused: true,
                    bounds: Some(Rect { x: 10.0, y: 5.0, w: 80.0, h: 30.0 }),
                    actions: vec!["AXPress".to_string()],
                    source: NodeSource::Ax,
                    children: vec![],
                },
                UiNode {
                    id: "knb_c3d4".to_string(),
                    role: "knob".to_string(),
                    title: Some("Filter".to_string()),
                    value: Some("0.6".to_string()),
                    enabled: true,
                    focused: false,
                    bounds: Some(Rect { x: 100.0, y: 50.0, w: 60.0, h: 60.0 }),
                    actions: vec![],
                    source: NodeSource::Vision { confidence: 0.87 },
                    children: vec![],
                },
            ],
        }]
    }

    #[test]
    fn test_compact_format() {
        let tree = sample_tree();
        let text = format_compact(&tree);
        assert!(text.contains("[window \"Test App\" id=w_0001"));
        assert!(text.contains("bounds=0,0,800,600"));
        assert!(text.contains("  [button \"Play\" id=btn_a1b2"));
        assert!(text.contains("enabled focused]"));
        assert!(text.contains("  [knob \"Filter\" id=knb_c3d4"));
        assert!(text.contains("value≈0.6")); // vision value uses ≈
        assert!(text.contains("source=vision"));
    }

    #[test]
    fn test_json_format() {
        let tree = sample_tree();
        let json = format_json(&tree).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["nodes"][0]["role"], "window");
        assert_eq!(parsed["nodes"][0]["children"][0]["title"], "Play");
    }

    #[test]
    fn test_stable_id_generation() {
        let id1 = generate_id("button", Some("Play"), 0);
        let id2 = generate_id("button", Some("Play"), 0);
        let id3 = generate_id("button", Some("Stop"), 0);
        let id4 = generate_id("button", Some("Play"), 1);
        assert_eq!(id1, id2); // same input → same ID
        assert_ne!(id1, id3); // different title → different ID
        assert_ne!(id1, id4); // different index → different ID
        assert!(id1.starts_with("btn_")); // correct prefix
    }

    #[test]
    fn test_role_prefix() {
        assert_eq!(role_prefix("window"), "w");
        assert_eq!(role_prefix("AXButton"), "btn");
        assert_eq!(role_prefix("slider"), "sld");
        assert_eq!(role_prefix("unknownWidget"), "el");
    }

    #[test]
    fn test_count_nodes() {
        let tree = sample_tree();
        assert_eq!(count_nodes(&tree), 3); // window + button + knob
    }
}
```

**Step 2: Create module root**

In `src/ui/mod.rs`:

```rust
pub mod tree;

#[cfg(target_os = "macos")]
pub mod accessibility;

#[cfg(target_os = "macos")]
pub mod capture;
```

**Step 3: Register module**

In `src/lib.rs`, add after `pub mod test;` (line 10):

```rust
pub mod ui;
```

**Step 4: Run tests**

```bash
cargo test ui::tree --lib
```

Expected: All 5 tree tests pass.

**Checkpoint:** `UiNode` data model works. Compact text and JSON formatters produce correct output. Stable IDs are deterministic.

---

### Task 3: Implement macOS accessibility provider

**Files:**
- Create: `src/ui/accessibility.rs`
- Modify: `Cargo.toml` (add macOS-specific dependencies)

**Step 1: Add dependencies**

In `Cargo.toml`, add at the end (after dev-dependencies):

```toml
[target.'cfg(target_os = "macos")'.dependencies]
accessibility-sys = "0.2"
core-foundation = "0.10"
core-foundation-sys = "0.8"
core-graphics = "0.24"
```

**Step 2: Implement the accessibility provider**

Create `src/ui/accessibility.rs`:

```rust
//! macOS AXUIElement-based accessibility tree queries.
//!
//! Walks the accessibility tree for a given PID, collecting role, title, value,
//! enabled, focused, bounds, and actions for each element.

use crate::ui::tree::{generate_id, NodeSource, Rect, UiNode};
use crate::Result;
use accessibility_sys::*;
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_foundation::array::CFArray;
use core_foundation_sys::base::{CFTypeRef, CFGetTypeID};
use core_foundation_sys::string::CFStringRef;
use core_foundation_sys::number::CFNumberRef;
use std::ffi::c_void;

/// Check if this process has accessibility permissions.
/// If `prompt` is true, shows the system dialog asking user to grant permission.
pub fn check_accessibility_permission(prompt: bool) -> bool {
    unsafe {
        if prompt {
            let key = CFString::new("AXTrustedCheckOptionPrompt");
            let value = CFBoolean::true_value();
            let keys = [key.as_concrete_TypeRef() as *const c_void];
            let values = [value.as_concrete_TypeRef() as *const c_void];
            let options = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&keys, &values);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as _) != 0
        } else {
            AXIsProcessTrusted() != 0
        }
    }
}

/// Query the accessibility tree for a process by PID.
pub fn query_ax_tree(pid: u32) -> Result<Vec<UiNode>> {
    // Check permission first
    if !check_accessibility_permission(false) {
        // Try with prompt on first call
        if !check_accessibility_permission(true) {
            return Err(crate::Error::UiNotAvailable(
                "Accessibility permission required. Grant in System Settings > Privacy & Security > Accessibility".to_string()
            ));
        }
    }

    unsafe {
        let app_ref = AXUIElementCreateApplication(pid as i32);
        if app_ref.is_null() {
            return Err(crate::Error::UiQueryFailed(
                format!("Failed to create AXUIElement for PID {}", pid)
            ));
        }

        // Get windows
        let mut children = Vec::new();
        let windows = get_ax_children(app_ref);
        for (i, window_ref) in windows.iter().enumerate() {
            if let Some(node) = build_node(*window_ref, i) {
                children.push(node);
            }
        }

        CFRelease(app_ref as *const c_void);

        // Also release window refs
        for w in &windows {
            CFRelease(*w as *const c_void);
        }

        Ok(children)
    }
}

/// Recursively build a UiNode from an AXUIElementRef.
unsafe fn build_node(element: AXUIElementRef, sibling_index: usize) -> Option<UiNode> {
    let role = get_ax_string(element, kAXRoleAttribute)?;
    let title = get_ax_string(element, kAXTitleAttribute)
        .or_else(|| get_ax_string(element, kAXDescriptionAttribute));
    let value = get_ax_value_string(element);
    let enabled = get_ax_bool(element, kAXEnabledAttribute).unwrap_or(true);
    let focused = get_ax_bool(element, kAXFocusedAttribute).unwrap_or(false);
    let bounds = get_ax_bounds(element);
    let actions = get_ax_actions(element);

    let id = generate_id(&role, title.as_deref(), sibling_index);

    // Recurse into children
    let child_refs = get_ax_children(element);
    let mut children = Vec::new();
    for (i, child_ref) in child_refs.iter().enumerate() {
        if let Some(child_node) = build_node(*child_ref, i) {
            children.push(child_node);
        }
    }
    for c in &child_refs {
        CFRelease(*c as *const c_void);
    }

    Some(UiNode {
        id,
        role,
        title,
        value,
        enabled,
        focused,
        bounds,
        actions,
        source: NodeSource::Ax,
        children,
    })
}

/// Get a string attribute from an AX element.
unsafe fn get_ax_string(element: AXUIElementRef, attribute: CFStringRef) -> Option<String> {
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attribute, &mut value);
    if err != 0 || value.is_null() {
        return None;
    }
    // Verify it's a CFString
    if CFGetTypeID(value) != core_foundation_sys::string::CFStringGetTypeID() {
        CFRelease(value);
        return None;
    }
    let cf_str = CFString::wrap_under_get_rule(value as CFStringRef);
    let result = cf_str.to_string();
    Some(result)
}

/// Get value attribute as string (handles CFString, CFNumber, etc.).
unsafe fn get_ax_value_string(element: AXUIElementRef) -> Option<String> {
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, kAXValueAttribute, &mut value);
    if err != 0 || value.is_null() {
        return None;
    }

    let type_id = CFGetTypeID(value);
    let result = if type_id == core_foundation_sys::string::CFStringGetTypeID() {
        let cf_str = CFString::wrap_under_get_rule(value as CFStringRef);
        Some(cf_str.to_string())
    } else if type_id == core_foundation_sys::number::CFNumberGetTypeID() {
        // Read as f64
        let mut f: f64 = 0.0;
        if core_foundation_sys::number::CFNumberGetValue(
            value as CFNumberRef,
            core_foundation_sys::number::kCFNumberFloat64Type,
            &mut f as *mut f64 as *mut c_void,
        ) != 0 {
            Some(format!("{}", f))
        } else {
            None
        }
    } else {
        None
    };

    CFRelease(value);
    result
}

/// Get a boolean attribute.
unsafe fn get_ax_bool(element: AXUIElementRef, attribute: CFStringRef) -> Option<bool> {
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attribute, &mut value);
    if err != 0 || value.is_null() {
        return None;
    }
    if CFGetTypeID(value) != core_foundation_sys::boolean::CFBooleanGetTypeID() {
        CFRelease(value);
        return None;
    }
    let result = core_foundation_sys::number::CFBooleanGetValue(value as _) != 0;
    CFRelease(value);
    Some(result)
}

/// Get bounding box (position + size).
unsafe fn get_ax_bounds(element: AXUIElementRef) -> Option<Rect> {
    // Position
    let mut pos_value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, kAXPositionAttribute, &mut pos_value);
    if err != 0 || pos_value.is_null() {
        return None;
    }

    let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
    if AXValueGetValue(
        pos_value as AXValueRef,
        kAXValueTypeCGPoint,
        &mut point as *mut _ as *mut c_void,
    ) == 0 {
        CFRelease(pos_value);
        return None;
    }
    CFRelease(pos_value);

    // Size
    let mut size_value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, kAXSizeAttribute, &mut size_value);
    if err != 0 || size_value.is_null() {
        return None;
    }

    let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);
    if AXValueGetValue(
        size_value as AXValueRef,
        kAXValueTypeCGSize,
        &mut size as *mut _ as *mut c_void,
    ) == 0 {
        CFRelease(size_value);
        return None;
    }
    CFRelease(size_value);

    Some(Rect {
        x: point.x,
        y: point.y,
        w: size.width,
        h: size.height,
    })
}

/// Get children elements.
unsafe fn get_ax_children(element: AXUIElementRef) -> Vec<AXUIElementRef> {
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, kAXChildrenAttribute, &mut value);
    if err != 0 || value.is_null() {
        return vec![];
    }

    if CFGetTypeID(value) != core_foundation_sys::array::CFArrayGetTypeID() {
        CFRelease(value);
        return vec![];
    }

    let array = CFArray::<*const c_void>::wrap_under_create_rule(value as _);
    let len = array.len();
    let mut result = Vec::with_capacity(len as usize);
    for i in 0..len {
        let child = array.get(i) as AXUIElementRef;
        // Retain each child since we'll use them after the array is released
        core_foundation_sys::base::CFRetain(child as *const c_void);
        result.push(child);
    }
    result
}

/// Get available actions.
unsafe fn get_ax_actions(element: AXUIElementRef) -> Vec<String> {
    let mut names: core_foundation_sys::array::CFArrayRef = std::ptr::null();
    let err = AXUIElementCopyActionNames(element, &mut names);
    if err != 0 || names.is_null() {
        return vec![];
    }

    let array = CFArray::<*const c_void>::wrap_under_create_rule(names as _);
    let mut result = Vec::new();
    for i in 0..array.len() {
        let name = array.get(i) as CFStringRef;
        let cf_str = CFString::wrap_under_get_rule(name);
        result.push(cf_str.to_string());
    }
    result
}
```

**Step 3: Verify it compiles**

```bash
cargo build 2>&1 | head -20
```

Expected: Compiles on macOS. May need minor FFI signature adjustments depending on exact `accessibility-sys` API.

**Checkpoint:** AX tree query compiles and can walk AXUIElement trees. Not yet wired to MCP tool.

---

### Task 4: Implement macOS screenshot capture

**Files:**
- Create: `src/ui/capture.rs`

**Step 1: Implement screenshot capture**

Create `src/ui/capture.rs`:

```rust
//! Screenshot capture via macOS CGWindowListCreateImage.

use crate::Result;
use core_graphics::display::*;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::dictionary::CFDictionary;
use core_foundation::array::CFArray;
use core_graphics::image::CGImage;
use std::io::Write;

/// Capture a screenshot of the main window for a given PID.
/// Returns PNG bytes.
pub fn capture_window_screenshot(pid: u32) -> Result<Vec<u8>> {
    unsafe {
        // Find the main window for this PID
        let window_id = find_main_window(pid)?;

        // Capture just that window
        let image = CGDisplay::screenshot(
            CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0)),
            kCGWindowListOptionIncludingWindow,
            window_id,
            kCGWindowImageBoundsIgnoreFraming,
        );

        match image {
            Some(img) => cg_image_to_png(&img),
            None => Err(crate::Error::UiQueryFailed(
                format!("Failed to capture screenshot for PID {} (window {})", pid, window_id)
            )),
        }
    }
}

/// Find the main (largest, on-screen) window for a PID.
unsafe fn find_main_window(pid: u32) -> Result<CGWindowID> {
    let windows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    );

    if windows.is_null() {
        return Err(crate::Error::UiQueryFailed(
            "Failed to list windows".to_string()
        ));
    }

    let window_list = CFArray::<CFDictionary<CFString, CFNumber>>::wrap_under_create_rule(windows as _);
    let pid_key = CFString::new("kCGWindowOwnerPID");
    let id_key = CFString::new("kCGWindowNumber");
    let bounds_key = CFString::new("kCGWindowBounds");

    let mut best_window: Option<(CGWindowID, f64)> = None;

    for i in 0..window_list.len() {
        let dict = window_list.get(i);
        // This is a raw CFDictionary — we need to use Core Foundation getters
        let dict_ref = dict as core_foundation_sys::dictionary::CFDictionaryRef;

        // Check PID
        let mut pid_val: *const std::ffi::c_void = std::ptr::null();
        if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
            dict_ref, pid_key.as_concrete_TypeRef() as _, &mut pid_val
        ) == 0 {
            continue;
        }
        let window_pid = CFNumber::wrap_under_get_rule(pid_val as _);
        if window_pid.to_i64().unwrap_or(0) as u32 != pid {
            continue;
        }

        // Get window ID
        let mut id_val: *const std::ffi::c_void = std::ptr::null();
        if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
            dict_ref, id_key.as_concrete_TypeRef() as _, &mut id_val
        ) == 0 {
            continue;
        }
        let window_id = CFNumber::wrap_under_get_rule(id_val as _)
            .to_i64().unwrap_or(0) as CGWindowID;

        // Get bounds for area calculation
        let mut bounds_val: *const std::ffi::c_void = std::ptr::null();
        let area = if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
            dict_ref, bounds_key.as_concrete_TypeRef() as _, &mut bounds_val
        ) != 0 {
            // Parse bounds dict for Width/Height
            let bounds_dict = bounds_val as core_foundation_sys::dictionary::CFDictionaryRef;
            let w_key = CFString::new("Width");
            let h_key = CFString::new("Height");
            let mut w_val: *const std::ffi::c_void = std::ptr::null();
            let mut h_val: *const std::ffi::c_void = std::ptr::null();
            let w = if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                bounds_dict, w_key.as_concrete_TypeRef() as _, &mut w_val
            ) != 0 {
                CFNumber::wrap_under_get_rule(w_val as _).to_f64().unwrap_or(0.0)
            } else { 0.0 };
            let h = if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                bounds_dict, h_key.as_concrete_TypeRef() as _, &mut h_val
            ) != 0 {
                CFNumber::wrap_under_get_rule(h_val as _).to_f64().unwrap_or(0.0)
            } else { 0.0 };
            w * h
        } else {
            0.0
        };

        if best_window.is_none() || area > best_window.unwrap().1 {
            best_window = Some((window_id, area));
        }
    }

    best_window
        .map(|(id, _)| id)
        .ok_or_else(|| crate::Error::UiQueryFailed(
            format!("No visible window found for PID {}", pid)
        ))
}

/// Convert CGImage to PNG bytes.
fn cg_image_to_png(image: &CGImage) -> Result<Vec<u8>> {
    let width = image.width();
    let height = image.height();
    let bytes_per_row = image.bytes_per_row();
    let data = image.data();
    let bytes = data.bytes();

    // Use a minimal PNG encoder
    let mut png_data = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_data, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header()
        .map_err(|e| crate::Error::UiQueryFailed(format!("PNG encode error: {}", e)))?;

    // CGImage is BGRA, PNG expects RGBA — swap channels
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row_start = y * bytes_per_row;
        for x in 0..width {
            let offset = row_start + x * 4;
            if offset + 3 < bytes.len() {
                rgba.push(bytes[offset + 2]); // R (from B)
                rgba.push(bytes[offset + 1]); // G
                rgba.push(bytes[offset]);     // B (from R)
                rgba.push(bytes[offset + 3]); // A
            }
        }
    }

    writer.write_image_data(&rgba)
        .map_err(|e| crate::Error::UiQueryFailed(format!("PNG write error: {}", e)))?;

    drop(writer);
    Ok(png_data)
}
```

**Step 2: Add png crate dependency**

In `Cargo.toml`, add to `[dependencies]`:

```toml
png = "0.17"
```

**Step 3: Add `base64` for encoding screenshots in responses**

In `Cargo.toml`, add to `[dependencies]`:

```toml
base64 = "0.22"
```

**Checkpoint:** Screenshot capture compiles. Can capture a window for a given PID and return PNG bytes.

---

### Task 5: Wire debug_ui MCP tool handler

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Add tool registration**

In `handle_tools_list()`, add before the closing `];` (line 940):

```rust
            McpTool {
                name: "debug_ui".to_string(),
                description: "Query the UI state of a running process. Returns accessibility tree (native widgets) and optionally AI-detected custom widgets. Use mode to select output.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "sessionId": { "type": "string", "description": "Session ID (from debug_launch)" },
                        "mode": { "type": "string", "enum": ["tree", "screenshot", "both"], "description": "Output mode: tree (UI element hierarchy), screenshot (PNG image), or both" },
                        "vision": { "type": "boolean", "description": "Enable AI vision pass for custom widgets (default: false). Requires vision sidecar." },
                        "verbose": { "type": "boolean", "description": "Return JSON instead of compact text (default: false)" }
                    },
                    "required": ["sessionId", "mode"]
                }),
            },
```

**Step 2: Add dispatch entry**

In `handle_tools_call()`, add before the `_ => Err(...)` arm (line 983):

```rust
            "debug_ui" => self.tool_debug_ui(&call.arguments).await,
```

**Step 3: Implement handler**

Add method to `Daemon` impl (after `tool_debug_logpoint`):

```rust
    async fn tool_debug_ui(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
        let req: crate::mcp::DebugUiRequest = serde_json::from_value(args.clone())?;
        req.validate()?;

        let session = self.require_session(&req.session_id)?;
        if session.status != crate::db::SessionStatus::Running {
            return Err(crate::Error::UiQueryFailed(
                format!("Process not running (PID {} exited). Cannot query UI.", session.pid)
            ));
        }

        let start = std::time::Instant::now();
        let vision_requested = req.vision.unwrap_or(false);
        let verbose = req.verbose.unwrap_or(false);

        let mut tree_output = None;
        let mut screenshot_output = None;
        let mut ax_count = 0;
        let vision_count = 0;
        let merged_count = 0;

        let needs_tree = matches!(req.mode, crate::mcp::UiMode::Tree | crate::mcp::UiMode::Both);
        let needs_screenshot = matches!(req.mode, crate::mcp::UiMode::Screenshot | crate::mcp::UiMode::Both);

        // Query AX tree
        if needs_tree {
            #[cfg(target_os = "macos")]
            {
                let pid = session.pid;
                let nodes = tokio::task::spawn_blocking(move || {
                    crate::ui::accessibility::query_ax_tree(pid)
                }).await.map_err(|e| crate::Error::Internal(format!("AX query task failed: {}", e)))??;

                ax_count = crate::ui::tree::count_nodes(&nodes);

                // TODO (M2): If vision_requested, run vision sidecar and merge
                if vision_requested {
                    tracing::warn!("Vision pipeline not yet implemented, returning AX-only tree");
                }

                tree_output = Some(if verbose {
                    crate::ui::tree::format_json(&nodes)?
                } else {
                    crate::ui::tree::format_compact(&nodes)
                });
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Err(crate::Error::UiNotAvailable(
                    "UI observation is only supported on macOS".to_string()
                ));
            }
        }

        // Capture screenshot
        if needs_screenshot {
            #[cfg(target_os = "macos")]
            {
                let pid = session.pid;
                let png_bytes = tokio::task::spawn_blocking(move || {
                    crate::ui::capture::capture_window_screenshot(pid)
                }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

                use base64::Engine;
                screenshot_output = Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes));
            }

            #[cfg(not(target_os = "macos"))]
            {
                return Err(crate::Error::UiNotAvailable(
                    "Screenshot capture is only supported on macOS".to_string()
                ));
            }
        }

        let latency_ms = start.elapsed().as_millis() as u64;

        let response = crate::mcp::DebugUiResponse {
            tree: tree_output,
            screenshot: screenshot_output,
            stats: Some(crate::mcp::UiStats {
                ax_nodes: ax_count,
                vision_nodes: vision_count,
                merged_nodes: merged_count,
                latency_ms,
            }),
        };

        Ok(serde_json::to_value(response)?)
    }
```

**Step 4: Verify it compiles and the handler is reachable**

```bash
cargo build 2>&1 | head -20
```

**Checkpoint:** `debug_ui` tool is registered and dispatched. Can query AX trees and capture screenshots for running sessions. Vision returns a warning stub.

---

### Task 6: Build SwiftUI test app

**Files:**
- Create: `tests/fixtures/ui-test-app/UITestApp.swift`
- Create: `tests/fixtures/ui-test-app/build.sh`

**Step 1: Create the SwiftUI test app**

Create `tests/fixtures/ui-test-app/UITestApp.swift`:

```swift
import SwiftUI

@main
struct UITestApp: App {
    var body: some Scene {
        WindowGroup {
            ContentView()
        }
    }
}

struct ContentView: View {
    @State private var sliderValue: Double = 0.5
    @State private var textValue: String = "test"
    @State private var toggleValue: Bool = true
    @State private var selectedItem: String? = nil

    var body: some View {
        VStack(spacing: 16) {
            // Toolbar area
            HStack {
                Button("Action") {
                    print("ACTION_PRESSED")
                }
                .accessibilityIdentifier("action_button")

                Toggle("Enable", isOn: $toggleValue)
                    .accessibilityIdentifier("enable_toggle")
            }
            .padding()

            Divider()

            // Main panel
            VStack(alignment: .leading, spacing: 12) {
                Text("Volume")
                    .accessibilityIdentifier("volume_label")

                Slider(value: $sliderValue, in: 0...1)
                    .accessibilityIdentifier("volume_slider")
                    .accessibilityValue("\(sliderValue)")

                TextField("Name", text: $textValue)
                    .accessibilityIdentifier("name_field")
                    .textFieldStyle(.roundedBorder)

                List {
                    Text("Alpha").tag("Alpha")
                    Text("Beta").tag("Beta")
                    Text("Gamma").tag("Gamma")
                }
                .accessibilityIdentifier("items_list")
                .frame(height: 120)
            }
            .padding()

            // Custom canvas (no accessibility)
            Canvas { context, size in
                context.fill(
                    Path(ellipseIn: CGRect(x: 20, y: 10, width: 60, height: 60)),
                    with: .color(.blue)
                )
                context.fill(
                    Path(CGRect(x: 100, y: 10, width: 80, height: 40)),
                    with: .color(.red)
                )
            }
            .frame(height: 80)
            .accessibilityHidden(true)  // Deliberately hidden from AX

            Spacer()
        }
        .frame(width: 400, height: 500)
    }
}
```

**Step 2: Create build script**

Create `tests/fixtures/ui-test-app/build.sh`:

```bash
#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"

mkdir -p "$BUILD_DIR"

# Compile as a macOS app bundle
swiftc \
    -o "$BUILD_DIR/UITestApp" \
    -framework SwiftUI \
    -framework AppKit \
    "$SCRIPT_DIR/UITestApp.swift" \
    2>&1

echo "Built: $BUILD_DIR/UITestApp"
```

```bash
chmod +x tests/fixtures/ui-test-app/build.sh
```

**Step 3: Build and verify**

```bash
cd tests/fixtures/ui-test-app && bash build.sh
```

**Checkpoint:** SwiftUI test app builds and can be launched. Has known widget hierarchy for deterministic testing.

---

### Task 7: Write integration and E2E tests (M1)

**Files:**
- Create: `tests/ui_observation.rs`
- Modify: `tests/common/mod.rs` (add UI test helper)

**Step 1: Add UI test app helper to common**

In `tests/common/mod.rs`, add:

```rust
/// Build and return the SwiftUI UI test app path.
pub fn ui_test_app() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ui-test-app");
            let binary = fixture_dir.join("build/UITestApp");

            if !binary.exists() || needs_rebuild(&[&fixture_dir.join("UITestApp.swift")], &binary) {
                eprintln!("Building SwiftUI UI test app...");
                let status = Command::new("bash")
                    .arg(fixture_dir.join("build.sh"))
                    .current_dir(&fixture_dir)
                    .status()
                    .expect("Failed to run build.sh");
                assert!(status.success(), "UI test app build failed");
            } else {
                eprintln!("UI test app up-to-date, skipping build");
            }

            assert!(binary.exists(), "UI test app not found after build: {:?}", binary);
            binary
        })
        .clone()
}
```

**Step 2: Create integration test**

Create `tests/ui_observation.rs`:

```rust
//! Phase 4 — UI Observation integration tests.
//! Requires macOS with Accessibility permissions granted.

mod common;

#[cfg(target_os = "macos")]
mod macos_tests {
    use super::common::*;
    use std::time::Duration;

    // ---- Unit-level tests (no app needed) ----

    #[test]
    fn test_stable_ids_deterministic() {
        use strobe::ui::tree::generate_id;
        for _ in 0..10 {
            let id = generate_id("button", Some("Play"), 0);
            assert_eq!(id, generate_id("button", Some("Play"), 0));
        }
    }

    // ---- Integration tests (need running app + AX permissions) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ax_tree_from_test_app() {
        // Launch UI test app
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-ax-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Give the app time to render its window
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Query AX tree
        let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
        assert!(!nodes.is_empty(), "Should find at least one window");

        // Verify we got a window
        let window = &nodes[0];
        assert!(window.role.contains("Window") || window.role == "window",
            "First node should be a window, got: {}", window.role);

        // Verify tree has children
        let total = strobe::ui::tree::count_nodes(&nodes);
        assert!(total >= 3, "Expected at least 3 nodes, got {}", total);

        // Verify compact text format
        let text = strobe::ui::tree::format_compact(&nodes);
        assert!(text.contains("id="), "Compact text should contain IDs");

        // Verify stable IDs across calls
        let nodes2 = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
        let text2 = strobe::ui::tree::format_compact(&nodes2);
        assert_eq!(text, text2, "IDs should be stable across consecutive calls");

        // Cleanup
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_screenshot_capture() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-screenshot-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let png_bytes = strobe::ui::capture::capture_window_screenshot(pid).unwrap();
        assert!(png_bytes.len() > 100, "PNG should be non-trivial, got {} bytes", png_bytes.len());

        // Verify PNG header
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47], "Should be valid PNG");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ax_latency_under_50ms() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-latency-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let start = std::time::Instant::now();
        let _nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed.as_millis() < 50, "AX query should be <50ms, took {}ms", elapsed.as_millis());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[test]
    fn test_ax_query_invalid_pid() {
        // PID 999999 shouldn't exist
        let result = strobe::ui::accessibility::query_ax_tree(999999);
        // Should return empty tree or error, not panic
        match result {
            Ok(nodes) => assert!(nodes.is_empty(), "Invalid PID should return empty tree"),
            Err(_) => {} // Error is also acceptable
        }
    }
}
```

**Step 3: Run tests**

```bash
cargo test ui_observation --test ui_observation -- --test-threads=1
```

Expected: Tests pass on macOS with Accessibility permissions. Tests verify tree structure, stable IDs, screenshot validity, and latency.

**Checkpoint:** M1 complete. `debug_ui` works for AX-only tree queries and screenshots. Commit M1.

---

## M2: Vision Sidecar + OmniParser

### Task 8: Create vision sidecar Python package

**Files:**
- Create: `vision-sidecar/pyproject.toml`
- Create: `vision-sidecar/strobe_vision/__init__.py`
- Create: `vision-sidecar/strobe_vision/protocol.py`
- Create: `vision-sidecar/strobe_vision/models.py`
- Create: `vision-sidecar/strobe_vision/omniparser.py`
- Create: `vision-sidecar/strobe_vision/server.py`

**Step 1: Create pyproject.toml**

```toml
[project]
name = "strobe-vision"
version = "0.1.0"
requires-python = ">=3.10"
dependencies = [
    "torch>=2.0",
    "ultralytics>=8.0",
    "transformers>=4.36",
    "Pillow>=10.0",
]

[project.scripts]
strobe-vision = "strobe_vision.server:main"
```

**Step 2: Create protocol types**

`vision-sidecar/strobe_vision/protocol.py`:

```python
"""JSON protocol types for daemon <-> sidecar communication."""

from dataclasses import dataclass, field, asdict
from typing import Optional
import json


@dataclass
class DetectRequest:
    id: str
    type: str  # "detect"
    image: str  # base64 PNG
    confidence_threshold: float = 0.3
    iou_threshold: float = 0.5

    @classmethod
    def from_json(cls, data: dict) -> "DetectRequest":
        opts = data.get("options", {})
        return cls(
            id=data["id"],
            type=data["type"],
            image=data["image"],
            confidence_threshold=opts.get("confidence_threshold", 0.3),
            iou_threshold=opts.get("iou_threshold", 0.5),
        )


@dataclass
class DetectedElement:
    label: str
    description: str
    confidence: float
    bounds: dict  # {"x": int, "y": int, "w": int, "h": int}


@dataclass
class DetectResponse:
    id: str
    type: str = "result"
    elements: list = field(default_factory=list)
    latency_ms: int = 0

    def to_json(self) -> str:
        return json.dumps(asdict(self))


@dataclass
class ErrorResponse:
    id: str
    type: str = "error"
    message: str = ""

    def to_json(self) -> str:
        return json.dumps(asdict(self))


@dataclass
class PongResponse:
    id: str
    type: str = "pong"
    models_loaded: bool = False
    device: str = "cpu"

    def to_json(self) -> str:
        return json.dumps(asdict(self))
```

**Step 3: Create model loader**

`vision-sidecar/strobe_vision/models.py`:

```python
"""Model loading and device selection."""

import os
import sys
import torch


def select_device() -> str:
    """Auto-detect best available device: mps > cuda > cpu."""
    if torch.backends.mps.is_available():
        return "mps"
    elif torch.cuda.is_available():
        return "cuda"
    return "cpu"


def models_dir() -> str:
    """Resolve models directory. Check bundled location first, then ~/.strobe/models/."""
    # Bundled with sidecar package
    pkg_dir = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    bundled = os.path.join(pkg_dir, "models")
    if os.path.isdir(bundled):
        return bundled

    # User home
    home = os.path.join(os.path.expanduser("~"), ".strobe", "models")
    if os.path.isdir(home):
        return home

    print(f"ERROR: No models directory found at {bundled} or {home}", file=sys.stderr)
    sys.exit(1)
```

**Step 4: Create OmniParser wrapper**

`vision-sidecar/strobe_vision/omniparser.py`:

```python
"""OmniParser v2 wrapper (YOLOv8 + Florence-2)."""

import base64
import io
import time
from PIL import Image
from .models import models_dir, select_device
from .protocol import DetectedElement


class OmniParser:
    def __init__(self):
        self.device = select_device()
        self.yolo_model = None
        self.caption_model = None
        self.caption_processor = None
        self._loaded = False

    def load(self):
        """Load models into device memory."""
        if self._loaded:
            return

        import sys
        mdir = models_dir()

        # Load YOLO detection model
        from ultralytics import YOLO
        yolo_path = f"{mdir}/icon_detect/best.pt"
        self.yolo_model = YOLO(yolo_path)
        print(f"Loaded YOLO from {yolo_path}", file=sys.stderr)

        # Load Florence-2 caption model
        from transformers import AutoModelForCausalLM, AutoProcessor
        caption_path = f"{mdir}/icon_caption"
        self.caption_processor = AutoProcessor.from_pretrained(
            caption_path, trust_remote_code=True
        )
        self.caption_model = AutoModelForCausalLM.from_pretrained(
            caption_path, trust_remote_code=True
        )
        if self.device != "cpu":
            self.caption_model = self.caption_model.to(self.device)
        print(f"Loaded Florence-2 from {caption_path} on {self.device}", file=sys.stderr)

        self._loaded = True

    def detect(
        self, image_b64: str, confidence_threshold: float = 0.3, iou_threshold: float = 0.5
    ) -> list[DetectedElement]:
        """Detect UI elements in a base64-encoded PNG image."""
        self.load()

        # Decode image
        img_bytes = base64.b64decode(image_b64)
        image = Image.open(io.BytesIO(img_bytes)).convert("RGB")

        # Run YOLO detection
        results = self.yolo_model(
            image, conf=confidence_threshold, iou=iou_threshold, verbose=False
        )

        elements = []
        if results and len(results) > 0:
            boxes = results[0].boxes
            for box in boxes:
                x1, y1, x2, y2 = box.xyxy[0].tolist()
                conf = float(box.conf[0])
                cls_id = int(box.cls[0])

                # Crop for captioning
                crop = image.crop((int(x1), int(y1), int(x2), int(y2)))
                label, description = self._caption_crop(crop)

                elements.append(DetectedElement(
                    label=label or f"element_{cls_id}",
                    description=description or "",
                    confidence=round(conf, 3),
                    bounds={
                        "x": int(x1),
                        "y": int(y1),
                        "w": int(x2 - x1),
                        "h": int(y2 - y1),
                    },
                ))

        return elements

    def _caption_crop(self, crop: Image.Image) -> tuple[str, str]:
        """Use Florence-2 to caption a cropped UI element."""
        try:
            import torch
            prompt = "<CAPTION>"
            inputs = self.caption_processor(
                text=prompt, images=crop, return_tensors="pt"
            )
            if self.device != "cpu":
                inputs = {k: v.to(self.device) if hasattr(v, 'to') else v for k, v in inputs.items()}

            with torch.no_grad():
                generated = self.caption_model.generate(
                    **inputs, max_length=50, num_beams=3
                )
            caption = self.caption_processor.batch_decode(
                generated, skip_special_tokens=True
            )[0].strip()

            # Extract label (first word) and description (full caption)
            parts = caption.split()
            label = parts[0].lower() if parts else "element"
            return label, caption
        except Exception as e:
            import sys
            print(f"Caption error: {e}", file=sys.stderr)
            return "element", ""

    @property
    def is_loaded(self) -> bool:
        return self._loaded
```

**Step 5: Create main server loop**

`vision-sidecar/strobe_vision/server.py`:

```python
"""Main sidecar server: reads JSON from stdin, writes JSON to stdout."""

import json
import sys
import time
from .protocol import DetectRequest, DetectResponse, ErrorResponse, PongResponse, DetectedElement
from .omniparser import OmniParser
from .models import select_device


def main():
    parser = OmniParser()
    device = select_device()

    print(f"strobe-vision sidecar starting (device={device})", file=sys.stderr)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            data = json.loads(line)
        except json.JSONDecodeError as e:
            resp = ErrorResponse(id="unknown", message=f"Invalid JSON: {e}")
            sys.stdout.write(resp.to_json() + "\n")
            sys.stdout.flush()
            continue

        req_id = data.get("id", "unknown")
        req_type = data.get("type", "")

        try:
            if req_type == "ping":
                resp = PongResponse(
                    id=req_id,
                    models_loaded=parser.is_loaded,
                    device=device,
                )
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

            elif req_type == "detect":
                req = DetectRequest.from_json(data)

                start = time.monotonic()
                elements = parser.detect(
                    req.image,
                    confidence_threshold=req.confidence_threshold,
                    iou_threshold=req.iou_threshold,
                )
                elapsed_ms = int((time.monotonic() - start) * 1000)

                resp = DetectResponse(
                    id=req_id,
                    elements=[
                        {
                            "label": e.label,
                            "description": e.description,
                            "confidence": e.confidence,
                            "bounds": e.bounds,
                        }
                        for e in elements
                    ],
                    latency_ms=elapsed_ms,
                )
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

            else:
                resp = ErrorResponse(id=req_id, message=f"Unknown request type: {req_type}")
                sys.stdout.write(resp.to_json() + "\n")
                sys.stdout.flush()

        except Exception as e:
            import traceback
            traceback.print_exc(file=sys.stderr)
            resp = ErrorResponse(id=req_id, message=str(e))
            sys.stdout.write(resp.to_json() + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
```

Create `vision-sidecar/strobe_vision/__init__.py` (empty):

```python
```

**Checkpoint:** Python sidecar package is complete. Can run via `python -m strobe_vision.server`, reads JSON from stdin, returns detections to stdout.

---

### Task 9: Implement Rust-side sidecar management

**Files:**
- Create: `src/ui/vision.rs`
- Modify: `src/ui/mod.rs` (add module)

**Step 1: Implement sidecar manager**

Create `src/ui/vision.rs`:

```rust
//! Vision sidecar process management.
//!
//! Manages a long-running Python process that runs OmniParser v2 for
//! UI element detection. Communication via JSON over stdin/stdout.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60); // Model loading can be slow
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionElement {
    pub label: String,
    pub description: String,
    pub confidence: f32,
    pub bounds: VisionBounds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionBounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub struct VisionSidecar {
    process: Option<Child>,
    last_used: Instant,
    request_counter: u64,
}

impl VisionSidecar {
    pub fn new() -> Self {
        Self {
            process: None,
            last_used: Instant::now(),
            request_counter: 0,
        }
    }

    /// Detect UI elements in a base64-encoded PNG screenshot.
    pub fn detect(
        &mut self,
        screenshot_b64: &str,
        confidence_threshold: f32,
        iou_threshold: f32,
    ) -> Result<Vec<VisionElement>> {
        self.ensure_running()?;
        self.last_used = Instant::now();

        let req_id = format!("req_{}", self.request_counter);
        self.request_counter += 1;

        let request = serde_json::json!({
            "id": req_id,
            "type": "detect",
            "image": screenshot_b64,
            "options": {
                "confidence_threshold": confidence_threshold,
                "iou_threshold": iou_threshold,
            }
        });

        let response = self.send_request(&request)?;

        if response.get("type").and_then(|t| t.as_str()) == Some("error") {
            return Err(crate::Error::UiQueryFailed(
                format!("Vision sidecar error: {}",
                    response.get("message").and_then(|m| m.as_str()).unwrap_or("unknown"))
            ));
        }

        let elements: Vec<VisionElement> = response
            .get("elements")
            .and_then(|e| serde_json::from_value(e.clone()).ok())
            .unwrap_or_default();

        Ok(elements)
    }

    /// Check if sidecar should be shut down due to idle timeout.
    pub fn check_idle_timeout(&mut self) {
        if self.process.is_some() && self.last_used.elapsed() > IDLE_TIMEOUT {
            tracing::info!("Vision sidecar idle for {}s, shutting down", IDLE_TIMEOUT.as_secs());
            self.shutdown();
        }
    }

    /// Gracefully shutdown the sidecar.
    pub fn shutdown(&mut self) {
        if let Some(ref mut child) = self.process {
            // Close stdin to signal EOF
            drop(child.stdin.take());
            // Wait briefly, then kill
            match child.wait() {
                Ok(_) => tracing::info!("Vision sidecar exited gracefully"),
                Err(_) => {
                    let _ = child.kill();
                    tracing::warn!("Vision sidecar killed after timeout");
                }
            }
        }
        self.process = None;
    }

    fn ensure_running(&mut self) -> Result<()> {
        // Check if process is still alive
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!("Vision sidecar exited with status {:?}, restarting", status);
                    self.process = None;
                }
                Ok(None) => return Ok(()), // Still running
                Err(e) => {
                    tracing::warn!("Failed to check sidecar status: {}, restarting", e);
                    self.process = None;
                }
            }
        }

        // Start new process
        self.start()
    }

    fn start(&mut self) -> Result<()> {
        let sidecar_dir = self.find_sidecar_dir()?;

        let child = Command::new("python3")
            .args(["-m", "strobe_vision.server"])
            .current_dir(&sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Pass through to daemon stderr
            .spawn()
            .map_err(|e| crate::Error::UiQueryFailed(
                format!("Failed to start vision sidecar: {}. Ensure Python 3.10+ is installed with torch, ultralytics, transformers.", e)
            ))?;

        self.process = Some(child);

        // Health check — wait for pong
        let ping = serde_json::json!({"id": "health", "type": "ping"});
        let pong = self.send_request(&ping)?;
        if pong.get("type").and_then(|t| t.as_str()) != Some("pong") {
            self.shutdown();
            return Err(crate::Error::UiQueryFailed(
                "Vision sidecar failed health check".to_string()
            ));
        }

        let device = pong.get("device").and_then(|d| d.as_str()).unwrap_or("unknown");
        tracing::info!("Vision sidecar started (device={})", device);

        Ok(())
    }

    fn send_request(&mut self, request: &serde_json::Value) -> Result<serde_json::Value> {
        let child = self.process.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar not running".to_string()))?;

        let stdin = child.stdin.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar stdin closed".to_string()))?;

        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        stdin.write_all(line.as_bytes())
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to write to sidecar: {}", e)))?;
        stdin.flush()
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to flush sidecar stdin: {}", e)))?;

        // Read response line
        let stdout = child.stdout.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar stdout closed".to_string()))?;
        let mut reader = BufReader::new(stdout);
        let mut response_line = String::new();
        reader.read_line(&mut response_line)
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to read sidecar response: {}", e)))?;

        serde_json::from_str(&response_line)
            .map_err(|e| crate::Error::UiQueryFailed(format!("Invalid sidecar response: {}", e)))
    }

    fn find_sidecar_dir(&self) -> Result<std::path::PathBuf> {
        // Check relative to binary
        let exe = std::env::current_exe()
            .map_err(|e| crate::Error::Internal(format!("Cannot find exe path: {}", e)))?;
        let exe_dir = exe.parent().unwrap();

        // Development: relative to cargo manifest
        let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vision-sidecar");
        if dev_path.is_dir() {
            return Ok(dev_path);
        }

        // Installed: next to binary
        let installed_path = exe_dir.join("vision-sidecar");
        if installed_path.is_dir() {
            return Ok(installed_path);
        }

        Err(crate::Error::UiQueryFailed(
            format!("Vision sidecar not found. Checked: {:?}, {:?}", dev_path, installed_path)
        ))
    }
}

impl Drop for VisionSidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}
```

**Step 2: Update mod.rs**

Add to `src/ui/mod.rs`:

```rust
pub mod vision;
```

**Checkpoint:** Rust sidecar manager can start, health-check, send detection requests to, and shutdown the Python process.

---

### Task 10: Add vision configuration to settings

**Files:**
- Modify: `src/config.rs`

**Step 1: Add vision settings**

In `src/config.rs`, add fields to `StrobeSettings`:

```rust
pub struct StrobeSettings {
    pub events_max_per_session: usize,
    pub test_status_retry_ms: u64,
    pub vision_enabled: bool,
    pub vision_confidence_threshold: f32,
    pub vision_iou_merge_threshold: f32,
    pub vision_sidecar_idle_timeout_seconds: u64,
}
```

Update `Default`:

```rust
impl Default for StrobeSettings {
    fn default() -> Self {
        Self {
            events_max_per_session: 200_000,
            test_status_retry_ms: 5_000,
            vision_enabled: false,
            vision_confidence_threshold: 0.3,
            vision_iou_merge_threshold: 0.5,
            vision_sidecar_idle_timeout_seconds: 300,
        }
    }
}
```

Update `SettingsFile`:

```rust
struct SettingsFile {
    #[serde(rename = "events.maxPerSession")]
    events_max_per_session: Option<usize>,
    #[serde(rename = "test.statusRetryMs")]
    test_status_retry_ms: Option<u64>,
    #[serde(rename = "vision.enabled")]
    vision_enabled: Option<bool>,
    #[serde(rename = "vision.confidenceThreshold")]
    vision_confidence_threshold: Option<f32>,
    #[serde(rename = "vision.iouMergeThreshold")]
    vision_iou_merge_threshold: Option<f32>,
    #[serde(rename = "vision.sidecarIdleTimeoutSeconds")]
    vision_sidecar_idle_timeout_seconds: Option<u64>,
}
```

Update `apply_file` to handle new fields:

```rust
    if let Some(v) = file.vision_enabled {
        settings.vision_enabled = v;
    }
    if let Some(v) = file.vision_confidence_threshold {
        if v > 0.0 && v <= 1.0 {
            settings.vision_confidence_threshold = v;
        }
    }
    if let Some(v) = file.vision_iou_merge_threshold {
        if v > 0.0 && v <= 1.0 {
            settings.vision_iou_merge_threshold = v;
        }
    }
    if let Some(v) = file.vision_sidecar_idle_timeout_seconds {
        if v >= 30 && v <= 3600 {
            settings.vision_sidecar_idle_timeout_seconds = v;
        }
    }
```

Add config tests for new fields.

**Checkpoint:** Vision settings configurable via `~/.strobe/settings.json`. Default: vision disabled.

---

### Task 11: Create golden screenshot test fixtures

**Files:**
- Create: `tests/fixtures/ui-golden/README.md`
- Create: `tests/fixtures/ui-golden/capture_golden.sh`

**Step 1: Create golden screenshot capture script**

This captures reference screenshots from the test app for regression testing.

`tests/fixtures/ui-golden/capture_golden.sh`:

```bash
#!/bin/bash
set -euo pipefail

# Build and launch the test app, capture screenshot, then kill it
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_DIR="$SCRIPT_DIR/../ui-test-app"
GOLDEN_DIR="$SCRIPT_DIR"

# Build test app
cd "$APP_DIR" && bash build.sh

# Launch app in background
"$APP_DIR/build/UITestApp" &
APP_PID=$!
sleep 2  # Wait for window to render

# Capture screenshot using screencapture
screencapture -l$(osascript -e "tell app \"System Events\" to id of first window of (processes whose unix id is $APP_PID)") "$GOLDEN_DIR/test_app.png"

# Kill app
kill $APP_PID 2>/dev/null || true

echo "Captured golden screenshot: $GOLDEN_DIR/test_app.png"
```

`tests/fixtures/ui-golden/README.md`:

```markdown
# Golden Screenshots

Reference screenshots for vision pipeline regression testing.
Captured from the SwiftUI UI test app via `capture_golden.sh`.

Regenerate after UI test app changes:
```bash
bash capture_golden.sh
```

**Checkpoint:** M2 complete. Vision sidecar can start, process screenshots via OmniParser v2, and return detected elements. Configuration integrated into settings. Commit M2.

---

## M3: Merge Pipeline + Comprehensive Testing

### Task 12: Implement IoU merge algorithm

**Files:**
- Create: `src/ui/merge.rs`
- Modify: `src/ui/mod.rs` (add module)

**Step 1: Write merge unit tests first (TDD)**

Create `src/ui/merge.rs`:

```rust
//! Merge AX tree nodes with vision-detected elements via IoU matching.

use crate::ui::tree::{generate_id, NodeSource, Rect, UiNode};
use crate::ui::vision::{VisionBounds, VisionElement};

/// Compute Intersection over Union for two rectangles.
pub fn iou(a: &Rect, b: &Rect) -> f64 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);

    if x2 <= x1 || y2 <= y1 {
        return 0.0;
    }

    let intersection = (x2 - x1) * (y2 - y1);
    let area_a = a.w * a.h;
    let area_b = b.w * b.h;
    let union = area_a + area_b - intersection;

    if union <= 0.0 { 0.0 } else { intersection / union }
}

/// Convert VisionBounds to Rect.
fn vision_bounds_to_rect(b: &VisionBounds) -> Rect {
    Rect {
        x: b.x as f64,
        y: b.y as f64,
        w: b.w as f64,
        h: b.h as f64,
    }
}

/// Merge vision-detected elements into an AX tree.
///
/// 1. For each vision element, find best IoU match among AX leaf nodes.
/// 2. IoU >= threshold → merge (AX node gets source=Merged, vision confidence).
/// 3. IoU < threshold → add as vision-only node under nearest containing AX parent.
pub fn merge_vision_into_tree(
    ax_nodes: &mut Vec<UiNode>,
    vision_elements: &[VisionElement],
    iou_threshold: f64,
) -> (usize, usize) {
    let mut merged_count = 0;
    let mut added_count = 0;

    for ve in vision_elements {
        let vr = vision_bounds_to_rect(&ve.bounds);
        let mut best_match: Option<(f64, Vec<usize>)> = None;

        // Find best IoU match among leaf nodes
        find_best_match(ax_nodes, &vr, &mut best_match, &mut vec![]);

        if let Some((best_iou, path)) = best_match {
            if best_iou >= iou_threshold {
                // Merge: update existing node
                if let Some(node) = get_node_mut(ax_nodes, &path) {
                    node.source = NodeSource::Merged { confidence: ve.confidence };
                    if node.value.is_none() {
                        // Use vision-estimated value if AX didn't provide one
                        node.value = Some(ve.description.clone());
                    }
                }
                merged_count += 1;
                continue;
            }
        }

        // No match — add as vision-only node
        let vision_node = UiNode {
            id: generate_id(&ve.label, Some(&ve.description), added_count),
            role: ve.label.clone(),
            title: if ve.description.is_empty() { None } else { Some(ve.description.clone()) },
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(vr),
            actions: vec![],
            source: NodeSource::Vision { confidence: ve.confidence },
            children: vec![],
        };

        // Find nearest containing parent and insert
        let center_x = ve.bounds.x as f64 + ve.bounds.w as f64 / 2.0;
        let center_y = ve.bounds.y as f64 + ve.bounds.h as f64 / 2.0;
        if !insert_into_container(ax_nodes, vision_node.clone(), center_x, center_y) {
            // No container found — add to root level
            ax_nodes.push(vision_node);
        }
        added_count += 1;
    }

    (merged_count, added_count)
}

fn find_best_match(
    nodes: &[UiNode],
    target: &Rect,
    best: &mut Option<(f64, Vec<usize>)>,
    current_path: &mut Vec<usize>,
) {
    for (i, node) in nodes.iter().enumerate() {
        current_path.push(i);

        if node.children.is_empty() {
            // Leaf node — compute IoU
            if let Some(ref bounds) = node.bounds {
                let score = iou(bounds, target);
                if best.is_none() || score > best.as_ref().unwrap().0 {
                    *best = Some((score, current_path.clone()));
                }
            }
        } else {
            find_best_match(&node.children, target, best, current_path);
        }

        current_path.pop();
    }
}

fn get_node_mut<'a>(nodes: &'a mut [UiNode], path: &[usize]) -> Option<&'a mut UiNode> {
    if path.is_empty() {
        return None;
    }
    let mut current = &mut nodes[path[0]];
    for &idx in &path[1..] {
        current = &mut current.children[idx];
    }
    Some(current)
}

fn insert_into_container(nodes: &mut Vec<UiNode>, node: UiNode, cx: f64, cy: f64) -> bool {
    // Find deepest container whose bounds contain the center point
    for parent in nodes.iter_mut() {
        if let Some(ref bounds) = parent.bounds {
            if cx >= bounds.x && cx <= bounds.x + bounds.w
                && cy >= bounds.y && cy <= bounds.y + bounds.h
            {
                // Try to insert deeper first
                if !insert_into_container(&mut parent.children, node.clone(), cx, cy) {
                    parent.children.push(node);
                }
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iou_identical() {
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        assert!((iou(&a, &a) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_iou_no_overlap() {
        let a = Rect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 };
        let b = Rect { x: 100.0, y: 100.0, w: 50.0, h: 50.0 };
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn test_iou_partial() {
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let b = Rect { x: 50.0, y: 50.0, w: 100.0, h: 100.0 };
        // Intersection: 50x50 = 2500, Union: 10000 + 10000 - 2500 = 17500
        let expected = 2500.0 / 17500.0;
        assert!((iou(&a, &b) - expected).abs() < 0.001);
    }

    #[test]
    fn test_iou_contained() {
        let outer = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let inner = Rect { x: 25.0, y: 25.0, w: 50.0, h: 50.0 };
        // Intersection: 50x50 = 2500, Union: 10000 + 2500 - 2500 = 10000
        assert!((iou(&outer, &inner) - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_merge_matched_node() {
        let mut tree = vec![UiNode {
            id: "btn_1".to_string(),
            role: "button".to_string(),
            title: Some("Play".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 10.0, y: 10.0, w: 80.0, h: 30.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Play button".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 12, y: 8, w: 78, h: 32 }, // High IoU with ax node
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 1);
        assert_eq!(added, 0);
        assert!(matches!(tree[0].source, NodeSource::Merged { .. }));
    }

    #[test]
    fn test_merge_unmatched_added() {
        let mut tree = vec![UiNode {
            id: "w_1".to_string(),
            role: "window".to_string(),
            title: Some("Test".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "knob".to_string(),
            description: "Filter Cutoff".to_string(),
            confidence: 0.85,
            bounds: VisionBounds { x: 100, y: 100, w: 60, h: 60 }, // No AX match
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 0);
        assert_eq!(added, 1);
        // Vision node should be added as child of window (spatial containment)
        assert_eq!(tree[0].children.len(), 1);
        assert!(matches!(tree[0].children[0].source, NodeSource::Vision { .. }));
    }
}
```

**Step 2: Update mod.rs**

Add to `src/ui/mod.rs`:

```rust
pub mod merge;
```

**Step 3: Run tests**

```bash
cargo test ui::merge --lib
```

Expected: All merge tests pass.

**Checkpoint:** IoU calculation and merge algorithm work correctly.

---

### Task 13: Wire vision into debug_ui handler

**Files:**
- Modify: `src/daemon/server.rs` (update `tool_debug_ui`)
- Modify: `src/daemon/server.rs` (add `VisionSidecar` to `Daemon` struct)

**Step 1: Add VisionSidecar to Daemon**

In `Daemon` struct (line 18), add:

```rust
    #[cfg(target_os = "macos")]
    vision_sidecar: Arc<Mutex<crate::ui::vision::VisionSidecar>>,
```

Initialize in `Daemon::new()` (or wherever the struct is constructed):

```rust
    #[cfg(target_os = "macos")]
    vision_sidecar: Arc::new(Mutex::new(crate::ui::vision::VisionSidecar::new())),
```

Also add `use std::sync::Mutex;` to imports if not already present.

**Step 2: Update tool_debug_ui to use vision**

Replace the `// TODO (M2)` section in `tool_debug_ui` with:

```rust
                if vision_requested {
                    // Capture screenshot for vision
                    let pid_for_screenshot = session.pid;
                    let screenshot_bytes = tokio::task::spawn_blocking(move || {
                        crate::ui::capture::capture_window_screenshot(pid_for_screenshot)
                    }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

                    use base64::Engine;
                    let screenshot_b64 = base64::engine::general_purpose::STANDARD.encode(&screenshot_bytes);

                    // Run vision detection
                    let sidecar = Arc::clone(&self.vision_sidecar);
                    let vision_result = tokio::task::spawn_blocking(move || {
                        let mut sidecar = sidecar.lock().unwrap();
                        sidecar.detect(&screenshot_b64, 0.3, 0.5)
                    }).await.map_err(|e| crate::Error::Internal(format!("Vision task failed: {}", e)))?;

                    match vision_result {
                        Ok(vision_elements) => {
                            let (m, a) = crate::ui::merge::merge_vision_into_tree(
                                &mut nodes, &vision_elements, 0.5
                            );
                            merged_count = m;
                            vision_count = a;
                        }
                        Err(e) => {
                            tracing::warn!("Vision pipeline failed, returning AX-only tree: {}", e);
                        }
                    }
                }
```

Fix the variable mutability — change `let vision_count = 0;` and `let merged_count = 0;` to `let mut`.

**Checkpoint:** `debug_ui(vision=true)` now runs the full pipeline: AX → screenshot → OmniParser → merge → unified tree.

---

### Task 14: Add idle timeout checking for vision sidecar

**Files:**
- Modify: `src/daemon/server.rs`

**Step 1: Add periodic idle check**

In the daemon's main loop (where idle timeout is checked), add vision sidecar cleanup:

```rust
    // In the idle timeout checking loop, add:
    #[cfg(target_os = "macos")]
    {
        if let Ok(mut sidecar) = self.vision_sidecar.lock() {
            sidecar.check_idle_timeout();
        }
    }
```

**Checkpoint:** Vision sidecar auto-shuts down after 5 minutes of inactivity.

---

### Task 15: Write comprehensive E2E tests

**Files:**
- Modify: `tests/ui_observation.rs`

**Step 1: Add E2E tests for full pipeline**

Add these tests to the existing `macos_tests` module:

```rust
    #[tokio::test(flavor = "multi_thread")]
    async fn test_debug_ui_tree_mode() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-e2e-tree";
        let pid = sm.spawn_with_frida(
            session_id, binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Test tree mode
        let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
        let compact = strobe::ui::tree::format_compact(&nodes);
        assert!(!compact.is_empty());
        assert!(compact.contains("id="));

        // Test JSON mode
        let json = strobe::ui::tree::format_json(&nodes).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["nodes"].is_array());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_stable_ids_across_10_calls() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-stability";
        let pid = sm.spawn_with_frida(
            session_id, binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;

        let baseline = strobe::ui::tree::format_compact(
            &strobe::ui::accessibility::query_ax_tree(pid).unwrap()
        );

        for i in 1..10 {
            let current = strobe::ui::tree::format_compact(
                &strobe::ui::accessibility::query_ax_tree(pid).unwrap()
            );
            assert_eq!(baseline, current, "ID stability failed on call {}", i);
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_process_exit_returns_error() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-exit-test";
        let pid = sm.spawn_with_frida(
            session_id, binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Kill the process
        let _ = sm.stop_frida(session_id).await;

        // Query should return empty or error (process gone)
        let result = strobe::ui::accessibility::query_ax_tree(pid);
        match result {
            Ok(nodes) => assert!(nodes.is_empty()),
            Err(_) => {} // Error is expected
        }

        sm.stop_session(session_id).unwrap();
    }

    #[test]
    fn test_iou_merge_unit() {
        use strobe::ui::merge::iou;
        use strobe::ui::tree::Rect;

        // Identical boxes
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        assert!((iou(&a, &a) - 1.0).abs() < 0.01);

        // No overlap
        let b = Rect { x: 200.0, y: 200.0, w: 50.0, h: 50.0 };
        assert_eq!(iou(&a, &b), 0.0);
    }
```

**Step 2: Run all tests**

```bash
cargo test ui_observation --test ui_observation -- --test-threads=1
```

**Checkpoint:** All E2E tests pass. Stable IDs verified over 10 calls. Error handling for process exit works.

---

### Task 16: Field tests and documentation

**Files:**
- Create: `docs/field-tests/2026-02-XX-ui-observation.md`

**Step 1: Manual field test procedure**

Document the field test in `docs/field-tests/`:

```markdown
# UI Observation Field Test

## Test 1: Calculator.app (Native Cocoa)
1. `debug_launch(command="/System/Applications/Calculator.app/Contents/MacOS/Calculator")`
2. `debug_ui(sessionId, mode="tree")`
3. Verify: buttons (0-9, +, -, =), display, window all present
4. Verify: IDs stable across 3 calls

## Test 2: ERAE MK2 Simulator (JUCE)
1. `debug_launch(command="path/to/erae_mk2_simulator")`
2. `debug_ui(sessionId, mode="tree")` — AX-only first
3. `debug_ui(sessionId, mode="tree", vision=true)` — with vision
4. Verify: vision detects knobs/sliders that AX missed
5. Verify: merged tree contains both source=ax and source=vision nodes

## Test 3: VS Code (Electron)
1. `debug_launch(command="/Applications/Visual Studio Code.app/Contents/MacOS/Electron")`
2. `debug_ui(sessionId, mode="tree")`
3. Verify: large tree renders without timeout
4. Verify: node count in stats matches actual tree

## Pass Criteria
- All 3 apps return valid trees
- AX latency <50ms for all apps
- Vision+merge latency <2s
- No crashes or panics
- IDs stable across consecutive calls
```

**Step 2: Run field tests manually and document results**

**Checkpoint:** M3 complete. Full pipeline works end-to-end. All validation criteria verified. Commit M3.

---

## Summary of All Files

### New Files (14)
| File | Task |
|------|------|
| `src/ui/mod.rs` | 2 |
| `src/ui/tree.rs` | 2 |
| `src/ui/accessibility.rs` | 3 |
| `src/ui/capture.rs` | 4 |
| `src/ui/vision.rs` | 9 |
| `src/ui/merge.rs` | 12 |
| `vision-sidecar/pyproject.toml` | 8 |
| `vision-sidecar/strobe_vision/__init__.py` | 8 |
| `vision-sidecar/strobe_vision/protocol.py` | 8 |
| `vision-sidecar/strobe_vision/models.py` | 8 |
| `vision-sidecar/strobe_vision/omniparser.py` | 8 |
| `vision-sidecar/strobe_vision/server.py` | 8 |
| `tests/fixtures/ui-test-app/UITestApp.swift` | 6 |
| `tests/ui_observation.rs` | 7 |

### Modified Files (5)
| File | Task |
|------|------|
| `src/lib.rs` | 2 |
| `src/error.rs` | 1 |
| `src/mcp/types.rs` | 1 |
| `src/daemon/server.rs` | 5, 13, 14 |
| `Cargo.toml` | 3, 4 |
| `src/config.rs` | 10 |
| `tests/common/mod.rs` | 7 |
