//! macOS UI interaction via AX actions and CGEvent injection.

use crate::mcp::{DebugUiActionRequest, DebugUiActionResponse, ScrollDirection, UiActionType};
use crate::ui::accessibility::{find_ax_element, query_ax_tree};
use crate::ui::input::{drag_interpolation_points, element_center, modifier_string_to_flags};
use crate::ui::tree::{diff_nodes, UiNode};
use accessibility_sys::*;
use core_foundation::base::{CFRelease, TCFType};
use core_foundation::string::CFString;
use core_foundation_sys::base::CFTypeRef;
use core_foundation_sys::number::{kCFNumberFloat64Type, CFNumberCreate};
use core_graphics::event::{
    CGEvent, CGEventType, CGMouseButton, ScrollEventUnit,
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
        let key_str = req.key.as_ref().unwrap().clone(); // validated by caller
        let modifiers = req.modifiers.clone().unwrap_or_default();
        let send_result = tokio::task::spawn_blocking(move || {
            send_key_event(pid, &key_str, &modifiers)
        })
        .await
        .map_err(|e| crate::Error::Internal(format!("Key event task failed: {}", e)))?;

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
            .await
            .map_err(|e| crate::Error::Internal(format!("AX query failed: {}", e)))??;
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
            .await
            .map_err(|e| crate::Error::Internal(format!("AX query failed: {}", e)))??;
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
            pid,
            &action_req,
            &target_id_for_action,
            &node_before_clone,
            bounds.as_ref(),
            to_bounds.as_ref(),
        )
    })
    .await
    .map_err(|e| crate::Error::Internal(format!("Action task failed: {}", e)))?;

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
    })
    .await
    .map_err(|e| crate::Error::Internal(format!("Verify task failed: {}", e)))??;

    let changed = node_after
        .as_ref()
        .map(|after| diff_nodes(&node_before, after));

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
            .ok_or_else(|| {
                crate::Error::UiQueryFailed("node not found during set_value".to_string())
            })?;

        let result = if let Some(num) = value.as_f64() {
            // Check if this is a text field — convert to string
            let role = get_ax_role(ax_ref);
            if role.as_deref() == Some("AXTextField")
                || role.as_deref() == Some("AXTextArea")
            {
                set_ax_string_value(ax_ref, &num.to_string())
            } else {
                // Try CFNumber first, fall back to string (SwiftUI sliders use string values)
                set_ax_number_value(ax_ref, num)
                    .or_else(|_| set_ax_string_value(ax_ref, &num.to_string()))
            }
        } else if let Some(s) = value.as_str() {
            set_ax_string_value(ax_ref, s)
        } else {
            set_ax_string_value(ax_ref, &value.to_string())
        };

        CFRelease(ax_ref as *const c_void);

        result.map_err(|_| {
            crate::Error::UiQueryFailed(
                "element does not support set_value; try 'type'".to_string(),
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
    let move_event = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::MouseMoved,
        CGPoint::new(cx, cy),
        CGMouseButton::Left,
    )
    .map_err(|_| crate::Error::Internal("Failed to create mouse move event".to_string()))?;
    move_event.post_to_pid(pid as i32);

    let event = CGEvent::new_scroll_event(
        source,
        ScrollEventUnit::LINE,
        2, // wheel_count
        wheel1,
        wheel2,
        0,
    )
    .map_err(|_| crate::Error::Internal("Failed to create scroll event".to_string()))?;
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
        source.clone(),
        CGEventType::LeftMouseDown,
        CGPoint::new(x0, y0),
        CGMouseButton::Left,
    )
    .map_err(|_| crate::Error::Internal("Failed to create mouse down event".to_string()))?;
    down.post_to_pid(pid as i32);

    // Interpolated drag moves
    let points = drag_interpolation_points(x0, y0, x1, y1, DRAG_STEPS);
    for (px, py) in points {
        std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
        let drag = CGEvent::new_mouse_event(
            source.clone(),
            CGEventType::LeftMouseDragged,
            CGPoint::new(px, py),
            CGMouseButton::Left,
        )
        .map_err(|_| crate::Error::Internal("Failed to create drag event".to_string()))?;
        drag.post_to_pid(pid as i32);
    }

    // Mouse up at destination
    std::thread::sleep(std::time::Duration::from_millis(DRAG_STEP_INTERVAL_MS));
    let up = CGEvent::new_mouse_event(
        source,
        CGEventType::LeftMouseUp,
        CGPoint::new(x1, y1),
        CGMouseButton::Left,
    )
    .map_err(|_| crate::Error::Internal("Failed to create mouse up event".to_string()))?;
    up.post_to_pid(pid as i32);

    Ok("cgevent".to_string())
}

// ---- CGEvent helpers ----

fn cg_click(pid: u32, x: f64, y: f64) -> crate::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;
    let point = CGPoint::new(x, y);

    let down = CGEvent::new_mouse_event(
        source.clone(),
        CGEventType::LeftMouseDown,
        point,
        CGMouseButton::Left,
    )
    .map_err(|_| crate::Error::Internal("Failed to create mouse down".to_string()))?;
    let up = CGEvent::new_mouse_event(
        source,
        CGEventType::LeftMouseUp,
        point,
        CGMouseButton::Left,
    )
    .map_err(|_| crate::Error::Internal("Failed to create mouse up".to_string()))?;
    down.post_to_pid(pid as i32);
    up.post_to_pid(pid as i32);
    Ok(())
}

fn cg_type_string(pid: u32, text: &str) -> crate::Result<()> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| crate::Error::Internal("Failed to create CGEventSource".to_string()))?;

    let key_down = CGEvent::new_keyboard_event(source, 0, true)
        .map_err(|_| crate::Error::Internal("Failed to create key event".to_string()))?;
    let utf16: Vec<u16> = text.encode_utf16().collect();
    key_down.set_string_from_utf16_unchecked(&utf16);
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
fn key_name_to_keycode(key: &str) -> crate::Result<u16> {
    match key.to_lowercase().as_str() {
        "a" => Ok(0x00),
        "s" => Ok(0x01),
        "d" => Ok(0x02),
        "f" => Ok(0x03),
        "h" => Ok(0x04),
        "g" => Ok(0x05),
        "z" => Ok(0x06),
        "x" => Ok(0x07),
        "c" => Ok(0x08),
        "v" => Ok(0x09),
        "b" => Ok(0x0B),
        "q" => Ok(0x0C),
        "w" => Ok(0x0D),
        "e" => Ok(0x0E),
        "r" => Ok(0x0F),
        "y" => Ok(0x10),
        "t" => Ok(0x11),
        "1" => Ok(0x12),
        "2" => Ok(0x13),
        "3" => Ok(0x14),
        "4" => Ok(0x15),
        "5" => Ok(0x17),
        "6" => Ok(0x16),
        "7" => Ok(0x1A),
        "8" => Ok(0x1C),
        "9" => Ok(0x19),
        "0" => Ok(0x1D),
        "o" => Ok(0x1F),
        "u" => Ok(0x20),
        "i" => Ok(0x22),
        "p" => Ok(0x23),
        "l" => Ok(0x25),
        "j" => Ok(0x26),
        "k" => Ok(0x28),
        "n" => Ok(0x2D),
        "m" => Ok(0x2E),
        "return" | "enter" => Ok(0x24),
        "tab" => Ok(0x30),
        "space" => Ok(0x31),
        "delete" | "backspace" => Ok(0x33),
        "escape" | "esc" => Ok(0x35),
        "left" => Ok(0x7B),
        "right" => Ok(0x7C),
        "down" => Ok(0x7D),
        "up" => Ok(0x7E),
        "f1" => Ok(0x7A),
        "f2" => Ok(0x78),
        "f3" => Ok(0x63),
        "f4" => Ok(0x76),
        "f5" => Ok(0x60),
        "f6" => Ok(0x61),
        "f7" => Ok(0x62),
        "f8" => Ok(0x64),
        "f9" => Ok(0x65),
        "f10" => Ok(0x6D),
        "f11" => Ok(0x67),
        "f12" => Ok(0x6F),
        other => Err(crate::Error::ValidationError(format!(
            "unknown key: '{}'. Supported: a-z, 0-9, return, tab, space, delete, escape, arrow keys, f1-f12",
            other
        ))),
    }
}

// ---- AX action helpers ----

fn perform_ax_action(pid: u32, target_id: &str, action_name: &str) -> crate::Result<()> {
    unsafe {
        let ax_ref = find_ax_element(pid, target_id)?
            .ok_or_else(|| {
                crate::Error::UiQueryFailed("node not found for AX action".to_string())
            })?;

        let action = CFString::new(action_name);
        let err = AXUIElementPerformAction(ax_ref, action.as_concrete_TypeRef());
        CFRelease(ax_ref as *const c_void);

        if err != 0 {
            return Err(crate::Error::UiQueryFailed(format!(
                "AX action '{}' failed with error {}",
                action_name, err
            )));
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
    if core_foundation_sys::base::CFGetTypeID(value)
        != core_foundation_sys::string::CFStringGetTypeID()
    {
        CFRelease(value as *const c_void);
        return None;
    }
    let cf_str =
        CFString::wrap_under_create_rule(value as core_foundation_sys::string::CFStringRef);
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
    if err != 0 {
        Err(())
    } else {
        Ok(())
    }
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
    if err != 0 {
        Err(())
    } else {
        Ok(())
    }
}

/// Find a node by ID in a tree of UiNodes.
fn find_node_in_tree(nodes: &[UiNode], target_id: &str) -> Option<UiNode> {
    crate::ui::tree::find_node_by_id(nodes, target_id)
}
