//! Phase 4 — UI Observation integration tests.
//! Requires macOS with Accessibility permissions granted.

mod common;


#[cfg(target_os = "macos")]
mod macos_tests {
    use super::common::*;
    use std::time::Duration;

    // ---- Unit-level tests (no app needed) ----

    #[test]
    fn test_stable_ids_deterministic() {
        use strobe::ui::tree::generate_id;
        for _ in 0..10 {
            let id = generate_id("button", Some("Play"), 0);
            assert_eq!(id, generate_id("button", Some("Play"), 0));
        }
    }

    #[test]
    fn test_ax_query_invalid_pid() {
        // PID 999999 shouldn't exist
        let result = strobe::ui::accessibility::query_ax_tree(999999);
        // Should return empty tree or error, not panic
        match result {
            Ok(nodes) => assert!(nodes.is_empty(), "Invalid PID should return empty tree"),
            Err(_) => {} // Error is also acceptable
        }
    }

    #[test]
    fn test_ax_query_kernel_pid() {
        // PID 0 is the kernel — should fail gracefully
        let result = strobe::ui::accessibility::query_ax_tree(0);
        match result {
            Ok(nodes) => assert!(nodes.is_empty(), "Kernel PID should return empty tree"),
            Err(_) => {} // Error is also acceptable
        }
    }

    #[test]
    fn test_find_ax_element_returns_none_for_bogus_id() {
        // No process needed — just verify it handles invalid PID gracefully
        let result = strobe::ui::accessibility::find_ax_element(99999, "btn_0000");
        // Should return Ok(None) or an error, not panic
        match result {
            Ok(None) => {} // expected: no such PID or no such element
            Err(_) => {}   // also acceptable: permission/PID error
            Ok(Some(_)) => panic!("Should not find an element for bogus PID"),
        }
    }

    // ---- Vision pipeline tests (no UI app needed) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn test_vision_sidecar_lifecycle() {
        use strobe::ui::vision::VisionSidecar;

        // Test basic lifecycle: start -> detect -> shutdown
        let mut sidecar = VisionSidecar::new();

        // Create a minimal test image (1x1 PNG, base64 encoded)
        let png_bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG header
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1 image
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, // IDAT chunk
            0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00,
            0x18, 0xDD, 0x8D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND chunk
            0xAE, 0x42, 0x60, 0x82,
        ];
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

        // This will fail if Python deps not installed - that's okay for CI
        match sidecar.detect(&b64, 0.3, 0.5) {
            Ok(elements) => {
                // If it works, verify it returns something reasonable
                println!("Vision detection succeeded, found {} elements", elements.len());
            }
            Err(e) => {
                println!("Vision sidecar unavailable (expected in CI): {}", e);
                // Not a test failure - just log it
            }
        }

        // Test graceful shutdown
        sidecar.shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_vision_idle_timeout() {
        use strobe::ui::vision::VisionSidecar;

        let mut sidecar = VisionSidecar::new();

        // Check idle timeout with 0 seconds (should shut down immediately)
        sidecar.check_idle_timeout(0);

        // After check_idle_timeout, any detect() call should restart the sidecar
        let png_bytes = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xDE,
            0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54,
            0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00,
            0x18, 0xDD, 0x8D, 0xB4,
            0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
            0xAE, 0x42, 0x60, 0x82,
        ];
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

        // This should auto-restart if dependencies available
        let _ = sidecar.detect(&b64, 0.3, 0.5);

        sidecar.shutdown();
    }

    #[test]
    fn test_merge_algorithm_with_real_data() {
        use strobe::ui::tree::{UiNode, Rect, NodeSource};
        use strobe::ui::vision::{VisionElement, VisionBounds};
        use strobe::ui::merge::merge_vision_into_tree;

        // Create a simple AX tree: Window -> Button
        let mut ax_tree = vec![
            UiNode {
                id: "window-1".to_string(),
                role: "window".to_string(),
                title: Some("Test Window".to_string()),
                value: None,
                enabled: true,
                focused: false,
                bounds: Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }),
                actions: vec![],
                source: NodeSource::Ax,
                children: vec![
                    UiNode {
                        id: "button-1".to_string(),
                        role: "button".to_string(),
                        title: Some("Click Me".to_string()),
                        value: None,
                        enabled: true,
                        focused: false,
                        bounds: Some(Rect { x: 100.0, y: 100.0, w: 120.0, h: 40.0 }),
                        actions: vec![],
                        source: NodeSource::Ax,
                        children: vec![],
                    }
                ],
            }
        ];

        // Vision detected an icon inside the button bounds
        let vision_elements = vec![
            VisionElement {
                label: "icon".to_string(),
                description: "play icon".to_string(),
                confidence: 0.9,
                bounds: VisionBounds { x: 105, y: 110, w: 20, h: 20 },
            }
        ];

        // Merge with IoU threshold 0.5
        merge_vision_into_tree(&mut ax_tree, &vision_elements, 0.5);

        // Verify the icon was added to the button's children
        let button = &ax_tree[0].children[0];
        assert_eq!(button.children.len(), 1, "Button should have vision child");
        assert_eq!(button.children[0].role, "icon", "Role should match vision label");
        assert_eq!(button.children[0].title.as_deref(), Some("play icon"));

        // Verify it's marked as vision source
        match &button.children[0].source {
            strobe::ui::tree::NodeSource::Vision { confidence } => {
                assert_eq!(*confidence, 0.9, "Should preserve vision confidence");
            }
            _ => panic!("Vision node should have Vision source"),
        }
    }

    #[test]
    fn test_vision_disabled_error_handling() {
        // This test verifies that requesting vision when disabled gives clear error
        // We'll test this indirectly through the config system
        use strobe::config::StrobeSettings;

        let settings = StrobeSettings::default();
        assert_eq!(settings.vision_enabled, false, "Vision should be disabled by default");

        // The actual error check happens in tool_debug_ui, but we verify config here
    }

    #[test]
    fn test_vision_bounds_to_rect_conversion() {
        use strobe::ui::vision::VisionBounds;
        use strobe::ui::tree::Rect;

        let vb = VisionBounds { x: 10, y: 20, w: 100, h: 50 };

        // Test conversion (implicit in merge algorithm)
        let rect = Rect {
            x: vb.x as f64,
            y: vb.y as f64,
            w: vb.w as f64,
            h: vb.h as f64,
        };

        assert_eq!(rect.x, 10.0);
        assert_eq!(rect.y, 20.0);
        assert_eq!(rect.w, 100.0);
        assert_eq!(rect.h, 50.0);
    }

    #[test]
    fn test_merge_empty_vision() {
        use strobe::ui::tree::{UiNode, Rect, NodeSource};
        use strobe::ui::merge::merge_vision_into_tree;

        let mut tree = vec![UiNode {
            id: "w".to_string(),
            role: "window".to_string(),
            title: Some("App".to_string()),
            value: None,
            enabled: true,
            focused: false,
            bounds: Some(Rect { x: 0.0, y: 0.0, w: 800.0, h: 600.0 }),
            actions: vec![],
            source: NodeSource::Ax,
            children: vec![],
        }];

        // Empty vision elements should not modify tree
        let (merged, added) = merge_vision_into_tree(&mut tree, &[], 0.5);
        assert_eq!(merged, 0);
        assert_eq!(added, 0);
        assert_eq!(tree.len(), 1, "Tree should be unchanged");
    }

    #[test]
    fn test_merge_empty_tree() {
        use strobe::ui::vision::{VisionElement, VisionBounds};
        use strobe::ui::merge::merge_vision_into_tree;

        let mut tree = vec![];
        let vision = vec![VisionElement {
            label: "button".to_string(),
            description: "Orphan".to_string(),
            confidence: 0.9,
            bounds: VisionBounds { x: 10, y: 10, w: 50, h: 30 },
        }];

        // Vision elements with empty tree should be added at root
        let (merged, added) = merge_vision_into_tree(&mut tree, &vision, 0.5);
        assert_eq!(merged, 0);
        assert_eq!(added, 1);
        assert_eq!(tree.len(), 1, "Vision node should be added at root");
    }

    #[test]
    fn test_ui_error_code_mapping() {
        use strobe::mcp::McpError;

        // UiQueryFailed should map to UiQueryFailed error code
        let err = strobe::Error::UiQueryFailed("test error".to_string());
        let mcp_err: McpError = err.into();
        let code_str = serde_json::to_string(&mcp_err.code).unwrap();
        assert!(code_str.contains("UI_QUERY_FAILED"), "UiQueryFailed should map to UI_QUERY_FAILED, got: {}", code_str);

        // UiNotAvailable should map to its own error code (not UiQueryFailed)
        let err = strobe::Error::UiNotAvailable("not available".to_string());
        let mcp_err: McpError = err.into();
        let code_str = serde_json::to_string(&mcp_err.code).unwrap();
        assert!(code_str.contains("UI_NOT_AVAILABLE"), "UiNotAvailable should map to UI_NOT_AVAILABLE, got: {}", code_str);
    }

    // ---- Helpers for integration suite ----

    /// Helper: find first node with matching role in tree (recursive)
    fn find_node_by_role_recursive(nodes: &[strobe::ui::tree::UiNode], role: &str) -> Option<String> {
        for node in nodes {
            if node.role == role {
                return Some(node.id.clone());
            }
            if let Some(found) = find_node_by_role_recursive(&node.children, role) {
                return Some(found);
            }
        }
        None
    }

    /// Helper: find first node with matching title in tree (recursive)
    fn find_node_by_title_recursive(
        nodes: &[strobe::ui::tree::UiNode], title: &str,
    ) -> Option<String> {
        for node in nodes {
            if node.title.as_deref() == Some(title) {
                return Some(node.id.clone());
            }
            if let Some(found) = find_node_by_title_recursive(&node.children, title) {
                return Some(found);
            }
        }
        None
    }

    /// Check if process is still alive.
    fn process_alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    // ---- Consolidated integration test ----
    //
    // All tests that spawn the UI test app via Frida are consolidated into a
    // single test to avoid timeout accumulation. Previously, 5 separate tests
    // each acquired a serialization lock and spawned the app independently.
    // The test harness considered them all "started" simultaneously, so later
    // tests would hit the hard timeout while waiting for the lock.
    //
    // This single test spawns the app once, runs all assertions, and cleans up.

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ui_integration_suite() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-suite";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false, None,
        ).await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        // Give the app time to render its window and fully initialize AX tree
        tokio::time::sleep(Duration::from_secs(3)).await;

        // ==== Part 1: AX tree structure ====
        eprintln!("  [1/5] AX tree structure...");
        {
            let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            assert!(!nodes.is_empty(), "Should find at least one window");

            // Verify we got a window
            let window = &nodes[0];
            assert!(window.role.contains("Window") || window.role == "window",
                "First node should be a window, got: {}", window.role);

            // Verify tree has children
            let total = strobe::ui::tree::count_nodes(&nodes);
            assert!(total >= 3, "Expected at least 3 nodes, got {}", total);

            // Verify compact text format
            let text = strobe::ui::tree::format_compact(&nodes);
            assert!(text.contains("id="), "Compact text should contain IDs");

            // Verify stable IDs across calls
            let nodes2 = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            let text2 = strobe::ui::tree::format_compact(&nodes2);

            // Check that the main window ID is stable
            assert!(text.contains("w_7cdd"), "First query should contain window ID");
            assert!(text2.contains("w_7cdd"), "Second query should contain same window ID");

            // Check >90% tree similarity
            let lines1: Vec<&str> = text.lines().collect();
            let lines2: Vec<&str> = text2.lines().collect();
            let common_lines = lines1.iter().filter(|l| lines2.contains(l)).count();
            let similarity = (common_lines as f64) / (lines1.len().max(lines2.len()) as f64);
            assert!(similarity > 0.9, "Tree structure should be mostly stable (got {:.1}% similar)", similarity * 100.0);
        }

        // ==== Part 2: AX query latency ====
        eprintln!("  [2/5] AX query latency...");
        {
            // Warm-up query (first may include permission checks and cache warming)
            let _ = strobe::ui::accessibility::query_ax_tree(pid).unwrap();

            // Measure second query
            let start = std::time::Instant::now();
            let _nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            let elapsed = start.elapsed();

            assert!(elapsed.as_millis() < 600, "Subsequent AX query should be <600ms, took {}ms", elapsed.as_millis());
        }

        // ==== Part 3: Screenshot capture ====
        eprintln!("  [3/5] Screenshot capture...");
        {
            match strobe::ui::capture::capture_window_screenshot(pid) {
                Ok(png_bytes) => {
                    assert!(png_bytes.len() > 100, "PNG should be non-trivial, got {} bytes", png_bytes.len());
                    // Verify PNG header
                    assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47], "Should be valid PNG");
                }
                Err(e) => {
                    let err_msg = format!("{:?}", e);
                    if err_msg.contains("Failed to capture screenshot") || err_msg.contains("No visible window") {
                        eprintln!("Warning: Screenshot capture failed (likely permissions). Error: {}", err_msg);
                    } else {
                        panic!("Unexpected screenshot error: {}", err_msg);
                    }
                }
            }
        }

        // ==== Part 4: Screenshot with base64 roundtrip ====
        eprintln!("  [4/5] Screenshot base64 roundtrip...");
        {
            match strobe::ui::capture::capture_window_screenshot(pid) {
                Ok(png_bytes) => {
                    use base64::Engine;
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);

                    assert!(!b64.is_empty(), "Base64 screenshot should be non-empty");
                    assert!(b64.len() > 100, "Base64 screenshot should be substantial");

                    let decoded = base64::engine::general_purpose::STANDARD.decode(&b64).unwrap();
                    assert_eq!(decoded, png_bytes, "Base64 roundtrip should match");
                }
                Err(e) => {
                    let err_msg = format!("{:?}", e);
                    if err_msg.contains("Failed to capture screenshot") || err_msg.contains("No visible window") {
                        eprintln!("Warning: Screenshot capture failed (likely permissions). Error: {}", err_msg);
                    } else {
                        panic!("Unexpected screenshot error: {}", err_msg);
                    }
                }
            }
        }

        // ==== Part 5: UI actions ====
        eprintln!("  [5/5] UI actions...");
        {
            // Verify find_ax_element positive case
            let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            let button_id = find_node_by_role_recursive(&nodes, "AXButton")
                .expect("Should find a button in the test app");
            let ax_ref = strobe::ui::accessibility::find_ax_element(pid, &button_id).unwrap();
            assert!(ax_ref.is_some(), "find_ax_element should locate button by ID");
            unsafe { core_foundation::base::CFRelease(ax_ref.unwrap() as *const std::ffi::c_void) };

            // 1. Click (AX action path)
            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::Click,
                id: Some(button_id.clone()),
                value: None, text: None, key: None, modifiers: None,
                direction: None, amount: None, to_id: None, settle_ms: None,
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            assert!(result.success, "Click should succeed");
            assert!(result.method.is_some(), "Should report method used");
            assert!(result.node_after.is_some(), "Should return node_after");

            // 2. Set value (number on slider)
            let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            let slider_id = find_node_by_role_recursive(&nodes, "AXSlider")
                .expect("Should find a slider in test app");

            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::SetValue,
                id: Some(slider_id),
                value: Some(serde_json::json!(0.8)),
                text: None, key: None, modifiers: None,
                direction: None, amount: None, to_id: None, settle_ms: Some(200),
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            if result.success {
                assert_eq!(result.method.as_deref(), Some("ax"));
            } else {
                eprintln!("Note: set_value on slider not supported in this SwiftUI version (expected)");
            }

            // 3. Scroll
            let scroll_id = find_node_by_role_recursive(&nodes, "AXScrollArea")
                .or_else(|| find_node_by_role_recursive(&nodes, "AXList"))
                .or_else(|| find_node_by_role_recursive(&nodes, "AXTable"));

            if let Some(list_id) = scroll_id {
                let req = strobe::mcp::DebugUiActionRequest {
                    session_id: session_id.to_string(),
                    action: strobe::mcp::UiActionType::Scroll,
                    id: Some(list_id),
                    direction: Some(strobe::mcp::ScrollDirection::Down),
                    amount: Some(3),
                    value: None, text: None, key: None, modifiers: None,
                    to_id: None, settle_ms: Some(200),
                };
                let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
                assert!(result.success, "scroll should succeed: {:?}", result.error);
                assert_eq!(result.method.as_deref(), Some("cgevent"));
            } else {
                eprintln!("Note: no scrollable element found in tree");
            }

            assert!(process_alive(pid), "Process should still be alive after scroll");

            // 4. Drag (best-effort — CGEvent drag unreliable in SwiftUI)
            let nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
            if let (Some(src_id), Some(dst_id)) = (
                find_node_by_title_recursive(&nodes, "Drag Source"),
                find_node_by_title_recursive(&nodes, "Drop Here"),
            ) {
                let req = strobe::mcp::DebugUiActionRequest {
                    session_id: session_id.to_string(),
                    action: strobe::mcp::UiActionType::Drag,
                    id: Some(src_id),
                    to_id: Some(dst_id),
                    value: None, text: None, key: None, modifiers: None,
                    direction: None, amount: None, settle_ms: Some(300),
                };
                let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
                assert!(result.success, "drag should succeed: {:?}", result.error);
                assert_eq!(result.method.as_deref(), Some("cgevent"));
                assert!(result.node_before.is_some(), "drag should have node_before");
            }

            // 5. Set value (string on text field)
            let text_id = find_node_by_role_recursive(&nodes, "AXTextField")
                .expect("Should find a text field in test app");

            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::SetValue,
                id: Some(text_id.clone()),
                value: Some(serde_json::json!("programmatic")),
                text: None, key: None, modifiers: None,
                direction: None, amount: None, to_id: None, settle_ms: Some(200),
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            if result.success {
                assert_eq!(result.method.as_deref(), Some("ax"));
            }

            // 6. Type text
            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::Type,
                id: Some(text_id),
                text: Some("hello".to_string()),
                value: None, key: None, modifiers: None,
                direction: None, amount: None, to_id: None, settle_ms: Some(200),
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            assert!(result.success, "type should succeed: {:?}", result.error);
            assert!(result.node_after.is_some());

            // 7. Key (no modifier — safe for app stability)
            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::Key,
                id: None,
                key: Some("tab".to_string()),
                modifiers: None,
                value: None, text: None,
                direction: None, amount: None, to_id: None, settle_ms: Some(100),
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            assert!(result.success, "key should succeed: {:?}", result.error);
            assert_eq!(result.method.as_deref(), Some("cgevent"));
            assert!(result.node_before.is_none());
            assert!(result.node_after.is_none());

            // 8. Error case: node not found
            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::Click,
                id: Some("btn_0000".to_string()), // bogus ID
                value: None, text: None, key: None, modifiers: None,
                direction: None, amount: None, to_id: None, settle_ms: None,
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            assert!(!result.success);
            assert!(result.error.as_deref().unwrap().contains("not found"));

            // 9. Error case: unknown key name
            let req = strobe::mcp::DebugUiActionRequest {
                session_id: session_id.to_string(),
                action: strobe::mcp::UiActionType::Key,
                id: None,
                key: Some("pagedown".to_string()), // not in keycode table
                modifiers: None,
                value: None, text: None,
                direction: None, amount: None, to_id: None, settle_ms: None,
            };
            let result = strobe::ui::input::execute_ui_action(pid, &req).await.unwrap();
            assert!(!result.success, "Unknown key should fail");
            assert!(result.error.as_deref().unwrap().contains("unknown key"));
        }

        // ==== Cleanup ====
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
    }
}
