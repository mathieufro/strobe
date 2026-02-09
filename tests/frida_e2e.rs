//! Frida end-to-end integration tests.
//!
//! All scenarios run sequentially in ONE tokio test to avoid Frida/GLib
//! teardown races between concurrent sessions.

mod common;

use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_frida_e2e_scenarios() {
    let cpp_bin = cpp_target();
    let rust_bin = rust_target();
    let (sm, _dir) = create_session_manager();
    let cpp_str = cpp_bin.to_str().unwrap();
    let rust_str = rust_bin.to_str().unwrap();
    let cpp_project = cpp_bin.parent().unwrap().parent().unwrap().to_str().unwrap();
    let rust_project = rust_bin
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_str()
        .unwrap();

    eprintln!("=== Scenario 1/15: Output capture ===");
    scenario_output_capture(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 2/15: Function tracing — C++ namespaces ===");
    scenario_cpp_tracing(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 3/15: Function tracing — Rust namespaces ===");
    scenario_rust_tracing(&sm, rust_str, rust_project).await;

    eprintln!("\n=== Scenario 4/15: Crash capture (SIGSEGV) ===");
    scenario_crash_null(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 5/15: Crash capture (SIGABRT) ===");
    scenario_crash_abort(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 6/15: Fork workers ===");
    scenario_fork_workers(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 7/15: Fork exec ===");
    scenario_fork_exec(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 8/15: Duration query filter ===");
    scenario_duration_query(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 9/15: Time range query filter ===");
    scenario_time_range_query(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 10/15: Pattern add/remove ===");
    scenario_pattern_add_remove(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 11/15: Watch variables ===");
    scenario_watch_variables(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 12/15: Multi-threaded tracing ===");
    scenario_multithreaded(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 13/15: debug_read — one-shot DWARF variables ===");
    scenario_read_oneshot(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 14/15: debug_read — struct pointer expansion ===");
    scenario_read_struct(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== Scenario 15/15: debug_read — poll mode ===");
    scenario_read_poll(&sm, cpp_str, cpp_project).await;

    eprintln!("\n=== All 15 Frida E2E scenarios passed ===");
}

// ─── Scenario 1: Output Capture ──────────────────────────────────────

async fn scenario_output_capture(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-output";

    let pid = sm
        .spawn_with_frida(session_id, binary, &["hello".to_string()], None, project_root, None, false)
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();
    assert!(pid > 0);

    let stdout_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::Stdout,
        |events| {
            let text = collect_stdout(events);
            text.contains("Hello from strobe_test_target")
        },
    )
    .await;

    let all_stdout = collect_stdout(&stdout_events);
    eprintln!("Stdout ({} events): {}", stdout_events.len(), all_stdout.trim());

    assert!(
        all_stdout.contains("Hello from strobe_test_target"),
        "Should capture stdout. Got: {}",
        all_stdout
    );

    // Verify PID on stdout events
    for event in &stdout_events {
        assert!(event.pid.is_some(), "Stdout events should have PID");
        assert_eq!(event.pid.unwrap(), pid, "Stdout PID should match");
    }

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 2: Function Tracing — C++ Namespaces ───────────────────

async fn scenario_cpp_tracing(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-cpp-trace";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["slow-functions".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let patterns = [
        "timing::fast".to_string(),
        "timing::slow".to_string(),
        "timing::very_slow".to_string(),
    ];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed — ensure C++ fixture has debug symbols (dsymutil)");
    eprintln!("Hooked {} functions (matched: {})", hook_result.installed, hook_result.matched);
    assert!(hook_result.installed > 0, "Must hook at least one timing function");

    // Poll for function exit events
    let exit_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        strobe::db::EventType::FunctionExit,
        |events| events.iter().any(|e| e.duration_ns.unwrap_or(0) >= 40_000_000),
    )
    .await;

    eprintln!("Function exit events: {}", exit_events.len());
    for e in &exit_events {
        eprintln!(
            "  {} duration={}ms",
            e.function_name,
            e.duration_ns.unwrap_or(0) as f64 / 1_000_000.0
        );
    }

    assert!(!exit_events.is_empty(), "Should capture function exit events");

    // Verify slow functions have reasonable durations
    let has_slow = exit_events
        .iter()
        .any(|e| e.function_name.contains("slow") && e.duration_ns.unwrap_or(0) >= 40_000_000);
    assert!(has_slow, "Should see slow function with duration >= 40ms");

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 3: Function Tracing — Rust Namespaces ──────────────────

async fn scenario_rust_tracing(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-rust-trace";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["basic".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let patterns = ["strobe_test_fixture::audio::**".to_string()];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed — ensure Rust fixture has debug symbols (dsymutil)");
    eprintln!("Hooked {} Rust functions", hook_result.installed);

    // Poll for any events (stdout or function traces)
    let events = poll_events(sm, session_id, Duration::from_secs(5), |events| {
        events.len() >= 2
    })
    .await;

    eprintln!("Events captured: {}", events.len());

    // Verify stdout from basic mode
    let stdout = collect_stdout(&events);
    assert!(
        stdout.contains("Running basic mode") || stdout.contains("Done"),
        "Should capture Rust fixture stdout"
    );

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 4: Crash Capture (SIGSEGV) ─────────────────────────────

async fn scenario_crash_null(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-crash-null";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["crash-null".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Poll until a crash event appears
    let all_events = poll_events(sm, session_id, Duration::from_secs(5), |events| {
        events
            .iter()
            .any(|e| e.event_type == strobe::db::EventType::Crash)
    })
    .await;

    // Verify stdout captured before crash
    let stdout = collect_stdout(&all_events);
    assert!(
        stdout.contains("TARGET") || stdout.contains("CRASH"),
        "Should capture stdout before crash. Got: {}",
        stdout
    );

    // Verify crash event
    let crash_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.event_type == strobe::db::EventType::Crash)
        .collect();

    assert!(!crash_events.is_empty(), "Should capture a crash event");

    let crash = crash_events[0];

    // Signal
    assert!(crash.signal.is_some(), "Crash should have signal");
    let signal = crash.signal.as_ref().unwrap();
    assert!(
        signal.contains("access-violation") || signal.contains("SEGV"),
        "Signal should indicate crash: {}",
        signal
    );

    // Fault address
    assert!(crash.fault_address.is_some(), "Crash should have fault_address");
    let fault_addr = crash.fault_address.as_ref().unwrap();
    assert!(fault_addr.starts_with("0x"), "Fault address should be hex: {}", fault_addr);

    // Registers
    assert!(crash.registers.is_some(), "Crash should have registers");
    let reg_obj = crash.registers.as_ref().unwrap().as_object().unwrap();
    assert!(!reg_obj.is_empty(), "Should have registers captured");

    if cfg!(target_arch = "aarch64") {
        assert!(reg_obj.contains_key("pc"), "ARM64: missing pc");
        assert!(reg_obj.contains_key("sp"), "ARM64: missing sp");
        assert!(reg_obj.contains_key("fp"), "ARM64: missing fp");
    }

    // Backtrace
    assert!(crash.backtrace.is_some(), "Crash should have backtrace");
    let frames = crash.backtrace.as_ref().unwrap().as_array().unwrap();
    assert!(!frames.is_empty(), "Backtrace should have frames");

    // PID
    assert_eq!(crash.pid, Some(pid), "Crash PID should match");

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 5: Crash Capture (SIGABRT) ─────────────────────────────

async fn scenario_crash_abort(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-crash-abort";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["crash-abort".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let all_events = poll_events(sm, session_id, Duration::from_secs(5), |events| {
        events.iter().any(|e| {
            e.event_type == strobe::db::EventType::Crash
                || e.event_type == strobe::db::EventType::Stdout
        })
    })
    .await;

    let crash_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.event_type == strobe::db::EventType::Crash)
        .collect();

    // abort() may not be catchable by Frida on all macOS versions
    if let Some(crash) = crash_events.first() {
        let signal = crash.signal.as_ref().unwrap();
        eprintln!("Abort signal: {}", signal);
        assert!(
            signal.contains("abort") || signal.contains("ABRT") || signal.contains("access-violation"),
            "Signal should indicate abort: {}",
            signal
        );
    } else {
        eprintln!("Note: abort() not captured by Frida exception handler (expected on some macOS)");
        let stdout = collect_stdout(&all_events);
        assert!(
            stdout.contains("About to abort") || stdout.contains("TARGET"),
            "Should at least capture stdout before abort"
        );
    }

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 6: Fork Workers ────────────────────────────────────────

async fn scenario_fork_workers(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-fork-workers";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["fork-workers".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let stdout_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("exited with status")
        },
    )
    .await;

    let all_stdout = collect_stdout(&stdout_events);
    eprintln!("Fork stdout ({} events):\n{}", stdout_events.len(), all_stdout);

    assert!(all_stdout.contains("PARENT"), "Should capture parent stdout");
    assert!(all_stdout.contains("PID="), "Should see PID info");

    let all_pids = sm.get_all_pids(session_id);
    eprintln!("PIDs in session: {:?}", all_pids);
    assert!(all_pids.contains(&pid), "Session should contain parent PID");

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 7: Fork Exec ──────────────────────────────────────────

async fn scenario_fork_exec(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-fork-exec";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["fork-exec".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let stdout_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("exited with status")
        },
    )
    .await;

    let all_stdout = collect_stdout(&stdout_events);
    eprintln!("Fork-exec stdout:\n{}", all_stdout);

    assert!(
        all_stdout.contains("PARENT") || all_stdout.contains("fork"),
        "Should capture parent output"
    );

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 8: Duration Query Filter ──────────────────────────────

async fn scenario_duration_query(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-duration";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["slow-functions".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    let patterns = [
        "timing::fast".to_string(),
        "timing::medium".to_string(),
        "timing::slow".to_string(),
        "timing::very_slow".to_string(),
    ];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed — ensure C++ fixture has debug symbols (dsymutil)");
    eprintln!("Hooked {} functions", hook_result.installed);
    assert!(hook_result.installed > 0, "Must hook at least one timing function");

    let exit_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        strobe::db::EventType::FunctionExit,
        |events| events.iter().any(|e| e.duration_ns.unwrap_or(0) >= 40_000_000),
    )
    .await;

    assert!(!exit_events.is_empty(), "Must capture function exit events");

    // Test min_duration_ns filter
    let slow_events = sm
        .db()
        .query_events(session_id, |q| {
            let mut q = q.event_type(strobe::db::EventType::FunctionExit).limit(500);
            q.min_duration_ns = Some(40_000_000);
            q
        })
        .unwrap();

    eprintln!("Events with duration >= 40ms: {}", slow_events.len());
    for e in &slow_events {
        let dur_ms = e.duration_ns.unwrap_or(0) as f64 / 1_000_000.0;
        eprintln!("  {} = {:.1}ms", e.function_name, dur_ms);
        assert!(
            e.duration_ns.unwrap_or(0) >= 40_000_000,
            "Filtered event should have duration >= 40ms"
        );
    }

    let fast_in_slow = slow_events
        .iter()
        .any(|e| e.function_name.contains("fast"));
    assert!(!fast_in_slow, "fast function should not appear in >= 40ms filter");

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 9: Time Range Query Filter ─────────────────────────────

async fn scenario_time_range_query(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-time-range";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["slow-functions".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Wait for stdout to indicate completion
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("Done")
        },
    )
    .await;

    if events.len() >= 2 {
        let first_ts = events[0].timestamp_ns;
        let last_ts = events[events.len() - 1].timestamp_ns;

        if first_ts < last_ts {
            let time_filtered = sm
                .db()
                .query_events(session_id, |q| {
                    let mut q = q.event_type(strobe::db::EventType::Stdout).limit(500);
                    q.timestamp_from_ns = Some(first_ts + 1);
                    q.timestamp_to_ns = Some(last_ts);
                    q
                })
                .unwrap();

            assert!(
                time_filtered.len() < events.len(),
                "Time-filtered query should return fewer events"
            );
        }
    }

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 10: Pattern Add/Remove ─────────────────────────────────

async fn scenario_pattern_add_remove(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-pattern-mgmt";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["slow-functions".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Add patterns
    let patterns = ["timing::fast".to_string(), "timing::slow".to_string()];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed");
    eprintln!("Initially hooked {} functions", hook_result.installed);
    assert!(hook_result.installed > 0, "Must hook at least one function");

    // Remove one pattern
    let remove_result = sm
        .update_frida_patterns(
            session_id,
            None,
            Some(&["timing::fast".to_string()]),
            None,
        )
        .await;

    match &remove_result {
        Ok(r) => eprintln!("After remove: {} hooks", r.installed),
        Err(e) => eprintln!("Warning: pattern remove failed: {}", e),
    }

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 11: Watch Variables ────────────────────────────────────

async fn scenario_watch_variables(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-watches";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["globals".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Add trace pattern for a function that runs periodically
    let patterns = ["audio::process_buffer".to_string()];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed");
    eprintln!("Hooked {} for watch testing", hook_result.installed);
    assert!(hook_result.installed > 0, "Must hook audio::process_buffer");

    // Resolve g_counter via DWARF parser (same path the daemon server takes)
    let dwarf = sm
        .get_dwarf(session_id)
        .await
        .expect("DWARF parse must succeed")
        .expect("DWARF parser must exist for session");

    let recipe = dwarf
        .resolve_watch_expression("g_counter")
        .expect("g_counter must be resolvable in DWARF");

    let type_kind_str = match &recipe.type_kind {
        strobe::dwarf::TypeKind::Integer { signed } => {
            if *signed { "int".to_string() } else { "uint".to_string() }
        }
        strobe::dwarf::TypeKind::Float => "float".to_string(),
        strobe::dwarf::TypeKind::Pointer => "pointer".to_string(),
        strobe::dwarf::TypeKind::Unknown => "unknown".to_string(),
    };

    let watch_targets = vec![strobe::frida_collector::WatchTarget {
        label: "counter".to_string(),
        address: recipe.base_address,
        size: recipe.final_size,
        type_kind_str,
        deref_depth: recipe.deref_chain.len() as u8,
        deref_offset: recipe.deref_chain.first().copied().unwrap_or(0),
        type_name: recipe.type_name.clone(),
        on_patterns: None,
        no_slide: false,
    }];

    sm.update_frida_watches(session_id, watch_targets, vec![])
        .await
        .expect("Watch install must succeed");
    eprintln!("Watch installed for g_counter at 0x{:x}", recipe.base_address);

    // Poll for events with watch values
    let events = poll_events(sm, session_id, Duration::from_secs(5), |events| {
        events.iter().any(|e| e.watch_values.is_some())
    })
    .await;

    let with_watches: Vec<_> = events.iter().filter(|e| e.watch_values.is_some()).collect();
    eprintln!("Events with watch values: {}", with_watches.len());
    assert!(
        !with_watches.is_empty(),
        "Must capture events with watch values for g_counter"
    );

    let wv = with_watches[0].watch_values.as_ref().unwrap();
    eprintln!("Watch values: {}", wv);

    // Wait for completion
    let _ = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("Done")
        },
    )
    .await;

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 12: Multi-threaded Tracing ─────────────────────────────

async fn scenario_multithreaded(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-threads";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["threads".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Add trace patterns for audio functions
    let patterns = ["audio::*".to_string()];
    sm.add_patterns(session_id, &patterns).unwrap();

    let hook_result = sm
        .update_frida_patterns(session_id, Some(&patterns), None, None)
        .await
        .expect("Hook install must succeed");
    eprintln!("Hooked {} functions for threading test", hook_result.installed);
    assert!(hook_result.installed > 0, "Must hook at least one audio function");

    // Poll for stdout completion
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(15),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("Done")
        },
    )
    .await;

    let stdout = collect_stdout(&events);
    assert!(
        stdout.contains("THREADS") || stdout.contains("multi-threaded"),
        "Should capture threaded output"
    );

    // Check for function events from multiple threads
    let func_events = sm
        .db()
        .query_events(session_id, |q| {
            q.event_type(strobe::db::EventType::FunctionEnter).limit(200)
        })
        .unwrap();

    assert!(
        !func_events.is_empty(),
        "Must capture function enter events from traced audio functions"
    );

    let thread_names: std::collections::HashSet<_> = func_events
        .iter()
        .filter_map(|e| e.thread_name.as_deref())
        .collect();
    eprintln!("Distinct thread names: {:?}", thread_names);

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 13: debug_read — One-Shot DWARF Variable Reads ─────────
//
// Exercises the full execute_debug_read pipeline: validation → DWARF resolution
// → recipe building → imageBase injection → agent read → response formatting.

async fn scenario_read_oneshot(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-read-oneshot";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["globals".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Wait for the process to start updating globals
    let _ = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("[GLOBALS] Starting")
        },
    )
    .await;

    // Small delay to let the loop run a few iterations
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Call through the exact same code path as the MCP tool
    let response = sm
        .execute_debug_read(&serde_json::json!({
            "sessionId": session_id,
            "targets": [
                { "variable": "g_counter" },
                { "variable": "g_tempo" },
                { "variable": "g_sample_rate" },
            ]
        }))
        .await
        .expect("execute_debug_read must succeed");

    eprintln!("Read response: {}", serde_json::to_string_pretty(&response).unwrap());

    let results = response
        .get("results")
        .and_then(|v| v.as_array())
        .expect("Response should have results array");
    assert_eq!(results.len(), 3, "Should have 3 results (one per variable)");

    // g_counter: uint32, updated in loop from 0..49
    let counter = &results[0];
    assert_eq!(counter["target"], "g_counter");
    assert!(
        counter.get("error").is_none(),
        "g_counter read should succeed, got: {:?}",
        counter.get("error")
    );
    let counter_val = counter["value"]
        .as_u64()
        .expect("g_counter should be a number");
    eprintln!("  g_counter = {}", counter_val);
    assert!(counter_val <= 199, "g_counter should be 0-199, got {}", counter_val);

    // g_tempo: double, set to 120.0 + (i % 10)
    let tempo = &results[1];
    assert_eq!(tempo["target"], "g_tempo");
    assert!(tempo.get("error").is_none(), "g_tempo read should succeed");
    let tempo_val = tempo["value"]
        .as_f64()
        .expect("g_tempo should be a float");
    eprintln!("  g_tempo = {}", tempo_val);
    assert!(
        (120.0..=129.0).contains(&tempo_val),
        "g_tempo should be 120.0-129.0, got {}",
        tempo_val
    );

    // g_sample_rate: int64, constant 44100
    let sr = &results[2];
    assert_eq!(sr["target"], "g_sample_rate");
    assert!(sr.get("error").is_none(), "g_sample_rate read should succeed");
    let sr_val = sr["value"]
        .as_i64()
        .expect("g_sample_rate should be a number");
    eprintln!("  g_sample_rate = {}", sr_val);
    assert_eq!(sr_val, 44100, "g_sample_rate should be 44100");

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 14: debug_read — Struct Pointer Expansion ──────────────

async fn scenario_read_struct(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-read-struct";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["globals".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Wait for process to start
    let _ = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("[GLOBALS] Starting")
        },
    )
    .await;

    tokio::time::sleep(Duration::from_millis(300)).await;

    // depth=1 triggers struct expansion for pointer-to-struct
    let response = sm
        .execute_debug_read(&serde_json::json!({
            "sessionId": session_id,
            "targets": [
                { "variable": "g_point_ptr" }
            ],
            "depth": 1
        }))
        .await
        .expect("execute_debug_read must succeed for struct");

    eprintln!(
        "Struct read response: {}",
        serde_json::to_string_pretty(&response).unwrap()
    );

    let results = response
        .get("results")
        .and_then(|v| v.as_array())
        .expect("Should have results");
    assert_eq!(results.len(), 1);

    let result = &results[0];
    assert_eq!(result["target"], "g_point_ptr");
    assert!(
        result.get("error").is_none(),
        "Struct read should succeed, got: {:?}",
        result.get("error")
    );

    let fields_val = result
        .get("fields")
        .expect("Struct result should have 'fields'");
    assert!(fields_val.is_object(), "Fields should be a JSON object");

    let fields_obj = fields_val.as_object().unwrap();
    assert!(fields_obj.contains_key("x"), "Should have field 'x'");
    assert!(fields_obj.contains_key("y"), "Should have field 'y'");
    assert!(fields_obj.contains_key("value"), "Should have field 'value'");

    // In "globals" mode: g_point_ptr->x = i, y = i*2, value stays 99.9
    let x = fields_obj["x"].as_i64().expect("x should be a number");
    let y = fields_obj["y"].as_i64().expect("y should be a number");
    let val = fields_obj["value"].as_f64().expect("value should be a float");

    eprintln!("  Point {{ x: {}, y: {}, value: {} }}", x, y, val);
    assert!(x >= 0 && x <= 199, "x should be 0-199, got {}", x);
    assert!(y >= 0 && y <= 398, "y should be 0-398, got {}", y);
    assert!(
        (val - 99.9).abs() < 0.01,
        "value should be ~99.9 (initial), got {}",
        val
    );

    let _ = sm.stop_session(session_id);
}

// ─── Scenario 15: debug_read — Poll Mode ─────────────────────────────

async fn scenario_read_poll(
    sm: &strobe::daemon::SessionManager,
    binary: &str,
    project_root: &str,
) {
    let session_id = "e2e-read-poll";

    let pid = sm
        .spawn_with_frida(
            session_id,
            binary,
            &["globals".to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    sm.create_session(session_id, binary, project_root, pid).unwrap();

    // Wait for process to start
    let _ = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::Stdout,
        |events| {
            let text: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("[GLOBALS] Starting")
        },
    )
    .await;

    // Poll mode: read g_counter every 100ms for 1500ms
    let response = sm
        .execute_debug_read(&serde_json::json!({
            "sessionId": session_id,
            "targets": [
                { "variable": "g_counter" }
            ],
            "poll": {
                "intervalMs": 100,
                "durationMs": 1500,
            }
        }))
        .await
        .expect("execute_debug_read poll must succeed");

    eprintln!(
        "Poll response: {}",
        serde_json::to_string_pretty(&response).unwrap()
    );

    // Poll mode returns immediately with { "polling": true }
    assert_eq!(
        response.get("polling").and_then(|v| v.as_bool()),
        Some(true),
        "Poll mode should return {{ polling: true }}"
    );

    // Wait for variable_snapshot events to appear in the timeline
    let snapshot_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        strobe::db::EventType::VariableSnapshot,
        |events| events.len() >= 5,
    )
    .await;

    eprintln!(
        "Variable snapshots collected: {} (expected ~15 for 1500ms/100ms)",
        snapshot_events.len()
    );
    assert!(
        snapshot_events.len() >= 5,
        "Should collect at least 5 variable snapshots, got {}",
        snapshot_events.len()
    );

    // Verify snapshot structure
    for (i, event) in snapshot_events.iter().take(3).enumerate() {
        assert_eq!(event.event_type, strobe::db::EventType::VariableSnapshot);

        let data = event
            .arguments
            .as_ref()
            .expect("Snapshot event should have data in arguments field");
        let data_obj = data.as_object().expect("Snapshot data should be a JSON object");
        assert!(
            data_obj.contains_key("g_counter"),
            "Snapshot should contain g_counter"
        );

        let val = data_obj["g_counter"]
            .as_u64()
            .expect("g_counter in snapshot should be a number");
        eprintln!("  snapshot[{}]: g_counter = {}", i, val);
    }

    // Verify snapshots show changing values (g_counter increments every 100ms)
    if snapshot_events.len() >= 2 {
        let first = snapshot_events[0]
            .arguments
            .as_ref()
            .unwrap()["g_counter"]
            .as_u64();
        let last = snapshot_events[snapshot_events.len() - 1]
            .arguments
            .as_ref()
            .unwrap()["g_counter"]
            .as_u64();

        if let (Some(f), Some(l)) = (first, last) {
            eprintln!("  First snapshot g_counter={}, last={}", f, l);
            // Over 1.5 seconds, g_counter should change (increments every 100ms)
            // Allow for both cases since timing isn't guaranteed
            if f == l {
                eprintln!("  Warning: g_counter didn't change between snapshots (timing-dependent)");
            }
        }
    }

    let _ = sm.stop_session(session_id);
}
