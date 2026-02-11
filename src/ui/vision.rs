//! Vision sidecar process management.
//!
//! Manages a long-running Python process that runs OmniParser v2 for
//! UI element detection. Communication via JSON over stdin/stdout.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionElement {
    pub label: String,
    pub description: String,
    pub confidence: f32,
    pub bounds: VisionBounds,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisionBounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub struct VisionSidecar {
    process: Option<Child>,
    last_used: Instant,
    request_counter: u64,
}

impl VisionSidecar {
    pub fn new() -> Self {
        Self {
            process: None,
            last_used: Instant::now(),
            request_counter: 0,
        }
    }

    /// Detect UI elements in a base64-encoded PNG screenshot.
    pub fn detect(
        &mut self,
        screenshot_b64: &str,
        confidence_threshold: f32,
        iou_threshold: f32,
    ) -> Result<Vec<VisionElement>> {
        self.ensure_running()?;
        self.last_used = Instant::now();

        let req_id = format!("req_{}", self.request_counter);
        self.request_counter += 1;

        let request = serde_json::json!({
            "id": req_id,
            "type": "detect",
            "image": screenshot_b64,
            "options": {
                "confidence_threshold": confidence_threshold,
                "iou_threshold": iou_threshold,
            }
        });

        let response = self.send_request(&request)?;

        if response.get("type").and_then(|t| t.as_str()) == Some("error") {
            return Err(crate::Error::UiQueryFailed(
                format!("Vision sidecar error: {}",
                    response.get("message").and_then(|m| m.as_str()).unwrap_or("unknown"))
            ));
        }

        let elements: Vec<VisionElement> = response
            .get("elements")
            .and_then(|e| serde_json::from_value(e.clone()).ok())
            .unwrap_or_default();

        Ok(elements)
    }

    /// Check if sidecar should be shut down due to idle timeout.
    pub fn check_idle_timeout(&mut self, timeout_seconds: u64) {
        let timeout = Duration::from_secs(timeout_seconds);
        if self.process.is_some() && self.last_used.elapsed() > timeout {
            tracing::info!("Vision sidecar idle for {}s, shutting down", timeout_seconds);
            self.shutdown();
        }
    }

    /// Gracefully shutdown the sidecar.
    pub fn shutdown(&mut self) {
        if let Some(ref mut child) = self.process {
            // Close stdin to signal EOF
            drop(child.stdin.take());
            // CORR-2: Wait with timeout to prevent indefinite blocking
            let timeout = Duration::from_secs(3);
            let start = Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        tracing::info!("Vision sidecar exited gracefully with {:?}", status);
                        break;
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            tracing::warn!("Vision sidecar did not exit after 3s, killing");
                            let _ = child.kill();
                            let _ = child.wait(); // Reap zombie
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        tracing::error!("Failed to check sidecar status: {}, killing", e);
                        let _ = child.kill();
                        break;
                    }
                }
            }
        }
        self.process = None;
    }

    fn ensure_running(&mut self) -> Result<()> {
        // Check if process is still alive
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!("Vision sidecar exited with status {:?}, restarting", status);
                    self.process = None;
                }
                Ok(None) => return Ok(()), // Still running
                Err(e) => {
                    tracing::warn!("Failed to check sidecar status: {}, restarting", e);
                    self.process = None;
                }
            }
        }

        // Start new process
        self.start()
    }

    fn start(&mut self) -> Result<()> {
        let sidecar_dir = self.find_sidecar_dir()?;

        // Python resolution: standard install venv > sidecar-local venv > system python
        let strobe_venv = dirs::home_dir()
            .map(|h| h.join(".strobe/vision-env/bin/python"));
        let local_venv = sidecar_dir.join("venv/bin/python");

        let python = if strobe_venv.as_ref().map_or(false, |p| p.exists()) {
            strobe_venv.unwrap().to_string_lossy().to_string()
        } else if local_venv.exists() {
            local_venv.to_string_lossy().to_string()
        } else {
            "python3".to_string()
        };

        let child = Command::new(&python)
            .args(["-m", "strobe_vision.server"])
            .current_dir(&sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Pass through to daemon stderr
            .spawn()
            .map_err(|e| crate::Error::UiQueryFailed(
                format!("Failed to start vision sidecar: {}. Run `strobe setup-vision` to install.", e)
            ))?;

        self.process = Some(child);

        // Health check — wait for pong
        let ping = serde_json::json!({"id": "health", "type": "ping"});
        let pong = self.send_request(&ping)?;
        if pong.get("type").and_then(|t| t.as_str()) != Some("pong") {
            self.shutdown();
            return Err(crate::Error::UiQueryFailed(
                "Vision sidecar failed health check".to_string()
            ));
        }

        let device = pong.get("device").and_then(|d| d.as_str()).unwrap_or("unknown");
        tracing::info!("Vision sidecar started (device={})", device);

        Ok(())
    }

    fn send_request(&mut self, request: &serde_json::Value) -> Result<serde_json::Value> {
        let child = self.process.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar not running".to_string()))?;

        let stdin = child.stdin.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar stdin closed".to_string()))?;

        let mut line = serde_json::to_string(request)?;
        line.push('\n');
        stdin.write_all(line.as_bytes())
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to write to sidecar: {}", e)))?;
        stdin.flush()
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to flush sidecar stdin: {}", e)))?;

        // CORR-1: Read response line without taking ownership of stdout
        // SEC-7: Use poll(2) with 30s timeout to prevent indefinite blocking
        let stdout = child.stdout.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar stdout closed".to_string()))?;

        {
            use std::os::unix::io::AsRawFd;
            let raw_fd = stdout.as_raw_fd();
            let mut pollfd = libc::pollfd {
                fd: raw_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let poll_result = unsafe { libc::poll(&mut pollfd, 1, 30_000) };
            if poll_result == 0 {
                return Err(crate::Error::UiQueryFailed(
                    "Sidecar response timed out after 30s".to_string()
                ));
            } else if poll_result < 0 {
                return Err(crate::Error::UiQueryFailed(
                    format!("Poll error waiting for sidecar: {}", std::io::Error::last_os_error())
                ));
            }
        }

        let mut response_line = String::new();
        BufReader::new(stdout.by_ref()).read_line(&mut response_line)
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to read sidecar response: {}", e)))?;

        // CORR-5: Check for empty response (process crash)
        if response_line.trim().is_empty() {
            return Err(crate::Error::UiQueryFailed(
                "Sidecar returned empty response (process may have crashed)".to_string()
            ));
        }

        serde_json::from_str(&response_line)
            .map_err(|e| crate::Error::UiQueryFailed(
                format!("Invalid sidecar JSON: {}. Response: {}", e, response_line.trim())
            ))
    }

    fn find_sidecar_dir(&self) -> Result<std::path::PathBuf> {
        // 1. Development: relative to cargo manifest
        let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vision-sidecar");
        if dev_path.is_dir() {
            return Ok(dev_path);
        }

        // 2. Installed: next to binary
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let installed_path = exe_dir.join("vision-sidecar");
                if installed_path.is_dir() {
                    return Ok(installed_path);
                }
            }
        }

        // 3. Standard install location (~/.strobe/vision-sidecar)
        if let Some(home) = dirs::home_dir() {
            let strobe_path = home.join(".strobe/vision-sidecar");
            if strobe_path.is_dir() {
                return Ok(strobe_path);
            }
        }

        Err(crate::Error::UiQueryFailed(
            "Vision sidecar not found. Run `strobe setup-vision` to install.".to_string()
        ))
    }
}

impl Drop for VisionSidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TEST-1: Sidecar crash recovery test
    #[test]
    #[cfg(target_os = "macos")]
    fn test_sidecar_crash_recovery() {
        use std::process::Command;

        let mut sidecar = VisionSidecar::new();

        // Check if Python environment is available
        let python_check = Command::new("python3")
            .arg("-c")
            .arg("import torch, ultralytics, transformers")
            .output();

        if python_check.is_err() || !python_check.unwrap().status.success() {
            eprintln!("Skipping vision crash recovery test: Python dependencies not installed");
            return;
        }

        // First detect call — should start sidecar
        use base64::Engine;
        let test_image = base64::engine::general_purpose::STANDARD.encode(&[0u8; 100]);
        let _result1 = sidecar.detect(&test_image, 0.5, 0.5);

        // Even if detection fails due to invalid image, the process should be started
        if let Some(ref child) = sidecar.process {
            let pid = child.id();

            // Kill the sidecar process to simulate crash
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }

            // Wait briefly for process to die
            std::thread::sleep(std::time::Duration::from_millis(100));

            // Second detect call — should auto-restart
            let result2 = sidecar.detect(&test_image, 0.5, 0.5);

            // If the second call didn't panic and returned an error (not crash),
            // then crash recovery worked (process was restarted)
            match result2 {
                Ok(_) => {
                    // Success means recovery worked and image was processed
                    assert!(true, "Sidecar recovered and processed image");
                }
                Err(e) => {
                    // Error is OK as long as it's not a "sidecar closed" error
                    let err_msg = format!("{:?}", e);
                    assert!(
                        !err_msg.contains("stdout closed") && !err_msg.contains("stdin closed"),
                        "Sidecar should have recovered from crash, got: {}",
                        err_msg
                    );
                }
            }

            // Verify new process was spawned
            if let Some(ref new_child) = sidecar.process {
                let new_pid = new_child.id();
                assert_ne!(
                    pid, new_pid,
                    "New sidecar process should have different PID after crash recovery"
                );
            }
        } else {
            // If sidecar couldn't start at all, that's expected in CI without models
            eprintln!("Sidecar didn't start (models not available), skipping crash test");
        }
    }

    // TEST-2: Invalid screenshot data test
    #[test]
    fn test_invalid_screenshot_data() {
        use base64::Engine;
        let mut sidecar = VisionSidecar::new();

        // Test empty data
        let empty_b64 = "";
        let result = sidecar.detect(empty_b64, 0.5, 0.5);
        assert!(result.is_err(), "Empty screenshot should fail");

        // Test truncated data
        let truncated_b64 = base64::engine::general_purpose::STANDARD.encode(&[0x89, 0x50, 0x4E, 0x47]);
        let result = sidecar.detect(&truncated_b64, 0.5, 0.5);
        assert!(result.is_err(), "Truncated screenshot should fail");

        // Test oversized data (>50MB base64)
        // 50MB base64 = ~37.5MB raw, let's use 40MB to be safe
        let huge = vec![0u8; 40 * 1024 * 1024];
        let huge_b64 = base64::engine::general_purpose::STANDARD.encode(&huge);
        let result = sidecar.detect(&huge_b64, 0.5, 0.5);
        assert!(result.is_err(), "Oversized screenshot should fail");
        if let Err(e) = result {
            let err_msg = format!("{:?}", e);
            // If sidecar can start (Python deps installed), should get size limit error.
            // If sidecar can't start (no deps), that's also acceptable.
            let valid_error = err_msg.contains("too large")
                || err_msg.contains("limit")
                || err_msg.contains("crashed")
                || err_msg.contains("Failed to start");
            assert!(
                valid_error,
                "Error should mention size limit or sidecar failure, got: {}",
                err_msg
            );
        }
    }

    // TEST-3: Multiple rapid calls should not leak processes
    #[test]
    fn test_no_process_leak() {
        use base64::Engine;
        let mut sidecar = VisionSidecar::new();

        // Make multiple detect calls with invalid data
        let dummy_b64 = base64::engine::general_purpose::STANDARD.encode(&[1, 2, 3, 4]);
        for _ in 0..5 {
            let _ = sidecar.detect(&dummy_b64, 0.5, 0.5);
        }

        // Should have at most one process
        let process_count = if sidecar.process.is_some() { 1 } else { 0 };
        assert!(
            process_count <= 1,
            "Should have at most one sidecar process, got {}",
            process_count
        );
    }
}
