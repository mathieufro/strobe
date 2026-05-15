use super::artifacts::{Artifacts, ConsoleLevel};
use tempfile::tempdir;

#[test]
fn creates_artifact_dir_with_safe_id() {
    let run_dir = tempdir().unwrap();
    let artifacts = Artifacts::new(run_dir.path());
    let dir = artifacts
        .create_for("auth/login::rejects bad password (with quotes)")
        .unwrap();

    assert!(dir.exists());
    assert!(
        dir.to_str()
            .unwrap()
            .contains("auth_login__rejects_bad_password_with_quotes"),
        "got: {}",
        dir.display()
    );
    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(!name.contains('/') && !name.contains('"') && !name.contains(' '));
}

#[test]
fn captures_stdout_stderr_to_artifact_dir() {
    let run_dir = tempdir().unwrap();
    let artifacts = Artifacts::new(run_dir.path());
    let mut writer = artifacts.writer_for("t1").unwrap();
    writer.write_stdout(b"line one\n").unwrap();
    writer.write_stderr(b"error line\n").unwrap();
    writer.write_console(ConsoleLevel::Warn, "warning x").unwrap();
    writer.close();

    let dir = run_dir.path().join("tests/t1");
    assert_eq!(
        std::fs::read_to_string(dir.join("stdout.log")).unwrap(),
        "line one\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("stderr.log")).unwrap(),
        "error line\n"
    );
    assert!(std::fs::read_to_string(dir.join("console.log"))
        .unwrap()
        .contains("WARN warning x"));
}

#[test]
fn truncates_long_ids_with_hash() {
    let run_dir = tempdir().unwrap();
    let artifacts = Artifacts::new(run_dir.path());
    let long_id = "a".repeat(500) + "::some::nested::descriptor";
    let dir = artifacts.create_for(&long_id).unwrap();

    let name = dir.file_name().unwrap().to_str().unwrap();
    assert!(name.len() <= 208, "expected ≤208 chars, got {}", name.len());
    let other = artifacts
        .create_for(&("a".repeat(500) + "::other"))
        .unwrap();
    assert_ne!(
        dir, other,
        "different inputs must yield distinct dirs even when truncated"
    );
}

#[test]
fn concurrent_workers_isolated() {
    use std::thread;
    let run_dir = tempdir().unwrap();
    let root = run_dir.path().to_path_buf();
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let root = root.clone();
            thread::spawn(move || {
                let artifacts = Artifacts::new(&root);
                let mut writer = artifacts.writer_for(&format!("worker-{}", i)).unwrap();
                for _ in 0..50 {
                    writer
                        .write_stdout(format!("w{} line\n", i).as_bytes())
                        .unwrap();
                }
                writer.close();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    let w0 = std::fs::read_to_string(root.join("tests/worker-0/stdout.log")).unwrap();
    let w1 = std::fs::read_to_string(root.join("tests/worker-1/stdout.log")).unwrap();
    assert!(w0.lines().all(|l| l.starts_with("w0")));
    assert!(w1.lines().all(|l| l.starts_with("w1")));
}
