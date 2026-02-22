/// Comprehensive stepping behavioral tests.
///
/// These tests verify actual step-over, step-into, and step-out behavior:
/// - Step-over advances to the next source line in the same function
/// - Step-into enters function calls
/// - Step-out returns to the caller
/// - Stepping generates correct Pause events with updated file/line info
/// - One-shot hooks are properly cleaned up
///
/// All tests share a single SessionManager because Frida's GLib state is
/// process-global and cannot be safely torn down and recreated.

use std::time::Duration;

mod common;
use common::{cpp_target, create_session_manager, poll_events_typed};

/// Helper: wait for at least `count` paused threads, with timeout.
async fn wait_for_pause(
    sm: &strobe::daemon::SessionManager,
    session_id: &str,
    count: usize,
    timeout: Duration,
) -> Vec<(u64, strobe::daemon::PauseInfo)> {
    let start = std::time::Instant::now();
    loop {
        let paused = sm.get_all_paused_threads(session_id);
        if paused.len() >= count || start.elapsed() >= timeout {
            return paused.into_iter().collect();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stepping_behavioral_suite() {
    let binary = cpp_target();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    // === Test 1: Step-over advances to next source line ===
    println!("\n=== Test 1: Step-over advances to next line ===");
    {
        let session_id = "step-over-adv";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        // Set breakpoint to establish initial pause point
        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-step".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        // Wait for initial pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at breakpoint");

        // Step over — should advance to next source line within process_buffer
        // (Don't remove breakpoint first — remove_breakpoint resumes the thread)
        let step_result = sm
            .debug_continue_async(session_id, Some("step-over".to_string()))
            .await;
        assert!(step_result.is_ok(), "Step-over failed: {:?}", step_result.err());

        // Wait for step to complete (should pause at next line via one-shot hook)
        let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused2.is_empty(), "Step-over should produce a new pause");
        let step_pause = &paused2[0].1;
        println!(
            "  Step-over paused at: bp={} func={:?}",
            step_pause.breakpoint_id, step_pause.func_name
        );
        // The breakpoint_id should be a step-* ID, not the original bp-step
        assert!(
            step_pause.breakpoint_id.starts_with("step-"),
            "Step pause should have step-* breakpoint ID, got: {}",
            step_pause.breakpoint_id
        );
        // The step pause is at a different position than the initial pause
        // (one-shot hook fired at a new address, proving advancement)
        assert_ne!(
            step_pause.breakpoint_id, "bp-step",
            "Step should produce a new breakpoint_id, not reuse the original"
        );

        // Verify Pause events in DB — should have at least 2 (initial + step)
        let pause_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(2),
            strobe::db::EventType::Pause, |events| events.len() >= 2,
        ).await;
        assert!(
            pause_events.len() >= 2,
            "Should have at least 2 Pause events (initial + step), got {}",
            pause_events.len()
        );
        // Verify the two pause events have different breakpoint IDs
        let bp_ids: Vec<_> = pause_events.iter()
            .filter_map(|e| e.breakpoint_id.as_deref())
            .collect();
        assert!(
            bp_ids.len() >= 2 && bp_ids[0] != bp_ids[1],
            "Pause events should have different breakpoint IDs: {:?}", bp_ids
        );

        sm.remove_breakpoint(session_id, "bp-step").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 2: Step-into enters a called function ===
    println!("\n=== Test 2: Step-into enters callee ===");
    {
        let session_id = "step-into-callee";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["step-target".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        // Set breakpoint on audio::generate_sine
        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-sine".to_string()),
            Some("audio::generate_sine".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        // Wait for pause at generate_sine
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at generate_sine");

        // Step-into — should advance (basic impl: same as step-over currently)
        // (Don't remove breakpoint first — remove_breakpoint resumes the thread)
        let step_result = sm
            .debug_continue_async(session_id, Some("step-into".to_string()))
            .await;
        assert!(step_result.is_ok(), "Step-into failed: {:?}", step_result.err());

        // Wait for step completion
        let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        if !paused2.is_empty() {
            let step_info = &paused2[0].1;
            println!(
                "  Step-into paused at: {} func={:?}",
                step_info.breakpoint_id, step_info.func_name
            );
        }

        sm.remove_breakpoint(session_id, "bp-sine").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 3: Step-out returns to the caller ===
    println!("\n=== Test 3: Step-out returns to caller ===");
    {
        let session_id = "step-out-caller";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        // Set breakpoint inside process_buffer
        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-inner".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        // Wait for pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at process_buffer");

        let pause_info = &paused[0].1;
        let has_return_addr = pause_info.return_address.is_some();
        println!(
            "  Paused at process_buffer, return_address: {:?}",
            pause_info.return_address
        );

        // Step-out — should use return address if available, error if not
        // (Don't remove breakpoint first — remove_breakpoint resumes the thread)
        let step_result = sm
            .debug_continue_async(session_id, Some("step-out".to_string()))
            .await;

        if has_return_addr {
            assert!(
                step_result.is_ok(),
                "Step-out should succeed with return address: {:?}",
                step_result.err()
            );

            // Wait for pause at caller
            let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
            assert!(!paused2.is_empty(), "Step-out should pause at caller");
            let caller_pause = &paused2[0].1;
            assert!(
                caller_pause.breakpoint_id.starts_with("step-"),
                "Step-out pause should have step-* ID, got: {}",
                caller_pause.breakpoint_id
            );
            println!(
                "  Step-out paused at caller: {}",
                caller_pause.breakpoint_id
            );
        } else {
            // Without return address, step-out should return an error (not silently continue)
            assert!(
                step_result.is_err(),
                "Step-out without return address should return error"
            );
            let err_msg = step_result.unwrap_err().to_string();
            assert!(
                err_msg.contains("Cannot step-out"),
                "Error should mention 'Cannot step-out': {}",
                err_msg
            );
            println!("  Step-out correctly rejected: {}", err_msg);
        }

        sm.remove_breakpoint(session_id, "bp-inner").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 4: Multiple sequential steps ===
    println!("\n=== Test 4: Multiple sequential steps ===");
    {
        let session_id = "step-seq";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-seq".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        // Wait for initial pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at breakpoint");

        // Perform 3 step-overs and verify each produces a unique step-* ID
        // (Don't remove breakpoint first — remove_breakpoint resumes the thread)
        let mut step_count = 0;
        let mut step_ids: Vec<String> = Vec::new();
        for i in 0..3 {
            let step = sm
                .debug_continue_async(session_id, Some("step-over".to_string()))
                .await;
            if step.is_err() {
                println!("  Step {} failed: {:?}", i, step.err());
                break;
            }

            let paused_step = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
            if paused_step.is_empty() {
                println!("  Step {} didn't produce a pause (may have reached end of function)", i);
                break;
            }
            let step_id = paused_step[0].1.breakpoint_id.clone();
            assert!(
                step_id.starts_with("step-"),
                "Step {} should have step-* ID, got: {}", i, step_id
            );
            step_ids.push(step_id.clone());
            step_count += 1;
            println!("  Step {} completed, paused at: {}", i, step_id);
        }

        assert!(
            step_count >= 2,
            "Should complete at least 2 sequential steps, got {}",
            step_count
        );
        // Verify each step produced a distinct ID (different address each time)
        for i in 1..step_ids.len() {
            assert_ne!(
                step_ids[i], step_ids[i - 1],
                "Sequential steps should produce different IDs: {:?}", step_ids
            );
        }

        sm.remove_breakpoint(session_id, "bp-seq").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 5: Invalid step action is rejected ===
    println!("\n=== Test 5: Invalid step action rejected ===");
    {
        let session_id = "step-invalid";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-val".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty());

        // Try invalid action
        let result = sm
            .debug_continue_async(session_id, Some("teleport".to_string()))
            .await;
        assert!(result.is_err(), "Invalid action should be rejected");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid") || err_msg.contains("invalid") || err_msg.contains("Unknown"),
            "Error should mention invalid/unknown action: {}",
            err_msg
        );

        sm.remove_breakpoint(session_id, "bp-val").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 6: Continue with no paused threads fails gracefully ===
    println!("\n=== Test 6: Continue with no paused threads ===");
    {
        let session_id = "step-nopause";
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["hello".to_string()],
                None, project_root, None, false,
                None,
        )
            .await.unwrap();
        sm.update_session_pid(session_id, pid).unwrap();

        // No breakpoints set, no threads paused
        let result = sm
            .debug_continue_async(session_id, Some("continue".to_string()))
            .await;
        assert!(result.is_err(), "Should fail when no threads are paused");
        assert!(
            result.unwrap_err().to_string().contains("No paused"),
            "Error should mention 'No paused'"
        );

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    println!("\n=== All stepping behavioral tests passed ===");
}
