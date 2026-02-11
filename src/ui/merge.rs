//! Merge AX tree nodes with vision-detected elements via IoU matching.

use crate::ui::tree::{generate_id, NodeSource, Rect, UiNode};
use crate::ui::vision::{VisionBounds, VisionElement};

/// Compute Intersection over Union for two rectangles.
pub fn iou(a: &Rect, b: &Rect) -> f64 {
    let x1 = a.x.max(b.x);
    let y1 = a.y.max(b.y);
    let x2 = (a.x + a.w).min(b.x + b.w);
    let y2 = (a.y + a.h).min(b.y + b.h);

    if x2 <= x1 || y2 <= y1 {
        return 0.0;
    }

    let intersection = (x2 - x1) * (y2 - y1);
    let area_a = a.w * a.h;
    let area_b = b.w * b.h;
    let union = area_a + area_b - intersection;

    if union <= 0.0 { 0.0 } else { intersection / union }
}

/// Convert VisionBounds to Rect.
fn vision_bounds_to_rect(b: &VisionBounds) -> Rect {
    Rect {
        x: b.x as f64,
        y: b.y as f64,
        w: b.w as f64,
        h: b.h as f64,
    }
}

/// Merge vision-detected elements into an AX tree.
///
/// 1. For each vision element, find best IoU match among AX leaf nodes.
/// 2. IoU >= threshold → merge (AX node gets source=Merged, vision confidence).
/// 3. IoU < threshold → add as vision-only node under nearest containing AX parent.
pub fn merge_vision_into_tree(
    ax_nodes: &mut Vec<UiNode>,
    vision_elements: &[VisionElement],
    iou_threshold: f64,
) -> (usize, usize) {
    let mut merged_count = 0;
    let mut added_count = 0;

    for ve in vision_elements {
        let vr = vision_bounds_to_rect(&ve.bounds);
        let mut best_match: Option<(f64, Vec<usize>)> = None;

        // Find best IoU match among leaf nodes
        find_best_match(ax_nodes, &vr, &mut best_match, &mut vec![]);

        if let Some((best_iou, path)) = best_match {
            if best_iou >= iou_threshold {
                // Merge: update existing node
                if let Some(node) = get_node_mut(ax_nodes, &path) {
                    node.source = NodeSource::Merged { confidence: ve.confidence };
                    if node.value.is_none() {
                        // Use vision-estimated value if AX didn't provide one
                        node.value = Some(ve.description.clone());
                    }
                }
                merged_count += 1;
                continue;
            }
        }

        // No match — add as vision-only node
        let vision_node = UiNode {
            id: generate_id(&ve.label, Some(&ve.description), added_count),
            role: ve.label.clone(),
            title: if ve.description.is_empty() { None } else { Some(ve.description.clone()) },
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(vr),
            actions: vec![],
            source: NodeSource::Vision { confidence: ve.confidence },
            children: vec![],
        };

        // Find nearest containing parent and insert
        let center_x = ve.bounds.x as f64 + ve.bounds.w as f64 / 2.0;
        let center_y = ve.bounds.y as f64 + ve.bounds.h as f64 / 2.0;
        if !insert_into_container(ax_nodes, vision_node.clone(), center_x, center_y) {
            // No container found — add to root level
            ax_nodes.push(vision_node);
        }
        added_count += 1;
    }

    (merged_count, added_count)
}

fn find_best_match(
    nodes: &[UiNode],
    target: &Rect,
    best: &mut Option<(f64, Vec<usize>)>,
    current_path: &mut Vec<usize>,
) {
    for (i, node) in nodes.iter().enumerate() {
        current_path.push(i);

        if node.children.is_empty() {
            // Leaf node — compute IoU
            if let Some(ref bounds) = node.bounds {
                let score = iou(bounds, target);
                if best.is_none() || score > best.as_ref().unwrap().0 {
                    *best = Some((score, current_path.clone()));
                }
            }
        } else {
            find_best_match(&node.children, target, best, current_path);
        }

        current_path.pop();
    }
}

// CORR-3: Use safe indexing to prevent panics on invalid paths
fn get_node_mut<'a>(nodes: &'a mut [UiNode], path: &[usize]) -> Option<&'a mut UiNode> {
    if path.is_empty() {
        return None;
    }
    let mut current = nodes.get_mut(path[0])?;
    for &idx in &path[1..] {
        current = current.children.get_mut(idx)?;
    }
    Some(current)
}

fn insert_into_container(nodes: &mut Vec<UiNode>, node: UiNode, cx: f64, cy: f64) -> bool {
    // Find deepest container whose bounds contain the center point
    for parent in nodes.iter_mut() {
        if let Some(ref bounds) = parent.bounds {
            if cx >= bounds.x && cx <= bounds.x + bounds.w
                && cy >= bounds.y && cy <= bounds.y + bounds.h
            {
                // Try to insert deeper first
                if !insert_into_container(&mut parent.children, node.clone(), cx, cy) {
                    parent.children.push(node);
                }
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iou_identical() {
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        assert!((iou(&a, &a) - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_iou_no_overlap() {
        let a = Rect { x: 0.0, y: 0.0, w: 50.0, h: 50.0 };
        let b = Rect { x: 100.0, y: 100.0, w: 50.0, h: 50.0 };
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn test_iou_partial() {
        let a = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let b = Rect { x: 50.0, y: 50.0, w: 100.0, h: 100.0 };
        // Intersection: 50x50 = 2500, Union: 10000 + 10000 - 2500 = 17500
        let expected = 2500.0 / 17500.0;
        assert!((iou(&a, &b) - expected).abs() < 0.001);
    }

    #[test]
    fn test_iou_contained() {
        let outer = Rect { x: 0.0, y: 0.0, w: 100.0, h: 100.0 };
        let inner = Rect { x: 25.0, y: 25.0, w: 50.0, h: 50.0 };
        // Intersection: 50x50 = 2500, Union: 10000 + 2500 - 2500 = 10000
        assert!((iou(&outer, &inner) - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_merge_matched_node() {
        let mut tree = vec![UiNode {
            id: "btn_1".to_string(),
            role: "button".to_string(),
            title: Some("Play".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 10.0, y: 10.0, w: 80.0, h: 30.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Play button".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 12, y: 8, w: 78, h: 32 }, // High IoU with ax node
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 1);
        assert_eq!(added, 0);
        assert!(matches!(tree[0].source, NodeSource::Merged { .. }));
    }

    #[test]
    fn test_merge_unmatched_added() {
        let mut tree = vec![UiNode {
            id: "w_1".to_string(),
            role: "window".to_string(),
            title: Some("Test".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "knob".to_string(),
            description: "Filter Cutoff".to_string(),
            confidence: 0.85,
            bounds: VisionBounds { x: 100, y: 100, w: 60, h: 60 }, // No AX match
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 0);
        assert_eq!(added, 1);
        // Vision node should be added as child of window (spatial containment)
        assert_eq!(tree[0].children.len(), 1);
        assert!(matches!(tree[0].children[0].source, NodeSource::Vision { .. }));
    }

    // TEST-3: Edge case tests for merge algorithm

    #[test]
    fn test_merge_zero_area_bounds() {
        // Test IoU with zero-area rectangles
        let zero_w = Rect { x: 10.0, y: 10.0, w: 0.0, h: 50.0 };
        let zero_h = Rect { x: 10.0, y: 10.0, w: 50.0, h: 0.0 };
        let normal = Rect { x: 10.0, y: 10.0, w: 50.0, h: 50.0 };

        assert_eq!(iou(&zero_w, &normal), 0.0, "Zero width should have 0 IoU");
        assert_eq!(iou(&zero_h, &normal), 0.0, "Zero height should have 0 IoU");
        assert_eq!(iou(&zero_w, &zero_h), 0.0, "Two zero-area should have 0 IoU");

        // Merge should not crash with zero-area vision elements
        let mut tree = vec![UiNode {
            id: "btn_1".to_string(),
            role: "button".to_string(),
            title: Some("Test".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(normal),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Zero width".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 10, y: 10, w: 0, h: 50 },
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        // Should not merge (IoU = 0), should add as new node
        assert_eq!(merged, 0);
        assert_eq!(added, 1);
    }

    #[test]
    fn test_merge_negative_coordinates() {
        // Some window systems use negative coords for off-screen windows
        let neg = Rect { x: -100.0, y: -50.0, w: 200.0, h: 100.0 };
        let pos = Rect { x: 50.0, y: 25.0, w: 100.0, h: 50.0 };

        // Should have partial overlap
        let score = iou(&neg, &pos);
        assert!(score > 0.0 && score < 1.0, "Should have partial overlap: {}", score);
    }

    #[test]
    fn test_merge_exact_overlap() {
        // Two nodes with exactly the same bounds (IoU = 1.0)
        let mut tree = vec![UiNode {
            id: "btn_1".to_string(),
            role: "button".to_string(),
            title: Some("AX Button".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 100.0, y: 100.0, w: 80.0, h: 40.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Vision Button".to_string(),
            confidence: 0.95,
            bounds: VisionBounds { x: 100, y: 100, w: 80, h: 40 }, // Exact match
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 1, "Exact overlap should merge");
        assert_eq!(added, 0);
        assert!(matches!(tree[0].source, NodeSource::Merged { confidence } if (confidence - 0.95).abs() < 0.01));
    }

    #[test]
    fn test_merge_deeply_nested_tree() {
        // Create a deeply nested tree: window > panel > container > button
        let mut tree = vec![UiNode {
            id: "window".to_string(),
            role: "window".to_string(),
            title: Some("App".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![UiNode {
                id: "panel".to_string(),
                role: "panel".to_string(),
                title: None,
                value: None,
                enabled: true,
                focused: false,
                bounds: Some(Rect { x: 10.0, y: 10.0, w: 780.0, h: 580.0 }),
                actions: vec![],
                source: NodeSource::Ax,
                children: vec![UiNode {
                    id: "container".to_string(),
                    role: "group".to_string(),
                    title: None,
                    value: None,
                    enabled: true,
                    focused: false,
                    bounds: Some(Rect { x: 100.0, y: 100.0, w: 200.0, h: 100.0 }),
                    actions: vec![],
                    source: NodeSource::Ax,
                    children: vec![UiNode {
                        id: "button".to_string(),
                        role: "button".to_string(),
                        title: Some("Click Me".to_string()),
                        value: None,
                        enabled: true,
                        focused: false,
                        bounds: Some(Rect { x: 120.0, y: 130.0, w: 80.0, h: 30.0 }),
                        actions: vec![],
                        source: NodeSource::Ax,
                        children: vec![],
                    }],
                }],
            }],
        }];

        // Vision element matching the deep button
        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Click Me".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 121, y: 131, w: 78, h: 28 }, // High IoU with nested button
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 1, "Should find and merge deeply nested button");
        assert_eq!(added, 0);

        // Navigate to the deep button and verify merge
        let button = &tree[0].children[0].children[0].children[0];
        assert!(matches!(button.source, NodeSource::Merged { .. }));
    }

    #[test]
    fn test_merge_out_of_bounds_vision() {
        // Vision element completely outside any AX node bounds
        let mut tree = vec![UiNode {
            id: "window".to_string(),
            role: "window".to_string(),
            title: Some("App".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 400.0, h: 300.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Off Screen".to_string(),
            confidence: 0.8,
            bounds: VisionBounds { x: 1000, y: 1000, w: 100, h: 50 }, // Way outside
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 0, "Out of bounds should not merge");
        assert_eq!(added, 1, "Should add as root-level vision node");

        // Should be added at root level (not inside window)
        assert_eq!(tree.len(), 2, "Should have window + vision node at root");
        assert!(matches!(tree[1].source, NodeSource::Vision { .. }));
    }

    #[test]
    fn test_merge_tie_breaking() {
        // Two AX buttons with identical IoU to vision element
        let mut tree = vec![
            UiNode {
                id: "btn_1".to_string(),
                role: "button".to_string(),
                title: Some("Button 1".to_string()),
                value: None,
                enabled: true,
                focused: false,
                bounds: Some(Rect { x: 10.0, y: 10.0, w: 80.0, h: 30.0 }),
                actions: vec![],
                source: NodeSource::Ax,
                children: vec![],
            },
            UiNode {
                id: "btn_2".to_string(),
                role: "button".to_string(),
                title: Some("Button 2".to_string()),
                value: None,
                enabled: true,
                focused: false,
                bounds: Some(Rect { x: 10.0, y: 10.0, w: 80.0, h: 30.0 }), // Same bounds
                actions: vec![],
                source: NodeSource::Ax,
                children: vec![],
            },
        ];

        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Ambiguous".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 10, y: 10, w: 80, h: 30 },
        }];

        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);

        // Should merge with one of them (first found wins)
        assert_eq!(merged, 1, "Should merge with one button");
        assert_eq!(added, 0);

        // Exactly one should be merged
        let merged_count = tree.iter()
            .filter(|n| matches!(n.source, NodeSource::Merged { .. }))
            .count();
        assert_eq!(merged_count, 1, "Exactly one button should be merged");
    }
}
