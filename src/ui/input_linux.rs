//! Linux UI interaction via AT-SPI2 actions and X11 XTest input injection.

use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;
use x11rb::protocol::xtest;

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
}
