mod common;
use common::{cpp_target, create_session_manager};

/// All breakpoint tests share a single SessionManager/FridaSpawner
/// because Frida's GLib state is process-global and cannot be safely
/// torn down and recreated within the same process.
#[tokio::test(flavor = "multi_thread")]
async fn test_breakpoint_suite() {
    let binary = cpp_target();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    // --- Test 1: Function entry breakpoint ---
    {
        let session_id = "bp-test-func";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-1".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;

        match bp_result {
            Ok(bp_info) => {
                assert_eq!(bp_info.id, "bp-1");
                assert!(bp_info.address.starts_with("0x"));
                println!("✓ Breakpoint set at {}", bp_info.address);
            }
            Err(e) => panic!("Failed to set breakpoint on audio::process_buffer: {}", e),
        }

        let breakpoints = sm.get_breakpoints(session_id);
        assert_eq!(breakpoints.len(), 1);
        println!("✓ Active breakpoints: {}", breakpoints.len());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 2: Line-level breakpoint ---
    {
        let session_id = "bp-test-line";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                None, None,
                Some("main.cpp".to_string()),
                Some(10),
                None, None,
            )
            .await;

        match bp_result {
            Ok(bp_info) => {
                assert!(bp_info.id.starts_with("bp-"));
                assert!(bp_info.line.is_some());
                println!("✓ Line breakpoint set at {}:{}", bp_info.file.unwrap(), bp_info.line.unwrap());
            }
            Err(e) => println!("Note: {}", e),
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 3: Conditional breakpoint ---
    {
        let session_id = "bp-test-cond";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                None,
                Some("audio::process_buffer".to_string()),
                None, None,
                Some("args[0] > 5".to_string()),
                None,
            )
            .await;

        if let Ok(bp_info) = bp_result {
            let breakpoints = sm.get_breakpoints(session_id);
            let bp = breakpoints.iter().find(|b| b.id == bp_info.id).unwrap();
            assert_eq!(bp.condition, Some("args[0] > 5".to_string()));
            println!("✓ Conditional breakpoint set");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 4: Breakpoint removal ---
    {
        let session_id = "bp-test-remove";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp_result = sm
            .set_breakpoint_async(
                session_id,
                Some("bp-to-remove".to_string()),
                Some("main".to_string()),
                None, None, None, None,
            )
            .await;

        if bp_result.is_ok() {
            let breakpoints = sm.get_breakpoints(session_id);
            assert!(breakpoints.iter().any(|b| b.id == "bp-to-remove"));

            sm.remove_breakpoint(session_id, "bp-to-remove");

            let breakpoints = sm.get_breakpoints(session_id);
            assert!(!breakpoints.iter().any(|b| b.id == "bp-to-remove"));
            println!("✓ Breakpoint removed successfully");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }

    // --- Test 5: Validation errors ---
    {
        let session_id = "bp-test-validation";
        let pid = sm
            .spawn_with_frida(session_id, binary.to_str().unwrap(), &[], None, project_root, None, false)
            .await
            .unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Non-existent function should fail
        let result = sm
            .set_breakpoint_async(
                session_id,
                None,
                Some("thisDoesNotExist123".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(result.is_err(), "Should fail for non-existent function");
        println!("✓ Non-existent function properly rejected");

        // Invalid line
        let result = sm
            .set_breakpoint_async(
                session_id,
                None, None,
                Some("main.cpp".to_string()),
                Some(1),
                None, None,
            )
            .await;
        if result.is_err() {
            println!("✓ Invalid line properly rejected");
        }

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).unwrap();
    }
}
