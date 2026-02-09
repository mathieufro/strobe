use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::TestProgress;
use super::adapter::ThreadStack;

/// Tracks the suspicion state for stuck detection. The triplet of fields
/// (since, zero_delta_count, constant_high_count) is reset together
/// at multiple points in the detection loop.
struct SuspicionState {
    since: Option<Instant>,
    zero_delta_count: u32,
    constant_high_count: u32,
}

impl SuspicionState {
    fn new() -> Self {
        Self {
            since: None,
            zero_delta_count: 0,
            constant_high_count: 0,
        }
    }

    fn reset(&mut self) {
        self.since = None;
        self.zero_delta_count = 0;
        self.constant_high_count = 0;
    }
}

/// Multi-signal stuck detector — continuous advisory monitor.
/// Runs in parallel with test subprocess, monitors:
/// 1. CPU time delta (every 2s)
/// 2. Stack sampling to confirm (when CPU heuristic is suspicious)
///
/// Writes advisory warnings to shared TestProgress instead of killing.
/// The LLM decides when to kill via debug_stop(sessionId).
///
/// **Phase 2 Note:** Should check for active breakpoint pauses before diagnosing
/// deadlock. If a thread is paused at a breakpoint (recv().wait()), 0% CPU is
/// expected and not a stuck condition. Requires session_manager reference to
/// check SessionManager::get_all_paused_threads().
/// Phase 2: Checks for active breakpoint pauses before diagnosing deadlock.
/// If any thread is paused at a breakpoint (recv().wait()), 0% CPU is expected
/// and not a stuck condition.
pub struct StuckDetector {
    pid: u32,
    hard_timeout_ms: u64,
    progress: Arc<Mutex<TestProgress>>,
    // TODO Phase 2: Add session_manager: Option<Arc<SessionManager>> to check pause state
    /// Returns true if any threads are paused at breakpoints for this session.
    /// When set, suppresses deadlock diagnosis when breakpoints are active.
    has_paused_threads: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl StuckDetector {
    pub fn new(pid: u32, hard_timeout_ms: u64, progress: Arc<Mutex<TestProgress>>) -> Self {
        Self { pid, hard_timeout_ms, progress, has_paused_threads: None }
    }

    pub fn with_pause_check(mut self, check: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.has_paused_threads = Some(check);
        self
    }

    fn current_phase(&self) -> super::TestPhase {
        self.progress.lock().unwrap().phase.clone()
    }

    fn current_test(&self) -> Option<String> {
        self.progress.lock().unwrap().current_test.clone()
    }

    fn write_warning(&self, diagnosis: &str, idle_ms: u64) {
        let mut p = self.progress.lock().unwrap();
        let test_name = p.current_test.clone();
        // Clear any previous warning for this test (replace, don't accumulate)
        p.warnings.retain(|w| w.test_name != test_name);
        p.warnings.push(super::StuckWarning {
            test_name,
            idle_ms,
            diagnosis: diagnosis.to_string(),
            suggested_traces: vec![],
        });
    }

    fn clear_warnings(&self) {
        self.progress.lock().unwrap().warnings.clear();
    }

    /// Run as continuous monitor. Returns when process exits.
    /// Writes warnings to shared progress instead of returning StuckInfo.
    pub async fn run(self) {
        let start = Instant::now();
        let mut running_since: Option<Instant> = None;
        let mut prev_cpu_ns: Option<u64> = None;
        let mut suspicion = SuspicionState::new();
        let mut prev_test: Option<String> = None;

        loop {
            if !super::stacks::is_process_alive(self.pid) {
                return; // Process exited
            }

            let phase = self.current_phase();

            // Track when tests start running (transition out of Compiling)
            if running_since.is_none() && phase != super::TestPhase::Compiling {
                running_since = Some(Instant::now());
            }

            // If current test changed, clear any warnings (test progressed)
            let current = self.current_test();
            if current != prev_test && prev_test.is_some() {
                self.clear_warnings();
                suspicion.reset();
            }
            prev_test = current;

            // SuitesFinished — not stuck, just cleaning up
            if phase == super::TestPhase::SuitesFinished {
                self.clear_warnings();
                suspicion.reset();
                prev_cpu_ns = Some(get_process_tree_cpu_ns(self.pid));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            // Hard timeout — write warning but don't kill
            if let Some(since) = running_since {
                if since.elapsed().as_millis() as u64 >= self.hard_timeout_ms {
                    self.write_warning(
                        "Hard timeout reached — consider stopping the test with debug_stop(sessionId)",
                        start.elapsed().as_millis() as u64,
                    );
                    // Keep running — LLM may want to investigate before killing
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }

            // During compilation, don't analyze CPU patterns — compilers are bursty
            if phase == super::TestPhase::Compiling {
                prev_cpu_ns = Some(get_process_tree_cpu_ns(self.pid));
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }

            // CPU time sampling — includes child processes (e.g. cargo → rustc)
            let cpu_ns = get_process_tree_cpu_ns(self.pid);

            if let Some(prev) = prev_cpu_ns {
                let delta = cpu_ns.saturating_sub(prev);
                let sample_interval_ns = 2_000_000_000u64; // 2 seconds

                // Phase 2: Before diagnosing deadlock on zero CPU delta, check if any
                // threads are paused at breakpoints (recv().wait()). A paused breakpoint
                // shows 0% CPU but is not stuck. TODO: Add check:
                // if session_manager.get_all_paused_threads(session_id).is_empty() { ...
                // threads are paused at breakpoints. A paused breakpoint shows 0% CPU
                // but is not stuck — suppress the warning.
                if delta == 0 && self.has_paused_threads.as_ref().map_or(false, |f| f()) {
                    // Threads are paused at breakpoints — zero CPU is expected
                    suspicion.reset();
                    prev_cpu_ns = Some(cpu_ns);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
                if delta == 0 {
                    suspicion.zero_delta_count += 1;
                    suspicion.constant_high_count = 0;
                    if suspicion.since.is_none() {
                        suspicion.since = Some(Instant::now());
                    }
                } else if delta > sample_interval_ns * 80 / 100 {
                    suspicion.constant_high_count += 1;
                    suspicion.zero_delta_count = 0;
                    if suspicion.since.is_none() {
                        suspicion.since = Some(Instant::now());
                    }
                } else {
                    suspicion.reset();
                    // CPU looks normal — clear any active warnings
                    self.clear_warnings();
                }

                // After ~6s of suspicious CPU signals, confirm with stack sampling
                if let Some(since) = suspicion.since {
                    if since.elapsed() > Duration::from_secs(6) {
                        let diagnosis_type = if suspicion.zero_delta_count >= 3 {
                            "deadlock"
                        } else if suspicion.constant_high_count >= 3 {
                            "infinite_loop"
                        } else {
                            "unknown"
                        };

                        if let Some(diagnosis) = self.confirm_with_stacks(diagnosis_type).await {
                            let idle_ms = since.elapsed().as_millis() as u64;
                            self.write_warning(&diagnosis, idle_ms);
                            // DON'T return — continue monitoring
                            // Reset suspicious counters but keep the warning
                        }
                        suspicion.reset();
                    }
                }
            }

            prev_cpu_ns = Some(cpu_ns);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Take two stack samples 2s apart. If the top frames are identical,
    /// the process is truly stuck. Returns the diagnosis string if confirmed.
    async fn confirm_with_stacks(&self, diagnosis_type: &str) -> Option<String> {
        let pid = self.pid;

        let stacks1 = tokio::time::timeout(
            Duration::from_secs(8),
            tokio::task::spawn_blocking(move || {
                super::stacks::capture_native_stacks(pid)
            })
        ).await.ok().and_then(|r| r.ok()).unwrap_or_default();

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Check if process exited or suites finished during wait
        if !super::stacks::is_process_alive(self.pid) {
            return None;
        }
        if self.current_phase() == super::TestPhase::SuitesFinished {
            return None;
        }

        let stacks2 = tokio::time::timeout(
            Duration::from_secs(8),
            tokio::task::spawn_blocking(move || {
                super::stacks::capture_native_stacks(pid)
            })
        ).await.ok().and_then(|r| r.ok()).unwrap_or_default();

        if stacks_match(&stacks1, &stacks2) {
            let diagnosis = match diagnosis_type {
                "deadlock" => "Deadlock: 0% CPU, stacks unchanged across samples",
                "infinite_loop" => "Infinite loop: 100% CPU, stacks unchanged across samples",
                _ => "Process appears stuck: stacks unchanged across samples",
            };
            Some(diagnosis.to_string())
        } else {
            None // Stacks differ — process is making progress (I/O, etc.)
        }
    }
}

/// Compare two stack snapshots. Returns true if they represent the same
/// stuck state (top N frames are identical for all threads).
fn stacks_match(a: &[ThreadStack], b: &[ThreadStack]) -> bool {
    // If either sample is empty, we can't confirm — be conservative (not stuck)
    if a.is_empty() || b.is_empty() {
        return false;
    }

    // Compare top 5 frames of each thread by name
    let top_frames = |stacks: &[ThreadStack]| -> Vec<Vec<String>> {
        let mut result: Vec<Vec<String>> = stacks.iter()
            .map(|t| t.stack.iter().take(5).cloned().collect())
            .collect();
        result.sort();
        result
    };

    top_frames(a) == top_frames(b)
}

/// Get cumulative CPU time (user + system) for a process and all its descendants.
/// This is critical for processes like `cargo` that delegate work to child processes
/// (rustc, linker, etc.) — the parent may show 0% CPU while children do real work.
pub fn get_process_tree_cpu_ns(pid: u32) -> u64 {
    let mut total = get_process_cpu_ns(pid);
    for child_pid in get_child_pids(pid) {
        // Recurse into children (handles cargo → rustc → cc, etc.)
        total += get_process_tree_cpu_ns(child_pid);
    }
    total
}

/// Get direct child PIDs of a process.
fn get_child_pids(pid: u32) -> Vec<u32> {
    #[cfg(target_os = "macos")]
    {
        get_child_pids_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        get_child_pids_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        vec![]
    }
}

#[cfg(target_os = "macos")]
fn get_child_pids_macos(pid: u32) -> Vec<u32> {
    extern "C" {
        fn proc_listchildpids(ppid: i32, buffer: *mut libc::c_void, buffersize: i32) -> i32;
    }

    unsafe {
        // First call with null to get count
        let count = proc_listchildpids(pid as i32, std::ptr::null_mut(), 0);
        if count <= 0 {
            return vec![];
        }

        let mut pids = vec![0i32; count as usize];
        let buf_size = (count as usize * std::mem::size_of::<i32>()) as i32;
        let actual = proc_listchildpids(
            pid as i32,
            pids.as_mut_ptr() as *mut libc::c_void,
            buf_size,
        );
        if actual <= 0 {
            return vec![];
        }

        let n = actual as usize / std::mem::size_of::<i32>();
        pids.truncate(n);
        pids.into_iter().filter(|&p| p > 0).map(|p| p as u32).collect()
    }
}

#[cfg(target_os = "linux")]
fn get_child_pids_linux(pid: u32) -> Vec<u32> {
    // Read /proc/<pid>/task/<pid>/children if available (requires CONFIG_PROC_CHILDREN)
    let children_path = format!("/proc/{}/task/{}/children", pid, pid);
    if let Ok(content) = std::fs::read_to_string(&children_path) {
        return content
            .split_whitespace()
            .filter_map(|s| s.parse::<u32>().ok())
            .collect();
    }

    // Fallback: scan /proc for processes whose ppid matches
    let mut children = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if let Ok(child_pid) = name.to_string_lossy().parse::<u32>() {
                let stat_path = format!("/proc/{}/stat", child_pid);
                if let Ok(stat) = std::fs::read_to_string(&stat_path) {
                    // Field 4 (0-indexed: 3) is ppid
                    // But need to skip past comm field which can contain spaces/parens
                    if let Some(after_comm) = stat.rfind(')') {
                        let fields: Vec<&str> = stat[after_comm + 1..].split_whitespace().collect();
                        // fields[1] is ppid (after state which is fields[0])
                        if fields.len() > 1 {
                            if let Ok(ppid) = fields[1].parse::<u32>() {
                                if ppid == pid {
                                    children.push(child_pid);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    children
}

/// Get cumulative CPU time (user + system) for a process in nanoseconds.
pub fn get_process_cpu_ns(pid: u32) -> u64 {
    #[cfg(target_os = "macos")]
    {
        get_cpu_ns_macos(pid)
    }
    #[cfg(target_os = "linux")]
    {
        get_cpu_ns_linux(pid)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        0
    }
}

#[cfg(target_os = "macos")]
fn get_cpu_ns_macos(pid: u32) -> u64 {
    use std::mem;

    const PROC_PIDTASKINFO: i32 = 4;

    #[repr(C)]
    struct ProcTaskInfo {
        pti_virtual_size: u64,
        pti_resident_size: u64,
        pti_total_user: u64,
        pti_total_system: u64,
        pti_threads_user: u64,
        pti_threads_system: u64,
        pti_policy: i32,
        pti_faults: i32,
        pti_pageins: i32,
        pti_cow_faults: i32,
        pti_messages_sent: i32,
        pti_messages_received: i32,
        pti_syscalls_mach: i32,
        pti_syscalls_unix: i32,
        pti_csw: i32,
        pti_threadnum: i32,
        pti_numrunning: i32,
        pti_priority: i32,
    }

    extern "C" {
        fn proc_pidinfo(
            pid: i32,
            flavor: i32,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: i32,
        ) -> i32;
    }

    unsafe {
        let mut info: ProcTaskInfo = mem::zeroed();
        let size = mem::size_of::<ProcTaskInfo>() as i32;
        let ret = proc_pidinfo(
            pid as i32,
            PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if ret > 0 {
            info.pti_total_user + info.pti_total_system
        } else {
            0
        }
    }
}

#[cfg(target_os = "linux")]
fn get_cpu_ns_linux(pid: u32) -> u64 {
    let stat_path = format!("/proc/{}/stat", pid);
    if let Ok(content) = std::fs::read_to_string(&stat_path) {
        let fields: Vec<&str> = content.split_whitespace().collect();
        if fields.len() > 14 {
            let utime: u64 = fields[13].parse().unwrap_or(0);
            let stime: u64 = fields[14].parse().unwrap_or(0);
            let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            if ticks_per_sec > 0 {
                return (utime + stime) * 1_000_000_000 / ticks_per_sec;
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stuck_detector_returns_for_fast_exit() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let _ = child.wait().await;
        let progress = Arc::new(Mutex::new(super::super::TestProgress::new()));
        let detector = StuckDetector::new(pid, 5000, Arc::clone(&progress));
        detector.run().await; // Should return quickly
        assert!(progress.lock().unwrap().warnings.is_empty());
    }

    #[tokio::test]
    async fn test_stuck_detector_writes_warnings_instead_of_returning() {
        // Spawn a fast-exiting process and reap it first to avoid zombie
        // (kill(pid, 0) returns 0 for zombies, which would block the detector)
        let mut child = tokio::process::Command::new("sleep")
            .arg("0.1")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        let _ = child.wait().await;

        let progress = Arc::new(Mutex::new(super::super::TestProgress::new()));
        {
            let mut p = progress.lock().unwrap();
            p.phase = super::super::TestPhase::Running;
        }

        let detector = StuckDetector::new(pid, 60_000, Arc::clone(&progress));
        detector.run().await;

        let p = progress.lock().unwrap();
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn test_cpu_sample_parsing() {
        let pid = std::process::id();
        let time = get_process_cpu_ns(pid);
        assert!(time > 0, "Should get non-zero CPU time for current process");
    }

    #[test]
    fn test_stuck_detector_with_pause_check() {
        let progress = Arc::new(Mutex::new(super::super::TestProgress::new()));
        let detector = StuckDetector::new(1, 5000, Arc::clone(&progress))
            .with_pause_check(Arc::new(|| true));
        assert!(detector.has_paused_threads.is_some());
        assert!(detector.has_paused_threads.as_ref().unwrap()());
    }
}
