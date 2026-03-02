//! Linux screenshot capture via X11 (GetImage + _NET_WM_PID).

use crate::Result;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::*;

const MAX_IMAGE_WIDTH: u32 = 3840;
const MAX_IMAGE_HEIGHT: u32 = 2160;
const DEFAULT_DPI: f64 = 96.0;

/// Convert BGRX pixel data (depth 24, bpp 32) to RGBA.
/// The X byte is ignored and alpha is set to 255.
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

/// Convert BGRA pixel data (depth 32, bpp 32) to RGBA.
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

/// Encode raw RGBA pixel data as PNG.
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

/// Detect X11 HiDPI scale factor from the RESOURCE_MANAGER property (Xft.dpi).
/// Returns 1.0 if no scaling is detected or the property cannot be read.
/// XWayland windows use X11 coordinates while AT-SPI2 reports screen coordinates
/// that may differ under HiDPI scaling.
fn get_x11_scale_factor(conn: &impl Connection, screen: &Screen) -> f64 {
    let resource_manager = match conn.intern_atom(false, b"RESOURCE_MANAGER") {
        Ok(cookie) => match cookie.reply() {
            Ok(reply) => reply.atom,
            Err(_) => return 1.0,
        },
        Err(_) => return 1.0,
    };

    let reply = match conn.get_property(
        false,
        screen.root,
        resource_manager,
        AtomEnum::STRING,
        0,
        16384,
    ) {
        Ok(cookie) => match cookie.reply() {
            Ok(r) => r,
            Err(_) => return 1.0,
        },
        Err(_) => return 1.0,
    };

    let resources = match std::str::from_utf8(&reply.value) {
        Ok(s) => s,
        Err(_) => return 1.0,
    };

    for line in resources.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Xft.dpi:") {
            if let Ok(dpi) = rest.trim().parse::<f64>() {
                if dpi > 0.0 {
                    return dpi / DEFAULT_DPI;
                }
            }
        }
    }

    1.0
}

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
        if let Ok(cookie) = conn.get_geometry(win_id) {
            if let Ok(geom) = cookie.reply() {
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

    // Validate image size
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
    let stride = if h > 0 { image.data.len() / h } else { 0 };

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

    // Capture full window
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
    let stride = if h > 0 { image.data.len() / h } else { 0 };

    let rgba = match depth {
        24 if image.data.len() >= w * h * 4 => bgrx_to_rgba(&image.data, w, h, stride),
        32 if image.data.len() >= w * h * 4 => bgra_to_rgba(&image.data, w, h, stride),
        _ => {
            return Err(crate::Error::UiQueryFailed(format!(
                "Unsupported pixel depth: {}", depth
            )));
        }
    };

    // Detect HiDPI scale factor — AT-SPI2 reports screen coordinates which may
    // differ from X11 window coordinates under XWayland scaling.
    let scale = get_x11_scale_factor(&conn, screen);

    // Compute crop coordinates relative to window origin, adjusting for scale factor.
    // AT-SPI2 bounds are in logical (screen) coords; X11 image is in physical (pixel) coords.
    let crop_x = (((element_bounds.x - win.x as f64) * scale).round().max(0.0) as usize).min(w.saturating_sub(1));
    let crop_y = (((element_bounds.y - win.y as f64) * scale).round().max(0.0) as usize).min(h.saturating_sub(1));
    let crop_w = ((element_bounds.w * scale).round() as usize).min(w - crop_x);
    let crop_h = ((element_bounds.h * scale).round() as usize).min(h - crop_y);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bgrx_to_rgba_single_pixel() {
        let data = vec![0x10, 0x20, 0x30, 0xFF];
        let rgba = bgrx_to_rgba(&data, 1, 1, 4);
        assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0xFF]);
    }

    #[test]
    fn test_bgrx_to_rgba_two_pixels() {
        let data = vec![
            0x10, 0x20, 0x30, 0x00,
            0xAA, 0xBB, 0xCC, 0x00,
        ];
        let rgba = bgrx_to_rgba(&data, 2, 1, 8);
        assert_eq!(rgba, vec![
            0x30, 0x20, 0x10, 0xFF,
            0xCC, 0xBB, 0xAA, 0xFF,
        ]);
    }

    #[test]
    fn test_bgrx_to_rgba_with_stride_padding() {
        let data = vec![
            0x10, 0x20, 0x30, 0x00, 0xDE, 0xAD, 0xBE, 0xEF,
            0xAA, 0xBB, 0xCC, 0x00, 0xDE, 0xAD, 0xBE, 0xEF,
        ];
        let rgba = bgrx_to_rgba(&data, 1, 2, 8);
        assert_eq!(rgba, vec![
            0x30, 0x20, 0x10, 0xFF,
            0xCC, 0xBB, 0xAA, 0xFF,
        ]);
    }

    #[test]
    fn test_bgra_to_rgba_preserves_alpha() {
        let data = vec![0x10, 0x20, 0x30, 0x80];
        let rgba = bgra_to_rgba(&data, 1, 1, 4);
        assert_eq!(rgba, vec![0x30, 0x20, 0x10, 0x80]);
    }

    #[test]
    fn test_encode_png_produces_valid_png() {
        let rgba = vec![0xFF, 0x00, 0x00, 0xFF];
        let png_bytes = encode_png(&rgba, 1, 1).unwrap();
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47]);
        assert!(png_bytes.len() > 8);
    }

    #[test]
    fn test_encode_png_rejects_mismatched_dimensions() {
        let rgba = vec![0xFF; 4];
        let result = encode_png(&rgba, 2, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_bgrx_to_rgba_zero_dimensions() {
        let rgba = bgrx_to_rgba(&[], 0, 0, 0);
        assert!(rgba.is_empty());
    }

    #[test]
    fn test_bgra_to_rgba_zero_dimensions() {
        let rgba = bgra_to_rgba(&[], 0, 0, 0);
        assert!(rgba.is_empty());
    }

    #[test]
    fn test_encode_png_zero_dimensions() {
        // 0x0 image with empty data should fail (checked_mul returns Some(0),
        // and the png encoder rejects 0-dimension images)
        let result = encode_png(&[], 0, 0);
        // Either succeeds with degenerate PNG or fails — both are acceptable
        // The important thing is no panic
        let _ = result;
    }

    #[test]
    fn test_encode_png_zero_width_nonzero_height() {
        let result = encode_png(&[], 0, 10);
        let _ = result; // no panic
    }

    #[test]
    fn test_crop_coordinates_negative_offset_clamped() {
        // Simulate element bounds to the left/above the window origin.
        // The .max(0.0) clamp prevents usize underflow.
        let element_x: f64 = 50.0;
        let win_x: f64 = 100.0; // window is to the right of element
        let scale = 1.0;

        let crop_x = (((element_x - win_x) * scale).round().max(0.0) as usize).min(100);
        assert_eq!(crop_x, 0, "negative offset should clamp to 0");
    }

    #[test]
    fn test_crop_coordinates_with_hidpi_scale() {
        let element_x: f64 = 200.0;
        let win_x: f64 = 100.0;
        let scale = 2.0; // 2x HiDPI
        let w: usize = 1920;

        let crop_x = (((element_x - win_x) * scale).round().max(0.0) as usize).min(w.saturating_sub(1));
        assert_eq!(crop_x, 200, "100 logical pixels * 2x scale = 200 physical pixels");
    }
}
