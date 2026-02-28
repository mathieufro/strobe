//! Screenshot capture via macOS CGWindowListCreateImage.

use crate::Result;
use core_graphics::display::*;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::array::CFArray;
use core_graphics::image::CGImage;

/// Window info: ID and screen-space bounds in points.
struct WindowInfo {
    id: CGWindowID,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// Capture a screenshot of the main window for a given PID.
/// Returns PNG bytes.
pub fn capture_window_screenshot(pid: u32) -> Result<Vec<u8>> {
    unsafe {
        let win = find_main_window(pid)?;
        let image = capture_window_image(pid, &win)?;
        cg_image_to_png(&image)
    }
}

/// Capture a screenshot cropped to a specific element's bounds.
/// `element_bounds` is in screen-space points (from AX tree).
pub fn capture_element_screenshot(pid: u32, element_bounds: &crate::ui::tree::Rect) -> Result<Vec<u8>> {
    unsafe {
        let win = find_main_window(pid)?;
        let image = capture_window_image(pid, &win)?;

        // Compute scale factor: pixel dimensions / point dimensions
        let scale_x = if win.w > 0.0 { image.width() as f64 / win.w } else { 1.0 };
        let scale_y = if win.h > 0.0 { image.height() as f64 / win.h } else { 1.0 };

        // Convert element screen-space bounds to window-relative pixel coords
        let crop_x_f = (element_bounds.x - win.x) * scale_x;
        let crop_y_f = (element_bounds.y - win.y) * scale_y;
        if crop_x_f < 0.0 || crop_y_f < 0.0 {
            return Err(crate::Error::UiQueryFailed(
                "Element is outside the visible window area".to_string()
            ));
        }

        let img_w = image.width();
        let img_h = image.height();
        let crop_x = (crop_x_f.round() as usize).min(img_w.saturating_sub(1));
        let crop_y = (crop_y_f.round() as usize).min(img_h.saturating_sub(1));
        let crop_w = ((element_bounds.w * scale_x).round() as usize).min(img_w - crop_x);
        let crop_h = ((element_bounds.h * scale_y).round() as usize).min(img_h - crop_y);

        if crop_w == 0 || crop_h == 0 {
            return Err(crate::Error::UiQueryFailed(
                "Element has zero-size bounds after cropping".to_string()
            ));
        }

        crop_cg_image_to_png(&image, crop_x, crop_y, crop_w, crop_h)
    }
}

/// Capture the CGImage for the main window.
unsafe fn capture_window_image(pid: u32, win: &WindowInfo) -> Result<CGImage> {
    CGDisplay::screenshot(
        CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(0.0, 0.0)),
        kCGWindowListOptionIncludingWindow,
        win.id,
        kCGWindowImageBoundsIgnoreFraming,
    )
    .ok_or_else(|| crate::Error::UiQueryFailed(
        format!("Failed to capture screenshot for PID {} (window {})", pid, win.id)
    ))
}

/// Find the main (largest, on-screen) window for a PID.
unsafe fn find_main_window(pid: u32) -> Result<WindowInfo> {
    let windows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID,
    );

    if windows.is_null() {
        return Err(crate::Error::UiQueryFailed(
            "Failed to list windows".to_string()
        ));
    }

    let window_list = CFArray::<*const std::ffi::c_void>::wrap_under_create_rule(windows as _);
    let pid_key = CFString::new("kCGWindowOwnerPID");
    let id_key = CFString::new("kCGWindowNumber");
    let bounds_key = CFString::new("kCGWindowBounds");

    let mut best: Option<(WindowInfo, f64)> = None;

    for i in 0..window_list.len() {
        if let Some(dict) = window_list.get(i) {
            let dict_ref = *dict as core_foundation_sys::dictionary::CFDictionaryRef;

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

            // Get bounds
            let mut bounds_val: *const std::ffi::c_void = std::ptr::null();
            if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
                dict_ref, bounds_key.as_concrete_TypeRef() as _, &mut bounds_val
            ) == 0 {
                continue;
            }

            let bounds_dict = bounds_val as core_foundation_sys::dictionary::CFDictionaryRef;
            let x = cf_dict_f64(bounds_dict, "X");
            let y = cf_dict_f64(bounds_dict, "Y");
            let w = cf_dict_f64(bounds_dict, "Width");
            let h = cf_dict_f64(bounds_dict, "Height");
            let area = w * h;

            if best.is_none() || area > best.as_ref().unwrap().1 {
                best = Some((WindowInfo { id: window_id, x, y, w, h }, area));
            }
        }
    }

    best
        .map(|(info, _)| info)
        .ok_or_else(|| crate::Error::UiQueryFailed(
            format!("No visible window found for PID {}", pid)
        ))
}

/// Read an f64 from a CFDictionary by string key.
unsafe fn cf_dict_f64(dict: core_foundation_sys::dictionary::CFDictionaryRef, key: &str) -> f64 {
    let cf_key = CFString::new(key);
    let mut val: *const std::ffi::c_void = std::ptr::null();
    if core_foundation_sys::dictionary::CFDictionaryGetValueIfPresent(
        dict, cf_key.as_concrete_TypeRef() as _, &mut val
    ) != 0 {
        CFNumber::wrap_under_get_rule(val as _).to_f64().unwrap_or(0.0)
    } else {
        0.0
    }
}

/// Convert full CGImage to PNG bytes.
fn cg_image_to_png(image: &CGImage) -> Result<Vec<u8>> {
    crop_cg_image_to_png(image, 0, 0, image.width(), image.height())
}

/// Convert a cropped region of a CGImage to PNG bytes.
fn crop_cg_image_to_png(
    image: &CGImage,
    crop_x: usize,
    crop_y: usize,
    crop_w: usize,
    crop_h: usize,
) -> Result<Vec<u8>> {
    // SEC-3: Validate image size to prevent memory exhaustion
    const MAX_PIXELS: usize = 3840 * 2160; // 4K resolution limit
    if crop_w.checked_mul(crop_h).map_or(true, |pixels| pixels > MAX_PIXELS) {
        return Err(crate::Error::UiQueryFailed(
            format!("Screenshot too large: {}x{} exceeds 4K limit ({}x{})",
                crop_w, crop_h, 3840, 2160)
        ));
    }

    let bytes_per_row = image.bytes_per_row();
    let data = image.data();
    let bytes = data.bytes();

    let mut png_data = Vec::new();
    let mut encoder = png::Encoder::new(&mut png_data, crop_w as u32, crop_h as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    let mut writer = encoder.write_header()
        .map_err(|e| crate::Error::UiQueryFailed(format!("PNG encode error: {}", e)))?;

    // CGImage is BGRA, PNG expects RGBA — swap channels
    // SEC-4: Use checked multiplication to prevent integer overflow
    let pixel_count = crop_w.checked_mul(crop_h)
        .ok_or_else(|| crate::Error::UiQueryFailed("Image dimensions overflow".to_string()))?;
    let byte_count = pixel_count.checked_mul(4)
        .ok_or_else(|| crate::Error::UiQueryFailed("Image size overflow".to_string()))?;
    let mut rgba = Vec::with_capacity(byte_count);
    for y in crop_y..(crop_y + crop_h) {
        let row_start = y * bytes_per_row;
        for x in crop_x..(crop_x + crop_w) {
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
