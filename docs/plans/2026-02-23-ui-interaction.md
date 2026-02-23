# Plan: UI Interaction (Phase 6)

**Goal:** Add a `debug_ui_action` MCP tool that lets LLMs click, type, set values, send keys, scroll, and drag elements in running macOS processes.

**Spec:** `docs/specs/2026-02-23-ui-interaction.md`

**Architecture:** New `src/ui/input.rs` (trait + dispatch), `src/ui/input_mac.rs` (CGEvent + AX actions), `src/ui/input_linux.rs` (stub). Types in `src/mcp/types.rs`, handler in `src/daemon/server.rs`. Uses `core-graphics` crate's `CGEvent::post_to_pid(pid)` (feature `"elcapitan"`) instead of deprecated `GetProcessForPID` + PSN. Scroll uses `CGEvent::new_scroll_event` (feature `"highsierra"`).

**Tech stack:** `core-graphics` 0.24 (with `elcapitan` + `highsierra` features), `accessibility-sys` 0.2, `core-foundation` 0.10.

---

## Task 1: MCP types and validation

**Files:**
- Modify: `src/mcp/types.rs` (after line 1408, after `DebugUiResponse`)
- Test: inline `#[cfg(test)]` in `src/mcp/types.rs`

### 1a. Write failing tests

Add after the existing `write_tests` module at the bottom of `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod ui_action_tests {
    use super::*;

    #[test]
    fn test_ui_action_request_valid_click() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Click,
            id: Some("btn_a1b2".to_string()),
            value: None,
            text: None,
            key: None,
            modifiers: None,
            direction: None,
            amount: None,
            to_id: None,
            settle_ms: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_ui_action_request_empty_session_id() {
        let req = DebugUiActionRequest {
            session_id: "".to_string(),
            action: UiActionType::Click,
            id: Some("btn_a1b2".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_ui_action_request_click_missing_id() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Click,
            id: None,
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("id"));
    }

    #[test]
    fn test_ui_action_request_key_no_id_required() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Key,
            id: None,
            value: None, text: None,
            key: Some("s".to_string()),
            modifiers: Some(vec!["cmd".to_string()]),
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_ui_action_request_key_missing_key_field() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Key,
            id: None,
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("key"));
    }

    #[test]
    fn test_ui_action_request_type_missing_text() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Type,
            id: Some("txt_1234".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("text"));
    }

    #[test]
    fn test_ui_action_request_drag_missing_to_id() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Drag,
            id: Some("el_1234".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("toId"));
    }

    #[test]
    fn test_ui_action_request_scroll_missing_direction() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Scroll,
            id: Some("lst_1234".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("direction"));
    }

    #[test]
    fn test_ui_action_request_set_value_missing_value() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::SetValue,
            id: Some("sld_1234".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: None,
        };
        let err = req.validate().unwrap_err();
        assert!(err.to_string().contains("value"));
    }

    #[test]
    fn test_ui_action_request_camel_case_wire_format() {
        let req = DebugUiActionRequest {
            session_id: "s1".to_string(),
            action: UiActionType::Click,
            id: Some("btn_a1b2".to_string()),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, to_id: None, settle_ms: Some(100),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("sessionId").is_some());
        assert!(json.get("settleMs").is_some());
    }

    #[test]
    fn test_ui_action_response_serialization() {
        let resp = DebugUiActionResponse {
            success: true,
            method: Some("ax".to_string()),
            node_before: None,
            node_after: None,
            changed: Some(true),
            error: None,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["method"], "ax");
        assert_eq!(json["changed"], true);
        // null fields should be absent (skip_serializing_if)
        assert!(json.get("error").is_none());
    }
}
```

### 1b. Run tests — verify they fail

```
cargo test ui_action_tests -- --nocapture
```

Expected: compilation error — `DebugUiActionRequest` and related types don't exist yet.

### 1c. Write implementation

Add after the `DebugUiResponse` struct (after line 1408) in `src/mcp/types.rs`:

```rust
// ============ debug_ui_action ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiActionType {
    Click,
    SetValue,
    Type,
    Key,
    Scroll,
    Drag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiActionRequest {
    pub session_id: String,
    pub action: UiActionType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<ScrollDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settle_ms: Option<u64>,
}

impl DebugUiActionRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.session_id.is_empty() {
            return Err(crate::Error::ValidationError(
                "sessionId must not be empty".to_string(),
            ));
        }

        // All actions except Key require id
        if self.action != UiActionType::Key && self.id.is_none() {
            return Err(crate::Error::ValidationError(
                "id is required for all actions except 'key'".to_string(),
            ));
        }

        match self.action {
            UiActionType::SetValue => {
                if self.value.is_none() {
                    return Err(crate::Error::ValidationError(
                        "value is required for 'set_value' action".to_string(),
                    ));
                }
            }
            UiActionType::Type => {
                if self.text.is_none() {
                    return Err(crate::Error::ValidationError(
                        "text is required for 'type' action".to_string(),
                    ));
                }
            }
            UiActionType::Key => {
                if self.key.is_none() {
                    return Err(crate::Error::ValidationError(
                        "key is required for 'key' action".to_string(),
                    ));
                }
            }
            UiActionType::Scroll => {
                if self.direction.is_none() {
                    return Err(crate::Error::ValidationError(
                        "direction is required for 'scroll' action".to_string(),
                    ));
                }
            }
            UiActionType::Drag => {
                if self.to_id.is_none() {
                    return Err(crate::Error::ValidationError(
                        "toId is required for 'drag' action".to_string(),
                    ));
                }
            }
            UiActionType::Click => {}
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugUiActionResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_before: Option<crate::ui::tree::UiNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_after: Option<crate::ui::tree::UiNode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
```

### 1d. Run tests — verify they pass

```
cargo test ui_action_tests -- --nocapture
```

Expected: all 11 tests pass.

### Edge cases covered

- Empty session ID
- Missing `id` for each action that requires it
- Missing action-specific fields (`text`, `key`, `value`, `direction`, `toId`)
- `key` action does NOT require `id`
- camelCase wire format via serde
- `skip_serializing_if` for optional response fields

### Checkpoint

All MCP request/response types compile and validate correctly. No runtime behavior yet.

---

## Task 2: Node diff logic

**Files:**
- Modify: `src/ui/tree.rs` (add `diff_nodes` function after `count_nodes`, line 141)
- Test: inline `#[cfg(test)]` in same file

### 2a. Write failing tests

Add to the existing `mod tests` in `src/ui/tree.rs` (after `test_count_nodes`):

```rust
#[test]
fn test_diff_nodes_detects_value_change() {
    let before = UiNode {
        id: "sld_1234".to_string(),
        role: "AXSlider".to_string(),
        title: Some("Volume".to_string()),
        value: Some("0.5".to_string()),
        enabled: true,
        focused: false,
        bounds: Some(Rect { x: 10.0, y: 20.0, w: 200.0, h: 30.0 }),
        actions: vec!["AXIncrement".to_string()],
        source: NodeSource::Ax,
        children: vec![],
    };
    let mut after = before.clone();
    after.value = Some("0.8".to_string());
    assert!(diff_nodes(&before, &after));
}

#[test]
fn test_diff_nodes_no_change() {
    let node = UiNode {
        id: "btn_a1b2".to_string(),
        role: "AXButton".to_string(),
        title: Some("Play".to_string()),
        value: None,
        enabled: true,
        focused: false,
        bounds: Some(Rect { x: 10.0, y: 5.0, w: 80.0, h: 30.0 }),
        actions: vec!["AXPress".to_string()],
        source: NodeSource::Ax,
        children: vec![],
    };
    assert!(!diff_nodes(&node, &node));
}

#[test]
fn test_diff_nodes_detects_focus_change() {
    let before = UiNode {
        id: "txt_5678".to_string(),
        role: "AXTextField".to_string(),
        title: Some("Name".to_string()),
        value: Some("hello".to_string()),
        enabled: true,
        focused: false,
        bounds: None,
        actions: vec![],
        source: NodeSource::Ax,
        children: vec![],
    };
    let mut after = before.clone();
    after.focused = true;
    assert!(diff_nodes(&before, &after));
}

#[test]
fn test_diff_nodes_detects_enabled_change() {
    let before = UiNode {
        id: "btn_1234".to_string(),
        role: "AXButton".to_string(),
        title: None,
        value: None,
        enabled: true,
        focused: false,
        bounds: None,
        actions: vec![],
        source: NodeSource::Ax,
        children: vec![],
    };
    let mut after = before.clone();
    after.enabled = false;
    assert!(diff_nodes(&before, &after));
}

#[test]
fn test_diff_nodes_detects_title_change() {
    let before = UiNode {
        id: "btn_1234".to_string(),
        role: "AXButton".to_string(),
        title: Some("Play".to_string()),
        value: None,
        enabled: true,
        focused: false,
        bounds: None,
        actions: vec![],
        source: NodeSource::Ax,
        children: vec![],
    };
    let mut after = before.clone();
    after.title = Some("Pause".to_string());
    assert!(diff_nodes(&before, &after));
}
```

### 2b. Run tests — verify they fail

```
cargo test --lib ui::tree::tests::test_diff_nodes -- --nocapture
```

Expected: compilation error — `diff_nodes` doesn't exist.

### 2c. Write implementation

Add after `count_nodes` (line 141) in `src/ui/tree.rs`:

```rust
/// Compare two UiNode snapshots. Returns true if any observable field changed.
/// Compares: value, enabled, focused, title. Ignores children and bounds.
pub fn diff_nodes(before: &UiNode, after: &UiNode) -> bool {
    before.value != after.value
        || before.enabled != after.enabled
        || before.focused != after.focused
        || before.title != after.title
}
```

### 2d. Run tests — verify they pass

```
cargo test --lib ui::tree::tests::test_diff_nodes -- --nocapture
```

Expected: all 5 tests pass.

### Edge cases covered

- Value changed (slider moved)
- No change (identical nodes)
- Focus changed (field focused after click)
- Enabled state changed (button disabled after action)
- Title changed (button label updated)

### Checkpoint

Pure diffing logic works. Used by the handler to compute the `changed` flag in the response.

---

## Task 3: `find_ax_element` — AX tree traversal by ID

**Files:**
- Modify: `src/ui/accessibility.rs` (add `find_ax_element` public function)

### 3a. Write failing test

Add to `tests/ui_observation.rs` inside the `macos_tests` module:

```rust
#[test]
fn test_find_ax_element_returns_none_for_bogus_id() {
    // No process needed — just verify it handles invalid PID gracefully
    let result = strobe::ui::accessibility::find_ax_element(99999, "btn_0000");
    // Should return Ok(None) or an error, not panic
    match result {
        Ok(None) => {} // expected: no such PID or no such element
        Err(_) => {}   // also acceptable: permission/PID error
        Ok(Some(_)) => panic!("Should not find an element for bogus PID"),
    }
}
```

### 3b. Run test — verify it fails

```
cargo test --test ui_observation test_find_ax_element -- --nocapture
```

Expected: compilation error — `find_ax_element` doesn't exist.

### 3c. Write implementation

Add after the `get_ax_actions` function (after line 331) in `src/ui/accessibility.rs`:

```rust
/// Find a live AXUIElementRef by stable node ID.
/// Re-traverses the AX tree from root, matching generate_id() output.
/// Caller must CFRelease the returned ref when done.
pub fn find_ax_element(pid: u32, target_id: &str) -> crate::Result<Option<AXUIElementRef>> {
    if !check_accessibility_permission(false) {
        return Err(crate::Error::UiNotAvailable(
            "Accessibility permission required".to_string(),
        ));
    }

    unsafe {
        let app_ref = AXUIElementCreateApplication(pid as i32);
        if app_ref.is_null() {
            return Ok(None);
        }

        let result = find_element_recursive(app_ref, target_id, 0, 0);
        CFRelease(app_ref as *const c_void);
        Ok(result)
    }
}

/// Walk the app's children to find the target element.
/// Returns the AXUIElementRef with an extra retain (caller must release).
unsafe fn find_element_recursive(
    parent: AXUIElementRef,
    target_id: &str,
    sibling_index: usize,
    depth: usize,
) -> Option<AXUIElementRef> {
    if depth > MAX_AX_DEPTH {
        return None;
    }

    // Check if this element matches
    if depth > 0 {
        // depth 0 is the app ref itself, which has no role
        if let Some(role) = get_ax_string(parent, kAXRoleAttribute) {
            let title = get_ax_string(parent, kAXTitleAttribute)
                .or_else(|| get_ax_string(parent, kAXDescriptionAttribute));
            let id = generate_id(&role, title.as_deref(), sibling_index);
            if id == target_id {
                core_foundation_sys::base::CFRetain(parent as *const c_void);
                return Some(parent);
            }
        }
    }

    // Recurse into children
    let children = get_ax_children(parent);
    for (i, child) in children.iter().enumerate() {
        if let Some(found) = find_element_recursive(*child, target_id, i, depth + 1) {
            // Release all children before returning
            for c in &children {
                CFRelease(*c as *const c_void);
            }
            return Some(found);
        }
    }
    for c in &children {
        CFRelease(*c as *const c_void);
    }

    None
}
```

Note: The `find_element_recursive` at depth 0 is the `AXUIElementCreateApplication` ref. The actual windows are children at depth 1 with sibling_index based on window ordering, matching `query_ax_tree` which uses `window_index` as sibling_index after filtering MenuBars.

However, `query_ax_tree` filters out MenuBar children and uses a separate `window_index` counter. We need `find_element_recursive` to match this behavior. Update the function:

Replace the depth 0 handling. At depth 0 (app level), iterate children the same way `query_ax_tree` does — filter MenuBars and use a separate window counter:

```rust
unsafe fn find_element_recursive(
    parent: AXUIElementRef,
    target_id: &str,
    sibling_index: usize,
    depth: usize,
) -> Option<AXUIElementRef> {
    if depth > MAX_AX_DEPTH {
        return None;
    }

    // Check if this element matches (not at root level)
    if depth > 0 {
        if let Some(role) = get_ax_string(parent, kAXRoleAttribute) {
            let title = get_ax_string(parent, kAXTitleAttribute)
                .or_else(|| get_ax_string(parent, kAXDescriptionAttribute));
            let id = generate_id(&role, title.as_deref(), sibling_index);
            if id == target_id {
                core_foundation_sys::base::CFRetain(parent as *const c_void);
                return Some(parent);
            }
        }
    }

    let children = get_ax_children(parent);

    if depth == 0 {
        // App level: match query_ax_tree behavior — filter MenuBars, use window_index
        let mut window_index = 0;
        for child in &children {
            let role = get_ax_string(*child, kAXRoleAttribute);
            let is_menu_bar = role.as_deref().map_or(false, |r| r.contains("MenuBar"));
            if !is_menu_bar {
                if let Some(found) = find_element_recursive(*child, target_id, window_index, depth + 1) {
                    for c in &children {
                        CFRelease(*c as *const c_void);
                    }
                    return Some(found);
                }
                window_index += 1;
            }
        }
    } else {
        for (i, child) in children.iter().enumerate() {
            if let Some(found) = find_element_recursive(*child, target_id, i, depth + 1) {
                for c in &children {
                    CFRelease(*c as *const c_void);
                }
                return Some(found);
            }
        }
    }

    for c in &children {
        CFRelease(*c as *const c_void);
    }
    None
}
```

### 3d. Run test — verify it passes

```
cargo test --test ui_observation test_find_ax_element -- --nocapture
```

Expected: passes (returns `Ok(None)` or `Err` for bogus PID — both acceptable).

### Edge cases covered

- Invalid PID → returns `Ok(None)` or permission error
- MenuBar filtering matches `query_ax_tree` behavior
- Depth limit prevents stack overflow on pathological trees

### Checkpoint

Can resolve a node ID back to a live `AXUIElementRef` for AX action execution.

---

## Task 4: Input trait, Linux stub, and coordinate math

**Files:**
- Create: `src/ui/input.rs`
- Create: `src/ui/input_linux.rs`
- Modify: `src/ui/mod.rs` (add module declarations)

### 4a. Write failing tests

In `src/ui/input.rs` (new file — tests are inline):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::Rect;

    #[test]
    fn test_element_center() {
        let bounds = Rect { x: 100.0, y: 200.0, w: 80.0, h: 30.0 };
        let (cx, cy) = element_center(&bounds);
        assert!((cx - 140.0).abs() < 0.01);
        assert!((cy - 215.0).abs() < 0.01);
    }

    #[test]
    fn test_element_center_zero_origin() {
        let bounds = Rect { x: 0.0, y: 0.0, w: 100.0, h: 50.0 };
        let (cx, cy) = element_center(&bounds);
        assert!((cx - 50.0).abs() < 0.01);
        assert!((cy - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_drag_interpolation_points() {
        let points = drag_interpolation_points(0.0, 0.0, 100.0, 200.0, 10);
        assert_eq!(points.len(), 10);
        // First point should be near start (but not exactly at start)
        assert!(points[0].0 > 0.0 && points[0].0 < 100.0);
        // Last point should be near end (but not exactly at end)
        assert!(points[9].0 > 0.0 && points[9].0 <= 100.0);
        assert!(points[9].1 > 0.0 && points[9].1 <= 200.0);
        // Should be monotonically increasing in both axes
        for i in 1..points.len() {
            assert!(points[i].0 >= points[i - 1].0);
            assert!(points[i].1 >= points[i - 1].1);
        }
    }

    #[test]
    fn test_drag_interpolation_single_step() {
        let points = drag_interpolation_points(10.0, 20.0, 50.0, 80.0, 1);
        assert_eq!(points.len(), 1);
        assert!((points[0].0 - 50.0).abs() < 0.01);
        assert!((points[0].1 - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_modifier_flags() {
        let flags = modifier_string_to_flags(&["cmd".to_string(), "shift".to_string()]);
        assert!(flags & MOD_COMMAND != 0);
        assert!(flags & MOD_SHIFT != 0);
        assert!(flags & MOD_CONTROL == 0);
    }

    #[test]
    fn test_modifier_flags_empty() {
        let flags = modifier_string_to_flags(&[]);
        assert_eq!(flags, 0);
    }

    #[test]
    fn test_modifier_flags_all() {
        let flags = modifier_string_to_flags(&[
            "cmd".to_string(), "shift".to_string(),
            "alt".to_string(), "ctrl".to_string(),
        ]);
        assert!(flags & MOD_COMMAND != 0);
        assert!(flags & MOD_SHIFT != 0);
        assert!(flags & MOD_ALTERNATE != 0);
        assert!(flags & MOD_CONTROL != 0);
    }
}
```

### 4b. Run tests — verify they fail

```
cargo test --lib ui::input::tests -- --nocapture
```

Expected: compilation error — module doesn't exist.

### 4c. Write implementation

**`src/ui/mod.rs`** — add after line 17:

```rust
pub mod input;

#[cfg(target_os = "macos")]
pub mod input_mac;

#[cfg(target_os = "linux")]
mod input_linux;
```

**`src/ui/input.rs`** — new file:

```rust
use crate::ui::tree::{Rect, UiNode};
use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse, UiActionType, ScrollDirection};

/// Modifier flag constants (match CGEventFlags bit positions).
pub const MOD_SHIFT: u64 = 0x00020000;     // kCGEventFlagMaskShift
pub const MOD_CONTROL: u64 = 0x00040000;   // kCGEventFlagMaskControl
pub const MOD_ALTERNATE: u64 = 0x00080000; // kCGEventFlagMaskAlternate
pub const MOD_COMMAND: u64 = 0x00100000;   // kCGEventFlagMaskCommand

/// Compute center point of an element's bounding box.
pub fn element_center(bounds: &Rect) -> (f64, f64) {
    (bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0)
}

/// Generate intermediate points for drag interpolation.
/// Returns `steps` points linearly interpolated from (x0,y0) to (x1,y1).
/// Point at index i is at t = (i+1)/steps.
pub fn drag_interpolation_points(
    x0: f64, y0: f64, x1: f64, y1: f64, steps: usize,
) -> Vec<(f64, f64)> {
    (1..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        })
        .collect()
}

/// Convert modifier string names to CGEventFlags bitmask.
pub fn modifier_string_to_flags(modifiers: &[String]) -> u64 {
    let mut flags: u64 = 0;
    for m in modifiers {
        match m.to_lowercase().as_str() {
            "cmd" | "command" => flags |= MOD_COMMAND,
            "shift" => flags |= MOD_SHIFT,
            "alt" | "option" => flags |= MOD_ALTERNATE,
            "ctrl" | "control" => flags |= MOD_CONTROL,
            _ => {} // ignore unknown modifiers
        }
    }
    flags
}

/// Execute a UI action. Dispatches to platform-specific implementation.
pub async fn execute_ui_action(
    pid: u32,
    req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    #[cfg(target_os = "macos")]
    {
        crate::ui::input_mac::execute_action(pid, req).await
    }

    #[cfg(not(target_os = "macos"))]
    {
        crate::ui::input_linux::execute_action(pid, req).await
    }
}

// Tests at bottom of file (see task 4a)
```

**`src/ui/input_linux.rs`** — new file:

```rust
use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse};

pub async fn execute_action(
    _pid: u32,
    _req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    Err(crate::Error::UiNotAvailable(
        "UI interaction is only supported on macOS".to_string(),
    ))
}
```

### 4d. Run tests — verify they pass

```
cargo test --lib ui::input::tests -- --nocapture
```

Expected: all 7 tests pass.

### Edge cases covered

- Zero-origin bounding box
- Single-step drag interpolation
- Empty modifier list → 0 flags
- All modifier flags combined
- Unknown modifiers silently ignored

### Checkpoint

Platform dispatch, coordinate math, and modifier mapping work. Linux stub returns `UiNotAvailable`.

---

## Task 5: macOS CGEvent motor

**Files:**
- Create: `src/ui/input_mac.rs`
- Modify: `Cargo.toml` (add `core-graphics` features)

### 5a. Update Cargo.toml

In the `[target.'cfg(target_os = "macos")'.dependencies]` section of `Cargo.toml`, change line 70:

```toml
core-graphics = { version = "0.24", features = ["elcapitan", "highsierra"] }
```

The `elcapitan` feature enables `CGEvent::post_to_pid` (targeted event delivery without deprecated PSN APIs). The `highsierra` feature enables `CGEvent::new_scroll_event`.

### 5b. Integration test

Integration tests for all action types are consolidated in Task 8 (`test_ui_actions_integration`) to avoid spawning multiple Frida sessions (per project convention: "Integration tests with Frida MUST run sequentially in single `#[tokio::test]`").

### 5c. Run compilation check — verify module compiles

```
cargo check
```

Expected: compilation error — `input_mac.rs` doesn't exist. After writing the implementation, `cargo check` should pass.

### 5d. Write implementation

**`src/ui/input_mac.rs`** — new file:

```rust
//! macOS UI interaction via AX actions and CGEvent injection.

use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse, UiActionType, ScrollDirection};
use crate::ui::accessibility::{find_ax_element, query_ax_tree};
use crate::ui::input::{
    element_center, drag_interpolation_points, modifier_string_to_flags,
    MOD_SHIFT, MOD_CONTROL, MOD_ALTERNATE, MOD_COMMAND,
};
use crate::ui::tree::{diff_nodes, UiNode};
use accessibility_sys::*;
use core_foundation::base::CFRelease;
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::number::{CFNumberCreate, kCFNumberFloat64Type};
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit,
};
use core_graphics::event_source::CGEventSource;
use core_graphics::event_source::CGEventSourceStateID;
use core_graphics::geometry::CGPoint;
use std::ffi::c_void;

const DEFAULT_SETTLE_MS: u64 = 80;
const DRAG_STEPS: usize = 10;
const DRAG_STEP_INTERVAL_MS: u64 = 16;

/// Execute a UI action against a macOS process.
pub async fn execute_action(
    pid: u32,
    req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    let settle_ms = req.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);

    // For key action, no node resolution needed
    if req.action == UiActionType::Key {
        let key_str = req.key.as_ref().unwrap(); // validated by caller
        let modifiers = req.modifiers.as_deref().unwrap_or(&[]);
        let send_result = tokio::task::spawn_blocking({
            let key_str = key_str.clone();
            let modifiers = modifiers.to_vec();
            move || send_key_event(pid, &key_str, &modifiers)
        }).await.map_err(|e| crate::Error::Internal(format!("Key event task failed: {}", e)))?;

        return match send_result {
            Ok(()) => Ok(DebugUiActionResponse {
                success: true,
                method: Some("cgevent".to_string()),
                node_before: None,
                node_after: None,
                changed: None,
                error: None,
            }),
            Err(e) => Ok(DebugUiActionResponse {
                success: false,
                method: None,
                node_before: None,
                node_after: None,
                changed: None,
                error: Some(e.to_string()),
            }),
        };
    }

    let target_id = req.id.as_ref().unwrap().clone(); // validated by caller

    // Resolve node + snapshot before
    let node_before = {
        let target_id = target_id.clone();
        let nodes = tokio::task::spawn_blocking(move || query_ax_tree(pid))
            .await.map_err(|e| crate::Error::Internal(format!("AX query failed: {}", e)))??;
        find_node_in_tree(&nodes, &target_id)
    };

    let node_before = match node_before {
        Some(n) => n,
        None => {
            return Ok(DebugUiActionResponse {
                success: false,
                method: None,
                node_before: None,
                node_after: None,
                changed: None,
                error: Some("node not found".to_string()),
            });
        }
    };

    let bounds = node_before.bounds.clone();

    // For drag, also resolve the destination node
    let to_bounds = if req.action == UiActionType::Drag {
        let to_id = req.to_id.as_ref().unwrap().clone();
        let nodes = tokio::task::spawn_blocking(move || query_ax_tree(pid))
            .await.map_err(|e| crate::Error::Internal(format!("AX query failed: {}", e)))??;
        match find_node_in_tree(&nodes, &to_id) {
            Some(n) => n.bounds.clone(),
            None => {
                return Ok(DebugUiActionResponse {
                    success: false,
                    method: None,
                    node_before: Some(node_before),
                    node_after: None,
                    changed: None,
                    error: Some("drag destination node not found".to_string()),
                });
            }
        }
    } else {
        None
    };

    // Execute the action
    let action_req = req.clone();
    let target_id_for_action = target_id.clone();
    let node_before_clone = node_before.clone();
    let execute_result = tokio::task::spawn_blocking(move || {
        execute_action_blocking(
            pid, &action_req, &target_id_for_action,
            &node_before_clone, bounds.as_ref(), to_bounds.as_ref(),
        )
    }).await.map_err(|e| crate::Error::Internal(format!("Action task failed: {}", e)))?;

    let method = match &execute_result {
        Ok(m) => m.clone(),
        Err(e) => {
            return Ok(DebugUiActionResponse {
                success: false,
                method: None,
                node_before: Some(node_before),
                node_after: None,
                changed: None,
                error: Some(e.to_string()),
            });
        }
    };

    // Settle
    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;

    // Verify — re-query target node
    let target_id_for_verify = target_id.clone();
    let node_after = tokio::task::spawn_blocking(move || {
        let nodes = query_ax_tree(pid)?;
        Ok::<_, crate::Error>(find_node_in_tree(&nodes, &target_id_for_verify))
    }).await.map_err(|e| crate::Error::Internal(format!("Verify task failed: {}", e)))??;

    let changed = node_after.as_ref().map(|after| diff_nodes(&node_before, after));

    Ok(DebugUiActionResponse {
        success: true,
        method: Some(method),
        node_before: Some(node_before),
        node_after,
        changed,
        error: None,
    })
}

/// Execute action on a blocking thread. Returns the method used ("ax" or "cgevent").
fn execute_action_blocking(
    pid: u32,
    req: &DebugUiActionRequest,
    target_id: &str,
    node: &UiNode,
    bounds: Option<&crate::ui::tree::Rect>,
    to_bounds: Option<&crate::ui::tree::Rect>,
) -> crate::Result<String> {
    match req.action {
        UiActionType::Click => execute_click(pid, target_id, node, bounds),
        UiActionType::SetValue => execute_set_value(pid, target_id, req),
        UiActionType::Type => execute_type(pid, target_id, req, bounds),
        UiActionType::Scroll => execute_scroll(pid, req, bounds),
        UiActionType::Drag => execute_drag(pid, bounds, to_bounds),
        UiActionType::Key => unreachable!("Key handled before blocking dispatch"),
    }
}

// ---- Action implementations ----

fn execute_click(
    pid: u32,
    target_id: &str,
    node: &UiNode,
    bounds: Option<&crate::ui::tree::Rect>,
) -> crate::Result<String> {
    // Try AX first if AXPress is available
    if node.actions.iter().any(|a| a == "AXPress") {
        if let Ok(()) = perform_ax_action(pid, target_id, "AXPress") {
            return Ok("ax".to_string());
        }
    }
    // Fall back to CGEvent click
    let bounds = bounds.ok_or_else(|| {
        crate::Error::UiQueryFailed("Element has no bounds for CGEvent click".to_string())
    })?;
    let (cx, cy) = element_center(bounds);
    cg_click(pid, cx, cy)?;
    Ok("cgevent".to_string())
}

fn execute_set_value(
    pid: u32,
    target_id: &str,
    req: &DebugUiActionRequest,
) -> crate::Result<String> {
    let value = req.value.as_ref().unwrap();

    unsafe {
        let ax_ref = find_ax_element(pid, target_id)?
            .ok_or_else(|| crate::Error::UiQueryFailed("node not found during set_value".to_string()))?;

        let result = if let Some(num) = value.as_f64() {
            // Check if this is a text field — convert to string
            let role = get_ax_role(ax_ref);
            if role.as_deref() == Some("AXTextField") || role.as_deref() == Some("AXTextArea") {
                set_ax_string_value(ax_ref, &num.to_string())
            } else {
                set_ax_number_value(ax_ref, num)
            }
        } else if let Some(s) = value.as_str() {
            set_ax_string_value(ax_ref, s)
        } else {
            set_ax_string_value(ax_ref, &value.to_string())
        };

        CFRelease(ax_ref as *const c_void);

        result.map_err(|_| {
            crate::Error::UiQueryFailed(
                "element does not support set_value; try 'type'".to_string()
            )
        })?;
    }

    Ok("ax".to_string())
}

fn execute_type(
    pid: u32,
    target_id: &str,
    req: &DebugUiActionRequest,
    bounds: Option<&crate::ui::tree::Rect>,
) -> crate::Result<String> {
    let text = req.text.as_ref().unwrap();

    // Try AX focus first
    let focused = unsafe {
        if let Ok(Some(ax_ref)) = find_ax_element(pid, target_id) {
            let attr = CFString::new(kAXFocusedAttribute);
            let true_val = core_foundation::boolean::CFBoolean::true_value();
            let err = AXUIElementSetAttributeValue(
                ax_ref,
                attr.as_concrete_TypeRef(),
                true_val.as_concrete_TypeRef() as CFTypeRef,
            );
            CFRelease(ax_ref as *const c_void);
            err == 0
        } else {
            false
        }
    };

    if !focused {
        // Fall back to CGEvent click to focus
        if let Some(bounds) = bounds {
            let (cx, cy) = element_center(bounds);
            cg_click(pid, cx, cy)?;
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    // Type characters via CGEvent
    cg_type_string(pid, text)?;
    Ok("cgevent".to_string())
}

fn execute_scroll(
    pid: u32,
    req: &DebugUiActionRequest,
    bounds: Option<&crate::ui::tree::Rect>,
) -> crate::Result<String> {
    let direction = req.direction.as_ref().unwrap();
    let amount = req.amount.unwrap_or(3);
    let bounds = bounds.ok_or_else(|| {
        crate::Error::UiQueryFailed("Element has no bounds for scroll".to_string())
    })?;
    let (cx, cy) = element_center(bounds);

    let (wheel1, wheel2) = match direction {
        ScrollDirection::Up => (amount, 0),
        ScrollDirection::Down => (-amount, 0),
        ScrollDirection::Left => (0, amount),
        ScrollDirection::Right => (0, -amount),
    };

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;
    // Position the cursor at the element center before scrolling.
    // CGEvent::set_location is not exposed by core-graphics 0.24 — use a mouse-move event instead.
    let move_event = CGEvent::new_mouse_event(
        source.clone(), CGEventType::MouseMoved,
        CGPoint::new(cx, cy), CGMouseButton::Left,
    ).map_err(|_| crate::Error::Internal("Failed to create mouse move event".to_string()))?;
    move_event.post_to_pid(pid as i32);

    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2, // wheel_count
        wheel1,
        wheel2,
        0,
    ).map_err(|_| crate::Error::Internal("Failed to create scroll event".to_string()))?;
    event.post_to_pid(pid as i32);

    Ok("cgevent".to_string())
}

fn execute_drag(
    pid: u32,
    from_bounds: Option<&crate::ui::tree::Rect>,
    to_bounds: Option<&crate::ui::tree::Rect>,
) -> crate::Result<String> {
    let from = from_bounds.ok_or_else(|| {
        crate::Error::UiQueryFailed("Source element has no bounds for drag".to_string())
    })?;
    let to = to_bounds.ok_or_else(|| {
        crate::Error::UiQueryFailed("Destination element has no bounds for drag".to_string())
    })?;
    let (x0, y0) = element_center(from);
    let (x1, y1) = element_center(to);

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;

    // Mouse down at source
    let down = CGEvent::new_mouse_event(
        source.clone(), CGEventType::LeftMouseDown,
        CGPoint::new(x0, y0), CGMouseButton::Left,
    ).map_err(|_| crate::Error::Internal("Failed to create mouse down event".to_string()))?;
    down.post_to_pid(pid as i32);

    // Interpolated drag moves
    let points = drag_interpolation_points(x0, y0, x1, y1, DRAG_STEPS);
    for (px, py) in points {
        std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
        let drag = CGEvent::new_mouse_event(
            source.clone(), CGEventType::LeftMouseDragged,
            CGPoint::new(px, py), CGMouseButton::Left,
        ).map_err(|_| crate::Error::Internal("Failed to create drag event".to_string()))?;
        drag.post_to_pid(pid as i32);
    }

    // Mouse up at destination
    std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
    let up = CGEvent::new_mouse_event(
        source, CGEventType::LeftMouseUp,
        CGPoint::new(x1, y1), CGMouseButton::Left,
    ).map_err(|_| crate::Error::Internal("Failed to create mouse up event".to_string()))?;
    up.post_to_pid(pid as i32);

    Ok("cgevent".to_string())
}

// ---- CGEvent helpers ----

fn cg_click(pid: u32, x: f64, y: f64) -> crate::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;
    let point = CGPoint::new(x, y);

    let down = CGEvent::new_mouse_event(source.clone(), CGEventType::LeftMouseDown, point, CGMouseButton::Left)
        .map_err(|_| crate::Error::Internal("Failed to create mouse down".to_string()))?;
    let up = CGEvent::new_mouse_event(source, CGEventType::LeftMouseUp, point, CGMouseButton::Left)
        .map_err(|_| crate::Error::Internal("Failed to create mouse up".to_string()))?;
    down.post_to_pid(pid as i32);
    up.post_to_pid(pid as i32);
    Ok(())
}

fn cg_type_string(pid: u32, text: &str) -> crate::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;

    // Inject the full string via set_string_from_utf16_unchecked on a dummy key event.
    // This is simpler than the spec's UCKeyTranslate approach but handles all Unicode
    // including emoji and non-ASCII — sufficient for LLM-driven text entry.
    let key_down = CGEvent::new_keyboard_event(source, 0, true)
        .map_err(|_| crate::Error::Internal("Failed to create key event".to_string()))?;
    let utf16: Vec<u16> = text.encode_utf16().collect();
    unsafe {
        key_down.set_string_from_utf16_unchecked(&utf16);
    }
    key_down.post_to_pid(pid as i32);
    Ok(())
}

fn send_key_event(pid: u32, key: &str, modifiers: &[String]) -> crate::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;

    let keycode = key_name_to_keycode(key)?;
    let flags = modifier_string_to_flags(modifiers);

    let down = CGEvent::new_keyboard_event(source.clone(), keycode, true)
        .map_err(|_| crate::Error::Internal("Failed to create key down event".to_string()))?;
    let up = CGEvent::new_keyboard_event(source, keycode, false)
        .map_err(|_| crate::Error::Internal("Failed to create key up event".to_string()))?;

    if flags != 0 {
        use core_graphics::event::CGEventFlags;
        let cg_flags = CGEventFlags::from_bits_truncate(flags);
        down.set_flags(cg_flags);
        up.set_flags(cg_flags);
    }

    down.post_to_pid(pid as i32);
    up.post_to_pid(pid as i32);
    Ok(())
}

/// Map common key names to macOS virtual key codes.
/// Returns ValidationError for unrecognized key names.
fn key_name_to_keycode(key: &str) -> crate::Result<u16> {
    match key.to_lowercase().as_str() {
        "a" => Ok(0x00), "s" => Ok(0x01), "d" => Ok(0x02), "f" => Ok(0x03),
        "h" => Ok(0x04), "g" => Ok(0x05), "z" => Ok(0x06), "x" => Ok(0x07),
        "c" => Ok(0x08), "v" => Ok(0x09), "b" => Ok(0x0B), "q" => Ok(0x0C),
        "w" => Ok(0x0D), "e" => Ok(0x0E), "r" => Ok(0x0F), "y" => Ok(0x10),
        "t" => Ok(0x11), "1" => Ok(0x12), "2" => Ok(0x13), "3" => Ok(0x14),
        "4" => Ok(0x15), "5" => Ok(0x17), "6" => Ok(0x16), "7" => Ok(0x1A),
        "8" => Ok(0x1C), "9" => Ok(0x19), "0" => Ok(0x1D), "o" => Ok(0x1F),
        "u" => Ok(0x20), "i" => Ok(0x22), "p" => Ok(0x23), "l" => Ok(0x25),
        "j" => Ok(0x26), "k" => Ok(0x28), "n" => Ok(0x2D), "m" => Ok(0x2E),
        "return" | "enter" => Ok(0x24),
        "tab" => Ok(0x30),
        "space" => Ok(0x31),
        "delete" | "backspace" => Ok(0x33),
        "escape" | "esc" => Ok(0x35),
        "left" => Ok(0x7B), "right" => Ok(0x7C), "down" => Ok(0x7D), "up" => Ok(0x7E),
        "f1" => Ok(0x7A), "f2" => Ok(0x78), "f3" => Ok(0x63), "f4" => Ok(0x76),
        "f5" => Ok(0x60), "f6" => Ok(0x61), "f7" => Ok(0x62), "f8" => Ok(0x64),
        "f9" => Ok(0x65), "f10" => Ok(0x6D), "f11" => Ok(0x67), "f12" => Ok(0x6F),
        other => Err(crate::Error::ValidationError(
            format!("unknown key: '{}'. Supported: a-z, 0-9, return, tab, space, delete, escape, arrow keys, f1-f12", other)
        )),
    }
}

// ---- AX action helpers ----

fn perform_ax_action(pid: u32, target_id: &str, action_name: &str) -> crate::Result<()> {
    unsafe {
        let ax_ref = find_ax_element(pid, target_id)?
            .ok_or_else(|| crate::Error::UiQueryFailed("node not found for AX action".to_string()))?;

        let action = CFString::new(action_name);
        let err = AXUIElementPerformAction(ax_ref, action.as_concrete_TypeRef());
        CFRelease(ax_ref as *const c_void);

        if err != 0 {
            return Err(crate::Error::UiQueryFailed(
                format!("AX action '{}' failed with error {}", action_name, err)
            ));
        }
        Ok(())
    }
}

unsafe fn get_ax_role(element: AXUIElementRef) -> Option<String> {
    let attr = CFString::new(kAXRoleAttribute);
    let mut value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr.as_concrete_TypeRef(), &mut value);
    if err != 0 || value.is_null() {
        return None;
    }
    if core_foundation_sys::base::CFGetTypeID(value) != core_foundation_sys::string::CFStringGetTypeID() {
        CFRelease(value as *const c_void);
        return None;
    }
    let cf_str = CFString::wrap_under_create_rule(value as core_foundation_sys::string::CFStringRef);
    Some(cf_str.to_string())
}

unsafe fn set_ax_string_value(element: AXUIElementRef, s: &str) -> Result<(), ()> {
    let attr = CFString::new(kAXValueAttribute);
    let value = CFString::new(s);
    let err = AXUIElementSetAttributeValue(
        element,
        attr.as_concrete_TypeRef(),
        value.as_concrete_TypeRef() as CFTypeRef,
    );
    if err != 0 { Err(()) } else { Ok(()) }
}

unsafe fn set_ax_number_value(element: AXUIElementRef, num: f64) -> Result<(), ()> {
    let attr = CFString::new(kAXValueAttribute);
    let cf_num = CFNumberCreate(
        std::ptr::null(),
        kCFNumberFloat64Type,
        &num as *const f64 as *const c_void,
    );
    if cf_num.is_null() {
        return Err(());
    }
    let err = AXUIElementSetAttributeValue(
        element,
        attr.as_concrete_TypeRef(),
        cf_num as CFTypeRef,
    );
    CFRelease(cf_num as *const c_void);
    if err != 0 { Err(()) } else { Ok(()) }
}

/// Find a node by ID in a tree of UiNodes.
fn find_node_in_tree(nodes: &[UiNode], target_id: &str) -> Option<UiNode> {
    for node in nodes {
        if node.id == target_id {
            return Some(node.clone());
        }
        if let Some(found) = find_node_in_tree(&node.children, target_id) {
            return Some(found);
        }
    }
    None
}
```

### 5e. Run test — verify it passes

```
cargo test --test ui_observation test_ui_action_click -- --nocapture
```

Expected: click test passes. The button gets AXPress action.

### Edge cases covered

- AX action available → uses AX path; AX unavailable → falls through to CGEvent
- Element with no bounds → error
- Drag destination not found → error with `node_before` still populated
- `set_value` number → CFNumber for sliders, string for text fields
- `type` AX focus fails → falls back to CGEvent click-to-focus
- Unicode text injection via `set_string_from_utf16_unchecked`
- Unknown key name → `ValidationError` with descriptive message (not silent fallback)
- Scroll positions cursor via mouse-move event (since `CGEvent::set_location` is not exposed in the Rust crate)

### Checkpoint

Full motor layer works on macOS. All 6 action types implemented. AX-first with CGEvent fallback for click and type.

---

## Task 6: UITestApp fixture — add drag targets

**Files:**
- Modify: `tests/fixtures/ui-test-app/UITestApp.swift`

### 6a. No TDD for Swift fixture — just verify the app builds

Add drag targets to UITestApp. Insert before the `Spacer()` (line 71) in UITestApp.swift:

```swift
            Divider()

            // Drag targets for testing
            HStack(spacing: 20) {
                Text(dragLabel)
                    .frame(width: 100, height: 40)
                    .background(Color.blue.opacity(0.3))
                    .accessibilityIdentifier("drag_source")
                    .accessibilityLabel("Drag Source")
                    .draggable("drag_payload")

                Text("Drop Here")
                    .frame(width: 100, height: 40)
                    .background(dragReceived ? Color.green.opacity(0.3) : Color.gray.opacity(0.3))
                    .accessibilityIdentifier("drop_target")
                    .accessibilityLabel(dragReceived ? "Drop Received" : "Drop Here")
                    .dropDestination(for: String.self) { items, _ in
                        if let _ = items.first {
                            dragReceived = true
                            return true
                        }
                        return false
                    }
            }
            .padding()
```

And add state variables to `ContentView`:

```swift
@State private var dragReceived: Bool = false
@State private var dragLabel: String = "Drag Me"
```

### 6b. Build and verify

```bash
cd tests/fixtures/ui-test-app && bash build.sh
```

Expected: clean build. Binary at `build/UITestApp`.

### Checkpoint

UITestApp has all widgets needed for testing: button, toggle, slider, text field, list, and drag source + drop target.

---

## Task 7: Server wiring — tool definition and handler

**Files:**
- Modify: `src/daemon/server.rs` (tool list + dispatch + handler)

### 7a. Integration tests

Integration tests for the handler are in Task 8's consolidated test (`test_ui_actions_integration`), which exercises all action types through the module API. The handler itself is a thin wrapper around `execute_ui_action` with session validation.

### 7b. Verify compilation

### 7c. Write implementation

**Add MCP tool definition** — in `src/daemon/server.rs`, after the `debug_ui` McpTool (after line 889, before the `];`):

```rust
McpTool {
    name: "debug_ui_action".to_string(),
    description: "Perform a UI action on a running process. Actions: click, set_value, type, key, scroll, drag. Uses accessibility actions when available, falls back to synthesized input events. Returns before/after node state for verification.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session ID (from debug_launch)" },
            "action": { "type": "string", "enum": ["click", "set_value", "type", "key", "scroll", "drag"], "description": "Action to perform" },
            "id": { "type": "string", "description": "Target node ID from debug_ui tree. Required for all except 'key'." },
            "value": { "description": "Value to set (number or string). Required for 'set_value'." },
            "text": { "type": "string", "description": "Text to type. Required for 'type'." },
            "key": { "type": "string", "description": "Key name (e.g. 's', 'return', 'escape'). Required for 'key'." },
            "modifiers": { "type": "array", "items": { "type": "string" }, "description": "Modifier keys: 'cmd', 'shift', 'alt', 'ctrl'" },
            "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "description": "Scroll direction. Required for 'scroll'." },
            "amount": { "type": "integer", "description": "Scroll amount in lines (default: 3)" },
            "toId": { "type": "string", "description": "Drag destination node ID. Required for 'drag'." },
            "settleMs": { "type": "integer", "description": "Wait time after action for UI to update (default: 80ms)" }
        },
        "required": ["sessionId", "action"]
    }),
},
```

**Add dispatch arm** — in `handle_tools_call`, after the `"debug_ui"` arm (after line 915):

```rust
"debug_ui_action" => {
    let content = self.tool_debug_ui_action(&call.arguments).await?;
    let response = McpToolCallResponse {
        content,
        is_error: None,
    };
    return Ok(serde_json::to_value(response)?);
}
```

**Add handler** — after the `tool_debug_ui` function:

```rust
async fn tool_debug_ui_action(&self, args: &serde_json::Value) -> Result<Vec<McpContent>> {
    let req: crate::mcp::DebugUiActionRequest = serde_json::from_value(args.clone())?;
    req.validate()?;

    let session = self.require_session(&req.session_id)?;
    if session.status != crate::db::SessionStatus::Running {
        return Err(crate::Error::UiQueryFailed(
            format!("Process not running (PID {} exited). Cannot perform UI action.", session.pid)
        ));
    }

    let pid = session.pid;
    let result = crate::ui::input::execute_ui_action(pid, &req).await?;

    let text = serde_json::to_string_pretty(&result)?;
    Ok(vec![McpContent::Text { text }])
}
```

### 7d. Run compilation check

```
cargo check
```

Expected: compiles. Full runtime tests are in Task 8.

### Edge cases covered

- Session not running → error before action
- Validation errors → rejected before execution

### Checkpoint

Full MCP tool wired in. `debug_ui_action` appears in tool list, dispatches to handler, returns JSON response. Runtime validation in Task 8.

---

## Task 8: Consolidated integration test

**Files:**
- Modify: `tests/ui_observation.rs`

All UI action integration tests are in a single `#[tokio::test]` to avoid spawning multiple Frida sessions (per project convention). This test covers all 6 action types, error cases, `find_ax_element` positive verification, `set_value` with string, and the MCP handler round-trip.

### 8a. Write tests

Add to `tests/ui_observation.rs` in `macos_tests`:

```rust
/// Helper: find first node with matching role in tree (recursive)
fn find_node_by_role_recursive(nodes: &[strobe::ui::tree::UiNode], role: &str) -> Option<String> {
    for node in nodes {
        if node.role == role {
            return Some(node.id.clone());
        }
        if let Some(found) = find_node_by_role_recursive(&node.children, role) {
            return Some(found);
        }
    }
    None
}

/// Consolidated UI action integration test.
/// Spawns the UI test app once, tests all 6 action types sequentially.
#[tokio::test(flavor = "multi_thread")]
async fn test_ui_actions_integration() {
    let _guard = ui_integration_lock().lock().await;

    let binary = ui_test_app();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    let session_id = "ui-actions";
    sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
    let pid = sm.spawn_with_frida(
        session_id, binary.to_str().unwrap(), &[],
        None, project_root, None, false, None,
    ).await.unwrap();
    sm.update_session_pid(session_id, pid).unwrap();

    tokio::time::sleep(Duration::from_secs(3)).await;

    // ---- Verify find_ax_element positive case ----
    let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
    let button_id = find_node_by_role_recursive(&nodes, "AXButton")
        .expect("Should find a button in the test app");
    // find_ax_element should locate the element by ID
    let ax_ref = strobe::ui::accessibility::find_ax_element(pid, &button_id).unwrap();
    assert!(ax_ref.is_some(), "find_ax_element should locate button by ID");
    unsafe { core_foundation::base::CFRelease(ax_ref.unwrap() as *const std::ffi::c_void) };

    // ---- 1. Click ----
    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Click,
        id: Some(button_id.clone()),
        value: None, text: None, key: None, modifiers: None,
        direction: None, amount: None, to_id: None, settle_ms: None,
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(result.success, "Click should succeed");
    assert!(result.method.is_some(), "Should report method used");
    assert!(result.node_after.is_some(), "Should return node_after");

    // ---- 2. Set value (number on slider) ----
    let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
    let slider_id = find_node_by_role_recursive(&nodes, "AXSlider")
        .expect("Should find a slider in test app");

    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::SetValue,
        id: Some(slider_id),
        value: Some(serde_json::json!(0.8)),
        text: None, key: None, modifiers: None,
        direction: None, amount: None, to_id: None, settle_ms: Some(200),
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(result.success, "set_value should succeed: {:?}", result.error);
    assert_eq!(result.method.as_deref(), Some("ax"));
    if let Some(true) = result.changed {
        let after_val = result.node_after.as_ref()
            .and_then(|n| n.value.as_ref())
            .and_then(|v| v.parse::<f64>().ok());
        if let Some(v) = after_val {
            assert!((v - 0.8).abs() < 0.15, "Slider value should be ~0.8, got {}", v);
        }
    }

    // ---- 3. Set value (string on text field) ----
    let text_id = find_node_by_role_recursive(&nodes, "AXTextField")
        .expect("Should find a text field in test app");

    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::SetValue,
        id: Some(text_id.clone()),
        value: Some(serde_json::json!("programmatic")),
        text: None, key: None, modifiers: None,
        direction: None, amount: None, to_id: None, settle_ms: Some(200),
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    // set_value on text field may succeed (AX settable) or fail (not all text fields support it)
    // Either outcome is valid — we're testing the string conversion path doesn't crash
    if result.success {
        assert_eq!(result.method.as_deref(), Some("ax"));
    }

    // ---- 4. Type text ----
    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Type,
        id: Some(text_id),
        text: Some("hello world".to_string()),
        value: None, key: None, modifiers: None,
        direction: None, amount: None, to_id: None, settle_ms: Some(200),
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(result.success, "type should succeed: {:?}", result.error);
    assert!(result.node_after.is_some());

    // ---- 5. Key shortcut ----
    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Key,
        id: None,
        key: Some("a".to_string()),
        modifiers: Some(vec!["cmd".to_string()]),
        value: None, text: None,
        direction: None, amount: None, to_id: None, settle_ms: None,
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(result.success, "key should succeed: {:?}", result.error);
    assert_eq!(result.method.as_deref(), Some("cgevent"));
    assert!(result.node_before.is_none());
    assert!(result.node_after.is_none());

    // ---- 6. Scroll ----
    let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
    let list_id = find_node_by_role_recursive(&nodes, "AXList")
        .or_else(|| find_node_by_role_recursive(&nodes, "AXTable"))
        .or_else(|| find_node_by_role_recursive(&nodes, "AXScrollArea"))
        .expect("Should find a scrollable element in test app");

    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Scroll,
        id: Some(list_id),
        direction: Some(strobe::mcp::ScrollDirection::Down),
        amount: Some(3),
        value: None, text: None, key: None, modifiers: None,
        to_id: None, settle_ms: Some(200),
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(result.success, "scroll should succeed: {:?}", result.error);
    assert_eq!(result.method.as_deref(), Some("cgevent"));
    // Note: changed may be false for scroll — acceptable per spec

    // ---- 7. Drag ----
    let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
    let drag_source_id = find_node_by_role_recursive(&nodes, "AXStaticText")
        .expect("Should find drag source in test app");
    // Find the drop target (second AXStaticText after drag source, or by title)
    fn find_node_by_title_recursive(
        nodes: &[strobe::ui::tree::UiNode], title: &str,
    ) -> Option<String> {
        for node in nodes {
            if node.title.as_deref() == Some(title) {
                return Some(node.id.clone());
            }
            if let Some(found) = find_node_by_title_recursive(&node.children, title) {
                return Some(found);
            }
        }
        None
    }
    // Drag test is best-effort — SwiftUI drag/drop via CGEvent is unreliable.
    // We verify the mechanics work (no crash, correct method) without asserting the drop landed.
    if let (Some(src_id), Some(dst_id)) = (
        find_node_by_title_recursive(&nodes, "Drag Source"),
        find_node_by_title_recursive(&nodes, "Drop Here"),
    ) {
        let req = strobe::mcp::DebugUiActionRequest {
            session_id: session_id.to_string(),
            action: strobe::mcp::UiActionType::Drag,
            id: Some(src_id),
            to_id: Some(dst_id),
            value: None, text: None, key: None, modifiers: None,
            direction: None, amount: None, settle_ms: Some(300),
        };
        let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
        assert!(result.success, "drag should succeed: {:?}", result.error);
        assert_eq!(result.method.as_deref(), Some("cgevent"));
        assert!(result.node_before.is_some(), "drag should have node_before");
    }

    // ---- 8. Error case: node not found ----
    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Click,
        id: Some("btn_0000".to_string()), // bogus ID
        value: None, text: None, key: None, modifiers: None,
        direction: None, amount: None, to_id: None, settle_ms: None,
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(!result.success);
    assert!(result.error.as_deref().unwrap().contains("not found"));

    // ---- 9. Error case: unknown key name ----
    let req = strobe::mcp::DebugUiActionRequest {
        session_id: session_id.to_string(),
        action: strobe::mcp::UiActionType::Key,
        id: None,
        key: Some("pagedown".to_string()), // not in keycode table
        modifiers: None,
        value: None, text: None,
        direction: None, amount: None, to_id: None, settle_ms: None,
    };
    let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
    assert!(!result.success, "Unknown key should fail");
    assert!(result.error.as_deref().unwrap().contains("unknown key"));

    // ---- Cleanup ----
    let _ = sm.stop_frida(session_id).await;
    sm.stop_session(session_id).await.unwrap();
}
```

### 8b. Run tests — verify they pass

```
cargo test --test ui_observation test_ui_actions_integration -- --nocapture
```

Expected: all 9 scenarios pass within the single test. One app spawn, one 3-second wait.

### Edge cases covered

- `find_ax_element` positive verification (button found by ID)
- Click via AX action path
- `set_value` with number on slider (CFNumber path)
- `set_value` with string on text field (CFString path)
- Type text into focused field
- Key shortcut with modifier (Cmd+A)
- Scroll with `changed: false` acceptable per spec
- Drag between elements (best-effort — CGEvent drag is unreliable in SwiftUI)
- Node not found → graceful `success: false`
- Unknown key name → error with descriptive message

### Checkpoint

All 6 action types work end-to-end. Error cases handled. Full MCP tool wired in and tested. Single Frida session for all integration tests.

---

## Summary

| Task | Files | Tests |
|------|-------|-------|
| 1. MCP types + validation | `src/mcp/types.rs` | 11 unit tests |
| 2. Node diff logic | `src/ui/tree.rs` | 5 unit tests |
| 3. `find_ax_element` | `src/ui/accessibility.rs` | 1 unit test |
| 4. Input trait + Linux stub | `src/ui/input.rs`, `src/ui/input_linux.rs`, `src/ui/mod.rs` | 7 unit tests |
| 5. macOS CGEvent motor | `src/ui/input_mac.rs`, `Cargo.toml` | (tested in Task 8) |
| 6. UITestApp drag targets | `tests/fixtures/ui-test-app/UITestApp.swift` | build verification |
| 7. Server wiring | `src/daemon/server.rs` | (tested in Task 8) |
| 8. Consolidated integration | `tests/ui_observation.rs` | 1 test (9 scenarios) |

**Total:** 8 tasks, 25 unit tests + 1 integration test (9 scenarios: click, set_value number, set_value string, type, key, scroll, drag, node-not-found, unknown-key).
