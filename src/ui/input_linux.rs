//! Linux UI interaction via AT-SPI2 actions and X11 XTest input injection.

use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse, ScrollDirection, UiActionType};
use crate::ui::accessibility_linux::{find_element_by_id, FindResult};
use crate::ui::input::{drag_interpolation_points, element_center};
use crate::ui::tree::diff_nodes;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest;

const DEFAULT_SETTLE_MS: u64 = 80;
const DRAG_STEPS: usize = 10;
const DRAG_STEP_INTERVAL_MS: u64 = 16;

/// Map key name to X11 keysym. Mirrors the ~50 key names from macOS input_mac.rs.
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

/// Resolve keysym to keycode via the X11 keyboard mapping.
fn keysym_to_keycode(
    conn: &impl Connection,
    keysym: u32,
) -> Result<u8, String> {
    let setup = conn.setup();
    let min_kc = setup.min_keycode;
    let max_kc = setup.max_keycode;

    let mapping = conn
        .get_keyboard_mapping(min_kc, max_kc - min_kc + 1)
        .map_err(|e| format!("get_keyboard_mapping: {}", e))?
        .reply()
        .map_err(|e| format!("keyboard mapping reply: {}", e))?;

    let syms_per_kc = mapping.keysyms_per_keycode as usize;
    for kc in min_kc..=max_kc {
        let offset = (kc - min_kc) as usize * syms_per_kc;
        for col in 0..syms_per_kc {
            if mapping.keysyms[offset + col] == keysym {
                return Ok(kc);
            }
        }
    }

    Err(format!("No keycode found for keysym 0x{:x}", keysym))
}

/// Send a fake key press/release via XTest.
fn xtest_key(
    conn: &impl Connection,
    keysym: u32,
    press: bool,
) -> Result<(), String> {
    let keycode = keysym_to_keycode(conn, keysym)?;
    let event_type = if press { KEY_PRESS_EVENT } else { KEY_RELEASE_EVENT };
    xtest::fake_input(conn, event_type, keycode, 0, 0, 0, 0, 0)
        .map_err(|e| format!("fake_input: {}", e))?
        .check()
        .map_err(|e| format!("fake_input check: {}", e))?;
    conn.flush().map_err(|e| format!("flush: {}", e))?;
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
) -> Result<(), String> {
    // Move to position first
    xtest::fake_input(conn, MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)
        .map_err(|e| format!("motion: {}", e))?
        .check()
        .map_err(|e| format!("motion check: {}", e))?;

    let event_type = if press { BUTTON_PRESS_EVENT } else { BUTTON_RELEASE_EVENT };
    xtest::fake_input(conn, event_type, button, 0, root, 0, 0, 0)
        .map_err(|e| format!("button: {}", e))?
        .check()
        .map_err(|e| format!("button check: {}", e))?;
    conn.flush().map_err(|e| format!("flush: {}", e))?;
    Ok(())
}

/// Move the mouse pointer via XTest.
fn xtest_motion(
    conn: &impl Connection,
    x: i16,
    y: i16,
    root: u32,
) -> Result<(), String> {
    xtest::fake_input(conn, MOTION_NOTIFY_EVENT, 0, 0, root, x, y, 0)
        .map_err(|e| format!("motion: {}", e))?
        .check()
        .map_err(|e| format!("motion check: {}", e))?;
    conn.flush().map_err(|e| format!("flush: {}", e))?;
    Ok(())
}

/// Try to perform a click action via AT-SPI2. Returns Some if attempted.
async fn try_atspi_click(target: &FindResult) -> Option<(bool, String, Option<String>)> {
    let connection = crate::ui::accessibility_linux::connect().await.ok()?;
    let conn = connection.connection();

    let action_proxy: atspi::proxy::action::ActionProxy<'_> =
        atspi::proxy::action::ActionProxy::builder(conn)
            .destination(target.destination.as_str())
            .ok()?
            .path(target.path.as_str())
            .ok()?
            .cache_properties(atspi::zbus::proxy::CacheProperties::No)
            .build()
            .await
            .ok()?;

    let n = action_proxy.nactions().await.unwrap_or(0);
    for i in 0..n {
        if let Ok(name) = action_proxy.get_name(i).await {
            if matches!(name.as_str(), "click" | "activate" | "press") {
                if action_proxy.do_action(i).await.is_ok() {
                    return Some((true, "atspi".into(), None));
                }
            }
        }
    }
    None
}

/// Try to set a value via AT-SPI2 Value interface. Returns Some if attempted.
async fn try_atspi_set_value(
    target: &FindResult,
    num: f64,
) -> Option<(bool, String, Option<String>)> {
    let connection = crate::ui::accessibility_linux::connect().await.ok()?;
    let conn = connection.connection();

    let val_proxy: atspi::proxy::value::ValueProxy<'_> =
        atspi::proxy::value::ValueProxy::builder(conn)
            .destination(target.destination.as_str())
            .ok()?
            .path(target.path.as_str())
            .ok()?
            .cache_properties(atspi::zbus::proxy::CacheProperties::No)
            .build()
            .await
            .ok()?;

    if val_proxy.set_current_value(num).await.is_ok() {
        Some((true, "atspi".into(), None))
    } else {
        None
    }
}

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
        crate::Error::UiQueryFailed(
            "Action requires an element ID (use debug_ui to find IDs)".into(),
        )
    })?;

    // Snapshot before
    let find_result = find_element_by_id(pid, target_id).await?.ok_or_else(|| {
        crate::Error::UiQueryFailed(format!("Node not found: {}", target_id))
    })?;
    let node_before = find_result.node.clone();

    // Execute action
    let (success, method, error) = match req.action {
        UiActionType::Click => execute_click(&find_result).await,
        UiActionType::SetValue => execute_set_value(&find_result, req).await,
        UiActionType::Type => execute_type(&find_result, req).await,
        UiActionType::Scroll => execute_scroll(&find_result, req).await,
        UiActionType::Drag => execute_drag(pid, &find_result, req).await,
        UiActionType::Key => unreachable!(),
    };

    // Settle
    tokio::time::sleep(std::time::Duration::from_millis(settle_ms)).await;

    // Snapshot after
    let node_after = find_element_by_id(pid, target_id)
        .await
        .ok()
        .flatten()
        .map(|r| r.node);

    let changed = node_after
        .as_ref()
        .map(|after| diff_nodes(&node_before, after));

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
    target: &FindResult,
) -> (bool, String, Option<String>) {
    // Tier 1: Try AT-SPI2 do_action("click")
    if target.interfaces.contains(atspi::Interface::Action) {
        if let Some(result) = try_atspi_click(target).await {
            return result;
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
            xtest_button(&conn, 1, true, cx, cy, root)?;
            xtest_button(&conn, 1, false, cx, cy, root)?;
            Ok(())
        })
        .await;

        match result {
            Ok(Ok(())) => return (true, "xtest".into(), None),
            Ok(Err(e)) => return (false, "xtest".into(), Some(e)),
            Err(e) => return (false, "xtest".into(), Some(format!("task failed: {}", e))),
        }
    }

    (
        false,
        "none".into(),
        Some("Element has no bounds and no click action".into()),
    )
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
    if target.interfaces.contains(atspi::Interface::Value) {
        if let Some(num) = value.as_f64() {
            if let Some(result) = try_atspi_set_value(target, num).await {
                return result;
            }
        }
    }

    (
        false,
        "none".into(),
        Some("SetValue not supported for this element".into()),
    )
}

async fn execute_type(
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
            xtest_button(&conn, 1, true, cx, cy, root)?;
            xtest_button(&conn, 1, false, cx, cy, root)?;
            Ok(())
        })
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Type via XTest key events
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (conn, _sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
        for ch in text.chars() {
            let keysym = ch as u32; // ASCII chars map directly to keysyms
            if let Ok(kc) = keysym_to_keycode(&conn, keysym) {
                if let Ok(cookie) = xtest::fake_input(&conn, KEY_PRESS_EVENT, kc, 0, 0, 0, 0, 0) {
                    let _ = cookie.check();
                }
                if let Ok(cookie) = xtest::fake_input(&conn, KEY_RELEASE_EVENT, kc, 0, 0, 0, 0, 0) {
                    let _ = cookie.check();
                }
                conn.flush().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await;

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
        xtest_motion(&conn, cx, cy, root)?;
        // Scroll button press/release repeated
        for _ in 0..amount {
            xtest_button(&conn, button, true, cx, cy, root)?;
            xtest_button(&conn, button, false, cx, cy, root)?;
        }
        Ok(())
    })
    .await;

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
        None => {
            return (
                false,
                "none".into(),
                Some("No destination (to_id) provided".into()),
            )
        }
    };

    let source_bounds = match &source.node.bounds {
        Some(b) => b.clone(),
        None => {
            return (
                false,
                "none".into(),
                Some("Source element has no bounds".into()),
            )
        }
    };

    // Find destination
    let dest = match find_element_by_id(pid, &to_id).await {
        Ok(Some(r)) => r,
        _ => {
            return (
                false,
                "none".into(),
                Some(format!("Destination node not found: {}", to_id)),
            )
        }
    };

    let dest_bounds = match &dest.node.bounds {
        Some(b) => b.clone(),
        None => {
            return (
                false,
                "none".into(),
                Some("Destination element has no bounds".into()),
            )
        }
    };

    let (sx, sy) = element_center(&source_bounds);
    let (dx, dy) = element_center(&dest_bounds);
    let points = drag_interpolation_points(sx, sy, dx, dy, DRAG_STEPS);

    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (conn, sn) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let root = conn.setup().roots[sn].root;

        // Move to source
        xtest_motion(&conn, sx as i16, sy as i16, root)?;
        // Press
        xtest_button(&conn, 1, true, sx as i16, sy as i16, root)?;

        // Interpolated moves
        for (px, py) in &points {
            std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
            xtest_motion(&conn, *px as i16, *py as i16, root)?;
        }

        // Release at destination
        xtest_button(&conn, 1, false, dx as i16, dy as i16, root)?;
        Ok(())
    })
    .await;

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

    let (conn, _screen_num) = x11rb::connect(None).map_err(|e| {
        crate::Error::UiNotAvailable(format!("X11 display not available: {}", e))
    })?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_name_letters() {
        for c in b'a'..=b'z' {
            let name = String::from(c as char);
            let ks = key_name_to_keysym(&name).unwrap();
            assert_eq!(ks, c as u32, "keysym for '{}'", name);
        }
    }

    #[test]
    fn test_key_name_digits() {
        for c in b'0'..=b'9' {
            let name = String::from(c as char);
            let ks = key_name_to_keysym(&name).unwrap();
            assert_eq!(ks, c as u32, "keysym for '{}'", name);
        }
    }

    #[test]
    fn test_key_name_special() {
        assert_eq!(key_name_to_keysym("return"), Some(0xff0d));
        assert_eq!(key_name_to_keysym("tab"), Some(0xff09));
        assert_eq!(key_name_to_keysym("space"), Some(0x0020));
        assert_eq!(key_name_to_keysym("delete"), Some(0xff08));
        assert_eq!(key_name_to_keysym("escape"), Some(0xff1b));
        assert_eq!(key_name_to_keysym("forwarddelete"), Some(0xffff));
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
        assert_eq!(key_name_to_keysym("shift"), Some(0xffe1));
        assert_eq!(key_name_to_keysym("control"), Some(0xffe3));
        assert_eq!(key_name_to_keysym("alt"), Some(0xffe9));
        assert_eq!(key_name_to_keysym("super"), Some(0xffeb));
    }

    #[test]
    fn test_key_name_unknown() {
        assert_eq!(key_name_to_keysym("nonexistent"), None);
    }

    #[test]
    fn test_key_name_case_insensitive() {
        assert_eq!(key_name_to_keysym("Return"), Some(0xff0d));
        assert_eq!(key_name_to_keysym("ESCAPE"), Some(0xff1b));
        assert_eq!(key_name_to_keysym("Tab"), Some(0xff09));
        assert_eq!(key_name_to_keysym("F1"), Some(0xffbe));
    }

    #[test]
    fn test_key_name_f13_out_of_range() {
        assert_eq!(key_name_to_keysym("f13"), None);
        assert_eq!(key_name_to_keysym("f0"), None);
    }

    // Action validation tests — these verify the argument checking in execute_action
    // without needing X11 or AT-SPI2 connections.

    fn make_req(action: UiActionType) -> DebugUiActionRequest {
        DebugUiActionRequest {
            session_id: String::new(),
            action,
            id: None,
            key: None,
            modifiers: None,
            text: None,
            value: None,
            direction: None,
            amount: None,
            to_id: None,
            settle_ms: None,
        }
    }

    #[tokio::test]
    async fn test_execute_action_click_requires_element_id() {
        let req = make_req(UiActionType::Click);
        let result = execute_action(std::process::id(), &req).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("element ID"), "error should mention element ID: {}", err);
    }

    #[tokio::test]
    async fn test_execute_action_scroll_requires_element_id() {
        let mut req = make_req(UiActionType::Scroll);
        req.direction = Some(ScrollDirection::Down);
        req.amount = Some(3);
        let result = execute_action(std::process::id(), &req).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("element ID"), "error should mention element ID: {}", err);
    }

    #[tokio::test]
    async fn test_execute_action_type_requires_element_id() {
        let mut req = make_req(UiActionType::Type);
        req.text = Some("hello".into());
        let result = execute_action(std::process::id(), &req).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("element ID"), "error should mention element ID: {}", err);
    }

    #[tokio::test]
    async fn test_execute_action_drag_requires_element_id() {
        let mut req = make_req(UiActionType::Drag);
        req.to_id = Some("dest_1234".into());
        let result = execute_action(std::process::id(), &req).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("element ID"), "error should mention element ID: {}", err);
    }

    #[tokio::test]
    async fn test_execute_action_unknown_key() {
        let mut req = make_req(UiActionType::Key);
        req.key = Some("nonexistent_key_xyz".into());
        let result = execute_action(std::process::id(), &req).await;
        // Should fail with "Unknown key" if X11 is available, or with X11 error if not
        assert!(result.is_err());
    }
}
