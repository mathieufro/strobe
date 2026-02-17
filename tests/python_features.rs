//! Python feature e2e tests: readVariable, breakpoints, logpoints.
//!
//! Tests the full daemon→agent→Python round-trip for:
//! - readVariable (eval_variable via Python C API)
//! - breakpoints (file:line + condition + continue)
//! - logpoints (file:line + format string)
//! - error handling for invalid expressions

mod common;

use common::*;
use std::time::Duration;
use strobe::db::EventType;

fn is_python3_available() -> bool {
    get_python3_path().is_some()
}

fn get_python3_path() -> Option<String> {
    std::process::Command::new("which")
        .arg("python3")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        })
}

/// Helper: stop session cleanly.
async fn stop_session(sm: &strobe::daemon::SessionManager, session_id: &str) {
    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id).await;
}

/// All Python feature tests share one SessionManager to avoid Frida singleton conflicts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_python_features() {
    if !is_python3_available() {
        eprintln!("Python 3 not available, skipping");
        return;
    }

    let python_script = python_target();
    let python_project = python_fixture_project();
    let (sm, _dir) = create_session_manager();
    let script_str = python_script.to_str().unwrap();
    let project_str = python_project.to_str().unwrap();
    let python3 = get_python3_path().expect("python3 should be available");

    eprintln!("=== Scenario 1/6: readVariable (expressions) ===");
    scenario_read_variable_expressions(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== Scenario 2/6: readVariable (module globals) ===");
    scenario_read_module_globals(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== Scenario 3/6: readVariable (error handling) ===");
    scenario_read_variable_errors(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== Scenario 4/6: breakpoints ===");
    scenario_breakpoints(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== Scenario 5/6: logpoints ===");
    scenario_logpoints(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== Scenario 6/6: combined (tracing + readVariable) ===");
    scenario_combined(&sm, &python3, script_str, project_str).await;

    eprintln!("\n=== All 6 Python feature scenarios passed ===");
}

// ─── Helper: spawn ──────────────────────────────────────────────────

async fn spawn_session(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
    session_id: &str,
    mode: &str,
) -> u32 {
    sm.create_session(session_id, script, project_root, 0).unwrap();
    let pid = sm
        .spawn_with_frida(
            session_id,
            python3,
            &[script.to_string(), mode.to_string()],
            None,
            project_root,
            None,
            false,
        )
        .await
        .unwrap();
    // Wait for agent to initialize
    tokio::time::sleep(Duration::from_millis(500)).await;
    pid
}

// ─── Scenario 1: readVariable expressions ───────────────────────────

async fn scenario_read_variable_expressions(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-readvar";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let read_args = serde_json::json!({
        "sessionId": session_id,
        "targets": [
            { "variable": "1 + 2" },
            { "variable": "list(range(5))" },
            { "variable": "{'key': 'value'}" },
        ]
    });

    let result = sm.execute_debug_read(&read_args).await.expect("execute_debug_read should succeed");
    let results = result.get("results").and_then(|v| v.as_array()).expect("Should have results");
    assert_eq!(results.len(), 3);

    // 1 + 2 = 3
    let v0 = results[0].get("value").expect("Should have value for 1+2");
    assert_eq!(v0.as_i64(), Some(3));
    eprintln!("  1 + 2 = {}", v0);

    // list(range(5)) = [0,1,2,3,4]
    let v1 = results[1].get("value").expect("Should have value for range");
    assert!(v1.is_array());
    assert_eq!(v1.as_array().unwrap().len(), 5);
    eprintln!("  list(range(5)) = {}", v1);

    // {'key': 'value'} = {"key":"value"}
    let v2 = results[2].get("value").expect("Should have value for dict");
    assert!(v2.is_object());
    eprintln!("  dict = {}", v2);

    stop_session(sm, session_id).await;
    eprintln!("✓ readVariable works for Python expressions");
}

// ─── Scenario 2: readVariable module globals ────────────────────────

async fn scenario_read_module_globals(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-readglobal";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let read_args = serde_json::json!({
        "sessionId": session_id,
        "targets": [
            { "variable": "__import__('modules.engine', fromlist=['g_counter']).g_counter" },
            { "variable": "__import__('modules.engine', fromlist=['g_tempo']).g_tempo" },
        ]
    });

    let result = sm.execute_debug_read(&read_args).await.expect("should succeed");
    let results = result.get("results").and_then(|v| v.as_array()).unwrap();

    let counter = results[0].get("value").and_then(|v| v.as_i64()).unwrap_or(-1);
    eprintln!("  g_counter = {}", counter);
    assert!(counter > 0, "g_counter should have been incremented");

    let tempo = results[1].get("value").and_then(|v| v.as_f64()).unwrap_or(-1.0);
    eprintln!("  g_tempo = {}", tempo);
    assert!(tempo >= 120.0, "g_tempo should be >= 120.0");

    stop_session(sm, session_id).await;
    eprintln!("✓ readVariable works for module globals");
}

// ─── Scenario 3: readVariable error handling ────────────────────────

async fn scenario_read_variable_errors(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-readvar-err";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;
    tokio::time::sleep(Duration::from_secs(1)).await;

    let read_args = serde_json::json!({
        "sessionId": session_id,
        "targets": [
            { "variable": "undefined_variable_xyz" },
        ]
    });

    let result = sm.execute_debug_read(&read_args).await.expect("should succeed");
    let results = result.get("results").and_then(|v| v.as_array()).unwrap();
    let err = results[0].get("error").and_then(|v| v.as_str()).expect("Should have error");
    eprintln!("  error: {}", err);
    assert!(
        err.contains("NameError") || err.contains("not defined"),
        "Error should mention NameError, got: {}", err
    );

    stop_session(sm, session_id).await;
    eprintln!("✓ readVariable error handling works");
}

// ─── Scenario 4: breakpoints ────────────────────────────────────────

async fn scenario_breakpoints(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-bp";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;

    // Set breakpoint on audio.py line 7 (first line inside generate_sine)
    let bp_info = sm.set_breakpoint_async(
        session_id,
        Some("bp-test-1".to_string()),
        None,
        Some("audio.py".to_string()),
        Some(7),
        None,
        None,
    ).await.expect("set_breakpoint should succeed");

    eprintln!("  breakpoint set: id={} file={:?} line={:?}", bp_info.id, bp_info.file, bp_info.line);
    assert_eq!(bp_info.id, "bp-test-1");
    assert_eq!(bp_info.address, "interpreted");

    // Wait for breakpoint hit
    let pause_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        EventType::Pause,
        |evs| !evs.is_empty(),
    ).await;

    assert!(!pause_events.is_empty(), "Should have at least one pause event");
    eprintln!("  breakpoint hit! {} pause events", pause_events.len());

    // Resume execution
    let continue_result = sm.debug_continue_async(session_id, None).await;
    assert!(continue_result.is_ok(), "debug_continue should succeed");
    eprintln!("  resumed execution");

    stop_session(sm, session_id).await;
    eprintln!("✓ Python breakpoints work (set + hit + continue)");
}

// ─── Scenario 5: logpoints ──────────────────────────────────────────

async fn scenario_logpoints(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-lp";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;

    // Set logpoint on audio.py line 13 (sum_sq line inside process_buffer)
    let lp_info = sm.set_logpoint_async(
        session_id,
        Some("lp-test-1".to_string()),
        None,
        Some("audio.py".to_string()),
        Some(13),
        "process_buffer called".to_string(),
        None,
    ).await.expect("set_logpoint should succeed");

    eprintln!("  logpoint set: id={} file={:?} line={:?}", lp_info.id, lp_info.file, lp_info.line);
    assert_eq!(lp_info.id, "lp-test-1");

    // Wait for logpoint output
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        EventType::Stdout,
        |evs| {
            let text: String = evs.iter()
                .filter_map(|e| e.text.clone())
                .collect::<Vec<_>>()
                .join("");
            text.contains("[logpoint lp-test-1]")
        },
    ).await;

    let output: String = events.iter()
        .filter_map(|e| e.text.clone())
        .collect::<Vec<_>>()
        .join("");

    assert!(
        output.contains("[logpoint lp-test-1]"),
        "Should have logpoint output, got: {}", &output[..output.len().min(200)]
    );

    let lp_lines: Vec<&str> = output.lines()
        .filter(|l| l.contains("[logpoint"))
        .take(3)
        .collect();
    eprintln!("  logpoint output: {}", lp_lines.join(" | "));

    stop_session(sm, session_id).await;
    eprintln!("✓ Python logpoints work");
}

// ─── Scenario 6: combined (tracing + readVariable) ──────────────────

async fn scenario_combined(
    sm: &strobe::daemon::SessionManager,
    python3: &str,
    script: &str,
    project_root: &str,
) {
    let session_id = "py-combined";
    let _pid = spawn_session(sm, python3, script, project_root, session_id, "globals").await;

    // Add trace patterns for audio functions
    let hook_result = sm.update_frida_patterns(
        session_id,
        Some(&["modules.audio.*".to_string()]),
        None,
        None,
    ).await;

    if let Ok(ref hr) = hook_result {
        eprintln!("  hooks: installed={} matched={}", hr.installed, hr.matched);
    }

    // Wait for traces
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Read variable while tracing is active
    let read_args = serde_json::json!({
        "sessionId": session_id,
        "targets": [
            { "variable": "2 ** 10" },
        ]
    });

    let result = sm.execute_debug_read(&read_args).await.expect("should succeed");
    let results = result.get("results").and_then(|v| v.as_array()).unwrap();
    let val = results[0].get("value").and_then(|v| v.as_i64());
    assert_eq!(val, Some(1024), "2**10 should be 1024");
    eprintln!("  readVariable during tracing: 2**10 = {}", val.unwrap());

    // Verify trace events
    let trace_events = sm.db().query_events(session_id, |q| {
        q.event_type(EventType::FunctionEnter).limit(50)
    }).unwrap();

    eprintln!("  trace events captured: {}", trace_events.len());
    assert!(trace_events.len() > 0, "Should have trace events during combined scenario");

    stop_session(sm, session_id).await;
    eprintln!("✓ Combined scenario (tracing + readVariable) complete");
}
