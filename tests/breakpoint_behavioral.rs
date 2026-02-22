/// Comprehensive breakpoint behavioral tests.
///
/// These tests go beyond smoke-level "it set a breakpoint" and actually verify:
/// - Threads pause at breakpoints (Pause events appear in DB)
/// - Resume continues execution (stdout appears after resume)
/// - Conditional breakpoints only fire when condition is true
/// - Hit count breakpoints fire only at the Nth invocation
/// - Breakpoint removal stops future pauses
/// - Multi-thread breakpoints pause independently
/// - Session cleanup removes all Phase 2 state
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
async fn test_breakpoint_behavioral_suite() {
    let binary = cpp_target();
    let project_root = binary.parent().unwrap().to_str().unwrap();
    let (sm, _temp_dir) = create_session_manager();

    // === Test 1: Breakpoint pauses a thread, resume continues execution ===
    println!("\n=== Test 1: Breakpoint pause and resume ===");
    {
        let session_id = "bp-pause-resume";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-1".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());
        let bp_info = bp.unwrap();
        assert_eq!(bp_info.id, "bp-1");
        assert!(bp_info.address.starts_with("0x"));

        sm.resume_process(pid).await.unwrap();

        // Wait for thread to pause
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Thread should have paused at breakpoint");
        assert_eq!(paused[0].1.breakpoint_id, "bp-1");
        println!("  Thread {} paused at bp-1", paused[0].0);

        // Verify Pause event in DB
        let pause_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(2),
            strobe::db::EventType::Pause, |events| !events.is_empty(),
        ).await;
        assert!(!pause_events.is_empty(), "Pause event should be in DB");
        assert_eq!(pause_events[0].breakpoint_id.as_deref(), Some("bp-1"));

        // Resume
        let resume = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        assert!(resume.is_ok(), "Resume failed: {:?}", resume.err());

        // Should pause again (breakpoint still active)
        let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused2.is_empty(), "Should pause again on next call");

        // Remove breakpoint and resume — should run to completion
        sm.remove_breakpoint(session_id, "bp-1").await;
        assert!(sm.get_breakpoints(session_id).is_empty());

        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;

        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("[BP-LOOP] Done")
            },
        ).await;
        let stdout: String = stdout_events.iter().filter_map(|e| e.text.as_deref()).collect();
        assert!(stdout.contains("[BP-LOOP] Done"), "Process should complete. Got: {}", &stdout[..stdout.len().min(300)]);

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 2: Conditional breakpoint — false never pauses, true pauses ===
    println!("\n=== Test 2: Conditional breakpoint (selective) ===");
    {
        // Part A: condition="false" should NOT pause — validates evaluator actually filters
        let session_id = "bp-cond-false";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-false".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, Some("false".to_string()), None,
        ).await;
        assert!(bp.is_ok(), "Failed to set conditional breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        // Wait briefly — should NOT pause because condition is always false
        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(3)).await;
        assert!(paused.is_empty(), "Condition='false' should NOT cause a pause, but {} threads paused", paused.len());

        // Process should complete normally (condition never fires)
        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("[BP-LOOP] Done")
            },
        ).await;
        let stdout: String = stdout_events.iter().filter_map(|e| e.text.as_deref()).collect();
        assert!(stdout.contains("[BP-LOOP] Done"), "Process should complete with condition='false'");
        println!("  Part A: condition='false' correctly did NOT pause");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();

        // Part B: condition="true" SHOULD pause (control test)
        let session_id = "bp-cond-true";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "5".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-true".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, Some("true".to_string()), None,
        ).await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Condition='true' should pause");
        println!("  Part B: condition='true' correctly paused");

        sm.remove_breakpoint(session_id, "bp-true").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 3: Hit count breakpoint ===
    println!("\n=== Test 3: Hit count breakpoint ===");
    {
        let session_id = "bp-hitcount";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-hit3".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, Some(3),
        ).await;
        assert!(bp.is_ok(), "Failed to set hit count breakpoint: {:?}", bp.err());

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Hit count breakpoint should pause");

        let breakpoints = sm.get_breakpoints(session_id);
        let bp_state = breakpoints.iter().find(|b| b.id == "bp-hit3").unwrap();
        assert_eq!(bp_state.hits, 3, "Should be hit 3 times, got {}", bp_state.hits);
        println!("  Hit count=3 verified (hits={})", bp_state.hits);

        sm.remove_breakpoint(session_id, "bp-hit3").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 4: Multi-thread breakpoint (recv().wait() per-thread proof) ===
    // This test proves recv().wait() blocks individual threads independently:
    // "threads" mode spawns 2 audio workers + 1 midi worker, each calling
    // process_buffer in a loop. We set a breakpoint and verify that 2+ threads
    // pause simultaneously with different thread IDs. Then we resume all and
    // verify execution continues (threads unblock independently).
    println!("\n=== Test 4: Multi-thread breakpoint (recv().wait() e2e) ===");
    {
        let session_id = "bp-mt";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["threads".to_string()],
                None, project_root, None, false,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-mt".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok(), "Failed to set breakpoint: {:?}", bp.err());

        // Wait for 2+ threads to pause — proves recv().wait() blocks per-thread
        let paused = wait_for_pause(&sm, session_id, 2, Duration::from_secs(8)).await;
        assert!(
            paused.len() >= 2,
            "At least 2 threads should pause independently (proves recv().wait() per-thread), got {}",
            paused.len()
        );
        let tids: Vec<u64> = paused.iter().map(|(t, _)| *t).collect();
        assert_ne!(tids[0], tids[1], "Paused threads must have different IDs");
        // All paused on the same breakpoint
        for (_, info) in &paused {
            assert_eq!(info.breakpoint_id, "bp-mt", "All threads paused on same breakpoint");
        }
        println!("  {} threads paused independently: {:?}", paused.len(), tids);

        // Resume all — proves threads unblock independently via per-thread resume messages
        let resume = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        assert!(resume.is_ok(), "Resume failed: {:?}", resume.err());

        // Wait for them to pause again (breakpoint still active, next iteration)
        let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused2.is_empty(), "Should pause again after resume (proves threads unblocked)");
        println!("  Resumed and paused again — recv().wait() per-thread verified");

        sm.remove_breakpoint(session_id, "bp-mt").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 5: Session cleanup removes Phase 2 state ===
    println!("\n=== Test 5: Session cleanup ===");
    {
        let session_id = "bp-cleanup";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "100".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-c1".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok());

        let lp = sm.set_logpoint_async(
            session_id, Some("lp-c1".to_string()),
            Some("audio::generate_sine".to_string()),
            None, None, "log msg".to_string(), None,
        ).await;
        assert!(lp.is_ok());

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty());

        // Verify state before cleanup
        assert!(!sm.get_breakpoints(session_id).is_empty());
        assert!(!sm.get_logpoints(session_id).is_empty());
        assert!(!sm.get_all_paused_threads(session_id).is_empty());

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();

        // Verify all Phase 2 state cleaned up
        assert!(sm.get_breakpoints(session_id).is_empty(), "Breakpoints not cleaned up");
        assert!(sm.get_logpoints(session_id).is_empty(), "Logpoints not cleaned up");
        assert!(sm.get_all_paused_threads(session_id).is_empty(), "Paused threads not cleaned up");
        println!("  All Phase 2 state cleaned up");
        println!("  PASSED");
    }

    // === Test 6: Breakpoint removal stops future pauses ===
    println!("\n=== Test 6: Breakpoint removal ===");
    {
        let session_id = "bp-removal";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp = sm.set_breakpoint_async(
            session_id, Some("bp-rm".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp.is_ok());

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause");

        sm.remove_breakpoint(session_id, "bp-rm").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;

        let stdout_events = poll_events_typed(
            &sm, session_id, Duration::from_secs(10),
            strobe::db::EventType::Stdout,
            |events| {
                let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
                text.contains("[BP-LOOP] Done")
            },
        ).await;
        let stdout: String = stdout_events.iter().filter_map(|e| e.text.as_deref()).collect();
        assert!(stdout.contains("[BP-LOOP] Done"), "Process should complete after removal");
        println!("  Process completed after breakpoint removal");

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    // === Test 7: Multiple breakpoints on different functions ===
    println!("\n=== Test 7: Multiple breakpoints ===");
    {
        let session_id = "bp-multi";
        let pid = sm
            .spawn_with_frida(
                session_id, binary.to_str().unwrap(),
                &["breakpoint-loop".to_string(), "10".to_string()],
                None, project_root, None, true,
                None,
        )
            .await.unwrap();
        sm.create_session(session_id, binary.to_str().unwrap(), project_root, pid).unwrap();

        let bp1 = sm.set_breakpoint_async(
            session_id, Some("bp-proc".to_string()),
            Some("audio::process_buffer".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp1.is_ok());

        let bp2 = sm.set_breakpoint_async(
            session_id, Some("bp-eff".to_string()),
            Some("audio::apply_effect".to_string()),
            None, None, None, None,
        ).await;
        assert!(bp2.is_ok());
        assert_eq!(sm.get_breakpoints(session_id).len(), 2);

        sm.resume_process(pid).await.unwrap();

        let paused = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        assert!(!paused.is_empty(), "Should pause at first breakpoint");
        let first_bp = paused[0].1.breakpoint_id.clone();
        println!("  First pause at: {}", first_bp);

        sm.remove_breakpoint(session_id, &first_bp).await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;

        let paused2 = wait_for_pause(&sm, session_id, 1, Duration::from_secs(5)).await;
        if !paused2.is_empty() {
            println!("  Second pause at: {}", paused2[0].1.breakpoint_id);
            assert_ne!(first_bp, paused2[0].1.breakpoint_id, "Different breakpoint");
        }

        sm.remove_breakpoint(session_id, "bp-proc").await;
        sm.remove_breakpoint(session_id, "bp-eff").await;
        let _ = sm.debug_continue_async(session_id, Some("continue".to_string())).await;

        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  PASSED");
    }

    println!("\n=== All breakpoint behavioral tests passed ===");
}
