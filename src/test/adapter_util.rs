//! Shared helpers for NDJSON-emitting adapter `run` methods.

use std::path::Path;

/// Strip ANSI escape sequences (CSI / SGR / cursor / OSC) from text.
///
/// Used when persisting captured stdout/stderr to a log file. Bun ignores
/// `NO_COLOR` (oven-sh/bun#21136) and frameworks routinely emit color codes
/// when spawned through a pty — those codes are pure noise once written to
/// disk. Stripping in the writer covers every adapter uniformly.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    // CSI sequence — read until a final byte in @..~
                    chars.next();
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence — terminated by BEL (\x07) or ESC \
                    chars.next();
                    while let Some(next) = chars.next() {
                        if next == '\u{07}' {
                            break;
                        }
                        if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    // Two-byte escape (e.g. ESC = / ESC > / ESC c) — drop next.
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Sanitize file + test name into a safe artifact directory.
/// Truncates to ≤208 chars with a hash suffix when too long.
pub fn artifact_dir(file: &str, name: &str) -> String {
    let safe_file = file.replace(['/', '\\', '.'], "_");
    let safe_name: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let combined = format!("tests/{}__{}", safe_file, safe_name);
    if combined.len() > 200 {
        let trunc: String = combined.chars().take(192).collect();
        format!("{}_{}", trunc, short_hash(&combined))
    } else {
        combined
    }
}

fn short_hash(s: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    format!("{:x}", h.finish())
}

/// Time-based pseudo-id for run identifiers (not a real UUID).
pub fn nano_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", ns)
}

/// Read ±3 lines around `line` from `file` (relative to `project_root`).
/// Returns Some(vec![]) when line is 0 (test has no source line) and None
/// only when the file cannot be read.
pub fn read_code_context(project_root: &Path, file: &str, line: u32) -> Option<Vec<String>> {
    if line == 0 {
        return Some(vec![]);
    }
    let path = if Path::new(file).is_absolute() {
        std::path::PathBuf::from(file)
    } else {
        project_root.join(file)
    };
    let text = std::fs::read_to_string(&path).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let idx = line.saturating_sub(1) as usize;
    let lo = idx.saturating_sub(3);
    let hi = (idx + 4).min(lines.len());
    Some(lines[lo..hi].iter().map(|s| s.to_string()).collect())
}
