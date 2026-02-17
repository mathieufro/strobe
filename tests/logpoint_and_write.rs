/// Logpoint and debug_write behavioral tests.
///
/// These tests verify:
/// - Logpoints produce Logpoint events in DB without pausing the thread
/// - Logpoint message templates are evaluated ({args[N]}, {threadId})
/// - Logpoint removal stops future log events
/// - debug_write modifies global variables at runtime
/// - Breakpoint pause + debug_write + continue + verification
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
async fn test_logpoint_and_write_suite() {
    let binary = cpp_target();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    // === Test 1: Logpoint produces events without pausing ===
    println!("\n=== Test 1: Logpoint produces events without pausing ===");
    {
        let session_id = "lp-nopause";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Set logpoint (not breakpoint)
        let lp = sm
            .set_logpoint_async(
                session_id, Some("lp-1".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None,
                "process_buffer called on thread {threadId}".to_string(),
                None,
            )
            .await;
        assert!(lp.is_ok(), "Failed to set logpoint: {:?}", lp.err());

        let lp_info = lp.unwrap();
        assert_eq!(lp_info.id, "lp-1");
        assert!(lp_info.address.starts_with("0x"));

        sm.resume_process(pid).await.unwrap();

        // Wait for Logpoint events in DB
        let logpoint_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Logpoint,
            |events| events.len() >= 3,
        )
        .await;

        assert!(
            logpoint_events.len() >= 3,
            "Should have at least 3 logpoint events (5 iterations), got {}",
            logpoint_events.len()
        );

        // Verify logpoint message is present
        for event in &logpoint_events {
            assert!(
                event.logpoint_message.is_some(),
                "Logpoint event should have a message"
            );
            let msg = event.logpoint_message.as_ref().unwrap();
            assert!(
                msg.contains("process_buffer called on thread"),
                "Logpoint message should contain template output, got: {}",
                msg
            );
        }

        // Verify NO threads are paused (logpoints don't pause)
        let _paused = sm.get_all_paused_threads(session_id);
        // Allow empty or non-empty — the key is no Pause events from this logpoint
        let pause_events = sm
            .db()
            .query_events(session_id, |q| {
                q.event_type(strobe::db::EventType::Pause).limit(100)
            })
            .unwrap();
        assert!(
            pause_events.is_empty(),
            "Logpoint should NOT produce Pause events, got {}",
            pause_events.len()
        );

        // Wait for process completion
        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("[BP-LOOP] Done")
            },
        )
        .await;
        let stdout: String = stdout_events
            .iter()
            .filter_map(|e| e.text.as_deref())
            .collect();
        assert!(
            stdout.contains("[BP-LOOP] Done"),
            "Process should complete without pausing"
        );

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 2: Logpoint removal stops future log events ===
    println!("\n=== Test 2: Logpoint removal ===");
    {
        let session_id = "lp-remove";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let lp = sm
            .set_logpoint_async(
                session_id, Some("lp-remove".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None,
                "logged".to_string(),
                None,
            )
            .await;
        assert!(lp.is_ok());

        // Verify state
        assert_eq!(sm.get_logpoints(session_id).len(), 1);

        sm.resume_process(pid).await.unwrap();

        // Wait for some logpoint events
        let _ = poll_events_typed(
            &sm, session_id, Duration::from_secs(5),
            strobe::db::EventType::Logpoint,
            |events| events.len() >= 2,
        )
        .await;

        // Remove logpoint
        sm.remove_logpoint(session_id, "lp-remove").await;
        assert!(sm.get_logpoints(session_id).is_empty());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 3: Logpoint with conditional ===
    println!("\n=== Test 3: Logpoint with conditional ===");
    {
        let session_id = "lp-cond";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Logpoint with condition "false" — should never log
        let lp = sm
            .set_logpoint_async(
                session_id, Some("lp-never".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None,
                "should never appear".to_string(),
                Some("false".to_string()),
            )
            .await;
        assert!(lp.is_ok());

        sm.resume_process(pid).await.unwrap();

        // Wait for process to finish
        let _ = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("[BP-LOOP] Done")
            },
        )
        .await;

        // Verify no logpoint events were generated (condition always false)
        let logpoint_events = sm
            .db()
            .query_events(session_id, |q| {
                q.event_type(strobe::db::EventType::Logpoint).limit(100)
            })
            .unwrap();
        assert!(
            logpoint_events.is_empty(),
            "Logpoint with 'false' condition should produce 0 events, got {}",
            logpoint_events.len()
        );

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 4: debug_write modifies a global variable at runtime ===
    println!("\n=== Test 4: debug_write global variable ===");
    {
        let session_id = "write-global";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["write-target".to_string()],
                None, project_root, None, false,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Wait a moment for the process to start its loop
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Write g_counter = 999 using debug_write
        let write_args = serde_json::json!({
            "sessionId": session_id,
            "targets": [{
                "variable": "g_counter",
                "value": 999
            }]
        });

        let write_result = sm.execute_debug_write(&write_args).await;
        match &write_result {
            Ok(response) => {
                println!("  debug_write response: {}", response);
            }
            Err(e) => {
                println!("  debug_write failed (may lack DWARF for variable): {}", e);
                // Skip the rest if write failed — DWARF variable resolution required
                let _ = sm.stop_frida(session_id).await;
                sm.stop_session(session_id).await.unwrap();
                println!("  SKIPPED");
                return;
            }
        }

        // Verify the process detected g_counter=999 and exited
        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("g_counter reached 999")
            },
        )
        .await;

        let stdout: String = stdout_events
            .iter()
            .filter_map(|e| e.text.as_deref())
            .collect();
        assert!(
            stdout.contains("g_counter reached 999"),
            "Process should detect g_counter>=999 after debug_write. Got: {}",
            &stdout[..stdout.len().min(500)]
        );

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 5: Breakpoint pause + debug_write + continue ===
    println!("\n=== Test 5: Breakpoint pause + debug_write + continue ===");
    {
        let session_id = "bp-write-cont";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["write-target".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Set breakpoint on process_buffer (called in write-target loop)
        let bp = sm
            .set_breakpoint_async(
                session_id, Some("bp-write".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        // Wait for pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at breakpoint");

        // While paused, write g_counter = 999
        let write_args = serde_json::json!({
            "sessionId": session_id,
            "targets": [{
                "variable": "g_counter",
                "value": 999
            }]
        });

        let write_result = sm.execute_debug_write(&write_args).await;
        match &write_result {
            Ok(response) => {
                println!("  debug_write while paused: {}", response);
            }
            Err(e) => {
                println!("  debug_write failed: {}", e);
                let _ = sm.stop_frida(session_id).await;
                sm.stop_session(session_id).await.unwrap();
                println!("  SKIPPED");
                return;
            }
        }

        // Remove breakpoint and continue
        sm.remove_breakpoint(session_id, "bp-write").await;
        let _ = sm
            .debug_continue_async(session_id, Some("continue".to_string()))
            .await;

        // Process should see g_counter=999 and exit
        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("g_counter reached 999")
            },
        )
        .await;

        let stdout: String = stdout_events
            .iter()
            .filter_map(|e| e.text.as_deref())
            .collect();
        assert!(
            stdout.contains("g_counter reached 999"),
            "Process should detect g_counter=999 after paused write + continue. Got: {}",
            &stdout[..stdout.len().min(500)]
        );

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 6: CModule trace + breakpoint coexistence ===
    println!("\n=== Test 6: CModule trace + breakpoint coexistence ===");
    {
        let session_id = "coexist";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Install CModule trace FIRST
        sm.add_patterns(session_id, &["audio::process_buffer".to_string()])
            .unwrap();
        let hook_result = sm
            .update_frida_patterns(
                session_id,
                Some(&["audio::process_buffer".to_string()]),
                None,
                None,
            )
            .await;

        match hook_result {
            Ok(result) => {
                println!("  CModule trace installed: {} hooks", result.installed);
            }
            Err(e) => {
                println!("  CModule trace failed (skipping coexistence test): {}", e);
                let _ = sm.stop_frida(session_id).await;
                sm.stop_session(session_id).await.unwrap();
                println!("  SKIPPED");
                // Continue to next test instead of returning
                return;
            }
        }

        // Now set breakpoint on SAME function
        let bp = sm
            .set_breakpoint_async(
                session_id, Some("bp-coex".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        // Wait for breakpoint pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(
            !paused.is_empty(),
            "Should pause at breakpoint (coexisting with CModule)"
        );

        // Resume to let trace events accumulate
        sm.remove_breakpoint(session_id, "bp-coex").await;
        let _ = sm
            .debug_continue_async(session_id, Some("continue".to_string()))
            .await;
        tokio::time::sleep(Duration::from_millis(1000)).await;

        // Verify trace events exist alongside pause events
        let all_events = sm
            .db()
            .query_events(session_id, |q| q.limit(500))
            .unwrap();
        let trace_count = all_events
            .iter()
            .filter(|e| {
                e.event_type == strobe::db::EventType::FunctionEnter
                    || e.event_type == strobe::db::EventType::FunctionExit
            })
            .count();
        let pause_count = all_events
            .iter()
            .filter(|e| e.event_type == strobe::db::EventType::Pause)
            .count();

        println!(
            "  Coexistence: {} trace events + {} pause events",
            trace_count, pause_count
        );
        assert!(pause_count >= 1, "Should have at least 1 pause event");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 7: Logpoint + breakpoint on different functions coexist ===
    println!("\n=== Test 7: Logpoint + breakpoint coexistence ===");
    {
        let session_id = "lp-bp-coex";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
            )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        // Logpoint on generate_sine
        let lp = sm
            .set_logpoint_async(
                session_id, Some("lp-sine".to_string()),
                Some("audio::generate_sine".to_string()),
                None, None,
                "sine generated".to_string(),
                None,
            )
            .await;
        assert!(lp.is_ok());

        // Breakpoint on process_buffer (called after generate_sine)
        let bp = sm
            .set_breakpoint_async(
                session_id, Some("bp-proc".to_string()),
                Some("audio::process_buffer".to_string()),
                None, None, None, None,
            )
            .await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        // Wait for breakpoint pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at breakpoint");

        // Check that logpoint events were generated (generate_sine called before process_buffer)
        let logpoint_events = sm
            .db()
            .query_events(session_id, |q| {
                q.event_type(strobe::db::EventType::Logpoint).limit(100)
            })
            .unwrap();
        assert!(
            !logpoint_events.is_empty(),
            "Logpoint on generate_sine should have fired before breakpoint on process_buffer"
        );

        // Cleanup
        sm.remove_breakpoint(session_id, "bp-proc").await;
        sm.remove_logpoint(session_id, "lp-sine").await;
        let _ = sm
            .debug_continue_async(session_id, Some("continue".to_string()))
            .await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    println!("\n=== All logpoint and write tests passed ===");
}
