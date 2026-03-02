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
