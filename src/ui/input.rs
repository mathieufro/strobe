use crate::mcp::DebugUiActionRequest;
use crate::mcp::DebugUiActionResponse;
use crate::ui::tree::Rect;

/// Modifier flag constants (match CGEventFlags bit positions).
pub const MOD_SHIFT: u64 = 0x00020000; // kCGEventFlagMaskShift
pub const MOD_CONTROL: u64 = 0x00040000; // kCGEventFlagMaskControl
pub const MOD_ALTERNATE: u64 = 0x00080000; // kCGEventFlagMaskAlternate
pub const MOD_COMMAND: u64 = 0x00100000; // kCGEventFlagMaskCommand

/// Compute center point of an element's bounding box.
pub fn element_center(bounds: &Rect) -> (f64, f64) {
    (bounds.x + bounds.w / 2.0, bounds.y + bounds.h / 2.0)
}

/// Generate intermediate points for drag interpolation.
/// Returns `steps` points linearly interpolated from (x0,y0) to (x1,y1).
/// Point at index i is at t = (i+1)/steps.
pub fn drag_interpolation_points(
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    steps: usize,
) -> Vec<(f64, f64)> {
    (1..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t)
        })
        .collect()
}

/// Convert modifier string names to CGEventFlags bitmask.
pub fn modifier_string_to_flags(modifiers: &[String]) -> u64 {
    let mut flags: u64 = 0;
    for m in modifiers {
        match m.to_lowercase().as_str() {
            "cmd" | "command" => flags |= MOD_COMMAND,
            "shift" => flags |= MOD_SHIFT,
            "alt" | "option" => flags |= MOD_ALTERNATE,
            "ctrl" | "control" => flags |= MOD_CONTROL,
            _ => {} // ignore unknown modifiers
        }
    }
    flags
}

/// Execute a UI action. Dispatches to platform-specific implementation.
pub async fn execute_ui_action(
    pid: u32,
    req: &DebugUiActionRequest,
) -> crate::Result<DebugUiActionResponse> {
    #[cfg(target_os = "macos")]
    {
        crate::ui::input_mac::execute_action(pid, req).await
    }

    #[cfg(not(target_os = "macos"))]
    {
        crate::ui::input_linux::execute_action(pid, req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tree::Rect;

    #[test]
    fn test_element_center() {
        let bounds = Rect {
            x: 100.0,
            y: 200.0,
            w: 80.0,
            h: 30.0,
        };
        let (cx, cy) = element_center(&bounds);
        assert!((cx - 140.0).abs() < 0.01);
        assert!((cy - 215.0).abs() < 0.01);
    }

    #[test]
    fn test_element_center_zero_origin() {
        let bounds = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        };
        let (cx, cy) = element_center(&bounds);
        assert!((cx - 50.0).abs() < 0.01);
        assert!((cy - 25.0).abs() < 0.01);
    }

    #[test]
    fn test_drag_interpolation_points() {
        let points = drag_interpolation_points(0.0, 0.0, 100.0, 200.0, 10);
        assert_eq!(points.len(), 10);
        // First point should be near start (but not exactly at start)
        assert!(points[0].0 > 0.0 && points[0].0 < 100.0);
        // Last point should be near end (but not exactly at end)
        assert!(points[9].0 > 0.0 && points[9].0 <= 100.0);
        assert!(points[9].1 > 0.0 && points[9].1 <= 200.0);
        // Should be monotonically increasing in both axes
        for i in 1..points.len() {
            assert!(points[i].0 >= points[i - 1].0);
            assert!(points[i].1 >= points[i - 1].1);
        }
    }

    #[test]
    fn test_drag_interpolation_single_step() {
        let points = drag_interpolation_points(10.0, 20.0, 50.0, 80.0, 1);
        assert_eq!(points.len(), 1);
        assert!((points[0].0 - 50.0).abs() < 0.01);
        assert!((points[0].1 - 80.0).abs() < 0.01);
    }

    #[test]
    fn test_modifier_flags() {
        let flags =
            modifier_string_to_flags(&["cmd".to_string(), "shift".to_string()]);
        assert!(flags & MOD_COMMAND != 0);
        assert!(flags & MOD_SHIFT != 0);
        assert!(flags & MOD_CONTROL == 0);
    }

    #[test]
    fn test_modifier_flags_empty() {
        let flags = modifier_string_to_flags(&[]);
        assert_eq!(flags, 0);
    }

    #[test]
    fn test_modifier_flags_all() {
        let flags = modifier_string_to_flags(&[
            "cmd".to_string(),
            "shift".to_string(),
            "alt".to_string(),
            "ctrl".to_string(),
        ]);
        assert!(flags & MOD_COMMAND != 0);
        assert!(flags & MOD_SHIFT != 0);
        assert!(flags & MOD_ALTERNATE != 0);
        assert!(flags & MOD_CONTROL != 0);
    }
}
