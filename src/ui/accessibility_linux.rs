//! Linux accessibility via AT-SPI2 (D-Bus).

use crate::ui::tree::UiNode;
use crate::Result;

/// Map AT-SPI2 role to macOS AX-style role for cross-platform consistency.
fn map_atspi_role(role: atspi::Role) -> String {
    match role {
        atspi::Role::Button => "AXButton".into(),
        atspi::Role::CheckBox | atspi::Role::ToggleButton => "AXCheckBox".into(),
        atspi::Role::RadioButton => "AXRadioButton".into(),
        atspi::Role::Entry => "AXTextField".into(),
        atspi::Role::PasswordText => "AXSecureTextField".into(),
        atspi::Role::ComboBox => "AXPopUpButton".into(),
        atspi::Role::Slider => "AXSlider".into(),
        atspi::Role::SpinButton => "AXIncrementor".into(),
        atspi::Role::ProgressBar => "AXProgressIndicator".into(),
        atspi::Role::Label | atspi::Role::Static => "AXStaticText".into(),
        atspi::Role::Link => "AXLink".into(),
        atspi::Role::Image => "AXImage".into(),
        atspi::Role::Table => "AXTable".into(),
        atspi::Role::TableCell => "AXCell".into(),
        atspi::Role::TableRow => "AXRow".into(),
        atspi::Role::TableColumnHeader => "AXColumn".into(),
        atspi::Role::TreeTable => "AXOutline".into(),
        atspi::Role::List => "AXList".into(),
        atspi::Role::ListItem => "AXRow".into(),
        atspi::Role::Menu => "AXMenu".into(),
        atspi::Role::MenuItem => "AXMenuItem".into(),
        atspi::Role::MenuBar => "AXMenuBar".into(),
        atspi::Role::ToolBar => "AXToolbar".into(),
        atspi::Role::StatusBar => "AXStatusBar".into(),
        atspi::Role::Dialog => "AXDialog".into(),
        atspi::Role::Alert => "AXSheet".into(),
        atspi::Role::Frame | atspi::Role::Window => "AXWindow".into(),
        atspi::Role::Panel | atspi::Role::Filler => "AXGroup".into(),
        atspi::Role::ScrollBar => "AXScrollBar".into(),
        atspi::Role::ScrollPane => "AXScrollArea".into(),
        atspi::Role::PageTabList => "AXTabGroup".into(),
        atspi::Role::PageTab => "AXTab".into(),
        atspi::Role::Separator => "AXSplitter".into(),
        atspi::Role::Heading => "AXHeading".into(),
        atspi::Role::Text => "AXTextArea".into(),
        other => format!("atspi:{:?}", other),
    }
}

/// Map AT-SPI2 action name to macOS AX-style action name.
fn map_atspi_action(action: &str) -> String {
    match action {
        "click" | "press" | "activate" | "toggle" => "AXPress".into(),
        "expand or contract" => "AXPress".into(),
        other => format!("atspi:{}", other),
    }
}

/// Validate that the target PID is owned by the current user.
fn validate_pid_ownership(pid: u32) -> crate::Result<()> {
    let status_path = format!("/proc/{}/status", pid);
    let content = std::fs::read_to_string(&status_path).map_err(|_| {
        crate::Error::UiQueryFailed(format!(
            "Cannot read /proc/{}/status — process may not exist",
            pid
        ))
    })?;

    let my_euid = unsafe { libc::geteuid() };

    for line in content.lines() {
        if let Some(uid_str) = line.strip_prefix("Uid:") {
            let fields: Vec<&str> = uid_str.split_whitespace().collect();
            if let Some(real_uid_str) = fields.first() {
                if let Ok(real_uid) = real_uid_str.parse::<u32>() {
                    if real_uid != my_euid && my_euid != 0 {
                        return Err(crate::Error::UiQueryFailed(format!(
                            "Permission denied: process {} is owned by another user",
                            pid
                        )));
                    }
                    return Ok(());
                }
            }
        }
    }

    Err(crate::Error::UiQueryFailed(format!(
        "Cannot determine UID for process {}",
        pid
    )))
}

/// Check if AT-SPI2 bus is available.
pub fn is_available() -> bool {
    std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--dest=org.a11y.Bus",
            "--print-reply",
            "/org/a11y/bus",
            "org.freedesktop.DBus.Peer.Ping",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check accessibility permissions. On Linux, AT-SPI2 doesn't require per-app
/// permissions like macOS. Returns is_available(). `prompt` parameter is ignored.
pub fn check_accessibility_permission(_prompt: bool) -> bool {
    is_available()
}

/// Query the AT-SPI2 accessibility tree for a given PID.
pub async fn query_ax_tree(_pid: u32) -> Result<Vec<UiNode>> {
    Err(crate::Error::UiNotAvailable(
        "Linux AT-SPI2 tree walking not yet implemented".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_mapping_common_roles() {
        assert_eq!(map_atspi_role(atspi::Role::Button), "AXButton");
        assert_eq!(map_atspi_role(atspi::Role::CheckBox), "AXCheckBox");
        assert_eq!(map_atspi_role(atspi::Role::RadioButton), "AXRadioButton");
        assert_eq!(map_atspi_role(atspi::Role::ToggleButton), "AXCheckBox");
    }

    #[test]
    fn test_role_mapping_text_fields() {
        assert_eq!(map_atspi_role(atspi::Role::Entry), "AXTextField");
        assert_eq!(map_atspi_role(atspi::Role::PasswordText), "AXSecureTextField");
    }

    #[test]
    fn test_role_mapping_containers() {
        assert_eq!(map_atspi_role(atspi::Role::Frame), "AXWindow");
        assert_eq!(map_atspi_role(atspi::Role::Panel), "AXGroup");
        assert_eq!(map_atspi_role(atspi::Role::Filler), "AXGroup");
        assert_eq!(map_atspi_role(atspi::Role::ScrollPane), "AXScrollArea");
    }

    #[test]
    fn test_role_mapping_tables() {
        assert_eq!(map_atspi_role(atspi::Role::Table), "AXTable");
        assert_eq!(map_atspi_role(atspi::Role::TableCell), "AXCell");
        assert_eq!(map_atspi_role(atspi::Role::TableRow), "AXRow");
        assert_eq!(map_atspi_role(atspi::Role::List), "AXList");
        assert_eq!(map_atspi_role(atspi::Role::ListItem), "AXRow");
    }

    #[test]
    fn test_role_mapping_menus() {
        assert_eq!(map_atspi_role(atspi::Role::Menu), "AXMenu");
        assert_eq!(map_atspi_role(atspi::Role::MenuItem), "AXMenuItem");
        assert_eq!(map_atspi_role(atspi::Role::MenuBar), "AXMenuBar");
    }

    #[test]
    fn test_role_mapping_misc() {
        assert_eq!(map_atspi_role(atspi::Role::Slider), "AXSlider");
        assert_eq!(map_atspi_role(atspi::Role::ProgressBar), "AXProgressIndicator");
        assert_eq!(map_atspi_role(atspi::Role::Label), "AXStaticText");
        assert_eq!(map_atspi_role(atspi::Role::Link), "AXLink");
        assert_eq!(map_atspi_role(atspi::Role::Image), "AXImage");
        assert_eq!(map_atspi_role(atspi::Role::Heading), "AXHeading");
        assert_eq!(map_atspi_role(atspi::Role::Separator), "AXSplitter");
    }

    #[test]
    fn test_role_mapping_unknown_passthrough() {
        assert!(map_atspi_role(atspi::Role::DesktopFrame).starts_with("atspi:"));
    }

    #[test]
    fn test_action_name_mapping() {
        assert_eq!(map_atspi_action("click"), "AXPress");
        assert_eq!(map_atspi_action("press"), "AXPress");
        assert_eq!(map_atspi_action("activate"), "AXPress");
        assert_eq!(map_atspi_action("toggle"), "AXPress");
    }

    #[test]
    fn test_action_name_mapping_unknown_passthrough() {
        assert_eq!(map_atspi_action("custom-action"), "atspi:custom-action");
    }

    #[test]
    fn test_validate_pid_ownership_self() {
        let pid = std::process::id();
        assert!(validate_pid_ownership(pid).is_ok());
    }

    #[test]
    fn test_validate_pid_ownership_nonexistent() {
        let result = validate_pid_ownership(999999);
        assert!(result.is_err());
    }
}
