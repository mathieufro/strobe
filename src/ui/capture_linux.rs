//! Linux screenshot capture via X11 (GetImage + _NET_WM_PID).

use crate::Result;

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
}
