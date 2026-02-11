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

// Helper to convert attribute name to CFStringRef
unsafe fn attr_to_cfstring(attr: &str) -> CFStringRef {
    CFString::new(attr).as_concrete_TypeRef()
}

/// Check if this process has accessibility permissions.
/// If `prompt` is true, shows the system dialog asking user to grant permission.
pub fn check_accessibility_permission(prompt: bool) -> bool {
    unsafe {
        if prompt {
            let key = CFString::new("AXTrustedCheckOptionPrompt");
            let value = CFBoolean::true_value();
            let pairs = [(key, value)];
            let options = core_foundation::dictionary::CFDictionary::from_CFType_pairs(&pairs);
            AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as _)
        } else {
            AXIsProcessTrusted()
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

    // SEC-5: Validate PID ownership to prevent privilege escalation
    unsafe {
        let current_uid = libc::geteuid();
        let mut proc_info: libc::proc_bsdinfo = std::mem::zeroed();
        let ret = libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut proc_info as *mut _ as *mut libc::c_void,
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
        );

        if ret <= 0 {
            return Err(crate::Error::UiQueryFailed(
                format!("Process {} not found or not accessible", pid)
            ));
        }

        // Check if the process is owned by the current user
        if proc_info.pbi_uid != current_uid && current_uid != 0 {
            return Err(crate::Error::UiQueryFailed(
                format!("Permission denied: process {} is owned by another user", pid)
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

        // Get all children of application (windows, menu bars, etc.)
        let mut children = Vec::new();
        let all_children = get_ax_children(app_ref);

        // Build nodes and filter out menu bars (include windows and other UI elements)
        let mut window_index = 0;
        for child_ref in all_children.iter() {
            if let Some(node) = build_node(*child_ref, window_index) {
                // Skip menu bars but include windows and other UI elements
                if !node.role.contains("MenuBar") {
                    children.push(node);
                    window_index += 1;
                }
            }
        }

        CFRelease(app_ref as *const c_void);

        // Release child refs
        for child in &all_children {
            CFRelease(*child as *const c_void);
        }

        Ok(children)
    }
}

/// Recursively build a UiNode from an AXUIElementRef.
unsafe fn build_node(element: AXUIElementRef, sibling_index: usize) -> Option<UiNode> {
    let role = get_ax_string(element, attr_to_cfstring(kAXRoleAttribute))?;
    let title = get_ax_string(element, attr_to_cfstring(kAXTitleAttribute))
        .or_else(|| get_ax_string(element, attr_to_cfstring(kAXDescriptionAttribute)));
    let value = get_ax_value_string(element);
    let enabled = get_ax_bool(element, attr_to_cfstring(kAXEnabledAttribute)).unwrap_or(true);
    let focused = get_ax_bool(element, attr_to_cfstring(kAXFocusedAttribute)).unwrap_or(false);
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
    let err = AXUIElementCopyAttributeValue(element, attr_to_cfstring(kAXValueAttribute), &mut value);
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
        ) {
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
    // Try to read as CFBoolean
    let cf_bool = CFBoolean::wrap_under_get_rule(value as _);
    let result = cf_bool.into();
    Some(result)
}

/// Get bounding box (position + size).
unsafe fn get_ax_bounds(element: AXUIElementRef) -> Option<Rect> {
    // Position
    let mut pos_value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr_to_cfstring(kAXPositionAttribute), &mut pos_value);
    if err != 0 || pos_value.is_null() {
        return None;
    }

    let mut point = core_graphics::geometry::CGPoint::new(0.0, 0.0);
    if !AXValueGetValue(
        pos_value as AXValueRef,
        kAXValueTypeCGPoint,
        &mut point as *mut _ as *mut c_void,
    ) {
        CFRelease(pos_value);
        return None;
    }
    CFRelease(pos_value);

    // Size
    let mut size_value: CFTypeRef = std::ptr::null();
    let err = AXUIElementCopyAttributeValue(element, attr_to_cfstring(kAXSizeAttribute), &mut size_value);
    if err != 0 || size_value.is_null() {
        return None;
    }

    let mut size = core_graphics::geometry::CGSize::new(0.0, 0.0);
    if !AXValueGetValue(
        size_value as AXValueRef,
        kAXValueTypeCGSize,
        &mut size as *mut _ as *mut c_void,
    ) {
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
    let err = AXUIElementCopyAttributeValue(element, attr_to_cfstring(kAXChildrenAttribute), &mut value);
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
        if let Some(child_ref) = array.get(i) {
            let child = *child_ref as AXUIElementRef;
            // Retain each child since we'll use them after the array is released
            core_foundation_sys::base::CFRetain(child as *const c_void);
            result.push(child);
        }
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
        if let Some(name_ref) = array.get(i) {
            let name = *name_ref as CFStringRef;
            let cf_str = CFString::wrap_under_get_rule(name);
            result.push(cf_str.to_string());
        }
    }
    result
}
