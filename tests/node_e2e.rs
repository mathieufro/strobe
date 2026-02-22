//! Node.js ESM end-to-end integration tests.
//!
//! Tests Node.js process spawning and output capture via Frida.
//! Requires `node` to be installed and on PATH.
//!
//! NOTE: ESM function tracing via V8 tracer requires ESM hook registration
//! (module.registerHooks) which transforms source at load time. Pattern-based
//! tracing (update_frida_patterns) currently works for CommonJS modules only.

mod common;

use common::*;
use std::time::Duration;
use strobe::db::EventType;

fn is_node_available() -> bool {
    std::process::Command::new("which")
        .arg("node")
        .output()
        .ok()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn get_node_path() -> String {
    std::process::Command::new("which")
        .arg("node")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })
        .expect("node not found")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_node_esm_scenarios() {
    if !is_node_available() {
        eprintln!("⚠️  Node.js not available, skipping Node E2E tests");
        return;
    }

    let fixture = node_esm_target();
    let fixture_str = fixture.to_str().unwrap();
    let project_root = fixture.parent().unwrap().to_str().unwrap();
    let node = get_node_path();
    let (sm, _dir) = create_session_manager();

    eprintln!("=== Scenario 1/2: Node ESM output capture ===");
    scenario_node_esm_output(&sm, &node, fixture_str, project_root).await;

    eprintln!("\n=== Scenario 2/2: Node ESM process lifecycle ===");
    scenario_node_esm_lifecycle(&sm, &node, fixture_str, project_root).await;

    eprintln!("\n=== All 2 Node ESM E2E scenarios passed ===");
}

async fn scenario_node_esm_output(
    sm: &strobe::daemon::SessionManager,
    node: &str,
    fixture: &str,
    project_root: &str,
) {
    let session_id = "node-esm-output";
    sm.create_session(session_id, fixture, project_root, 0).unwrap();

    let _pid = sm
        .spawn_with_frida(
            session_id,
            node,
            &[fixture.to_string()],
            None,
            project_root,
            None,
            false,
            None,
        )
        .await
        .unwrap();

    // Wait for stdout output
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(10),
        EventType::Stdout,
        |evs| {
            let text: String = evs.iter().filter_map(|e| e.text.clone()).collect::<Vec<_>>().join("");
            text.contains("esm_target: starting")
        },
    )
    .await;

    let output: String = events.iter().filter_map(|e| e.text.as_deref()).collect();
    assert!(
        output.contains("esm_target: starting"),
        "Expected 'esm_target: starting' in output, got: {}",
        output
    );

    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id).await;
    eprintln!("✓ Node ESM output capture works");
}

async fn scenario_node_esm_lifecycle(
    sm: &strobe::daemon::SessionManager,
    node: &str,
    fixture: &str,
    project_root: &str,
) {
    let session_id = "node-esm-lifecycle";
    sm.create_session(session_id, fixture, project_root, 0).unwrap();

    let pid = sm
        .spawn_with_frida(
            session_id,
            node,
            &[fixture.to_string()],
            None,
            project_root,
            None,
            false,
            None,
        )
        .await
        .unwrap();

    eprintln!("  Node spawned with PID {}", pid);
    assert!(pid > 0, "Expected valid PID");

    // Verify the process produces output
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        EventType::Stdout,
        |evs| !evs.is_empty(),
    )
    .await;

    assert!(
        !events.is_empty(),
        "Expected at least one stdout event from Node process"
    );

    // Clean stop
    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id).await;
    eprintln!("✓ Node ESM process lifecycle works");
}
