//! Linux accessibility via AT-SPI2 (D-Bus).

use crate::ui::tree::{generate_id, NodeSource, Rect, UiNode};
use crate::Result;
use atspi::proxy::accessible::ObjectRefExt;
use std::time::Duration;

const MAX_AX_DEPTH: usize = 50;
const MAX_AX_NODES: usize = 10_000;
const DBUS_TIMEOUT: Duration = Duration::from_secs(25);

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
pub(crate) fn map_atspi_action(action: &str) -> String {
    match action {
        "click" | "press" | "activate" | "toggle" => "AXPress".into(),
        "expand or contract" => "AXPress".into(),
        other => format!("atspi:{}", other),
    }
}

/// Classify a D-Bus/zbus error as a peer disconnect (app exited mid-query).
fn is_peer_disconnected(err: &dyn std::fmt::Display) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("peer disconnected")
        || msg.contains("connection closed")
        || msg.contains("name has no owner")
        || msg.contains("service unknown")
        || msg.contains("broken pipe")
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

/// Connect to the AT-SPI2 bus.
pub(crate) async fn connect() -> Result<atspi::AccessibilityConnection> {
    atspi::AccessibilityConnection::new().await.map_err(|e| {
        crate::Error::UiNotAvailable(format!(
            "AT-SPI2 accessibility bus not available: {}. \
             Enable accessibility: gsettings set org.gnome.desktop.interface toolkit-accessibility true",
            e
        ))
    })
}

/// Query the AT-SPI2 accessibility tree for a given PID.
/// Returns top-level window nodes (excluding MenuBar).
pub async fn query_ax_tree(pid: u32) -> Result<Vec<UiNode>> {
    validate_pid_ownership(pid)?;

    match tokio::time::timeout(DBUS_TIMEOUT, query_ax_tree_inner(pid)).await {
        Ok(result) => result,
        Err(_) => Err(crate::Error::UiQueryFailed(format!(
            "AT-SPI2 query timed out for PID {}. The application may be unresponsive.",
            pid
        ))),
    }
}

async fn query_ax_tree_inner(pid: u32) -> Result<Vec<UiNode>> {
    let connection = connect().await?;
    let conn = connection.connection();

    // Get registry root and enumerate applications
    let root = connection
        .root_accessible_on_registry()
        .await
        .map_err(|e| {
            if is_peer_disconnected(&e) {
                return crate::Error::UiQueryFailed("Process exited during query".into());
            }
            crate::Error::UiQueryFailed(format!("Failed to get AT-SPI2 registry root: {}", e))
        })?;

    let app_refs = root.get_children().await.map_err(|e| {
        if is_peer_disconnected(&e) {
            return crate::Error::UiQueryFailed("Process exited during query".into());
        }
        crate::Error::UiQueryFailed(format!("Failed to enumerate AT-SPI2 applications: {}", e))
    })?;

    // Create a D-Bus proxy to resolve PIDs from bus names
    let dbus_proxy = atspi::zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| crate::Error::Internal(format!("D-Bus proxy creation failed: {}", e)))?;

    // Find the application accessible matching the target PID
    let mut target_proxy = None;
    for app_ref in &app_refs {
        if app_ref.is_null() {
            continue;
        }
        let bus_name = match app_ref.name_as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let Ok(bus) = atspi::zbus::names::BusName::try_from(bus_name.as_str()) else {
            continue;
        };
        match dbus_proxy.get_connection_unix_process_id(bus).await {
            Ok(app_pid) if app_pid == pid => {
                if let Ok(proxy) = app_ref.as_accessible_proxy(conn).await {
                    target_proxy = Some(proxy);
                    break;
                }
            }
            _ => continue,
        }
    }

    let app = target_proxy.ok_or_else(|| {
        crate::Error::UiQueryFailed(format!(
            "No AT-SPI2 accessible found for PID {}. \
             The application may not support accessibility (GTK, Qt, Electron apps do). \
             For Electron apps, launch with --force-renderer-accessibility.",
            pid
        ))
    })?;

    // Walk the tree from the application root
    let mut node_count = 0;
    let child_refs = app.get_children().await.map_err(|e| {
        if is_peer_disconnected(&e) {
            return crate::Error::UiQueryFailed("Process exited during query".into());
        }
        crate::Error::UiQueryFailed(format!("Failed to read application children: {}", e))
    })?;

    let mut windows = Vec::new();
    for (i, child_ref) in child_refs.iter().enumerate() {
        if child_ref.is_null() {
            continue;
        }
        if let Ok(child_proxy) = child_ref.as_accessible_proxy(conn).await {
            if let Some(node) =
                Box::pin(build_node(conn, &child_proxy, i, 0, &mut node_count)).await
            {
                // Skip MenuBar nodes for consistency with macOS
                if node.role != "AXMenuBar" {
                    windows.push(node);
                }
            }
        }
    }

    Ok(windows)
}

/// Recursively build a UiNode from an AT-SPI2 accessible.
async fn build_node(
    conn: &atspi::zbus::Connection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
    sibling_index: usize,
    depth: usize,
    node_count: &mut usize,
) -> Option<UiNode> {
    if depth > MAX_AX_DEPTH || *node_count >= MAX_AX_NODES {
        return None;
    }
    *node_count += 1;

    // Get role
    let role_enum = accessible.get_role().await.ok()?;
    let role = map_atspi_role(role_enum);

    // Get name/title
    let title = match accessible.name().await {
        Ok(n) if !n.is_empty() => Some(n),
        _ => accessible
            .description()
            .await
            .ok()
            .filter(|s| !s.is_empty()),
    };

    // Get interfaces
    let interfaces = accessible.get_interfaces().await.unwrap_or_default();

    // Get bounds via Component interface
    let bounds = if interfaces.contains(atspi::Interface::Component) {
        get_component_bounds(conn, accessible).await
    } else {
        None
    };

    // Get actions via Action interface
    let actions = if interfaces.contains(atspi::Interface::Action) {
        get_action_names(conn, accessible).await
    } else {
        vec![]
    };

    // Get value via Value interface
    let value = if interfaces.contains(atspi::Interface::Value) {
        get_value(conn, accessible).await
    } else {
        None
    };

    // Get state for enabled/focused
    let states = accessible.get_state().await.unwrap_or_default();
    let enabled =
        states.contains(atspi::State::Enabled) || states.contains(atspi::State::Sensitive);
    let focused = states.contains(atspi::State::Focused);

    let id = generate_id(&role, title.as_deref(), sibling_index);

    // Recurse into children
    let child_refs = accessible.get_children().await.unwrap_or_default();
    let mut children = Vec::new();
    for (i, child_ref) in child_refs.iter().enumerate() {
        if child_ref.is_null() {
            continue;
        }
        if let Ok(child_proxy) = child_ref.as_accessible_proxy(conn).await {
            if let Some(child_node) =
                Box::pin(build_node(conn, &child_proxy, i, depth + 1, node_count)).await
            {
                children.push(child_node);
            }
        }
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

/// Get component bounds via ComponentProxy.
async fn get_component_bounds(
    conn: &atspi::zbus::Connection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
) -> Option<Rect> {
    let dest = accessible.inner().destination().to_owned();
    let path = accessible.inner().path().to_owned();

    let comp = atspi::proxy::component::ComponentProxy::builder(conn)
        .destination(dest)
        .ok()?
        .path(path)
        .ok()?
        .cache_properties(atspi::zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()?;

    let (x, y, w, h) = comp.get_extents(atspi::CoordType::Screen).await.ok()?;
    if w > 0 && h > 0 {
        Some(Rect {
            x: x as f64,
            y: y as f64,
            w: w as f64,
            h: h as f64,
        })
    } else {
        None
    }
}

/// Get action names via ActionProxy.
async fn get_action_names(
    conn: &atspi::zbus::Connection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
) -> Vec<String> {
    let dest = accessible.inner().destination().to_owned();
    let path = accessible.inner().path().to_owned();

    let action_proxy = match atspi::proxy::action::ActionProxy::builder(conn)
        .destination(dest)
        .ok()
        .and_then(|b| b.path(path).ok())
    {
        Some(b) => match b
            .cache_properties(atspi::zbus::proxy::CacheProperties::No)
            .build()
            .await
        {
            Ok(p) => p,
            Err(_) => return vec![],
        },
        None => return vec![],
    };

    let n = action_proxy.nactions().await.unwrap_or(0);
    let mut mapped = Vec::new();
    for i in 0..n {
        if let Ok(name) = action_proxy.get_name(i).await {
            mapped.push(map_atspi_action(&name));
        }
    }
    mapped
}

/// Get value via ValueProxy.
async fn get_value(
    conn: &atspi::zbus::Connection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
) -> Option<String> {
    let dest = accessible.inner().destination().to_owned();
    let path = accessible.inner().path().to_owned();

    let val_proxy = atspi::proxy::value::ValueProxy::builder(conn)
        .destination(dest)
        .ok()?
        .path(path)
        .ok()?
        .cache_properties(atspi::zbus::proxy::CacheProperties::No)
        .build()
        .await
        .ok()?;

    val_proxy
        .current_value()
        .await
        .ok()
        .map(|v| format!("{}", v))
}

/// Result of finding an element — carries the D-Bus destination and path
/// needed to create action/value proxies.
pub struct FindResult {
    pub destination: String,
    pub path: String,
    pub node: UiNode,
    pub interfaces: atspi::InterfaceSet,
}

/// Find an AT-SPI2 accessible by node ID. Returns the accessible's
/// destination/path and node for action execution. Used by input_linux.rs.
pub async fn find_element_by_id(pid: u32, target_id: &str) -> Result<Option<FindResult>> {
    validate_pid_ownership(pid)?;

    match tokio::time::timeout(DBUS_TIMEOUT, find_element_by_id_inner(pid, target_id)).await {
        Ok(result) => result,
        Err(_) => Err(crate::Error::UiQueryFailed(format!(
            "AT-SPI2 query timed out for PID {}. The application may be unresponsive.",
            pid
        ))),
    }
}

async fn find_element_by_id_inner(pid: u32, target_id: &str) -> Result<Option<FindResult>> {
    let connection = connect().await?;
    let conn = connection.connection();

    let root = connection
        .root_accessible_on_registry()
        .await
        .map_err(|e| {
            if is_peer_disconnected(&e) {
                return crate::Error::UiQueryFailed("Process exited during query".into());
            }
            crate::Error::UiQueryFailed(format!("Failed to get AT-SPI2 registry root: {}", e))
        })?;

    let app_refs = root.get_children().await.map_err(|e| {
        if is_peer_disconnected(&e) {
            return crate::Error::UiQueryFailed("Process exited during query".into());
        }
        crate::Error::UiQueryFailed(format!("Failed to enumerate apps: {}", e))
    })?;

    let dbus_proxy = atspi::zbus::fdo::DBusProxy::new(conn)
        .await
        .map_err(|e| crate::Error::Internal(format!("D-Bus proxy: {}", e)))?;

    for app_ref in &app_refs {
        if app_ref.is_null() {
            continue;
        }
        let bus_name = match app_ref.name_as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        let Ok(bus) = atspi::zbus::names::BusName::try_from(bus_name.as_str()) else {
            continue;
        };
        if let Ok(app_pid) = dbus_proxy.get_connection_unix_process_id(bus).await {
            if app_pid == pid {
                if let Ok(app_proxy) = app_ref.as_accessible_proxy(conn).await {
                    let child_refs = app_proxy.get_children().await.unwrap_or_default();
                    for (i, child_ref) in child_refs.iter().enumerate() {
                        if child_ref.is_null() {
                            continue;
                        }
                        if let Ok(child_proxy) = child_ref.as_accessible_proxy(conn).await {
                            if let Some(result) =
                                Box::pin(find_in_subtree(conn, &child_proxy, target_id, i, 0)).await
                            {
                                return Ok(Some(result));
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

async fn find_in_subtree(
    conn: &atspi::zbus::Connection,
    accessible: &atspi::proxy::accessible::AccessibleProxy<'_>,
    target_id: &str,
    sibling_index: usize,
    depth: usize,
) -> Option<FindResult> {
    if depth > MAX_AX_DEPTH {
        return None;
    }

    let role_enum = accessible.get_role().await.ok()?;
    let role = map_atspi_role(role_enum);
    let title = accessible.name().await.ok().filter(|s| !s.is_empty());
    let id = generate_id(&role, title.as_deref(), sibling_index);

    if id == target_id {
        let interfaces = accessible.get_interfaces().await.unwrap_or_default();
        let states = accessible.get_state().await.unwrap_or_default();
        let bounds = if interfaces.contains(atspi::Interface::Component) {
            get_component_bounds(conn, accessible).await
        } else {
            None
        };
        let value = if interfaces.contains(atspi::Interface::Value) {
            get_value(conn, accessible).await
        } else {
            None
        };

        return Some(FindResult {
            destination: accessible.inner().destination().to_string(),
            path: accessible.inner().path().to_string(),
            node: UiNode {
                id,
                role,
                title,
                value,
                enabled: states.contains(atspi::State::Enabled)
                    || states.contains(atspi::State::Sensitive),
                focused: states.contains(atspi::State::Focused),
                bounds,
                actions: vec![],
                source: NodeSource::Ax,
                children: vec![],
            },
            interfaces,
        });
    }

    // Recurse
    let child_refs = accessible.get_children().await.unwrap_or_default();
    for (i, child_ref) in child_refs.iter().enumerate() {
        if child_ref.is_null() {
            continue;
        }
        if let Ok(child_proxy) = child_ref.as_accessible_proxy(conn).await {
            if let Some(result) =
                Box::pin(find_in_subtree(conn, &child_proxy, target_id, i, depth + 1)).await
            {
                return Some(result);
            }
        }
    }

    None
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
        assert_eq!(
            map_atspi_role(atspi::Role::PasswordText),
            "AXSecureTextField"
        );
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
        assert_eq!(
            map_atspi_role(atspi::Role::ProgressBar),
            "AXProgressIndicator"
        );
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

    #[test]
    fn test_validate_pid_ownership_pid1_non_root() {
        // PID 1 (init) is owned by root — non-root users should be rejected
        let my_euid = unsafe { libc::geteuid() };
        let result = validate_pid_ownership(1);
        if my_euid == 0 {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
            let err_msg = format!("{}", result.unwrap_err());
            assert!(err_msg.contains("Permission denied") || err_msg.contains("another user"));
        }
    }

    #[test]
    fn test_is_peer_disconnected_detection() {
        assert!(is_peer_disconnected(&"peer disconnected"));
        assert!(is_peer_disconnected(&"Connection closed by peer"));
        assert!(is_peer_disconnected(
            &"org.freedesktop.DBus.Error.ServiceUnknown: name has no owner"
        ));
        assert!(is_peer_disconnected(&"Broken pipe"));
        assert!(!is_peer_disconnected(&"some other error"));
        assert!(!is_peer_disconnected(&"timeout expired"));
    }
}
