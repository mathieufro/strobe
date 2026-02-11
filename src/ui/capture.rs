//! Screenshot capture via macOS CGWindowListCreateImage.

use crate::Result;
use core_graphics::display::*;
use core_graphics::geometry::{CGPoint, CGRect, CGSize};
use core_foundation::base::TCFType;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::array::CFArray;
use core_graphics::image::CGImage;

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

    let window_list = CFArray::<*const std::ffi::c_void>::wrap_under_create_rule(windows as _);
    let pid_key = CFString::new("kCGWindowOwnerPID");
    let id_key = CFString::new("kCGWindowNumber");
    let bounds_key = CFString::new("kCGWindowBounds");

    let mut best_window: Option<(CGWindowID, f64)> = None;

    for i in 0..window_list.len() {
        if let Some(dict) = window_list.get(i) {
            // This is a raw CFDictionary — we need to use Core Foundation getters
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
