//! `strobe setup-vision` — automated Python venv + OmniParser model setup.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Standard strobe home directory.
fn strobe_home() -> PathBuf {
    dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".strobe")
}

struct ModelsStatus {
    yolo_ok: bool,
    yolo_size_mb: f64,
    florence2_ok: bool,
    florence2_size_mb: f64,
}

/// Check if OmniParser v2.0 models are installed.
fn check_models_installed(models_dir: &Path) -> ModelsStatus {
    let yolo_path = models_dir.join("icon_detect/model.pt");
    let florence2_path = models_dir.join("icon_caption/model.safetensors");

    let yolo_size = yolo_path.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1024.0 / 1024.0;
    let florence2_size = florence2_path.metadata().map(|m| m.len()).unwrap_or(0) as f64 / 1024.0 / 1024.0;

    ModelsStatus {
        yolo_ok: yolo_size > 20.0,        // Fine-tuned is ~39MB, generic COCO is ~6MB
        yolo_size_mb: yolo_size,
        florence2_ok: florence2_size > 500.0, // Fine-tuned is ~1GB
        florence2_size_mb: florence2_size,
    }
}

/// Find a suitable Python 3.10-3.12 interpreter.
fn find_suitable_python() -> Option<String> {
    for ver in ["3.12", "3.11", "3.10"] {
        let candidate = format!("python{}", ver);
        if check_python_version(&candidate) {
            return Some(candidate);
        }
    }
    if check_python_version("python3") {
        return Some("python3".to_string());
    }
    None
}

/// Verify a Python binary exists and is version 3.10-3.12.
fn check_python_version(python: &str) -> bool {
    let output = Command::new(python).args(["--version"]).output();
    match output {
        Ok(out) if out.status.success() => {
            let version_str = String::from_utf8_lossy(&out.stdout);
            if let Some(ver) = version_str.strip_prefix("Python ") {
                let parts: Vec<&str> = ver.trim().split('.').collect();
                if parts.len() >= 2 {
                    if let (Ok(3), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                        return (10..=12).contains(&minor);
                    }
                }
            }
            false
        }
        _ => false,
    }
}

/// Find the vision-sidecar directory.
fn find_sidecar_dir() -> Option<PathBuf> {
    // 1. Development: CARGO_MANIFEST_DIR
    let dev_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vision-sidecar");
    if dev_path.is_dir() {
        return Some(dev_path);
    }

    // 2. Next to binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("vision-sidecar");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }

    // 3. Standard install location
    let candidate = strobe_home().join("vision-sidecar");
    if candidate.is_dir() {
        return Some(candidate);
    }

    None
}

/// Run a command, inheriting stdout/stderr so the user sees progress.
fn run(cmd: &str, args: &[&str]) -> crate::Result<()> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| crate::Error::Internal(format!("Failed to run {} {:?}: {}", cmd, args, e)))?;
    if !status.success() {
        return Err(crate::Error::Internal(format!(
            "{} {:?} exited with {}",
            cmd, args, status
        )));
    }
    Ok(())
}

/// Ensure flash_attn stub exists in the venv (Florence-2 imports it but it's CUDA-only).
fn ensure_flash_attn_stub(venv_python: &Path) -> crate::Result<()> {
    let output = Command::new(venv_python)
        .args(["-c", "import flash_attn"])
        .output()
        .map_err(|e| crate::Error::Internal(format!("Failed to check flash_attn: {}", e)))?;

    if !output.status.success() {
        // Run setup_models.py's stub creation via Python
        let site_pkg_output = Command::new(venv_python)
            .args(["-c", "import site; print(site.getsitepackages()[0])"])
            .output()
            .map_err(|e| crate::Error::Internal(format!("Failed to get site-packages: {}", e)))?;

        let site_packages = String::from_utf8_lossy(&site_pkg_output.stdout).trim().to_string();
        let stub_dir = Path::new(&site_packages).join("flash_attn");
        std::fs::create_dir_all(&stub_dir)
            .map_err(|e| crate::Error::Internal(format!("Failed to create flash_attn stub dir: {}", e)))?;
        std::fs::write(
            stub_dir.join("__init__.py"),
            "\"\"\"Stub for flash_attn on non-CUDA platforms.\"\"\"\n",
        )
        .map_err(|e| crate::Error::Internal(format!("Failed to write flash_attn stub: {}", e)))?;
        println!("  Created flash_attn stub (macOS/CPU).");
    }

    Ok(())
}

/// Main entry point for `strobe setup-vision`.
pub fn setup_vision() -> crate::Result<()> {
    let home = strobe_home();
    let models_dir = home.join("models");
    let venv_dir = home.join("vision-env");
    let venv_python = venv_dir.join("bin/python");
    let venv_pip = venv_dir.join("bin/pip");

    println!("Strobe Vision Setup");
    println!("===================\n");

    // Check current state
    let status = check_models_installed(&models_dir);
    let venv_ok = venv_python.exists();

    if status.yolo_ok && status.florence2_ok && venv_ok {
        // Even when models are installed, ensure flash_attn stub exists (macOS/CPU)
        ensure_flash_attn_stub(&venv_python)?;

        println!("Already installed:");
        println!("  YOLO icon detection:   {:.1} MB", status.yolo_size_mb);
        println!("  Florence-2 captioning: {:.0} MB", status.florence2_size_mb);
        println!("  Python venv:           {}", venv_python.display());
        println!("\nVision is ready. Nothing to do.");
        return Ok(());
    }

    // Find Python
    let python = find_suitable_python().ok_or_else(|| {
        crate::Error::Internal(
            "No Python 3.10-3.12 found. Install Python 3.12: https://www.python.org/downloads/"
                .to_string(),
        )
    })?;
    println!("Python: {}", python);

    // Find sidecar source
    let sidecar_dir = find_sidecar_dir().ok_or_else(|| {
        crate::Error::Internal(
            "vision-sidecar/ directory not found next to strobe binary or in ~/.strobe/. Reinstall strobe."
                .to_string(),
        )
    })?;
    println!("Sidecar: {}\n", sidecar_dir.display());

    // Create venv
    if !venv_ok {
        println!("Creating Python virtual environment...");
        run(&python, &["-m", "venv", &venv_dir.to_string_lossy()])?;
        println!("  {}\n", venv_dir.display());
    } else {
        println!("Venv exists: {}\n", venv_dir.display());
    }

    // Install dependencies
    println!("Installing Python dependencies (~2 GB download)...");
    let pip = venv_pip.to_string_lossy().to_string();
    let sidecar = sidecar_dir.to_string_lossy().to_string();

    let req_file = sidecar_dir.join("requirements.txt");
    if req_file.exists() {
        run(&pip, &["install", "-r", &req_file.to_string_lossy()])?;
    }
    // Install sidecar package itself
    run(&pip, &["install", "-e", &sidecar])?;
    println!();

    // Download models
    let status = check_models_installed(&models_dir);
    if !status.yolo_ok || !status.florence2_ok {
        println!("Downloading OmniParser v2.0 models (~1.5 GB)...");
        let setup_script = sidecar_dir.join("setup_models.py");
        let vpy = venv_python.to_string_lossy().to_string();
        run(&vpy, &[&setup_script.to_string_lossy()])?;
        println!();
    } else {
        println!("Models already installed.\n");
    }

    // Validate
    println!("Validating...");
    let vpy = venv_python.to_string_lossy().to_string();
    let validate = Command::new(&vpy)
        .args([
            "-c",
            "from strobe_vision.omniparser import OmniParser; p = OmniParser(); p.load(); print('  Models loaded OK')",
        ])
        .status();

    match validate {
        Ok(s) if s.success() => {}
        _ => {
            println!("  WARNING: Validation failed. Vision may not work correctly.");
            println!("  Debug: {} -m strobe_vision.server", venv_python.display());
        }
    }

    let final_status = check_models_installed(&models_dir);
    println!("\n===================");
    println!("Vision setup complete!");
    println!("  Venv:   {}", venv_dir.display());
    println!("  Models: {}", models_dir.display());
    println!("  YOLO:   {:.1} MB", final_status.yolo_size_mb);
    println!("  Flo-2:  {:.0} MB", final_status.florence2_size_mb);
    println!("\nUse: debug_ui(sessionId, mode=\"both\", vision=true)");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_python() {
        let result = find_suitable_python();
        if let Some(ref python) = result {
            assert!(python.contains("python"));
        }
    }

    #[test]
    fn test_strobe_home() {
        let dir = strobe_home();
        assert!(dir.to_string_lossy().contains(".strobe"));
    }

    #[test]
    fn test_models_status_empty() {
        let dir = tempfile::tempdir().unwrap();
        let status = check_models_installed(dir.path());
        assert!(!status.yolo_ok);
        assert!(!status.florence2_ok);
        assert_eq!(status.yolo_size_mb, 0.0);
        assert_eq!(status.florence2_size_mb, 0.0);
    }

    #[test]
    fn test_check_python_version_invalid() {
        assert!(!check_python_version("nonexistent_python_binary"));
    }

    #[test]
    fn test_find_sidecar_dir() {
        let result = find_sidecar_dir();
        if let Some(ref dir) = result {
            assert!(dir.to_string_lossy().contains("vision-sidecar"));
        }
    }
}
