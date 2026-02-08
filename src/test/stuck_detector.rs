use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::TestProgress;

/// Result of stuck detection analysis.
pub struct StuckInfo {
    pub elapsed_ms: u64,
    pub diagnosis: String,
    pub suggested_traces: Vec<String>,
}

/// Multi-signal stuck detector.
/// Runs in parallel with test subprocess, monitors:
/// 1. CPU time delta (every 2s)
/// 2. Stack sampling (triggered when suspicious)
///
/// When shared progress is available, the detector skips stuck diagnosis
/// if all test suites have already finished (process is just cleaning up).
pub struct StuckDetector {
    pid: u32,
    hard_timeout_ms: u64,
    progress: Option<Arc<Mutex<TestProgress>>>,
}

impl StuckDetector {
    pub fn new(pid: u32, hard_timeout_ms: u64) -> Self {
        Self { pid, hard_timeout_ms, progress: None }
    }

    pub fn with_progress(mut self, progress: Arc<Mutex<TestProgress>>) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Check if test suites have finished (process is just exiting, not stuck).
    fn suites_finished(&self) -> bool {
        if let Some(ref p) = self.progress {
            let guard = p.lock().unwrap();
            guard.phase == super::TestPhase::SuitesFinished
        } else {
            false
        }
    }

    /// Run the detection loop. Returns Some(StuckInfo) if stuck, None if process exits first.
    pub async fn run(self) -> Option<StuckInfo> {
        let start = Instant::now();
        let mut prev_cpu_ns: Option<u64> = None;
        let mut suspicious_since: Option<Instant> = None;
        let mut zero_delta_count = 0u32;
        let mut constant_high_count = 0u32;

        loop {
            // Check if process is still alive
            let alive = unsafe { libc::kill(self.pid as i32, 0) } == 0;
            if !alive {
                return None; // Process exited — not stuck
            }

            // Check hard timeout
            let elapsed = start.elapsed();
            if elapsed.as_millis() as u64 >= self.hard_timeout_ms {
                // Even on hard timeout, if suites finished, it's not stuck
                if self.suites_finished() {
                    return None;
                }
                return Some(StuckInfo {
                    elapsed_ms: elapsed.as_millis() as u64,
                    diagnosis: "Hard timeout reached".to_string(),
                    suggested_traces: vec![],
                });
            }

            // CPU time sampling — includes child processes (e.g. cargo → rustc)
            let cpu_ns = get_process_tree_cpu_ns(self.pid);

            if let Some(prev) = prev_cpu_ns {
                let delta = cpu_ns.saturating_sub(prev);
                let sample_interval_ns = 2_000_000_000u64; // 2 seconds

                if delta == 0 {
                    // CPU idle — but if suites already finished, the process is
                    // just winding down (e.g. cargo waiting for child reap). Not stuck.
                    if self.suites_finished() {
                        // Reset and keep waiting for clean exit
                        suspicious_since = None;
                        zero_delta_count = 0;
                    } else {
                        // Potential deadlock
                        zero_delta_count += 1;
                        constant_high_count = 0;

                        if suspicious_since.is_none() {
                            suspicious_since = Some(Instant::now());
                        }
                    }
                } else if delta > sample_interval_ns * 80 / 100 {
                    // CPU near 100% — potential infinite loop
                    constant_high_count += 1;
                    zero_delta_count = 0;

                    if suspicious_since.is_none() {
                        suspicious_since = Some(Instant::now());
                    }
                } else {
                    // Normal activity — reset
                    zero_delta_count = 0;
                    constant_high_count = 0;
                    suspicious_since = None;
                }

                // Trigger stack sampling after ~6s of suspicious signals (3 samples)
                if let Some(since) = suspicious_since {
                    if since.elapsed() > Duration::from_secs(6) {
                        let diagnosis = if zero_delta_count >= 3 {
                            "Deadlock: 0% CPU, process completely blocked".to_string()
                        } else if constant_high_count >= 3 {
                            "Infinite loop: 100% CPU, no output progress".to_string()
                        } else {
                            "Process appears stuck".to_string()
                        };

                        return Some(StuckInfo {
                            elapsed_ms: start.elapsed().as_millis() as u64,
                            diagnosis,
                            suggested_traces: vec![],
                        });
                    }
                }
            }

            prev_cpu_ns = Some(cpu_ns);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
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
    async fn test_stuck_detector_returns_none_for_fast_exit() {
        let mut child = tokio::process::Command::new("true")
            .spawn()
            .unwrap();
        let pid = child.id().unwrap();
        // Wait for child to fully exit (avoid zombie keeping PID alive)
        let _ = child.wait().await;
        let detector = StuckDetector::new(pid, 5000);
        let result = detector.run().await;
        assert!(result.is_none());
    }

    #[test]
    fn test_cpu_sample_parsing() {
        let pid = std::process::id();
        let time = get_process_cpu_ns(pid);
        assert!(time > 0, "Should get non-zero CPU time for current process");
    }
}
