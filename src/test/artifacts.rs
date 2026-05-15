//! Per-test artifact directories.
//!
//! Each test gets a sanitized subdirectory under `<run_dir>/tests/` where
//! stdout, stderr, and console output are captured. The dir name is derived
//! deterministically from the test id (slashes → `_`, `::` preserved as `__`,
//! truncated with a hash suffix when too long).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

impl ConsoleLevel {
    fn as_str(self) -> &'static str {
        match self {
            ConsoleLevel::Log => "LOG",
            ConsoleLevel::Info => "INFO",
            ConsoleLevel::Warn => "WARN",
            ConsoleLevel::Error => "ERROR",
            ConsoleLevel::Debug => "DEBUG",
        }
    }
}

pub struct Artifacts {
    root: PathBuf,
}

impl Artifacts {
    pub fn new(run_dir: &Path) -> Self {
        Self {
            root: run_dir.to_path_buf(),
        }
    }

    pub fn create_for(&self, test_id: &str) -> io::Result<PathBuf> {
        let safe = sanitize(test_id);
        let dir = self.root.join("tests").join(&safe);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn writer_for(&self, test_id: &str) -> io::Result<ArtifactWriter> {
        let dir = self.create_for(test_id)?;
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("stdout.log"))?;
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("stderr.log"))?;
        let console = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("console.log"))?;
        Ok(ArtifactWriter {
            dir,
            stdout,
            stderr,
            console,
        })
    }
}

pub struct ArtifactWriter {
    dir: PathBuf,
    stdout: File,
    stderr: File,
    console: File,
}

impl ArtifactWriter {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn write_stdout(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stdout.write_all(bytes)
    }

    pub fn write_stderr(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stderr.write_all(bytes)
    }

    pub fn write_console(&mut self, level: ConsoleLevel, message: &str) -> io::Result<()> {
        writeln!(self.console, "{} {}", level.as_str(), message)
    }

    pub fn close(self) {
        // Files drop here, flushing buffers (File::drop on POSIX closes the fd).
    }
}

/// Deterministically sanitize a test id into a directory name.
///
/// Rules:
/// - `::` is preserved as `__` (test framework separator).
/// - All other non-alphanumeric characters collapse into single `_`.
/// - Runs of `_` collapse to a single `_` (but the `__` from `::` survives).
/// - Leading and trailing `_` are trimmed.
/// - Names ≥200 chars are truncated to 199 and suffixed with `_<8-char hash>`
///   so distinct inputs that share a long prefix remain distinct on disk.
fn sanitize(id: &str) -> String {
    // Step 1: replace `::` with a sentinel so it survives the collapse step.
    const SENTINEL: char = '\u{0001}';
    let with_sentinel = id.replace("::", &SENTINEL.to_string().repeat(2));

    // Step 2: any non-alphanumeric (other than the sentinel) becomes `_`.
    let mapped: String = with_sentinel
        .chars()
        .map(|c| {
            if c == SENTINEL {
                c
            } else if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // Step 3: collapse runs of `_` to a single `_`.
    let mut collapsed = String::with_capacity(mapped.len());
    let mut prev_us = false;
    for c in mapped.chars() {
        if c == '_' {
            if !prev_us {
                collapsed.push('_');
            }
            prev_us = true;
        } else {
            collapsed.push(c);
            prev_us = false;
        }
    }

    // Step 4: restore sentinel runs into `__`.
    let restored = collapsed.replace(&SENTINEL.to_string().repeat(2), "__");

    // Step 5: trim leading/trailing `_`.
    let trimmed = restored.trim_matches('_').to_string();

    // Step 6: truncate with hash suffix when long.
    if trimmed.len() < 200 {
        return trimmed;
    }
    let suffix = format!("_{}", short_hash(id));
    let max_prefix = 200usize.saturating_sub(suffix.len());
    let mut prefix: String = trimmed.chars().take(max_prefix).collect();
    // Avoid ending the prefix on `_` since we add `_<hash>` ourselves.
    while prefix.ends_with('_') {
        prefix.pop();
    }
    format!("{}{}", prefix, suffix)
}

fn short_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    let v = h.finish();
    format!("{:08x}", v as u32 ^ ((v >> 32) as u32))
}
