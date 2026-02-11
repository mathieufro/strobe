//! Linux accessibility stub (AT-SPI not yet implemented).
//!
//! This module provides a minimal stub for Linux platforms to allow
//! compilation. Actual AT-SPI integration is planned for future phases.

use crate::Result;
use super::tree::UiNode;

/// Check if AT-SPI is available (always false in stub).
pub fn is_available() -> bool {
    false
}

/// Query accessibility tree for a process (stub - returns error).
pub fn query_ax_tree(_pid: u32) -> Result<Vec<UiNode>> {
    Err(crate::Error::UiNotAvailable(
        "Linux AT-SPI support not yet implemented. Use macOS for UI observation.".to_string()
    ))
}

/// Check accessibility permissions (stub - always returns false).
pub fn check_accessibility_permission(_prompt: bool) -> bool {
    false
}
