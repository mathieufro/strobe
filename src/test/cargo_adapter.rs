use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;

pub struct CargoTestAdapter;

impl TestAdapter for CargoTestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if project_root.join("Cargo.toml").exists() {
            90
        } else {
            0
        }
    }

    fn name(&self) -> &str {
        "cargo"
    }

    fn suite_command(
        &self,
        _project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let mut args = vec!["test".to_string()];

        match level {
            Some(TestLevel::Unit) => args.push("--lib".to_string()),
            Some(TestLevel::Integration) => {
                args.push("--test".to_string());
                args.push("*".to_string());
            }
            Some(TestLevel::E2e) => {
                args.push("--test".to_string());
                args.push("e2e*".to_string());
            }
            None => {}
        }

        // --format json and -Zunstable-options are test harness flags (after --)
        args.push("--".to_string());
        args.push("-Zunstable-options".to_string());
        args.push("--format".to_string());
        args.push("json".to_string());

        Ok(TestCommand {
            program: "cargo".to_string(),
            args,
            env: HashMap::from([("RUSTC_BOOTSTRAP".to_string(), "1".to_string())]),
        })
    }

    fn single_test_command(
        &self,
        _project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "cargo".to_string(),
            args: vec![
                "test".to_string(),
                "--".to_string(),
                test_name.to_string(),
                "-Zunstable-options".to_string(),
                "--format".to_string(),
                "json".to_string(),
            ],
            env: HashMap::from([("RUSTC_BOOTSTRAP".to_string(), "1".to_string())]),
        })
    }

    fn parse_output(
        &self,
        stdout: &str,
        _stderr: &str,
        _exit_code: i32,
    ) -> TestResult {
        let mut passed = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut duration_ms = 0u64;
        let mut failures = Vec::new();
        let mut all_tests = Vec::new();

        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");

            match (event_type, event) {
                ("test", "ok") => {
                    passed += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    all_tests.push(TestDetail {
                        name,
                        status: "pass".to_string(),
                        duration_ms: (exec_time * 1000.0) as u64,
                        stdout: v.get("stdout").and_then(|s| s.as_str()).map(|s| s.to_string()),
                        stderr: None,
                        message: None,
                    });
                }
                ("test", "failed") => {
                    failed += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    let test_stdout = v.get("stdout").and_then(|s| s.as_str()).unwrap_or("");

                    let (file, line_num, message) = parse_panic_location(test_stdout);

                    failures.push(TestFailure {
                        name: name.clone(),
                        file: file.clone(),
                        line: line_num,
                        message: message.clone(),
                        rerun: Some(name.clone()),
                        suggested_traces: vec![],
                    });

                    all_tests.push(TestDetail {
                        name,
                        status: "fail".to_string(),
                        duration_ms: (exec_time * 1000.0) as u64,
                        stdout: Some(test_stdout.to_string()),
                        stderr: None,
                        message: Some(message),
                    });
                }
                ("test", "ignored") => {
                    skipped += 1;
                    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    all_tests.push(TestDetail {
                        name,
                        status: "skip".to_string(),
                        duration_ms: 0,
                        stdout: None,
                        stderr: None,
                        message: None,
                    });
                }
                ("suite", "ok") | ("suite", "failed") => {
                    // Only accumulate duration from suite summaries.
                    // Pass/fail/skip counts come from individual test events above,
                    // since multi-target runs emit multiple suite summaries.
                    let exec_time = v.get("exec_time").and_then(|t| t.as_f64()).unwrap_or(0.0);
                    duration_ms += (exec_time * 1000.0) as u64;
                }
                _ => {}
            }
        }

        TestResult {
            summary: TestSummary {
                passed,
                failed,
                skipped,
                stuck: None,
                duration_ms,
            },
            failures,
            stuck: vec![],
            all_tests,
        }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = Vec::new();

        // Extract module path from test name: "parser::tests::test_foo" → "parser::*"
        let parts: Vec<&str> = failure.name.split("::").collect();
        if parts.len() >= 2 {
            traces.push(format!("{}::*", parts[0]));
        }

        // If we have a source file, use @file: pattern
        if let Some(ref file) = failure.file {
            if let Some(filename) = Path::new(file).file_name().and_then(|n| n.to_str()) {
                traces.push(format!("@file:{}", filename));
            }
        }

        traces
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        capture_native_stacks(pid)
    }
}

/// Parse panic location from cargo test stdout.
/// Looks for patterns like: "panicked at src/parser.rs:142:5:\n<message>"
fn parse_panic_location(stdout: &str) -> (Option<String>, Option<u32>, String) {
    for line in stdout.lines() {
        if let Some(idx) = line.find("panicked at ") {
            let after = &line[idx + "panicked at ".len()..];
            let parts: Vec<&str> = after.splitn(4, ':').collect();
            if parts.len() >= 2 {
                let file = parts[0].trim().to_string();
                let line_num = parts[1].trim().parse::<u32>().ok();
                let msg_start = stdout.find(line)
                    .map(|i| i + line.len())
                    .unwrap_or(0);
                let message = stdout[msg_start..].trim().to_string();
                let message = if message.is_empty() {
                    stdout.to_string()
                } else {
                    message
                };
                return (Some(file), line_num, message);
            }
        }
    }
    (None, None, stdout.to_string())
}

/// Capture thread stacks using OS-level tools. Works for native code (Rust, C, C++).
pub fn capture_native_stacks(pid: u32) -> Vec<ThreadStack> {
    #[cfg(target_os = "macos")]
    {
        capture_stacks_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        capture_stacks_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        vec![]
    }
}

#[cfg(target_os = "macos")]
fn capture_stacks_macos(pid: u32) -> Vec<ThreadStack> {
    use std::io::Read as _;

    let mut child = match std::process::Command::new("sample")
        .args([&pid.to_string(), "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    // Wait up to 5 seconds for sample to complete (1s sampling + overhead)
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return vec![];
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => return vec![],
        }
    }

    let mut stdout = String::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_string(&mut stdout);
    }
    parse_sample_output(&stdout)
}

#[cfg(target_os = "macos")]
fn parse_sample_output(text: &str) -> Vec<ThreadStack> {
    let mut threads = Vec::new();
    let mut current_thread: Option<String> = None;
    let mut current_stack: Vec<String> = Vec::new();

    for line in text.lines() {
        if line.starts_with("Thread_") || line.starts_with("  Thread_") {
            if let Some(name) = current_thread.take() {
                if !current_stack.is_empty() {
                    threads.push(ThreadStack {
                        name,
                        stack: current_stack.clone(),
                    });
                    current_stack.clear();
                }
            }
            current_thread = Some(line.trim().to_string());
        } else if current_thread.is_some() && line.contains("+") {
            let frame = line.trim().to_string();
            if !frame.is_empty() {
                current_stack.push(frame);
            }
        }
    }

    if let Some(name) = current_thread {
        if !current_stack.is_empty() {
            threads.push(ThreadStack { name, stack: current_stack });
        }
    }

    threads
}

#[cfg(target_os = "linux")]
fn capture_stacks_linux(pid: u32) -> Vec<ThreadStack> {
    let mut threads = Vec::new();
    let task_dir = format!("/proc/{}/task", pid);

    if let Ok(entries) = std::fs::read_dir(&task_dir) {
        for entry in entries.flatten() {
            let tid = entry.file_name().to_string_lossy().to_string();
            let stack_path = format!("{}/{}/stack", task_dir, tid);
            if let Ok(stack) = std::fs::read_to_string(&stack_path) {
                let frames: Vec<String> = stack.lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !frames.is_empty() {
                    threads.push(ThreadStack {
                        name: format!("thread-{}", tid),
                        stack: frames,
                    });
                }
            }
        }
    }

    threads
}

/// Parse a single Cargo JSON line and update progress incrementally.
pub fn update_progress(line: &str, progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let v: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
    let mut p = progress.lock().unwrap();
    match (event_type, event) {
        ("suite", "started") => {
            // First suite started means compilation is done, tests are running
            if p.phase == super::TestPhase::Compiling {
                p.phase = super::TestPhase::Running;
            }
        }
        ("suite", "ok") | ("suite", "failed") => {
            // Suite finished — mark SuitesFinished so stuck detector knows tests
            // completed. If another suite starts, ("suite", "started") won't
            // regress this since it only transitions from Compiling.
            p.phase = super::TestPhase::SuitesFinished;
            p.current_test = None;
        }
        ("test", "started") => {
            p.phase = super::TestPhase::Running;
            p.current_test = v.get("name").and_then(|n| n.as_str()).map(String::from);
            p.current_test_started_at = Some(std::time::Instant::now());
        }
        ("test", "ok") => {
            p.passed += 1;
            // Keep current_test visible (shows last-run test); clear elapsed timer
            p.current_test_started_at = None;
        }
        ("test", "failed") => {
            p.failed += 1;
            p.current_test_started_at = None;
        }
        ("test", "ignored") => {
            p.skipped += 1;
            p.current_test_started_at = None;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_detect_cargo_project() {
        let adapter = CargoTestAdapter;
        let confidence = adapter.detect(Path::new("."), None);
        assert_eq!(confidence, 90);
    }

    #[test]
    fn test_detect_no_cargo() {
        let adapter = CargoTestAdapter;
        let confidence = adapter.detect(Path::new("/tmp"), None);
        assert_eq!(confidence, 0);
    }

    #[test]
    fn test_parse_cargo_json_all_pass() {
        let adapter = CargoTestAdapter;
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 3 }
{ "type": "test", "event": "started", "name": "tests::test_a" }
{ "type": "test", "event": "ok", "name": "tests::test_a", "exec_time": 0.001 }
{ "type": "test", "event": "started", "name": "tests::test_b" }
{ "type": "test", "event": "ok", "name": "tests::test_b", "exec_time": 0.002 }
{ "type": "test", "event": "started", "name": "tests::test_c" }
{ "type": "test", "event": "ignored", "name": "tests::test_c" }
{ "type": "suite", "event": "ok", "passed": 2, "failed": 0, "ignored": 1, "measured": 0, "filtered_out": 0, "exec_time": 0.003 }
"#;
        let result = adapter.parse_output(stdout, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_cargo_json_with_failure() {
        let adapter = CargoTestAdapter;
        let stdout = r#"{ "type": "suite", "event": "started", "test_count": 2 }
{ "type": "test", "event": "started", "name": "parser::tests::test_empty_input" }
{ "type": "test", "event": "failed", "name": "parser::tests::test_empty_input", "exec_time": 0.5, "stdout": "thread 'parser::tests::test_empty_input' panicked at src/parser.rs:142:5:\nassertion `left == right` failed\n  left: None\n  right: Some(Node { kind: Empty })\n" }
{ "type": "test", "event": "started", "name": "parser::tests::test_ok" }
{ "type": "test", "event": "ok", "name": "parser::tests::test_ok", "exec_time": 0.001 }
{ "type": "suite", "event": "failed", "passed": 1, "failed": 1, "ignored": 0, "measured": 0, "filtered_out": 0, "exec_time": 0.501 }
"#;
        let result = adapter.parse_output(stdout, "", 101);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "parser::tests::test_empty_input");
        assert_eq!(f.file.as_deref(), Some("src/parser.rs"));
        assert_eq!(f.line, Some(142));
        assert!(f.message.contains("assertion"));
    }

    #[test]
    fn test_suggest_traces_from_module_path() {
        let adapter = CargoTestAdapter;
        let failure = TestFailure {
            name: "parser::tests::test_empty_input".to_string(),
            file: Some("src/parser.rs".to_string()),
            line: Some(142),
            message: "assertion failed".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let traces = adapter.suggest_traces(&failure);
        assert!(traces.contains(&"parser::*".to_string()));
    }

    #[test]
    fn test_suite_command_unit() {
        let adapter = CargoTestAdapter;
        let cmd = adapter.suite_command(
            Path::new("/project"),
            Some(TestLevel::Unit),
            &HashMap::new(),
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        assert!(cmd.args.contains(&"--lib".to_string()));
        assert!(cmd.args.contains(&"--format".to_string()));
    }

    #[test]
    fn test_single_test_command() {
        let adapter = CargoTestAdapter;
        let cmd = adapter.single_test_command(
            Path::new("/project"),
            "parser::tests::test_empty_input",
        ).unwrap();
        assert_eq!(cmd.program, "cargo");
        // No --exact: substring matching allows both full paths and partial names
        assert!(!cmd.args.contains(&"--exact".to_string()));
        assert!(cmd.args.contains(&"parser::tests::test_empty_input".to_string()));
    }
}
