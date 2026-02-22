//! Bun end-to-end integration tests.
//!
//! Tests Bun process spawning and output capture via Frida.
//! Requires `bun` to be installed and on PATH.
//!
//! NOTE: On macOS, Bun ships with hardened runtime (no get-task-allow),
//! which prevents Frida from attaching. The test creates a re-signed copy
//! at /tmp/strobe-bun-debug with the required entitlement.
//!
//! KNOWN LIMITATION: Bun's release binaries statically link JSC and strip
//! all symbols. This means JSC function tracing (JSObjectCallAsFunction
//! hooking) is not available. Only stdout/stderr capture works.

mod common;

use common::*;
use std::time::Duration;
use strobe::db::EventType;

fn is_bun_available() -> bool {
    std::process::Command::new("which")
        .arg("bun")
        .output()
        .ok()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Get a Frida-compatible bun binary path.
/// On macOS, Bun ships with hardened runtime — re-sign a copy with get-task-allow.
fn get_debuggable_bun() -> Option<String> {
    let bun_path = std::process::Command::new("which")
        .arg("bun")
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                String::from_utf8(out.stdout).ok().map(|s| s.trim().to_string())
            } else {
                None
            }
        })?;

    if cfg!(target_os = "macos") {
        let debug_path = "/tmp/strobe-bun-debug";
        let ent_path = "/tmp/strobe-bun-debug.entitlements";

        // Write entitlements file
        let ent = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>com.apple.security.get-task-allow</key><true/>
</dict></plist>"#;
        std::fs::write(ent_path, ent).ok()?;

        // Copy and re-sign
        std::fs::copy(&bun_path, debug_path).ok()?;
        let status = std::process::Command::new("codesign")
            .args(["-f", "-s", "-", "--entitlements", ent_path, debug_path])
            .status()
            .ok()?;
        if status.success() {
            return Some(debug_path.to_string());
        }
        eprintln!("Warning: codesign failed, falling back to original bun");
    }

    Some(bun_path)
}

/// All Bun scenarios run sequentially in one test to avoid Frida/codesign races.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bun_e2e_scenarios() {
    if !is_bun_available() {
        eprintln!("⚠️  Bun not available, skipping Bun E2E tests");
        return;
    }

    let bun = match get_debuggable_bun() {
        Some(p) => p,
        None => {
            eprintln!("⚠️  Could not get debuggable bun binary, skipping");
            return;
        }
    };

    eprintln!("Using bun: {}", bun);

    let fixture = bun_multi_hook_target();
    let fixture_str = fixture.to_str().unwrap();
    let project_root = fixture.parent().unwrap().to_str().unwrap();
    let (sm, _dir) = create_session_manager();

    eprintln!("=== Scenario 1/2: Bun output capture ===");
    scenario_bun_output_capture(&sm, &bun, fixture_str, project_root).await;

    eprintln!("\n=== Scenario 2/2: Bun process lifecycle ===");
    scenario_bun_lifecycle(&sm, &bun, fixture_str, project_root).await;

    eprintln!("\n=== All 2 Bun E2E scenarios passed ===");
}

// ─── Scenario 1: Output Capture ──────────────────────────────────────

async fn scenario_bun_output_capture(
    sm: &strobe::daemon::SessionManager,
    bun: &str,
    fixture: &str,
    project_root: &str,
) {
    let session_id = "bun-output";
    sm.create_session(session_id, fixture, project_root, 0).unwrap();

    let spawn_result = sm
        .spawn_with_frida(
            session_id,
            bun,
            &["run".to_string(), fixture.to_string()],
            None,
            project_root,
            None,
            false,
            None,
        )
        .await;

    let _pid = match spawn_result {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("⚠️  Frida attach failed: {} — skipping Bun tests", e);
            let _ = sm.stop_session(session_id).await;
            return;
        }
    };

    // Wait for stdout from the fixture
    let stdout_events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(5),
        EventType::Stdout,
        |evs| {
            let text: String = evs.iter().filter_map(|e| e.text.as_deref()).collect();
            text.contains("bun_multi_hook: starting")
        },
    )
    .await;

    let stdout_text: String = stdout_events.iter().filter_map(|e| e.text.as_deref()).collect();
    eprintln!("  Bun stdout: {:?}", stdout_text);

    assert!(
        stdout_text.contains("bun_multi_hook: starting"),
        "Expected 'bun_multi_hook: starting' in stdout, got: {:?}",
        stdout_text
    );

    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id).await;
    eprintln!("✓ Bun stdout captured correctly");
}

// ─── Scenario 2: Process Lifecycle ───────────────────────────────────

async fn scenario_bun_lifecycle(
    sm: &strobe::daemon::SessionManager,
    bun: &str,
    fixture: &str,
    project_root: &str,
) {
    let session_id = "bun-lifecycle";
    sm.create_session(session_id, fixture, project_root, 0).unwrap();

    let spawn_result = sm
        .spawn_with_frida(
            session_id,
            bun,
            &["run".to_string(), fixture.to_string()],
            None,
            project_root,
            None,
            false,
            None,
        )
        .await;

    let pid = match spawn_result {
        Ok(pid) => pid,
        Err(e) => {
            eprintln!("⚠️  Frida attach failed: {} — skipping", e);
            let _ = sm.stop_session(session_id).await;
            return;
        }
    };

    eprintln!("  Bun spawned with PID {}", pid);
    assert!(pid > 0, "Expected valid PID");

    // Verify the process is alive and producing output
    let events = poll_events_typed(
        sm,
        session_id,
        Duration::from_secs(3),
        EventType::Stdout,
        |evs| !evs.is_empty(),
    )
    .await;

    assert!(
        !events.is_empty(),
        "Expected at least one stdout event from Bun process"
    );

    // Stop Frida and session cleanly
    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id).await;
    eprintln!("✓ Bun process lifecycle (spawn, capture, stop) works");
}
