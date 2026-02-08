#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Auto-build and return the C++ CLI target binary path.
/// Builds on first call, caches via OnceLock.
pub fn cpp_target() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cpp");
            let binary = fixture_dir.join("build/strobe_test_target");

            if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
                build_cpp_fixtures(&fixture_dir);
            }

            assert!(
                binary.exists(),
                "C++ target binary not found after build: {:?}",
                binary
            );
            binary
        })
        .clone()
}

/// Auto-build and return the C++ Catch2 test suite binary path.
pub fn cpp_test_suite() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cpp");
            let binary = fixture_dir.join("build/strobe_test_suite");

            if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
                build_cpp_fixtures(&fixture_dir);
            }

            assert!(
                binary.exists(),
                "C++ test suite binary not found after build: {:?}",
                binary
            );
            binary
        })
        .clone()
}

/// Auto-build and return the Rust fixture binary path (with dsymutil on macOS).
pub fn rust_target() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust");
            let binary = fixture_dir.join("target/debug/strobe_test_fixture");

            if !binary.exists() || needs_rebuild(&fixture_dir.join("src"), &binary) {
                build_rust_fixture(&fixture_dir);
            }

            assert!(
                binary.exists(),
                "Rust fixture binary not found after build: {:?}",
                binary
            );
            binary
        })
        .clone()
}

/// Return the Rust fixture project root (for debug_test Cargo adapter).
pub fn rust_fixture_project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rust")
}

/// Create a SessionManager with a temp database.
pub fn create_session_manager() -> (strobe::daemon::SessionManager, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let sm = strobe::daemon::SessionManager::new(&db_path).unwrap();
    (sm, dir)
}

/// Poll DB until predicate returns true or timeout.
pub async fn poll_events(
    sm: &strobe::daemon::SessionManager,
    session_id: &str,
    timeout: Duration,
    predicate: impl Fn(&[strobe::db::Event]) -> bool,
) -> Vec<strobe::db::Event> {
    let start = Instant::now();
    loop {
        let events = sm.db().query_events(session_id, |q| q.limit(500)).unwrap();
        if predicate(&events) || start.elapsed() >= timeout {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Poll with event type filter.
pub async fn poll_events_typed(
    sm: &strobe::daemon::SessionManager,
    session_id: &str,
    timeout: Duration,
    event_type: strobe::db::EventType,
    predicate: impl Fn(&[strobe::db::Event]) -> bool,
) -> Vec<strobe::db::Event> {
    let start = Instant::now();
    loop {
        let events = sm
            .db()
            .query_events(session_id, |q| q.event_type(event_type.clone()).limit(500))
            .unwrap();
        if predicate(&events) || start.elapsed() >= timeout {
            return events;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Collect all stdout text from events.
pub fn collect_stdout(events: &[strobe::db::Event]) -> String {
    events
        .iter()
        .filter(|e| e.event_type == strobe::db::EventType::Stdout)
        .filter_map(|e| e.text.as_deref())
        .collect()
}

/// Check if sources are newer than binary (for rebuild detection).
fn needs_rebuild(src_dir: &Path, binary: &Path) -> bool {
    let binary_mtime = match std::fs::metadata(binary) {
        Ok(m) => m.modified().unwrap(),
        Err(_) => return true,
    };

    fn newest_in_dir(dir: &Path) -> Option<std::time::SystemTime> {
        let mut newest = None;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if let Some(t) = newest_in_dir(&path) {
                        newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
                    }
                } else if let Ok(m) = path.metadata() {
                    if let Ok(t) = m.modified() {
                        newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
                    }
                }
            }
        }
        newest
    }

    match newest_in_dir(src_dir) {
        Some(src_time) => src_time > binary_mtime,
        None => true,
    }
}

/// Build C++ fixtures via cmake + dsymutil on macOS.
fn build_cpp_fixtures(fixture_dir: &Path) {
    eprintln!("Building C++ fixtures in {:?}...", fixture_dir);

    let status = Command::new("cmake")
        .args(["-B", "build", "-DCMAKE_BUILD_TYPE=Debug"])
        .current_dir(fixture_dir)
        .status()
        .expect("cmake not found. Install with: xcode-select --install");
    assert!(status.success(), "cmake configure failed");

    let status = Command::new("cmake")
        .args(["--build", "build", "--parallel"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "cmake build failed");

    // macOS: generate .dSYM bundles so DWARF parsing works
    if cfg!(target_os = "macos") {
        for bin in ["strobe_test_target", "strobe_test_suite"] {
            let binary = fixture_dir.join("build").join(bin);
            let status = Command::new("dsymutil").arg(&binary).status();
            assert!(
                status.map(|s| s.success()).unwrap_or(false),
                "dsymutil failed for {}",
                bin
            );
        }
    }
}

/// Build Rust fixture via cargo + dsymutil.
fn build_rust_fixture(fixture_dir: &Path) {
    eprintln!("Building Rust fixture in {:?}...", fixture_dir);

    let status = Command::new("cargo")
        .args(["build"])
        .current_dir(fixture_dir)
        .status()
        .unwrap();
    assert!(status.success(), "Rust fixture build failed");

    if cfg!(target_os = "macos") {
        let binary = fixture_dir.join("target/debug/strobe_test_fixture");
        let _ = Command::new("dsymutil").arg(&binary).status();
    }
}
