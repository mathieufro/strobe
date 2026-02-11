//! Vision sidecar process management.
//!
//! Manages a long-running Python process that runs OmniParser v2 for
//! UI element detection. Communication via JSON over stdin/stdout.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
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
            // Wait briefly, then kill
            match child.wait() {
                Ok(_) => tracing::info!("Vision sidecar exited gracefully"),
                Err(_) => {
                    let _ = child.kill();
                    tracing::warn!("Vision sidecar killed after timeout");
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

        let child = Command::new("python3")
            .args(["-m", "strobe_vision.server"])
            .current_dir(&sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Pass through to daemon stderr
            .spawn()
            .map_err(|e| crate::Error::UiQueryFailed(
                format!("Failed to start vision sidecar: {}. Ensure Python 3.10+ is installed with torch, ultralytics, transformers.", e)
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

        // Read response line
        let stdout = child.stdout.as_mut()
            .ok_or_else(|| crate::Error::UiQueryFailed("Sidecar stdout closed".to_string()))?;
        let mut reader = BufReader::new(stdout);
        let mut response_line = String::new();
        reader.read_line(&mut response_line)
            .map_err(|e| crate::Error::UiQueryFailed(format!("Failed to read sidecar response: {}", e)))?;

        serde_json::from_str(&response_line)
            .map_err(|e| crate::Error::UiQueryFailed(format!("Invalid sidecar response: {}", e)))
    }

    fn find_sidecar_dir(&self) -> Result<std::path::PathBuf> {
        // Check relative to binary
        let exe = std::env::current_exe()
            .map_err(|e| crate::Error::Internal(format!("Cannot find exe path: {}", e)))?;
        let exe_dir = exe.parent().unwrap();

        // Development: relative to cargo manifest
        let dev_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vision-sidecar");
        if dev_path.is_dir() {
            return Ok(dev_path);
        }

        // Installed: next to binary
        let installed_path = exe_dir.join("vision-sidecar");
        if installed_path.is_dir() {
            return Ok(installed_path);
        }

        Err(crate::Error::UiQueryFailed(
            format!("Vision sidecar not found. Checked: {:?}, {:?}", dev_path, installed_path)
        ))
    }
}

impl Drop for VisionSidecar {
    fn drop(&mut self) {
        self.shutdown();
    }
}
