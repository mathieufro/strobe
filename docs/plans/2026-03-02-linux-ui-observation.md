# Linux UI Observation Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Bring `debug_ui` and `debug_ui_action` MCP tools to Linux with identical output structure (same `UiNode` tree, same `DebugUiActionResponse`) as macOS.

**Architecture:** Three new modules (`accessibility_linux.rs`, `capture_linux.rs`, `input_linux.rs`) replace stubs, mirroring macOS structure. AT-SPI2 via `atspi` crate for accessibility, X11 via `x11rb` for screenshots and XTest input. Linux `query_ax_tree` is async (D-Bus is async by nature); server.rs gets parallel `#[cfg(target_os = "linux")]` blocks that call async directly instead of `spawn_blocking`.

**Tech Stack:** `atspi` 0.29 (zbus-based AT-SPI2), `x11rb` 0.13 (X11 protocol + XTest), `png` 0.17 (shared with macOS)

**Spec:** `.atelier/specs/2026-03-02-linux-ui-observation.md`

---

## Phase 1: Foundation

### Task 1: Add Linux dependencies to Cargo.toml

**Files:**
- Modify: `Cargo.toml:64-70`

**Step 1: Add Linux target dependencies and move png to shared**

Move `png` from macOS-only to shared dependencies, add Linux-specific crates:

```toml
# In [dependencies] section (shared), add after base64 line:
png = "0.17"

# Remove png from macOS section. Final macOS section:
[target.'cfg(target_os = "macos")'.dependencies]
accessibility-sys = "0.2"
core-foundation = "0.10"
core-foundation-sys = "0.8"
core-graphics = { version = "0.24", features = ["elcapitan", "highsierra"] }

# Add new Linux section:
[target.'cfg(target_os = "linux")'.dependencies]
atspi = { version = "0.29", features = ["proxies", "connection", "zbus"] }
x11rb = { version = "0.13", features = ["xtest", "composite"] }
```

**Step 2: Verify Cargo.toml parses**

Run: `cargo check --message-format=short 2>&1 | head -5`
Expected: No toml parse errors. May have unused import warnings — that's fine.

**Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "feat: add Linux AT-SPI2 and X11 dependencies for UI observation"
```

---

### Task 2: Module declarations and capture_linux stub

**Files:**
- Modify: `src/ui/mod.rs`
- Create: `src/ui/capture_linux.rs` (stub)

**Step 1: Add capture_linux module declaration to mod.rs**

Current `src/ui/mod.rs` has no `capture_linux` declaration. Add it alongside the existing accessibility_linux pattern:

```rust
pub mod tree;

#[cfg(target_os = "macos")]
pub mod accessibility;

#[cfg(target_os = "macos")]
pub mod capture;

// COMP-3: Linux accessibility stub (AT-SPI not yet implemented)
#[cfg(target_os = "linux")]
pub mod accessibility_linux;

#[cfg(target_os = "linux")]
pub use accessibility_linux as accessibility;

#[cfg(target_os = "linux")]
pub mod capture_linux;

#[cfg(target_os = "linux")]
pub use capture_linux as capture;

pub mod vision;
pub mod merge;

pub mod input;

#[cfg(target_os = "macos")]
pub mod input_mac;

#[cfg(target_os = "linux")]
mod input_linux;
```

**Step 2: Create capture_linux.rs stub**

```rust
//! Linux screenshot capture via X11 (GetImage + _NET_WM_PID).

use crate::Result;

/// Capture a screenshot of the main window for a given PID.
/// Returns PNG bytes.
pub fn capture_window_screenshot(_pid: u32) -> Result<Vec<u8>> {
    Err(crate::Error::UiNotAvailable(
        "Linux X11 screenshot capture not yet implemented".to_string(),
    ))
}

/// Capture a screenshot cropped to a specific element's bounds.
pub fn capture_element_screenshot(
    _pid: u32,
    _element_bounds: &crate::ui::tree::Rect,
) -> Result<Vec<u8>> {
    Err(crate::Error::UiNotAvailable(
        "Linux X11 screenshot capture not yet implemented".to_string(),
    ))
}
```

**Step 3: Verify it compiles**

Run: `cargo check 2>&1 | tail -5`
Expected: Compiles (warnings OK).

**Step 4: Commit**

```bash
git add src/ui/mod.rs src/ui/capture_linux.rs
git commit -m "feat: add capture_linux module stub and mod.rs declarations"
```

---

## Phase 2: Testable Pure Logic

### Task 3: AT-SPI2 role mapping table

**Files:**
- Modify: `src/ui/accessibility_linux.rs`

**Step 1: Write failing tests for role mapping**

Add to `accessibility_linux.rs` below the existing stub code:

```rust
/// Map AT-SPI2 role name to macOS AX-style role for cross-platform consistency.
fn map_atspi_role(role: atspi::Role) -> String {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_mapping_common_roles() {
        // Buttons
        assert_eq!(map_atspi_role(atspi::Role::PushButton), "AXButton");
        assert_eq!(map_atspi_role(atspi::Role::CheckBox), "AXCheckBox");
        assert_eq!(map_atspi_role(atspi::Role::RadioButton), "AXRadioButton");
        assert_eq!(map_atspi_role(atspi::Role::ToggleButton), "AXCheckBox");
    }

    #[test]
    fn test_role_mapping_text_fields() {
        assert_eq!(map_atspi_role(atspi::Role::Entry), "AXTextField");
        assert_eq!(map_atspi_role(atspi::Role::PasswordText), "AXSecureTextField");
    }

    #[test]
    fn test_role_mapping_containers() {
        assert_eq!(map_atspi_role(atspi::Role::Frame), "AXWindow");
        assert_eq!(map_atspi_role(atspi::Role::Panel), "AXGroup");
        assert_eq!(map_atspi_role(atspi::Role::Filler), "AXGroup");
        assert_eq!(map_atspi_role(atspi::Role::ScrollPane), "AXScrollArea");
    }

    #[test]
    fn test_role_mapping_tables() {
        assert_eq!(map_atspi_role(atspi::Role::Table), "AXTable");
        assert_eq!(map_atspi_role(atspi::Role::TableCell), "AXCell");
        assert_eq!(map_atspi_role(atspi::Role::TableRow), "AXRow");
        assert_eq!(map_atspi_role(atspi::Role::List), "AXList");
        assert_eq!(map_atspi_role(atspi::Role::ListItem), "AXRow");
    }

    #[test]
    fn test_role_mapping_menus() {
        assert_eq!(map_atspi_role(atspi::Role::Menu), "AXMenu");
        assert_eq!(map_atspi_role(atspi::Role::MenuItem), "AXMenuItem");
        assert_eq!(map_atspi_role(atspi::Role::MenuBar), "AXMenuBar");
    }

    #[test]
    fn test_role_mapping_misc() {
        assert_eq!(map_atspi_role(atspi::Role::Slider), "AXSlider");
        assert_eq!(map_atspi_role(atspi::Role::ProgressBar), "AXProgressIndicator");
        assert_eq!(map_atspi_role(atspi::Role::Label), "AXStaticText");
        assert_eq!(map_atspi_role(atspi::Role::Link), "AXLink");
        assert_eq!(map_atspi_role(atspi::Role::Image), "AXImage");
        assert_eq!(map_atspi_role(atspi::Role::Heading), "AXHeading");
        assert_eq!(map_atspi_role(atspi::Role::Separator), "AXSplitter");
    }

    #[test]
    fn test_role_mapping_unknown_passthrough() {
        // Unknown roles pass through as atspi:{RoleName}
        assert!(map_atspi_role(atspi::Role::DesktopFrame).starts_with("atspi:"));
    }
}
```

**Step 2: Run tests to see them fail**

Run: `cargo test --lib ui::accessibility_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: FAIL — `todo!()` panics.

**Step 3: Implement role mapping**

Replace `todo!()` with the full match table from the spec (~30 entries). The `atspi::Role` enum variants are PascalCase. Unknown roles use `format!("atspi:{:?}", role)`:

```rust
fn map_atspi_role(role: atspi::Role) -> String {
    match role {
        atspi::Role::PushButton => "AXButton".into(),
        atspi::Role::CheckBox | atspi::Role::ToggleButton => "AXCheckBox".into(),
        atspi::Role::RadioButton => "AXRadioButton".into(),
        atspi::Role::Entry => "AXTextField".into(),
        atspi::Role::PasswordText => "AXSecureTextField".into(),
        atspi::Role::ComboBox => "AXPopUpButton".into(),
        atspi::Role::Slider => "AXSlider".into(),
        atspi::Role::SpinButton => "AXIncrementor".into(),
        atspi::Role::ProgressBar => "AXProgressIndicator".into(),
        atspi::Role::Label | atspi::Role::StaticText => "AXStaticText".into(),
        atspi::Role::Link => "AXLink".into(),
        atspi::Role::Image => "AXImage".into(),
        atspi::Role::Table => "AXTable".into(),
        atspi::Role::TableCell => "AXCell".into(),
        atspi::Role::TableRow => "AXRow".into(),
        atspi::Role::TableColumnHeader => "AXColumn".into(),
        atspi::Role::TreeTable => "AXOutline".into(),
        atspi::Role::List => "AXList".into(),
        atspi::Role::ListItem => "AXRow".into(),
        atspi::Role::Menu => "AXMenu".into(),
        atspi::Role::MenuItem => "AXMenuItem".into(),
        atspi::Role::MenuBar => "AXMenuBar".into(),
        atspi::Role::ToolBar => "AXToolbar".into(),
        atspi::Role::StatusBar => "AXStatusBar".into(),
        atspi::Role::Dialog => "AXDialog".into(),
        atspi::Role::Alert => "AXSheet".into(),
        atspi::Role::Frame | atspi::Role::Window => "AXWindow".into(),
        atspi::Role::Panel | atspi::Role::Filler => "AXGroup".into(),
        atspi::Role::ScrollBar => "AXScrollBar".into(),
        atspi::Role::ScrollPane => "AXScrollArea".into(),
        atspi::Role::PageTabList => "AXTabGroup".into(),
        atspi::Role::PageTab => "AXTab".into(),
        atspi::Role::Separator => "AXSplitter".into(),
        atspi::Role::Heading => "AXHeading".into(),
        other => format!("atspi:{:?}", other),
    }
}
```

> **Note:** The exact `atspi::Role` variant names may differ from the spec (which uses AT-SPI2 D-Bus names). Check the `atspi` crate's `Role` enum docs during implementation. Adjust variant names to match the crate's API. The mapping logic stays the same.

**Step 4: Run tests to see them pass**

Run: `cargo test --lib ui::accessibility_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: All PASS.

**Step 5: Commit**

```bash
git add src/ui/accessibility_linux.rs
git commit -m "feat: AT-SPI2 role mapping table with unit tests"
```

---

### Task 4: AT-SPI2 action name mapping

**Files:**
- Modify: `src/ui/accessibility_linux.rs`

**Step 1: Write failing tests for action name mapping**

```rust
/// Map AT-SPI2 action name to macOS AX-style action name.
fn map_atspi_action(action: &str) -> String {
    todo!()
}

// Add to tests module:
#[test]
fn test_action_name_mapping() {
    assert_eq!(map_atspi_action("click"), "AXPress");
    assert_eq!(map_atspi_action("press"), "AXPress");
    assert_eq!(map_atspi_action("activate"), "AXPress");
    assert_eq!(map_atspi_action("toggle"), "AXPress");
}

#[test]
fn test_action_name_mapping_unknown_passthrough() {
    assert_eq!(map_atspi_action("custom-action"), "atspi:custom-action");
}
```

**Step 2: Run to see failure**

Run: `cargo test --lib ui::accessibility_linux::tests::test_action_name -- --nocapture 2>&1 | tail -5`
Expected: FAIL.

**Step 3: Implement**

```rust
fn map_atspi_action(action: &str) -> String {
    match action {
        "click" | "press" | "activate" | "toggle" => "AXPress".into(),
        "expand or contract" => "AXPress".into(),
        other => format!("atspi:{}", other),
    }
}
```

**Step 4: Run tests**

Run: `cargo test --lib ui::accessibility_linux::tests::test_action_name -- --nocapture`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/ui/accessibility_linux.rs
git commit -m "feat: AT-SPI2 action name mapping"
```

---

### Task 5: Pixel format conversion utilities

**Files:**
- Modify: `src/ui/capture_linux.rs`

**Step 1: Write failing tests for pixel format conversion**

```rust
/// Convert BGRX pixel data (depth 24, bpp 32) to RGBA.
/// The X byte is ignored and alpha is set to 255.
fn bgrx_to_rgba(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    todo!()
}

/// Convert BGRA pixel data (depth 32, bpp 32) to RGBA.
fn bgra_to_rgba(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    todo!()
}

/// Encode raw RGBA pixel data as PNG.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgrx_to_rgba_single_pixel() {
        // BGRX: B=0x10, G=0x20, R=0x30, X=0xFF
        let data = vec![0x10, 0x20, 0x30, 0xFF];
        let rgba = bgrx_to_rgba(&data, 1, 1, 4);
        assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0xFF]); // R, G, B, A=255
    }

    #[test]
    fn test_bgrx_to_rgba_two_pixels() {
        let data = vec![
            0x10, 0x20, 0x30, 0x00, // pixel 1: B,G,R,X
            0xAA, 0xBB, 0xCC, 0x00, // pixel 2: B,G,R,X
        ];
        let rgba = bgrx_to_rgba(&data, 2, 1, 8);
        assert_eq!(rgba, vec![
            0x30, 0x20, 0x10, 0xFF,
            0xCC, 0xBB, 0xAA, 0xFF,
        ]);
    }

    #[test]
    fn test_bgrx_to_rgba_with_stride_padding() {
        // 1 pixel wide, stride=8 (4 bytes padding after each row)
        let data = vec![
            0x10, 0x20, 0x30, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, // row 0 + padding
            0xAA, 0xBB, 0xCC, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, // row 1 + padding
        ];
        let rgba = bgrx_to_rgba(&data, 1, 2, 8);
        assert_eq!(rgba, vec![
            0x30, 0x20, 0x10, 0xFF,
            0xCC, 0xBB, 0xAA, 0xFF,
        ]);
    }

    #[test]
    fn test_bgra_to_rgba_preserves_alpha() {
        let data = vec![0x10, 0x20, 0x30, 0x80]; // B, G, R, A=128
        let rgba = bgra_to_rgba(&data, 1, 1, 4);
        assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0x80]); // R, G, B, A=128
    }

    #[test]
    fn test_encode_png_produces_valid_png() {
        let rgba = vec![0xFF, 0x00, 0x00, 0xFF]; // 1 red pixel
        let png_bytes = encode_png(&rgba, 1, 1).unwrap();
        // PNG magic bytes
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
        assert!(png_bytes.len() > 8); // not empty
    }

    #[test]
    fn test_encode_png_rejects_mismatched_dimensions() {
        let rgba = vec![0xFF; 4]; // 1 pixel
        let result = encode_png(&rgba, 2, 1); // claims 2 pixels
        assert!(result.is_err());
    }
}
```

**Step 2: Run to see failure**

Run: `cargo test --lib ui::capture_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: FAIL.

**Step 3: Implement pixel conversion and PNG encoding**

```rust
fn bgrx_to_rgba(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row_start = y * stride;
        for x in 0..width {
            let offset = row_start + x * 4;
            rgba.push(data[offset + 2]); // R
            rgba.push(data[offset + 1]); // G
            rgba.push(data[offset]);     // B
            rgba.push(0xFF);             // A
        }
    }
    rgba
}

fn bgra_to_rgba(data: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let row_start = y * stride;
        for x in 0..width {
            let offset = row_start + x * 4;
            rgba.push(data[offset + 2]); // R
            rgba.push(data[offset + 1]); // G
            rgba.push(data[offset]);     // B
            rgba.push(data[offset + 3]); // A
        }
    }
    rgba
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| crate::Error::Internal("Image dimensions overflow".into()))?;
    if rgba.len() != expected_len {
        return Err(crate::Error::Internal(format!(
            "RGBA buffer size {} doesn't match {}x{}x4={}",
            rgba.len(), width, height, expected_len
        )));
    }

    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder
            .write_header()
            .map_err(|e| crate::Error::Internal(format!("PNG header: {}", e)))?;
        writer
            .write_image_data(rgba)
            .map_err(|e| crate::Error::Internal(format!("PNG data: {}", e)))?;
    }
    Ok(buf)
}
```

**Step 4: Run tests**

Run: `cargo test --lib ui::capture_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: All PASS.

**Step 5: Commit**

```bash
git add src/ui/capture_linux.rs
git commit -m "feat: pixel format conversion and PNG encoding for Linux screenshots"
```

---

### Task 6: Key name to X11 keysym mapping

**Files:**
- Modify: `src/ui/input_linux.rs`

**Step 1: Write failing tests for key name mapping**

Replace the entire stub file. The tests reference keysyms from `x11rb::protocol::xproto` or hardcoded constants:

```rust
//! Linux UI interaction via AT-SPI2 actions and X11 XTest input injection.

use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse};

/// Map key name to X11 keysym. Mirrors the ~50 key names from macOS input_mac.rs.
fn key_name_to_keysym(name: &str) -> Option<u32> {
    todo!()
}

pub async fn execute_action(
    _pid: u32,
    _req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    Err(crate::Error::UiNotAvailable(
        "Linux UI interaction not yet implemented".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_name_letters() {
        // X11 keysyms for lowercase letters: 0x61-0x7a
        for c in b'a'..=b'z' {
            let name = String::from(c as char);
            let ks = key_name_to_keysym(&name).unwrap();
            assert_eq!(ks, c as u32, "keysym for '{}'", name);
        }
    }

    #[test]
    fn test_key_name_digits() {
        // X11 keysyms for digits: 0x30-0x39
        for c in b'0'..=b'9' {
            let name = String::from(c as char);
            let ks = key_name_to_keysym(&name).unwrap();
            assert_eq!(ks, c as u32, "keysym for '{}'", name);
        }
    }

    #[test]
    fn test_key_name_special() {
        assert_eq!(key_name_to_keysym("return"), Some(0xff0d));    // XK_Return
        assert_eq!(key_name_to_keysym("tab"), Some(0xff09));       // XK_Tab
        assert_eq!(key_name_to_keysym("space"), Some(0x0020));     // XK_space
        assert_eq!(key_name_to_keysym("delete"), Some(0xff08));    // XK_BackSpace
        assert_eq!(key_name_to_keysym("escape"), Some(0xff1b));    // XK_Escape
        assert_eq!(key_name_to_keysym("forwarddelete"), Some(0xffff)); // XK_Delete
    }

    #[test]
    fn test_key_name_arrows() {
        assert_eq!(key_name_to_keysym("leftarrow"), Some(0xff51));
        assert_eq!(key_name_to_keysym("rightarrow"), Some(0xff53));
        assert_eq!(key_name_to_keysym("uparrow"), Some(0xff52));
        assert_eq!(key_name_to_keysym("downarrow"), Some(0xff54));
    }

    #[test]
    fn test_key_name_function_keys() {
        assert_eq!(key_name_to_keysym("f1"), Some(0xffbe));
        assert_eq!(key_name_to_keysym("f12"), Some(0xffc9));
    }

    #[test]
    fn test_key_name_modifiers() {
        assert_eq!(key_name_to_keysym("shift"), Some(0xffe1));     // XK_Shift_L
        assert_eq!(key_name_to_keysym("control"), Some(0xffe3));   // XK_Control_L
        assert_eq!(key_name_to_keysym("alt"), Some(0xffe9));       // XK_Alt_L
        assert_eq!(key_name_to_keysym("super"), Some(0xffeb));     // XK_Super_L
    }

    #[test]
    fn test_key_name_unknown() {
        assert_eq!(key_name_to_keysym("nonexistent"), None);
    }
}
```

**Step 2: Run to see failure**

Run: `cargo test --lib ui::input_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: FAIL.

**Step 3: Implement key_name_to_keysym**

```rust
fn key_name_to_keysym(name: &str) -> Option<u32> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        // Letters (XK_a..XK_z = 0x61..0x7a, same as ASCII)
        s if s.len() == 1 && s.as_bytes()[0] >= b'a' && s.as_bytes()[0] <= b'z' => {
            Some(s.as_bytes()[0] as u32)
        }
        // Digits (XK_0..XK_9 = 0x30..0x39, same as ASCII)
        s if s.len() == 1 && s.as_bytes()[0] >= b'0' && s.as_bytes()[0] <= b'9' => {
            Some(s.as_bytes()[0] as u32)
        }
        // Special keys
        "return" | "enter" => Some(0xff0d),       // XK_Return
        "tab" => Some(0xff09),                     // XK_Tab
        "space" => Some(0x0020),                   // XK_space
        "delete" | "backspace" => Some(0xff08),    // XK_BackSpace
        "forwarddelete" => Some(0xffff),           // XK_Delete
        "escape" | "esc" => Some(0xff1b),          // XK_Escape
        "home" => Some(0xff50),                    // XK_Home
        "end" => Some(0xff57),                     // XK_End
        "pageup" => Some(0xff55),                  // XK_Page_Up
        "pagedown" => Some(0xff56),                // XK_Page_Down
        // Arrows
        "leftarrow" | "left" => Some(0xff51),      // XK_Left
        "uparrow" | "up" => Some(0xff52),          // XK_Up
        "rightarrow" | "right" => Some(0xff53),    // XK_Right
        "downarrow" | "down" => Some(0xff54),      // XK_Down
        // Function keys (XK_F1=0xffbe .. XK_F12=0xffc9)
        s if s.starts_with('f') && s.len() <= 3 => {
            let num: u32 = s[1..].parse().ok()?;
            if (1..=12).contains(&num) {
                Some(0xffbe + num - 1)
            } else {
                None
            }
        }
        // Modifier keys (as standalone keypresses)
        "shift" => Some(0xffe1),                   // XK_Shift_L
        "control" | "ctrl" => Some(0xffe3),        // XK_Control_L
        "alt" | "option" => Some(0xffe9),          // XK_Alt_L
        "super" | "command" | "cmd" | "meta" => Some(0xffeb), // XK_Super_L
        _ => None,
    }
}
```

**Step 4: Run tests**

Run: `cargo test --lib ui::input_linux::tests -- --nocapture 2>&1 | tail -10`
Expected: All PASS.

**Step 5: Commit**

```bash
git add src/ui/input_linux.rs
git commit -m "feat: X11 key name to keysym mapping table"
```

---

### Task 7: Platform-conditional modifier constants in input.rs

**Files:**
- Modify: `src/ui/input.rs:5-9`

The current modifier constants are CGEventFlags (macOS-specific). On Linux, X11 modifier masks are different. Make them platform-conditional so `modifier_string_to_flags` returns correct values per platform.

**Step 1: Write failing test for Linux modifier flags**

Add a cfg-gated test to `src/ui/input.rs`:

```rust
#[cfg(target_os = "linux")]
#[test]
fn test_modifier_flags_linux_values() {
    // X11 ShiftMask=1, ControlMask=4, Mod1Mask=8 (Alt), Mod4Mask=0x40 (Super)
    let shift = modifier_string_to_flags(&["shift".to_string()]);
    assert_eq!(shift, 0x1);
    let ctrl = modifier_string_to_flags(&["ctrl".to_string()]);
    assert_eq!(ctrl, 0x4);
    let alt = modifier_string_to_flags(&["alt".to_string()]);
    assert_eq!(alt, 0x8);
    let cmd = modifier_string_to_flags(&["cmd".to_string()]);
    assert_eq!(cmd, 0x40);
}
```

**Step 2: Run to see failure**

Run: `cargo test --lib ui::input::tests::test_modifier_flags_linux -- --nocapture 2>&1`
Expected: FAIL — current constants are CGEventFlags values.

**Step 3: Make constants platform-conditional**

```rust
/// Modifier flag constants.
#[cfg(target_os = "macos")]
pub const MOD_SHIFT: u64 = 0x00020000;     // kCGEventFlagMaskShift
#[cfg(target_os = "macos")]
pub const MOD_CONTROL: u64 = 0x00040000;   // kCGEventFlagMaskControl
#[cfg(target_os = "macos")]
pub const MOD_ALTERNATE: u64 = 0x00080000; // kCGEventFlagMaskAlternate
#[cfg(target_os = "macos")]
pub const MOD_COMMAND: u64 = 0x00100000;   // kCGEventFlagMaskCommand

#[cfg(target_os = "linux")]
pub const MOD_SHIFT: u64 = 0x1;           // X11 ShiftMask
#[cfg(target_os = "linux")]
pub const MOD_CONTROL: u64 = 0x4;         // X11 ControlMask
#[cfg(target_os = "linux")]
pub const MOD_ALTERNATE: u64 = 0x8;       // X11 Mod1Mask (Alt)
#[cfg(target_os = "linux")]
pub const MOD_COMMAND: u64 = 0x40;        // X11 Mod4Mask (Super/Meta)
```

**Step 4: Run all input tests**

Run: `cargo test --lib ui::input::tests -- --nocapture 2>&1 | tail -10`
Expected: All PASS (on Linux the new test passes, existing tests use relative checks that still work OR need adjustment — check the existing `test_modifier_flags` test which checks `flags & MOD_COMMAND != 0` — this still works with any non-zero value).

**Step 5: Commit**

```bash
git add src/ui/input.rs
git commit -m "feat: platform-conditional modifier constants for X11 vs CGEvent"
```

---

## Phase 3: AT-SPI2 Accessibility Tree

### Task 8: UID validation helper

**Files:**
- Modify: `src/ui/accessibility_linux.rs`

**Step 1: Write the UID validation function and test**

```rust
/// Validate that the target PID is owned by the current user.
/// Reads /proc/{pid}/status to extract UID and compares against euid.
fn validate_pid_ownership(pid: u32) -> crate::Result<()> {
    todo!()
}

// In tests:
#[test]
fn test_validate_pid_ownership_self() {
    // Our own PID should always pass
    let pid = std::process::id();
    assert!(validate_pid_ownership(pid).is_ok());
}

#[test]
fn test_validate_pid_ownership_nonexistent() {
    // PID 999999 unlikely to exist
    let result = validate_pid_ownership(999999);
    assert!(result.is_err());
}
```

**Step 2: Run to see failure**

Run: `cargo test --lib ui::accessibility_linux::tests::test_validate_pid -- --nocapture 2>&1`
Expected: FAIL.

**Step 3: Implement**

```rust
fn validate_pid_ownership(pid: u32) -> crate::Result<()> {
    let status_path = format!("/proc/{}/status", pid);
    let content = std::fs::read_to_string(&status_path).map_err(|_| {
        crate::Error::UiQueryFailed(format!(
            "Cannot read /proc/{}/status — process may not exist",
            pid
        ))
    })?;

    let my_euid = unsafe { libc::geteuid() };

    for line in content.lines() {
        if let Some(uid_str) = line.strip_prefix("Uid:") {
            // Format: "Uid:\treal\teffective\tsaved\tfs"
            let fields: Vec<&str> = uid_str.split_whitespace().collect();
            if let Some(real_uid_str) = fields.first() {
                if let Ok(real_uid) = real_uid_str.parse::<u32>() {
                    if real_uid != my_euid && my_euid != 0 {
                        return Err(crate::Error::UiQueryFailed(format!(
                            "Permission denied: process {} is owned by another user",
                            pid
                        )));
                    }
                    return Ok(());
                }
            }
        }
    }

    Err(crate::Error::UiQueryFailed(format!(
        "Cannot determine UID for process {}",
        pid
    )))
}
```

**Step 4: Run tests**

Run: `cargo test --lib ui::accessibility_linux::tests::test_validate_pid -- --nocapture 2>&1`
Expected: PASS.

**Step 5: Commit**

```bash
git add src/ui/accessibility_linux.rs
git commit -m "feat: UID validation for Linux AT-SPI2 queries"
```

---

### Task 9: AT-SPI2 connection, app discovery, and tree walking

**Files:**
- Modify: `src/ui/accessibility_linux.rs`

This is the core task. Replace the stub `query_ax_tree`, `is_available`, and `check_accessibility_permission` with real AT-SPI2 implementations. Also add `find_element_by_id` for use by input_linux.

> **Important:** This function is **async** (unlike macOS which is sync). server.rs will be adjusted in Task 15 to call it without `spawn_blocking`.

**Step 1: Write the full implementation**

Rewrite `accessibility_linux.rs` entirely. Keep all tests from Tasks 3, 4, 8. The key structures:

```rust
//! Linux accessibility via AT-SPI2 (D-Bus).

use crate::ui::tree::{generate_id, Rect, UiNode, NodeSource};
use crate::Result;

const MAX_AX_DEPTH: usize = 50;
const MAX_AX_NODES: usize = 10_000;

// -- role mapping (from Task 3) --
// -- action name mapping (from Task 4) --
// -- UID validation (from Task 8) --

/// Check if AT-SPI2 bus is available.
pub fn is_available() -> bool {
    // Try to connect synchronously by spawning a blocking check.
    // This is a quick D-Bus ping — not performance critical.
    std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.a11y.Bus",
            "--print-reply",
            "/org/a11y/bus",
            "org.freedesktop.DBus.Peer.Ping",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check accessibility permissions. On Linux, AT-SPI2 doesn't require per-app
/// permissions like macOS. Returns is_available(). `prompt` parameter is ignored.
pub fn check_accessibility_permission(_prompt: bool) -> bool {
    is_available()
}

/// Query the AT-SPI2 accessibility tree for a given PID.
/// Returns top-level window nodes (excluding MenuBar).
pub async fn query_ax_tree(pid: u32) -> Result<Vec<UiNode>> {
    validate_pid_ownership(pid)?;

    let connection = atspi::AccessibilityConnection::new()
        .await
        .map_err(|e| crate::Error::UiNotAvailable(format!(
            "AT-SPI2 accessibility bus not available: {}. \
             Enable accessibility: gsettings set org.gnome.desktop.interface toolkit-accessibility true",
            e
        )))?;

    connection.register_event::<atspi::events::object::StateChangedEvent>()
        .await
        .ok(); // Best-effort registration for bus activation

    // Find the application accessible matching the target PID
    let registry = connection.registry();
    let apps = registry.get_children().await.map_err(|e| {
        crate::Error::UiQueryFailed(format!("Failed to enumerate AT-SPI2 applications: {}", e))
    })?;

    let mut target_app = None;
    for app in &apps {
        // Get the bus name and resolve PID via D-Bus
        let bus_name = app.inner().destination().as_str();
        let dbus_conn = connection.connection();
        let dbus_proxy = zbus::fdo::DBusProxy::new(dbus_conn)
            .await
            .map_err(|e| crate::Error::Internal(format!("D-Bus proxy: {}", e)))?;

        match dbus_proxy.get_connection_unix_process_id(
            zbus::names::BusName::from_str_unchecked(bus_name).into()
        ).await {
            Ok(app_pid) if app_pid == pid => {
                target_app = Some(app.clone());
                break;
            }
            _ => continue,
        }
    }

    let app = target_app.ok_or_else(|| crate::Error::UiQueryFailed(format!(
        "No AT-SPI2 accessible found for PID {}. \
         The application may not support accessibility (GTK, Qt, Electron apps do).",
        pid
    )))?;

    // Walk the tree from the application root
    let mut node_count = 0;
    let children = app.get_children().await.unwrap_or_default();

    let mut windows = Vec::new();
    for (i, child) in children.iter().enumerate() {
        if let Some(node) = build_node(&connection, child, i, 0, &mut node_count).await {
            // Skip MenuBar nodes for consistency with macOS
            if node.role != "AXMenuBar" {
                windows.push(node);
            }
        }
    }

    Ok(windows)
}

/// Recursively build a UiNode from an AT-SPI2 accessible.
async fn build_node(
    connection: &atspi::AccessibilityConnection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
    sibling_index: usize,
    depth: usize,
    node_count: &mut usize,
) -> Option<UiNode> {
    if depth > MAX_AX_DEPTH || *node_count >= MAX_AX_NODES {
        return None;
    }
    *node_count += 1;

    // Get role
    let role_enum = accessible.get_role().await.ok()?;
    let role = map_atspi_role(role_enum);

    // Get name (title)
    let name = accessible.name().await.ok().filter(|s| !s.is_empty());

    // Get description as fallback title
    let title = name.or_else(|| {
        // Try description synchronously since we're already async
        None // Will be populated if name is empty — actual implementation
             // should try accessible.description().await.ok().filter(|s| !s.is_empty())
    });
    // Actually:
    let title = match accessible.name().await {
        Ok(n) if !n.is_empty() => Some(n),
        _ => accessible.description().await.ok().filter(|s| !s.is_empty()),
    };

    // Get interfaces to know what we can query
    let interfaces = accessible.get_interfaces().await.unwrap_or_default();

    // Get bounds via Component interface
    let bounds = if interfaces.contains("Component") {
        // Create ComponentProxy for this accessible's path
        match atspi::proxy::component::ComponentProxy::builder(connection.connection())
            .destination(accessible.inner().destination())?
            .path(accessible.inner().path())?
            .build()
            .await
        {
            Ok(comp) => {
                match comp.get_extents(atspi::CoordType::Screen).await {
                    Ok((x, y, w, h)) => {
                        if w > 0 && h > 0 {
                            Some(Rect {
                                x: x as f64,
                                y: y as f64,
                                w: w as f64,
                                h: h as f64,
                            })
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            }
            Err(_) => None,
        }
    } else {
        None
    };

    // Get actions via Action interface
    let actions = if interfaces.contains("Action") {
        match atspi::proxy::action::ActionProxy::builder(connection.connection())
            .destination(accessible.inner().destination())?
            .path(accessible.inner().path())?
            .build()
            .await
        {
            Ok(action_proxy) => {
                let n = action_proxy.n_actions().await.unwrap_or(0);
                let mut mapped = Vec::new();
                for i in 0..n {
                    if let Ok(name) = action_proxy.get_name(i).await {
                        mapped.push(map_atspi_action(&name));
                    }
                }
                mapped
            }
            Err(_) => vec![],
        }
    } else {
        vec![]
    };

    // Get value via Value interface
    let value = if interfaces.contains("Value") {
        match atspi::proxy::value::ValueProxy::builder(connection.connection())
            .destination(accessible.inner().destination())?
            .path(accessible.inner().path())?
            .build()
            .await
        {
            Ok(val_proxy) => val_proxy
                .current_value()
                .await
                .ok()
                .map(|v| format!("{}", v)),
            Err(_) => None,
        }
    } else {
        None
    };

    // Get state for enabled/focused
    let states = accessible.get_state().await.unwrap_or_default();
    let enabled = states.contains(atspi::State::Enabled)
        || states.contains(atspi::State::Sensitive);
    let focused = states.contains(atspi::State::Focused);

    let id = generate_id(&role, title.as_deref(), sibling_index);

    // Recurse into children
    let child_accessibles = accessible.get_children().await.unwrap_or_default();
    let mut children = Vec::new();
    for (i, child) in child_accessibles.iter().enumerate() {
        if let Some(child_node) = Box::pin(
            build_node(connection, child, i, depth + 1, node_count)
        ).await {
            children.push(child_node);
        }
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

/// Find an AT-SPI2 accessible by node ID. Returns the accessible proxy for
/// action execution. Used by input_linux.rs.
pub async fn find_element_by_id(
    pid: u32,
    target_id: &str,
) -> Result<Option<FindResult>> {
    // This re-walks the tree to find the matching accessible.
    // We return enough context to execute actions against it.
    validate_pid_ownership(pid)?;

    let connection = atspi::AccessibilityConnection::new().await.map_err(|e| {
        crate::Error::UiNotAvailable(format!("AT-SPI2 bus not available: {}", e))
    })?;

    let registry = connection.registry();
    let apps = registry.get_children().await.map_err(|e| {
        crate::Error::UiQueryFailed(format!("Failed to enumerate apps: {}", e))
    })?;

    // Find target app by PID (same logic as query_ax_tree)
    for app in &apps {
        let bus_name = app.inner().destination().as_str();
        let dbus_conn = connection.connection();
        let dbus_proxy = zbus::fdo::DBusProxy::new(dbus_conn).await.ok();
        if let Some(proxy) = dbus_proxy {
            if let Ok(app_pid) = proxy.get_connection_unix_process_id(
                zbus::names::BusName::from_str_unchecked(bus_name).into()
            ).await {
                if app_pid == pid {
                    // Walk this app looking for our target ID
                    let children = app.get_children().await.unwrap_or_default();
                    for (i, child) in children.iter().enumerate() {
                        if let Some(result) = find_in_subtree(
                            &connection, child, target_id, i, 0
                        ).await {
                            return Ok(Some(result));
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

/// Result of finding an element — carries the D-Bus destination and path
/// needed to create action/value proxies.
pub struct FindResult {
    pub destination: String,
    pub path: String,
    pub node: UiNode,
    pub interfaces: Vec<String>,
}

async fn find_in_subtree(
    connection: &atspi::AccessibilityConnection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
    target_id: &str,
    sibling_index: usize,
    depth: usize,
) -> Option<FindResult> {
    if depth > MAX_AX_DEPTH {
        return None;
    }

    let role_enum = accessible.get_role().await.ok()?;
    let role = map_atspi_role(role_enum);
    let title = accessible.name().await.ok().filter(|s| !s.is_empty());
    let id = generate_id(&role, title.as_deref(), sibling_index);

    if id == target_id {
        let interfaces = accessible.get_interfaces().await.unwrap_or_default();
        let states = accessible.get_state().await.unwrap_or_default();
        // Build minimal UiNode for the before snapshot
        let bounds = /* same Component logic as build_node */ None;
        let value = /* same Value logic as build_node */ None;

        return Some(FindResult {
            destination: accessible.inner().destination().to_string(),
            path: accessible.inner().path().to_string(),
            node: UiNode {
                id,
                role,
                title,
                value,
                enabled: states.contains(atspi::State::Enabled)
                    || states.contains(atspi::State::Sensitive),
                focused: states.contains(atspi::State::Focused),
                bounds,
                actions: vec![], // populated by caller if needed
                source: NodeSource::Ax,
                children: vec![],
            },
            interfaces: interfaces.iter().map(|i| i.to_string()).collect(),
        });
    }

    // Recurse
    let children = accessible.get_children().await.unwrap_or_default();
    for (i, child) in children.iter().enumerate() {
        if let Some(result) = Box::pin(
            find_in_subtree(connection, child, target_id, i, depth + 1)
        ).await {
            return Some(result);
        }
    }

    None
}
```

> **Critical implementation note:** The exact `atspi` crate API may differ from the code shown above. The proxy builder pattern, method names, and type signatures need to be verified against `atspi` 0.29 docs. The key concepts are correct: connect to bus → find app by PID → walk children → build UiNodes. Adjust method names to match the actual crate API.

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -20`
Expected: Compiles (may need API adjustments based on actual `atspi` crate).

**Step 3: Run unit tests (role mapping, action mapping, UID validation)**

Run: `cargo test --lib ui::accessibility_linux::tests -- --nocapture 2>&1`
Expected: All unit tests PASS. (Integration tests need live AT-SPI2.)

**Step 4: Commit**

```bash
git add src/ui/accessibility_linux.rs
git commit -m "feat: AT-SPI2 tree walking, app discovery, and element lookup"
```

---

## Phase 4: X11 Screenshot Capture

### Task 10: X11 window finding by PID

**Files:**
- Modify: `src/ui/capture_linux.rs`

**Step 1: Implement window finding and screenshot capture**

Replace the stubs with the full implementation. Keep tests from Task 5.

```rust
//! Linux screenshot capture via X11 (GetImage + _NET_WM_PID).

use crate::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::wrapper::ConnectionExt as _;

const MAX_IMAGE_WIDTH: u32 = 3840;
const MAX_IMAGE_HEIGHT: u32 = 2160;

// -- pixel conversion functions from Task 5 --
// -- PNG encoding from Task 5 --

struct WindowInfo {
    id: u32,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
}

/// Find the largest visible window for a given PID using _NET_WM_PID.
fn find_main_window(
    conn: &impl Connection,
    screen: &Screen,
    pid: u32,
) -> Result<WindowInfo> {
    let net_client_list = conn
        .intern_atom(false, b"_NET_CLIENT_LIST")
        .map_err(|e| crate::Error::Internal(format!("intern atom: {}", e)))?
        .reply()
        .map_err(|e| crate::Error::Internal(format!("intern atom reply: {}", e)))?
        .atom;

    let net_wm_pid = conn
        .intern_atom(false, b"_NET_WM_PID")
        .map_err(|e| crate::Error::Internal(format!("intern atom: {}", e)))?
        .reply()
        .map_err(|e| crate::Error::Internal(format!("intern atom reply: {}", e)))?
        .atom;

    // Get window list from root
    let reply = conn
        .get_property(false, screen.root, net_client_list, AtomEnum::WINDOW, 0, 1024)
        .map_err(|e| crate::Error::Internal(format!("get_property: {}", e)))?
        .reply()
        .map_err(|e| crate::Error::UiQueryFailed(format!(
            "Window manager does not support _NET_CLIENT_LIST: {}", e
        )))?;

    let window_ids: &[u32] = reply.value32().ok_or_else(|| {
        crate::Error::UiQueryFailed(
            "Window manager does not support _NET_WM_PID. Cannot identify windows by PID.".into()
        )
    })?.collect::<Vec<_>>().leak(); // lifetime hack — see actual impl note

    // Actually, collect into a Vec:
    let window_ids: Vec<u32> = reply
        .value32()
        .map(|iter| iter.collect())
        .unwrap_or_default();

    if window_ids.is_empty() {
        return Err(crate::Error::UiQueryFailed(
            "No windows reported by window manager".into()
        ));
    }

    let mut best: Option<WindowInfo> = None;
    let mut best_area: u64 = 0;

    for &win_id in &window_ids {
        // Get PID for this window
        let pid_reply = conn
            .get_property(false, win_id, net_wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()
            .and_then(|cookie| cookie.reply().ok());

        let win_pid = pid_reply
            .as_ref()
            .and_then(|r| r.value32())
            .and_then(|mut iter| iter.next());

        if win_pid != Some(pid) {
            continue;
        }

        // Get geometry
        if let Ok(geom) = conn.get_geometry(win_id).and_then(|c| Ok(c.reply())) {
            if let Ok(geom) = geom {
                let area = geom.width as u64 * geom.height as u64;
                if area > best_area {
                    best_area = area;
                    best = Some(WindowInfo {
                        id: win_id,
                        x: geom.x,
                        y: geom.y,
                        width: geom.width,
                        height: geom.height,
                    });
                }
            }
        }
    }

    best.ok_or_else(|| crate::Error::UiQueryFailed(format!(
        "No visible window found for PID {}", pid
    )))
}

/// Capture a screenshot of the main window for a given PID.
/// Returns PNG bytes.
pub fn capture_window_screenshot(pid: u32) -> Result<Vec<u8>> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| {
        crate::Error::UiNotAvailable(format!(
            "Cannot connect to X11 display: {}. Ensure DISPLAY is set or XWayland is running.",
            e
        ))
    })?;

    let screen = &conn.setup().roots[screen_num];
    let win = find_main_window(&conn, screen, pid)?;

    // SEC-3: Validate image size
    if win.width as u32 > MAX_IMAGE_WIDTH || win.height as u32 > MAX_IMAGE_HEIGHT {
        return Err(crate::Error::UiQueryFailed(format!(
            "Window size {}x{} exceeds 4K limit",
            win.width, win.height
        )));
    }

    // Check window is mapped (not minimized)
    let attrs = conn
        .get_window_attributes(win.id)
        .map_err(|e| crate::Error::Internal(format!("get_window_attributes: {}", e)))?
        .reply()
        .map_err(|e| crate::Error::UiQueryFailed(format!(
            "Cannot get window attributes: {}", e
        )))?;

    if attrs.map_state != MapState::VIEWABLE {
        return Err(crate::Error::UiQueryFailed(
            "Window is not visible (may be minimized)".into()
        ));
    }

    // Capture via GetImage
    let image = conn
        .get_image(
            ImageFormat::Z_PIXMAP,
            win.id,
            0,
            0,
            win.width,
            win.height,
            !0u32,
        )
        .map_err(|e| crate::Error::Internal(format!("get_image: {}", e)))?
        .reply()
        .map_err(|e| crate::Error::UiQueryFailed(format!(
            "Screenshot capture failed: {}", e
        )))?;

    let depth = image.depth;
    let w = win.width as usize;
    let h = win.height as usize;
    let stride = image.data.len() / h; // bytes per row

    let rgba = match depth {
        24 | 32 if image.data.len() >= w * h * 4 => {
            if depth == 24 {
                bgrx_to_rgba(&image.data, w, h, stride)
            } else {
                bgra_to_rgba(&image.data, w, h, stride)
            }
        }
        _ => {
            return Err(crate::Error::UiQueryFailed(format!(
                "Unsupported pixel depth: {}", depth
            )));
        }
    };

    encode_png(&rgba, win.width as u32, win.height as u32)
}

/// Capture a screenshot cropped to a specific element's bounds.
pub fn capture_element_screenshot(
    pid: u32,
    element_bounds: &crate::ui::tree::Rect,
) -> Result<Vec<u8>> {
    let (conn, screen_num) = x11rb::connect(None).map_err(|e| {
        crate::Error::UiNotAvailable(format!(
            "Cannot connect to X11 display: {}", e
        ))
    })?;

    let screen = &conn.setup().roots[screen_num];
    let win = find_main_window(&conn, screen, pid)?;

    let attrs = conn
        .get_window_attributes(win.id)?
        .reply()?;
    if attrs.map_state != MapState::VIEWABLE {
        return Err(crate::Error::UiQueryFailed(
            "Window is not visible (may be minimized)".into()
        ));
    }

    // Capture full window first
    let image = conn
        .get_image(ImageFormat::Z_PIXMAP, win.id, 0, 0, win.width, win.height, !0u32)?
        .reply()?;

    let depth = image.depth;
    let w = win.width as usize;
    let h = win.height as usize;
    let stride = image.data.len() / h;

    let rgba = match depth {
        24 => bgrx_to_rgba(&image.data, w, h, stride),
        32 => bgra_to_rgba(&image.data, w, h, stride),
        _ => return Err(crate::Error::UiQueryFailed(format!(
            "Unsupported pixel depth: {}", depth
        ))),
    };

    // Compute crop coordinates relative to window origin
    // Element bounds are in screen coordinates, window has (x, y) screen offset
    let crop_x = ((element_bounds.x - win.x as f64).round() as usize).min(w.saturating_sub(1));
    let crop_y = ((element_bounds.y - win.y as f64).round() as usize).min(h.saturating_sub(1));
    let crop_w = (element_bounds.w.round() as usize).min(w - crop_x);
    let crop_h = (element_bounds.h.round() as usize).min(h - crop_y);

    if crop_w == 0 || crop_h == 0 {
        return Err(crate::Error::UiQueryFailed(
            "Element has zero-size bounds after cropping".into()
        ));
    }

    // Extract crop region from RGBA buffer
    let mut cropped = Vec::with_capacity(crop_w * crop_h * 4);
    for y in crop_y..crop_y + crop_h {
        let row_offset = y * w * 4;
        let start = row_offset + crop_x * 4;
        let end = start + crop_w * 4;
        cropped.extend_from_slice(&rgba[start..end]);
    }

    encode_png(&cropped, crop_w as u32, crop_h as u32)
}
```

> **Implementation note:** The `x11rb` API may vary slightly. Key things to verify:
> - `x11rb::connect()` return type and error handling
> - `get_property` value extraction (`.value32()` returns an iterator)
> - `get_image` parameter order and response fields
> - Whether `MapState::VIEWABLE` is the correct variant name

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`
Expected: Compiles.

**Step 3: Run pixel conversion tests**

Run: `cargo test --lib ui::capture_linux::tests -- --nocapture 2>&1`
Expected: All PASS (these don't need X11).

**Step 4: Commit**

```bash
git add src/ui/capture_linux.rs
git commit -m "feat: X11 screenshot capture with window finding and element cropping"
```

---

## Phase 5: Input Actions

### Task 11: XTest input helpers

**Files:**
- Modify: `src/ui/input_linux.rs`

Add XTest helper functions for mouse and keyboard input. These are building blocks for action dispatch.

**Step 1: Implement XTest helpers**

```rust
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest;

/// Send a fake key press/release via XTest.
fn xtest_key(
    conn: &impl Connection,
    keysym: u32,
    press: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Look up keycode from keysym using keyboard mapping
    let keycode = keysym_to_keycode(conn, keysym)?;
    let event_type = if press { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT };
    xtest::fake_input(conn, event_type, keycode, 0, 0, 0, 0, 0)?.check()?;
    conn.flush()?;
    Ok(())
}

/// Send a fake mouse button press/release via XTest.
fn xtest_button(
    conn: &impl Connection,
    button: u8,
    press: bool,
    x: i16,
    y: i16,
    root: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let event_type = if press { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT };
    // Move to position first
    xtest::fake_input(conn, MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)?.check()?;
    xtest::fake_input(conn, event_type, button, 0, root, 0, 0, 0)?.check()?;
    conn.flush()?;
    Ok(())
}

/// Move the mouse pointer via XTest.
fn xtest_motion(
    conn: &impl Connection,
    x: i16,
    y: i16,
    root: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    xtest::fake_input(conn, MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)?.check()?;
    conn.flush()?;
    Ok(())
}

/// Resolve keysym to keycode via the X11 keyboard mapping.
fn keysym_to_keycode(
    conn: &impl Connection,
    keysym: u32,
) -> Result<u8, Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min_kc = setup.min_keycode;
    let max_kc = setup.max_keycode;

    let mapping = conn
        .get_keyboard_mapping(min_kc, max_kc - min_kc + 1)?
        .reply()?;

    let syms_per_kc = mapping.keysyms_per_keycode as usize;
    for kc in min_kc..=max_kc {
        let offset = (kc - min_kc) as usize * syms_per_kc;
        for col in 0..syms_per_kc {
            if mapping.keysyms[offset + col] == keysym {
                return Ok(kc);
            }
        }
    }

    Err(format!("No keycode found for keysym 0x{:x}", keysym).into())
}
```

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`

**Step 3: Commit**

```bash
git add src/ui/input_linux.rs
git commit -m "feat: XTest input helpers (key, button, motion)"
```

---

### Task 12: Full execute_action implementation

**Files:**
- Modify: `src/ui/input_linux.rs`

Replace the stub `execute_action` with the full two-tier dispatch (AT-SPI2 first, XTest fallback).

**Step 1: Implement execute_action**

```rust
use crate::mcp::{
    DebugUiActionRequest, DebugUiActionResponse, ScrollDirection, UiActionType,
};
use crate::ui::accessibility_linux::{find_element_by_id, query_ax_tree, FindResult};
use crate::ui::input::{drag_interpolation_points, element_center, modifier_string_to_flags};
use crate::ui::tree::{diff_nodes, find_node_by_id, UiNode};

const DEFAULT_SETTLE_MS: u64 = 80;
const DRAG_STEPS: usize = 10;
const DRAG_STEP_INTERVAL_MS: u64 = 16;

pub async fn execute_action(
    pid: u32,
    req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    let settle_ms = req.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);

    // For Key action: no node resolution needed
    if req.action == UiActionType::Key {
        let key_name = req.key.as_ref().unwrap().clone();
        let modifiers = req.modifiers.clone().unwrap_or_default();

        return tokio::task::spawn_blocking(move || {
            execute_key_action(&key_name, &modifiers)
        })
        .await
        .map_err(|e| crate::Error::Internal(format!("Key action task failed: {}", e)))?;
    }

    // Resolve target node
    let target_id = req.id.as_ref().ok_or_else(|| {
        crate::Error::UiQueryFailed("Action requires an element ID (use debug_ui to find IDs)".into())
    })?;

    // Snapshot before
    let find_result = find_element_by_id(pid, target_id).await?.ok_or_else(|| {
        crate::Error::UiQueryFailed(format!("Node not found: {}", target_id))
    })?;
    let node_before = find_result.node.clone();

    // Execute action
    let (success, method, error) = match req.action {
        UiActionType::Click => execute_click(pid, &find_result, req).await,
        UiActionType::SetValue => execute_set_value(&find_result, req).await,
        UiActionType::Type => execute_type(pid, &find_result, req).await,
        UiActionType::Scroll => execute_scroll(&find_result, req).await,
        UiActionType::Drag => execute_drag(pid, &find_result, req).await,
        UiActionType::Key => unreachable!(), // handled above
    };

    // Settle
    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;

    // Snapshot after
    let node_after = find_element_by_id(pid, target_id)
        .await
        .ok()
        .flatten()
        .map(|r| r.node);

    let changed = node_after.as_ref().map(|after| diff_nodes(&node_before, after));

    Ok(DebugUiActionResponse {
        success,
        method: Some(method),
        node_before: Some(node_before),
        node_after,
        changed,
        error,
    })
}

async fn execute_click(
    pid: u32,
    target: &FindResult,
    _req: &DebugUiActionRequest,
) -> (bool, String, Option<String>) {
    // Tier 1: Try AT-SPI2 do_action("click")
    if target.interfaces.iter().any(|i| i == "Action") {
        if let Ok(conn) = atspi::AccessibilityConnection::new().await {
            if let Ok(action_proxy) = atspi::proxy::action::ActionProxy::builder(conn.connection())
                .destination(&target.destination)
                .path(&target.path)
                .build()
                .await
            {
                let n = action_proxy.n_actions().await.unwrap_or(0);
                for i in 0..n {
                    if let Ok(name) = action_proxy.get_name(i).await {
                        if matches!(name.as_str(), "click" | "activate" | "press") {
                            if action_proxy.do_action(i).await.is_ok() {
                                return (true, "atspi".into(), None);
                            }
                        }
                    }
                }
            }
        }
    }

    // Tier 2: XTest click at element center
    if let Some(ref bounds) = target.node.bounds {
        let (cx, cy) = element_center(bounds);
        let cx = cx as i16;
        let cy = cy as i16;

        let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let (conn, screen_num) = x11rb::connect(None)
                .map_err(|e| format!("X11 connect: {}", e))?;
            let root = conn.setup().roots[screen_num].root;
            xtest_button(&conn, 1, true, cx, cy, root)
                .map_err(|e| format!("button press: {}", e))?;
            xtest_button(&conn, 1, false, cx, cy, root)
                .map_err(|e| format!("button release: {}", e))?;
            Ok(())
        })
        .await;

        match result {
            Ok(Ok(())) => return (true, "xtest".into(), None),
            Ok(Err(e)) => return (false, "xtest".into(), Some(e)),
            Err(e) => return (false, "xtest".into(), Some(format!("task failed: {}", e))),
        }
    }

    (false, "none".into(), Some("Element has no bounds and no click action".into()))
}

async fn execute_set_value(
    target: &FindResult,
    req: &DebugUiActionRequest,
) -> (bool, String, Option<String>) {
    let value = match &req.value {
        Some(v) => v,
        None => return (false, "none".into(), Some("No value provided".into())),
    };

    // Tier 1: AT-SPI2 Value interface
    if target.interfaces.iter().any(|i| i == "Value") {
        if let Some(num) = value.as_f64() {
            if let Ok(conn) = atspi::AccessibilityConnection::new().await {
                if let Ok(val_proxy) = atspi::proxy::value::ValueProxy::builder(conn.connection())
                    .destination(&target.destination)
                    .path(&target.path)
                    .build()
                    .await
                {
                    if val_proxy.set_current_value(num).await.is_ok() {
                        return (true, "atspi".into(), None);
                    }
                }
            }
        }
    }

    // Tier 2: Fall back to Type action (clear + type the value as text)
    let text = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    // Select all + type replacement (Ctrl+A, then type)
    (false, "none".into(), Some("SetValue fallback not implemented for this element".into()))
}

async fn execute_type(
    pid: u32,
    target: &FindResult,
    req: &DebugUiActionRequest,
) -> (bool, String, Option<String>) {
    let text = match &req.text {
        Some(t) => t.clone(),
        None => return (false, "none".into(), Some("No text provided".into())),
    };

    // Click to focus first
    if let Some(ref bounds) = target.node.bounds {
        let (cx, cy) = element_center(bounds);
        let cx = cx as i16;
        let cy = cy as i16;
        let _ = tokio::task::spawn_blocking(move || -> Result<(), String> {
            let (conn, sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
            let root = conn.setup().roots[sn].root;
            xtest_button(&conn, 1, true, cx, cy, root).map_err(|e| e.to_string())?;
            xtest_button(&conn, 1, false, cx, cy, root).map_err(|e| e.to_string())?;
            Ok(())
        }).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Type via XTest key events
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (conn, _sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
        for ch in text.chars() {
            let keysym = ch as u32; // ASCII chars map directly to keysyms
            // For non-ASCII, would need XKB — handle ASCII subset for now
            if let Ok(kc) = keysym_to_keycode(&conn, keysym) {
                let _ = xtest::fake_input(&conn, KEY_PRESS_EVENT, kc, 0, 0, 0, 0, 0);
                let _ = xtest::fake_input(&conn, KEY_RELEASE_EVENT, kc, 0, 0, 0, 0, 0);
                conn.flush().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => (true, "xtest".into(), None),
        Ok(Err(e)) => (false, "xtest".into(), Some(e)),
        Err(e) => (false, "xtest".into(), Some(format!("task failed: {}", e))),
    }
}

async fn execute_scroll(
    target: &FindResult,
    req: &DebugUiActionRequest,
) -> (bool, String, Option<String>) {
    let direction = match &req.direction {
        Some(d) => d.clone(),
        None => return (false, "none".into(), Some("No direction provided".into())),
    };
    let amount = req.amount.unwrap_or(3) as usize;

    let button = match direction {
        ScrollDirection::Up => 4u8,
        ScrollDirection::Down => 5,
        ScrollDirection::Left => 6,
        ScrollDirection::Right => 7,
    };

    let bounds = match &target.node.bounds {
        Some(b) => b.clone(),
        None => return (false, "none".into(), Some("Element has no bounds".into())),
    };

    let (cx, cy) = element_center(&bounds);
    let cx = cx as i16;
    let cy = cy as i16;

    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (conn, sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[sn].root;
        // Move to element center
        xtest_motion(&conn, cx, cy, root).map_err(|e| e.to_string())?;
        // Scroll button press/release repeated
        for _ in 0..amount {
            xtest_button(&conn, button, true, cx, cy, root).map_err(|e| e.to_string())?;
            xtest_button(&conn, button, false, cx, cy, root).map_err(|e| e.to_string())?;
        }
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => (true, "xtest".into(), None),
        Ok(Err(e)) => (false, "xtest".into(), Some(e)),
        Err(e) => (false, "xtest".into(), Some(format!("task failed: {}", e))),
    }
}

async fn execute_drag(
    pid: u32,
    source: &FindResult,
    req: &DebugUiActionRequest,
) -> (bool, String, Option<String>) {
    let to_id = match &req.to_id {
        Some(id) => id.clone(),
        None => return (false, "none".into(), Some("No destination (to_id) provided".into())),
    };

    let source_bounds = match &source.node.bounds {
        Some(b) => b.clone(),
        None => return (false, "none".into(), Some("Source element has no bounds".into())),
    };

    // Find destination
    let dest = match find_element_by_id(pid, &to_id).await {
        Ok(Some(r)) => r,
        _ => return (false, "none".into(), Some(format!("Destination node not found: {}", to_id))),
    };

    let dest_bounds = match &dest.node.bounds {
        Some(b) => b.clone(),
        None => return (false, "none".into(), Some("Destination element has no bounds".into())),
    };

    let (sx, sy) = element_center(&source_bounds);
    let (dx, dy) = element_center(&dest_bounds);
    let points = drag_interpolation_points(sx, sy, dx, dy, DRAG_STEPS);

    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (conn, sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[sn].root;

        // Move to source
        xtest_motion(&conn, sx as i16, sy as i16, root).map_err(|e| e.to_string())?;
        // Press
        xtest_button(&conn, 1, true, sx as i16, sy as i16, root).map_err(|e| e.to_string())?;

        // Interpolated moves
        for (px, py) in &points {
            std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
            xtest_motion(&conn, *px as i16, *py as i16, root).map_err(|e| e.to_string())?;
        }

        // Release at destination
        xtest_button(&conn, 1, false, dx as i16, dy as i16, root).map_err(|e| e.to_string())?;
        Ok(())
    }).await;

    match result {
        Ok(Ok(())) => (true, "xtest".into(), None),
        Ok(Err(e)) => (false, "xtest".into(), Some(e)),
        Err(e) => (false, "xtest".into(), Some(format!("task failed: {}", e))),
    }
}

fn execute_key_action(
    key_name: &str,
    modifiers: &[String],
) -> crate::Result<DebugUiActionResponse> {
    let keysym = key_name_to_keysym(key_name).ok_or_else(|| {
        crate::Error::UiQueryFailed(format!("Unknown key: {}", key_name))
    })?;

    let (conn, screen_num) = x11rb::connect(None).map_err(|e| {
        crate::Error::UiNotAvailable(format!("X11 display not available: {}", e))
    })?;
    let root = conn.setup().roots[screen_num].root;

    // Press modifier keys
    let modifier_keysyms: Vec<u32> = modifiers
        .iter()
        .filter_map(|m| match m.to_lowercase().as_str() {
            "shift" => Some(0xffe1u32),
            "ctrl" | "control" => Some(0xffe3),
            "alt" | "option" => Some(0xffe9),
            "cmd" | "command" | "super" | "meta" => Some(0xffeb),
            _ => None,
        })
        .collect();

    for &mod_ks in &modifier_keysyms {
        if let Err(e) = xtest_key(&conn, mod_ks, true) {
            return Ok(DebugUiActionResponse {
                success: false,
                method: None,
                node_before: None,
                node_after: None,
                changed: None,
                error: Some(format!("Modifier key press failed: {}", e)),
            });
        }
    }

    // Press and release main key
    let _ = xtest_key(&conn, keysym, true);
    let _ = xtest_key(&conn, keysym, false);

    // Release modifier keys (reverse order)
    for &mod_ks in modifier_keysyms.iter().rev() {
        let _ = xtest_key(&conn, mod_ks, false);
    }

    Ok(DebugUiActionResponse {
        success: true,
        method: Some("xtest".into()),
        node_before: None,
        node_after: None,
        changed: None,
        error: None,
    })
}
```

**Step 2: Verify it compiles**

Run: `cargo check 2>&1 | tail -10`

**Step 3: Run unit tests (keysym mapping)**

Run: `cargo test --lib ui::input_linux::tests -- --nocapture 2>&1`
Expected: All PASS.

**Step 4: Commit**

```bash
git add src/ui/input_linux.rs
git commit -m "feat: full input action dispatch with AT-SPI2 and XTest"
```

---

## Phase 6: Server Integration

### Task 13: Remove macOS-only cfg gates in server.rs

**Files:**
- Modify: `src/daemon/server.rs`

This is the critical integration task. Changes:

1. **Vision sidecar**: Remove `#[cfg(target_os = "macos")]` gate — make unconditional
2. **tool_debug_ui tree query**: Replace `#[cfg(target_os = "macos")]` / `#[cfg(not(...))]` blocks with per-platform blocks
3. **tool_debug_ui screenshot**: Same treatment
4. **Async adjustment**: Linux `query_ax_tree` is async, so don't wrap in `spawn_blocking`

**Step 1: Make vision_sidecar unconditional**

In the `Daemon` struct (line 32-33), `new()` (line 262-263), `idle_timeout_loop` (lines 332-339), `graceful_shutdown` (lines 371-377), and test helpers (lines 2545-2546, 2821-2822):

Remove all `#[cfg(target_os = "macos")]` annotations from `vision_sidecar` field and its uses.

```rust
// Daemon struct — remove cfg gate:
/// Vision sidecar for UI element detection
vision_sidecar: Arc<std::sync::Mutex<crate::ui::vision::VisionSidecar>>,

// All initialization sites — remove cfg gate:
vision_sidecar: Arc::new(std::sync::Mutex::new(crate::ui::vision::VisionSidecar::new())),

// idle_timeout_loop — remove cfg gate:
{
    let settings = crate::config::resolve(None);
    if let Ok(mut sidecar) = self.vision_sidecar.lock() {
        sidecar.check_idle_timeout(settings.vision_sidecar_idle_timeout_seconds);
    }
}

// graceful_shutdown — remove cfg gate:
{
    if let Ok(mut sidecar) = self.vision_sidecar.lock() {
        sidecar.shutdown();
    }
}
```

**Step 2: Replace tool_debug_ui tree query cfg blocks**

Replace lines 2337-2427 (the `#[cfg(target_os = "macos")]` and `#[cfg(not(target_os = "macos"))]` blocks inside `if needs_tree {}`) with:

```rust
if needs_tree {
    let pid = session.pid;

    #[cfg(target_os = "macos")]
    let nodes = tokio::task::spawn_blocking(move || {
        crate::ui::accessibility::query_ax_tree(pid)
    }).await.map_err(|e| crate::Error::Internal(format!("AX query task failed: {}", e)))??;

    #[cfg(target_os = "linux")]
    let nodes = crate::ui::accessibility::query_ax_tree(pid).await?;

    ax_count = crate::ui::tree::count_nodes(&nodes);

    let mut final_nodes = nodes;
    if vision_requested {
        let settings = crate::config::resolve(None);
        if !settings.vision_enabled {
            return Err(crate::Error::UiQueryFailed(
                "Vision pipeline requested but not enabled. Set vision.enabled=true in ~/.strobe/settings.json".to_string()
            ));
        }

        // SEC-8: Rate limit (same as before — code unchanged)
        {
            use std::sync::{Mutex, OnceLock};
            static LAST_VISION_CALL: OnceLock<Mutex<std::collections::HashMap<String, std::time::Instant>>>
                = OnceLock::new();
            let now = std::time::Instant::now();
            let rate_limiter = LAST_VISION_CALL.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
            let mut last_calls = rate_limiter.lock().unwrap();
            last_calls.retain(|_, last| now.duration_since(*last) < std::time::Duration::from_secs(60));
            if let Some(last_time) = last_calls.get(&req.session_id) {
                let elapsed = now.duration_since(*last_time);
                if elapsed < std::time::Duration::from_secs(1) {
                    return Err(crate::Error::UiQueryFailed(
                        format!("Vision rate limit exceeded. Please wait {:.1}s before next call.",
                            1.0 - elapsed.as_secs_f64())
                    ));
                }
            }
            last_calls.insert(req.session_id.clone(), now);
        }

        // Capture screenshot for vision
        let screenshot_b64 = {
            let pid = session.pid;
            let png_bytes = tokio::task::spawn_blocking(move || {
                crate::ui::capture::capture_window_screenshot(pid)
            }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

            use base64::Engine;
            base64::engine::general_purpose::STANDARD.encode(&png_bytes)
        };

        let vision_elements = {
            let mut sidecar = self.vision_sidecar.lock().unwrap();
            sidecar.detect(
                &screenshot_b64,
                settings.vision_confidence_threshold,
                settings.vision_iou_merge_threshold,
            )?
        };

        let (actual_merged, actual_added) = crate::ui::merge::merge_vision_into_tree(
            &mut final_nodes,
            &vision_elements,
            settings.vision_iou_merge_threshold as f64,
        );

        vision_count = actual_added;
        merged_count = actual_merged;
    }

    tree_output = Some(if verbose {
        crate::ui::tree::format_json(&final_nodes)?
    } else {
        crate::ui::tree::format_compact(&final_nodes)
    });
}
```

**Step 3: Replace tool_debug_ui screenshot cfg blocks**

Replace lines 2432-2470 with:

```rust
if needs_screenshot {
    let pid = session.pid;

    let element_bounds = if let Some(ref target_id) = req.id {
        let target_id = target_id.clone();

        #[cfg(target_os = "macos")]
        let nodes = tokio::task::spawn_blocking(move || {
            crate::ui::accessibility::query_ax_tree(pid)
        }).await.map_err(|e| crate::Error::Internal(format!("AX query task failed: {}", e)))??;

        #[cfg(target_os = "linux")]
        let nodes = crate::ui::accessibility::query_ax_tree(pid).await?;

        let node = crate::ui::tree::find_node_by_id(&nodes, &target_id)
            .ok_or_else(|| crate::Error::UiQueryFailed(
                format!("Element '{}' not found. Use debug_ui with mode=tree to see current element IDs.", target_id)
            ))?;
        Some(node.bounds.ok_or_else(|| crate::Error::UiQueryFailed(
            format!("Element '{}' has no bounds (may be off-screen or invisible)", target_id)
        ))?)
    } else {
        None
    };

    let png_bytes = tokio::task::spawn_blocking(move || {
        if let Some(bounds) = element_bounds {
            crate::ui::capture::capture_element_screenshot(pid, &bounds)
        } else {
            crate::ui::capture::capture_window_screenshot(pid)
        }
    }).await.map_err(|e| crate::Error::Internal(format!("Screenshot task failed: {}", e)))??;

    use base64::Engine;
    screenshot_output = Some(base64::engine::general_purpose::STANDARD.encode(&png_bytes));
}
```

**Step 4: Verify it compiles**

Run: `cargo check 2>&1 | tail -20`
Expected: Compiles. Watch for:
- Missing `use` statements for `capture` module on Linux
- Async/sync signature mismatches
- Vision sidecar field now unconditional — all cfg blocks removed

**Step 5: Run existing tests**

Run: `cargo test --lib 2>&1 | tail -20`
Expected: All existing tests PASS.

**Step 6: Commit**

```bash
git add src/daemon/server.rs
git commit -m "feat: enable UI observation on Linux, make vision sidecar cross-platform"
```

---

## Phase 7: Verification

### Task 14: Full build verification

**Step 1: Clean build**

Run: `cargo build 2>&1 | tail -20`
Expected: Build succeeds.

**Step 2: Run all unit tests**

Run: `cargo test --lib 2>&1 | tail -30`
Expected: All tests PASS, including:
- `ui::accessibility_linux::tests` (role mapping, action mapping, UID validation)
- `ui::capture_linux::tests` (pixel conversion, PNG encoding)
- `ui::input_linux::tests` (keysym mapping)
- `ui::input::tests` (modifier flags with Linux values)
- All existing tests unchanged

**Step 3: Verify error messages for unavailable services**

If running without X11/AT-SPI2, the functions should return clear error messages:
- `query_ax_tree` → "AT-SPI2 accessibility bus not available..."
- `capture_window_screenshot` → "Cannot connect to X11 display..."
- `execute_action` → appropriate error per action type

**Step 4: Commit (if any fixes were needed)**

```bash
git add -A
git commit -m "fix: address build verification findings"
```

---

### Task 15: Integration test scaffolding

**Files:**
- Create: `tests/linux_ui_integration.rs`

> **Note:** These tests require a running X11 + AT-SPI2 environment (e.g., Xvfb + dbus-daemon + at-spi2-registryd). They should be gated to skip gracefully when the environment isn't available.

**Step 1: Write gated integration tests**

```rust
//! Linux UI observation integration tests.
//! Requires: X11 display (Xvfb OK) + AT-SPI2 bus + a test application.
//! Skip gracefully when environment is not available.

#![cfg(target_os = "linux")]

use strobe::ui::accessibility_linux;

fn atspi_available() -> bool {
    accessibility_linux::is_available()
}

fn x11_available() -> bool {
    std::env::var("DISPLAY").is_ok()
        && x11rb::connect(None).is_ok()
}

#[test]
fn test_is_available_returns_bool() {
    // Just verify it doesn't panic
    let _ = accessibility_linux::is_available();
}

#[test]
fn test_check_accessibility_permission() {
    let _ = accessibility_linux::check_accessibility_permission(false);
}

#[tokio::test]
async fn test_query_nonexistent_pid() {
    if !atspi_available() {
        eprintln!("SKIP: AT-SPI2 not available");
        return;
    }
    let result = accessibility_linux::query_ax_tree(999999).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("No AT-SPI2 accessible") || err.contains("Permission denied"),
        "Unexpected error: {}",
        err
    );
}

#[test]
fn test_capture_nonexistent_pid() {
    if !x11_available() {
        eprintln!("SKIP: X11 not available");
        return;
    }
    let result = strobe::ui::capture::capture_window_screenshot(999999);
    assert!(result.is_err());
}
```

**Step 2: Run integration tests**

Run: `cargo test --test linux_ui_integration 2>&1 | tail -20`
Expected: Tests either PASS or SKIP with appropriate messages.

**Step 3: Commit**

```bash
git add tests/linux_ui_integration.rs
git commit -m "test: Linux UI observation integration test scaffolding"
```

---

## Post-Implementation Checklist

Before marking this feature complete, verify:

- [ ] `cargo build` succeeds on Linux
- [ ] `cargo test --lib` passes all unit tests
- [ ] `cargo test --test linux_ui_integration` runs without crashes
- [ ] Role mapping covers all 30+ entries from spec
- [ ] Key name mapping covers all ~50 entries from macOS parity
- [ ] Error messages include actionable guidance (gsettings, DISPLAY, etc.)
- [ ] Security constraints: UID validation, node cap (10,000), depth limit (50), image size limit (4K)
- [ ] Vision sidecar is no longer macOS-gated
- [ ] `capture_linux.rs` handles depth 24 and depth 32 pixel formats
- [ ] `input_linux.rs` tries AT-SPI2 actions before falling back to XTest
- [ ] Existing macOS functionality is unchanged (no regressions)

## Key API Discovery Notes

The `atspi` crate (version 0.29) API may differ from the code sketches above. During implementation, consult:

1. **`atspi::AccessibilityConnection`** — Constructor and method names for connecting to the AT-SPI2 bus
2. **`atspi::proxy::accessible::AccessibleProxy`** — How to get role, name, state, children, interfaces
3. **`atspi::Role` enum** — Exact variant names (may be PascalCase or differ from D-Bus names)
4. **`atspi::State` enum** — Exact variant names for Enabled, Sensitive, Focused
5. **`atspi::CoordType`** — Screen vs Window coordinate types
6. **`zbus::fdo::DBusProxy::get_connection_unix_process_id`** — Parameter type for bus name
7. **`x11rb::protocol::xtest::fake_input`** — Parameter order and types
8. **`x11rb` connection lifecycle** — Whether `connect()` returns `(impl Connection, usize)` or different types

These APIs should be verified against crate docs early in Phase 3 (Task 9) to avoid rework.
