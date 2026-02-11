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

    // ---- Integration tests (need running app + AX permissions) ----

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ax_tree_from_test_app() {
        // Launch UI test app
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-ax-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Give the app time to render its window
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Query AX tree
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
        assert_eq!(text, text2, "IDs should be stable across consecutive calls");

        // Cleanup
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_screenshot_capture() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-screenshot-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let png_bytes = strobe::ui::capture::capture_window_screenshot(pid).unwrap();
        assert!(png_bytes.len() > 100, "PNG should be non-trivial, got {} bytes", png_bytes.len());

        // Verify PNG header
        assert_eq!(&png_bytes[..4], &[0x89, 0x50, 0x4E, 0x47], "Should be valid PNG");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_ax_latency_under_50ms() {
        let binary = ui_test_app();
        let project_root = binary.parent().unwrap().to_str().unwrap();
        let (sm, _temp_dir) = create_session_manager();

        let session_id = "ui-latency-test";
        let pid = sm.spawn_with_frida(
            session_id,
            binary.to_str().unwrap(),
            &[], None, project_root, None, false,
        ).await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        let start = std::time::Instant::now();
        let _nodes = strobe::ui::accessibility::query_ax_tree(pid).unwrap();
        let elapsed = start.elapsed();

        assert!(elapsed.as_millis() < 50, "AX query should be <50ms, took {}ms", elapsed.as_millis());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
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
}
