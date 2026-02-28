use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeSource {
    Ax,
    Vision { confidence: f32 },
    Merged { confidence: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UiNode {
    pub id: String,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub enabled: bool,
    pub focused: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub actions: Vec<String>,
    pub source: NodeSource,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<UiNode>,
}

/// Role to short prefix for stable IDs.
pub fn role_prefix(role: &str) -> &str {
    match role {
        "window" | "AXWindow" => "w",
        "button" | "AXButton" => "btn",
        "slider" | "AXSlider" => "sld",
        "textField" | "AXTextField" | "textArea" | "AXTextArea" => "txt",
        "knob" => "knb",
        "list" | "AXList" | "AXTable" => "lst",
        "row" | "AXRow" | "cell" | "AXCell" => "itm",
        "toolbar" | "AXToolbar" => "tb",
        "group" | "AXGroup" | "AXSplitGroup" => "pnl",
        "staticText" | "AXStaticText" => "lbl",
        "image" | "AXImage" => "img",
        "menu" | "AXMenu" | "menuBar" | "AXMenuBar" => "mnu",
        "menuItem" | "AXMenuItem" => "mi",
        "tabGroup" | "AXTabGroup" => "tab",
        "checkbox" | "AXCheckBox" => "chk",
        "radioButton" | "AXRadioButton" => "rad",
        "popUpButton" | "AXPopUpButton" | "comboBox" | "AXComboBox" => "pop",
        "scrollArea" | "AXScrollArea" => "scr",
        "progressIndicator" | "AXProgressIndicator" => "prg",
        _ => "el",
    }
}

/// Generate a stable ID from role, title, and sibling index.
/// Uses a simple hash to keep IDs short and deterministic.
pub fn generate_id(role: &str, title: Option<&str>, sibling_index: usize) -> String {
    let prefix = role_prefix(role);
    let hash_input = format!("{}:{}:{}", role, title.unwrap_or(""), sibling_index);
    // Simple FNV-1a hash for speed (no crypto needed)
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in hash_input.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{}_{:04x}", prefix, hash & 0xFFFF)
}

/// Format a tree as compact indented text.
pub fn format_compact(nodes: &[UiNode]) -> String {
    let mut out = String::new();
    for node in nodes {
        format_node(&mut out, node, 0);
    }
    out
}

fn format_node(out: &mut String, node: &UiNode, depth: usize) {
    let indent = "  ".repeat(depth);
    out.push_str(&indent);
    out.push('[');
    out.push_str(&node.role);

    if let Some(ref title) = node.title {
        out.push_str(&format!(" \"{}\"", title));
    }

    out.push_str(&format!(" id={}", node.id));

    if let Some(ref bounds) = node.bounds {
        out.push_str(&format!(" bounds={},{},{},{}",
            bounds.x as i64, bounds.y as i64, bounds.w as i64, bounds.h as i64));
    }

    if let Some(ref value) = node.value {
        match &node.source {
            NodeSource::Vision { .. } => out.push_str(&format!(" value≈{}", value)),
            _ => out.push_str(&format!(" value={}", value)),
        }
    }

    if node.enabled {
        out.push_str(" enabled");
    }
    if node.focused {
        out.push_str(" focused");
    }

    match &node.source {
        NodeSource::Vision { .. } => out.push_str(" source=vision"),
        NodeSource::Merged { .. } => out.push_str(" source=merged"),
        NodeSource::Ax => {} // default, no tag
    }

    out.push_str("]\n");

    for child in &node.children {
        format_node(out, child, depth + 1);
    }
}

/// Format a tree as JSON string.
pub fn format_json(nodes: &[UiNode]) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&serde_json::json!({ "nodes": nodes }))
}

/// Find a node by ID in a tree of UiNodes.
pub fn find_node_by_id(nodes: &[UiNode], target_id: &str) -> Option<UiNode> {
    for node in nodes {
        if node.id == target_id {
            return Some(node.clone());
        }
        if let Some(found) = find_node_by_id(&node.children, target_id) {
            return Some(found);
        }
    }
    None
}

/// Compare two UiNode snapshots. Returns true if any observable field changed.
/// Compares: value, enabled, focused, title. Ignores children and bounds.
pub fn diff_nodes(before: &UiNode, after: &UiNode) -> bool {
    before.value != after.value
        || before.enabled != after.enabled
        || before.focused != after.focused
        || before.title != after.title
}

/// Count nodes recursively.
pub fn count_nodes(nodes: &[UiNode]) -> usize {
    nodes.iter().map(|n| 1 + count_nodes(&n.children)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> Vec<UiNode> {
        vec![UiNode {
            id: "w_0001".to_string(),
            role: "window".to_string(),
            title: Some("Test App".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![
                UiNode {
                    id: "btn_a1b2".to_string(),
                    role: "button".to_string(),
                    title: Some("Play".to_string()),
                    value: None,
                    enabled: true,
                    focused: true,
                    bounds: Some(Rect { x: 10.0, y: 5.0, w: 80.0, h: 30.0 }),
                    actions: vec!["AXPress".to_string()],
                    source: NodeSource::Ax,
                    children: vec![],
                },
                UiNode {
                    id: "knb_c3d4".to_string(),
                    role: "knob".to_string(),
                    title: Some("Filter".to_string()),
                    value: Some("0.6".to_string()),
                    enabled: true,
                    focused: false,
                    bounds: Some(Rect { x: 100.0, y: 50.0, w: 60.0, h: 60.0 }),
                    actions: vec![],
                    source: NodeSource::Vision { confidence: 0.87 },
                    children: vec![],
                },
            ],
        }]
    }

    #[test]
    fn test_compact_format() {
        let tree = sample_tree();
        let text = format_compact(&tree);
        assert!(text.contains("[window \"Test App\" id=w_0001"));
        assert!(text.contains("bounds=0,0,800,600"));
        assert!(text.contains("  [button \"Play\" id=btn_a1b2"));
        assert!(text.contains("enabled focused]"));
        assert!(text.contains("  [knob \"Filter\" id=knb_c3d4"));
        assert!(text.contains("value≈0.6")); // vision value uses ≈
        assert!(text.contains("source=vision"));
    }

    #[test]
    fn test_json_format() {
        let tree = sample_tree();
        let json = format_json(&tree).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["nodes"][0]["role"], "window");
        assert_eq!(parsed["nodes"][0]["children"][0]["title"], "Play");
    }

    #[test]
    fn test_stable_id_generation() {
        let id1 = generate_id("button", Some("Play"), 0);
        let id2 = generate_id("button", Some("Play"), 0);
        let id3 = generate_id("button", Some("Stop"), 0);
        let id4 = generate_id("button", Some("Play"), 1);
        assert_eq!(id1, id2); // same input → same ID
        assert_ne!(id1, id3); // different title → different ID
        assert_ne!(id1, id4); // different index → different ID
        assert!(id1.starts_with("btn_")); // correct prefix
    }

    #[test]
    fn test_role_prefix() {
        assert_eq!(role_prefix("window"), "w");
        assert_eq!(role_prefix("AXButton"), "btn");
        assert_eq!(role_prefix("slider"), "sld");
        assert_eq!(role_prefix("unknownWidget"), "el");
    }

    #[test]
    fn test_count_nodes() {
        let tree = sample_tree();
        assert_eq!(count_nodes(&tree), 3); // window + button + knob
    }

    #[test]
    fn test_diff_nodes_detects_value_change() {
        let before = UiNode {
            id: "sld_1234".to_string(),
            role: "AXSlider".to_string(),
            title: Some("Volume".to_string()),
            value: Some("0.5".to_string()),
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 10.0, y: 20.0, w: 200.0, h: 30.0 }),
            actions: vec!["AXIncrement".to_string()],
            source: NodeSource::Ax,
            children: vec![],
        };
        let mut after = before.clone();
        after.value = Some("0.8".to_string());
        assert!(diff_nodes(&before, &after));
    }

    #[test]
    fn test_diff_nodes_no_change() {
        let node = UiNode {
            id: "btn_a1b2".to_string(),
            role: "AXButton".to_string(),
            title: Some("Play".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 10.0, y: 5.0, w: 80.0, h: 30.0 }),
            actions: vec!["AXPress".to_string()],
            source: NodeSource::Ax,
            children: vec![],
        };
        assert!(!diff_nodes(&node, &node));
    }

    #[test]
    fn test_diff_nodes_detects_focus_change() {
        let before = UiNode {
            id: "txt_5678".to_string(),
            role: "AXTextField".to_string(),
            title: Some("Name".to_string()),
            value: Some("hello".to_string()),
            enabled: true,
            focused: false,
            bounds: None,
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        };
        let mut after = before.clone();
        after.focused = true;
        assert!(diff_nodes(&before, &after));
    }

    #[test]
    fn test_diff_nodes_detects_enabled_change() {
        let before = UiNode {
            id: "btn_1234".to_string(),
            role: "AXButton".to_string(),
            title: None,
            value: None,
            enabled: true,
            focused: false,
            bounds: None,
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        };
        let mut after = before.clone();
        after.enabled = false;
        assert!(diff_nodes(&before, &after));
    }

    #[test]
    fn test_diff_nodes_detects_title_change() {
        let before = UiNode {
            id: "btn_1234".to_string(),
            role: "AXButton".to_string(),
            title: Some("Play".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: None,
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        };
        let mut after = before.clone();
        after.title = Some("Pause".to_string());
        assert!(diff_nodes(&before, &after));
    }

    #[test]
    fn test_find_node_by_id_root() {
        let tree = sample_tree();
        let found = find_node_by_id(&tree, "w_0001");
        assert!(found.is_some());
        assert_eq!(found.unwrap().role, "window");
    }

    #[test]
    fn test_find_node_by_id_nested() {
        let tree = sample_tree();
        let found = find_node_by_id(&tree, "btn_a1b2");
        assert!(found.is_some());
        assert_eq!(found.unwrap().title.unwrap(), "Play");
    }

    #[test]
    fn test_find_node_by_id_deep_child() {
        let tree = sample_tree();
        let found = find_node_by_id(&tree, "knb_c3d4");
        assert!(found.is_some());
        assert_eq!(found.unwrap().role, "knob");
    }

    #[test]
    fn test_find_node_by_id_not_found() {
        let tree = sample_tree();
        assert!(find_node_by_id(&tree, "nonexistent").is_none());
    }
}
