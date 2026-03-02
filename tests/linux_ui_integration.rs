//! Linux UI observation integration tests.
//! Requires: X11 display (Xvfb OK) + AT-SPI2 bus + a test application.
//! Skip gracefully when environment is not available.

#![cfg(target_os = "linux")]

use strobe::ui::accessibility_linux;

fn atspi_available() -> bool {
    accessibility_linux::is_available()
}

fn x11_available() -> bool {
    std::env::var("DISPLAY").is_ok() && x11rb::connect(None).is_ok()
}

#[test]
fn test_is_available_returns_bool() {
    // Just verify it doesn't panic
    let _ = accessibility_linux::is_available();
}

#[test]
fn test_check_accessibility_permission() {
    let _ = accessibility_linux::check_accessibility_permission(false);
}

#[tokio::test]
async fn test_query_nonexistent_pid() {
    if !atspi_available() {
        eprintln!("SKIP: AT-SPI2 not available");
        return;
    }
    let result = accessibility_linux::query_ax_tree(999999).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("No AT-SPI2 accessible") || err.contains("Cannot read /proc"),
        "Unexpected error: {}",
        err
    );
}

#[test]
fn test_capture_nonexistent_pid() {
    if !x11_available() {
        eprintln!("SKIP: X11 not available");
        return;
    }
    let result = strobe::ui::capture::capture_window_screenshot(999999);
    assert!(result.is_err());
}
