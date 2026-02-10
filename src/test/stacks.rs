use super::adapter::ThreadStack;

/// Check if a process is alive. Returns true if the process exists,
/// even if we lack permission to signal it (EPERM).
pub fn is_process_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Collect all descendant PIDs of a process via recursive `pgrep -P`.
fn collect_descendants(pid: u32, result: &mut Vec<i32>) {
    let output = std::process::Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output();
    if let Ok(o) = output {
        if o.status.success() {
            for line in String::from_utf8_lossy(&o.stdout).split_whitespace() {
                if let Ok(child_pid) = line.parse::<i32>() {
                    // Recurse into grandchildren first (depth-first)
                    collect_descendants(child_pid as u32, result);
                    result.push(child_pid);
                }
            }
        }
    }
}

/// Kill a process and its entire descendant tree.
/// Finds all children recursively via `pgrep -P`, kills bottom-up (leaves first),
/// then kills the root. Also reaps zombies.
pub fn kill_process_tree(pid: u32) {
    let mut descendants = Vec::new();
    collect_descendants(pid, &mut descendants);

    if !descendants.is_empty() {
        tracing::info!(
            "Killing process tree: root PID {} + {} descendants {:?}",
            pid, descendants.len(), descendants
        );
    }

    // Kill descendants bottom-up (deepest children first)
    for &child_pid in &descendants {
        unsafe {
            libc::kill(child_pid, libc::SIGKILL);
        }
    }

    // Kill the root process
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }

    // Brief wait for signals to be delivered
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Reap zombies (only works if we're the parent)
    for &child_pid in &descendants {
        unsafe {
            libc::waitpid(child_pid, std::ptr::null_mut(), libc::WNOHANG);
        }
    }
    unsafe {
        libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG);
    }
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
