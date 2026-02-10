#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Auto-build and return the C++ CLI target binary path.
/// Builds on first call, caches via OnceLock. Skips rebuild if sources unchanged.
pub fn cpp_target() -> PathBuf {
    static CACHED: OnceLock<PathBuf> = OnceLock::new();
    CACHED
        .get_or_init(|| {
            let fixture_dir =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cpp");
            let binary = fixture_dir.join("build/strobe_test_target");
            let cmake = fixture_dir.join("CMakeLists.txt");

            if !binary.exists()
                || needs_rebuild(
                    &[&fixture_dir.join("src"), &cmake],
                    &binary,
                )
            {
                build_cpp_fixtures(&fixture_dir);
            } else {
                eprintln!("C++ fixtures up-to-date, skipping build");
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
            let cmake = fixture_dir.join("CMakeLists.txt");

            if !binary.exists()
                || needs_rebuild(
                    &[&fixture_dir.join("src"), &fixture_dir.join("tests"), &cmake],
                    &binary,
                )
            {
                build_cpp_fixtures(&fixture_dir);
            } else {
                eprintln!("C++ test suite up-to-date, skipping build");
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
            let cargo_toml = fixture_dir.join("Cargo.toml");

            if !binary.exists()
                || needs_rebuild(
                    &[&fixture_dir.join("src"), &cargo_toml],
                    &binary,
                )
            {
                build_rust_fixture(&fixture_dir);
            } else {
                eprintln!("Rust fixture up-to-date, skipping build");
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

/// Check if any source files are newer than binary (for rebuild detection).
/// Checks multiple directories/files to catch all relevant changes.
fn needs_rebuild(source_paths: &[&Path], binary: &Path) -> bool {
    let binary_mtime = match std::fs::metadata(binary) {
        Ok(m) => m.modified().unwrap(),
        Err(_) => return true,
    };

    fn newest_in_path(path: &Path) -> Option<std::time::SystemTime> {
        if path.is_file() {
            return path.metadata().ok().and_then(|m| m.modified().ok());
        }
        let mut newest = None;
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(t) = newest_in_path(&p) {
                    newest = Some(newest.map_or(t, |n: std::time::SystemTime| n.max(t)));
                }
            }
        }
        newest
    }

    for path in source_paths {
        if let Some(src_time) = newest_in_path(path) {
            if src_time > binary_mtime {
                return true;
            }
        }
    }
    false
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

    // macOS: generate .dSYM bundles so DWARF parsing works.
    // Skip dsymutil if the .dSYM is already newer than the binary.
    if cfg!(target_os = "macos") {
        for bin in ["strobe_test_target", "strobe_test_suite"] {
            let binary = fixture_dir.join("build").join(bin);
            if needs_dsymutil(&binary) {
                eprintln!("  Running dsymutil for {}...", bin);
                let status = Command::new("dsymutil").arg(&binary).status();
                assert!(
                    status.map(|s| s.success()).unwrap_or(false),
                    "dsymutil failed for {}",
                    bin
                );
            } else {
                eprintln!("  dSYM for {} up-to-date, skipping dsymutil", bin);
            }
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
        if needs_dsymutil(&binary) {
            eprintln!("  Running dsymutil for Rust fixture...");
            let _ = Command::new("dsymutil").arg(&binary).status();
        } else {
            eprintln!("  dSYM for Rust fixture up-to-date, skipping dsymutil");
        }
    }
}

/// Check if dsymutil needs to run (binary is newer than .dSYM).
fn needs_dsymutil(binary: &Path) -> bool {
    let dsym = binary.with_extension("dSYM");
    if !dsym.exists() {
        return true;
    }
    // Compare binary mtime against the DWARF file inside the dSYM bundle
    let dwarf_file = dsym
        .join("Contents/Resources/DWARF")
        .join(binary.file_name().unwrap());
    let bin_mtime = binary
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    let dsym_mtime = dwarf_file
        .metadata()
        .and_then(|m| m.modified())
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
    bin_mtime > dsym_mtime
}
