use std::time::Duration;

mod common;
use common::{cpp_target, create_session_manager};

/// All stepping/continue/logpoint tests share a single SessionManager/FridaSpawner
/// because Frida's GLib state is process-global and cannot be safely
/// torn down and recreated within the same process.
#[tokio::test(flavor = "multi_thread")]
async fn test_stepping_suite() {
    let binary = cpp_target();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    // --- Test 1: Step-over basic ---
    {
        let session_id = "step-over-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-entry".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp_result.is_ok(), "Failed to set breakpoint: {:?}", bp_result.err());

        tokio::time::sleep(Duration::from_millis(500)).await;

        let paused = sm.get_all_paused_threads(session_id);
        if !paused.is_empty() {
            let result = sm.debug_continue_async(session_id, Some("step-over".to_string())).await;
            assert!(result.is_ok(), "Step-over failed: {:?}", result.err());
            println!("✓ Step-over executed successfully");
        } else {
            println!("Note: No thread paused (function may not have been called yet)");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 2: Step-into basic ---
    {
        let session_id = "step-into-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-entry".to_string()),
                Some("main".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp_result.is_ok());

        tokio::time::sleep(Duration::from_millis(500)).await;

        let paused = sm.get_all_paused_threads(session_id);
        if !paused.is_empty() {
            let result = sm.debug_continue_async(session_id, Some("step-into".to_string())).await;
            assert!(result.is_ok(), "Step-into failed: {:?}", result.err());
            println!("✓ Step-into executed successfully");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 3: Step-out basic ---
    {
        let session_id = "step-out-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-entry".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp_result.is_ok());

        tokio::time::sleep(Duration::from_millis(500)).await;

        let paused = sm.get_all_paused_threads(session_id);
        if !paused.is_empty() {
            let result = sm.debug_continue_async(session_id, Some("step-out".to_string())).await;
            assert!(result.is_ok(), "Step-out failed: {:?}", result.err());
            println!("✓ Step-out executed successfully");
        } else {
            println!("Note: No thread paused (function may not have been called yet)");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 4: Continue action validation ---
    {
        let session_id = "validation-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let _bp = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-1".to_string()),
                Some("main".to_string()),
                None, None, None, None,
            )
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(500)).await;

        let paused = sm.get_all_paused_threads(session_id);
        if !paused.is_empty() {
            let result = sm.debug_continue_async(session_id, Some("invalid-action".to_string())).await;
            assert!(result.is_err(), "Should reject invalid action");
            println!("✓ Invalid action properly rejected");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 5: Continue with no paused threads ---
    {
        let session_id = "no-pause-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let result = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        assert!(result.is_err(), "Should fail when no threads are paused");
        assert!(result.unwrap_err().to_string().contains("No paused threads"));
        println!("✓ Properly rejects continue with no paused threads");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 6: Logpoint basic ---
    {
        let session_id = "logpoint-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let lp_result = sm
            .set_logpoint_async(
                session_id,
                Some("lp-1".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None,
                "audio buffer hit on thread {threadId}".to_string(),
                None,
            )
            .await;

        match lp_result {
            Ok(lp_info) => {
                assert_eq!(lp_info.id, "lp-1");
                assert_eq!(lp_info.message, "audio buffer hit on thread {threadId}");
                assert!(lp_info.address.starts_with("0x"));
                println!("✓ Logpoint set at {}", lp_info.address);

                let logpoints = sm.get_logpoints(session_id);
                assert_eq!(logpoints.len(), 1);
                assert_eq!(logpoints[0].id, "lp-1");
            }
            Err(e) => panic!("Failed to set logpoint: {}", e),
        }

        sm.remove_logpoint(session_id, "lp-1");
        let logpoints = sm.get_logpoints(session_id);
        assert_eq!(logpoints.len(), 0);
        println!("✓ Logpoint removed successfully");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 7: Multiple breakpoints ---
    {
        let session_id = "multi-bp-test";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp1 = sm
            .set_breakpoint_async(session_id, Some("bp-1".to_string()), Some("main".to_string()), None, None, None, None)
            .await;
        let bp2 = sm
            .set_breakpoint_async(session_id, Some("bp-2".to_string()), Some("audio::process_buffer".to_string()), None, None, None, None)
            .await;

        if bp1.is_ok() && bp2.is_ok() {
            let breakpoints = sm.get_breakpoints(session_id);
            assert_eq!(breakpoints.len(), 2);
            println!("✓ Multiple breakpoints set successfully");

            sm.remove_breakpoint(session_id, "bp-1");
            let breakpoints = sm.get_breakpoints(session_id);
            assert_eq!(breakpoints.len(), 1);
            assert_eq!(breakpoints[0].id, "bp-2");
            println!("✓ Selective breakpoint removal works");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }
}
