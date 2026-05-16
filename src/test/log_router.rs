//! Per-test log routing.
//!
//! Sits between the captured stdout/stderr stream and disk. Maintains a
//! buffer-since-last-marker for each stream, and when a framework boundary
//! marker (`(pass) X` / `(fail) X` for bun, `✓` / `×` for vitest, `  ✓  N
//! [proj] › … › X` for playwright …) appears in EITHER stream, flushes both
//! buffers into `<run_dir>/tests/<safe-id>/{stdout,stderr}.log`.
//!
//! At end of run, [`LogRouter::finalize`] writes:
//!   * `<run_dir>/summary.md` — table of every test with status, duration,
//!     and a relative link to its per-test directory.
//!   * `<run_dir>/failures.md` — one section per failure/stall, pointing at
//!     the same directory (which already contains the full per-test
//!     stdout/stderr captured at runtime).
//!
//! Unknown framework → passthrough fallback: everything goes to
//! `<run_dir>/raw.log` and `summary.md` lists the run as "unstructured".

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::test::artifacts::Artifacts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framework {
    Bun,
    Vitest,
    Jest,
    Playwright,
    Mocha,
    Other,
}

impl Framework {
    fn from_name(name: &str) -> Self {
        match name {
            "bun" => Framework::Bun,
            "vitest" => Framework::Vitest,
            "jest" => Framework::Jest,
            "playwright" => Framework::Playwright,
            "mocha" => Framework::Mocha,
            _ => Framework::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn filename(self) -> &'static str {
        match self {
            Stream::Stdout => "stdout.log",
            Stream::Stderr => "stderr.log",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Stalled,
    Skipped,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Stalled => "stalled",
            Status::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone)]
struct TestRow {
    test_id: String,
    safe_dir: String,
    status: Status,
    duration_ms: Option<u64>,
}

pub struct LogRouter {
    run_dir: PathBuf,
    artifacts: Artifacts,
    framework: Framework,

    /// The most recent failed/stalled test's safe_dir. Bun (and friends)
    /// emit failure context AFTER the marker — `^ this test timed out`,
    /// then later a separate `# Unhandled error between tests` block — so
    /// we keep this around to attribute post-marker context correctly.
    /// Cleared when a NEW fail marker arrives (i.e. always points at the
    /// most recent failure).
    last_failed_safe: Option<String>,
    /// How many lines after a fail marker we greedily route into the failed
    /// test's dir instead of pending. Sized for bun's "^ … timed out" line.
    carry_lines_left: u32,
    /// True while we're inside a `# Unhandled error between tests` block;
    /// `false` once the closing `------…` separator passes.
    in_unhandled_block: bool,
    unhandled_separator_seen: bool,

    /// Full unfiltered mirror of every chunk — fallback when boundary
    /// parsing missed something.
    raw_log: Option<File>,
    raw_log_path: PathBuf,

    /// **Live**-appended summary table. Opened at `new()` with a header;
    /// each marker appends one row; `finalize` writes the totals footer.
    /// `tail -f` on this file shows real-time progress.
    summary_log: Option<File>,
    summary_path: PathBuf,

    /// **Live**-appended failures index. Opened with a header; each fail /
    /// stall marker appends a section pointing at the per-test dir.
    failures_log: Option<File>,
    failures_path: PathBuf,

    /// Lines since the last marker, with their originating stream so we
    /// route to the right file in the test's dir.
    pending: Vec<(Stream, String)>,

    /// Partial-line accumulators per stream — bytes arrive in chunks that
    /// may split mid-line.
    buf_stdout: String,
    buf_stderr: String,

    /// In test-completion order. Used by `finalize` to write the totals
    /// footer (the live table is append-only, no sort).
    results: Vec<TestRow>,
    /// safe_dir -> index into `results`, so we can update status if a later
    /// marker overrides (rare; mostly for retries).
    by_safe: HashMap<String, usize>,
}

impl LogRouter {
    pub fn new(run_dir: &Path, framework: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(run_dir)?;
        let artifacts = Artifacts::new(run_dir);
        let raw_log_path = run_dir.join("raw.log");
        let raw_log = Some(File::create(&raw_log_path)?);

        // Open summary.md with the table header so `tail -f` shows progress.
        // The totals footer is written at finalize() once counts are known.
        let summary_path = run_dir.join("summary.md");
        let mut summary_log = File::create(&summary_path)?;
        writeln!(summary_log, "# Test Run — live")?;
        writeln!(summary_log)?;
        writeln!(summary_log, "Raw stream: [raw.log](raw.log)")?;
        writeln!(summary_log, "Failures: [failures.md](failures.md)")?;
        writeln!(summary_log)?;
        writeln!(summary_log, "| Path | Time | Status |")?;
        writeln!(summary_log, "|------|------|--------|")?;
        summary_log.sync_data()?;

        // failures.md with its header; rows append on fail/stalled markers.
        let failures_path = run_dir.join("failures.md");
        let mut failures_log = File::create(&failures_path)?;
        writeln!(failures_log, "# Failures — live")?;
        writeln!(failures_log)?;
        failures_log.sync_data()?;

        Ok(Self {
            run_dir: run_dir.to_path_buf(),
            artifacts,
            framework: Framework::from_name(framework),
            last_failed_safe: None,
            carry_lines_left: 0,
            in_unhandled_block: false,
            unhandled_separator_seen: false,
            raw_log,
            raw_log_path,
            summary_log: Some(summary_log),
            summary_path,
            failures_log: Some(failures_log),
            failures_path,
            pending: Vec::new(),
            buf_stdout: String::new(),
            buf_stderr: String::new(),
            results: Vec::new(),
            by_safe: HashMap::new(),
        })
    }

    /// Feed an ANSI-stripped chunk from stdout.
    pub fn ingest_stdout(&mut self, chunk: &str) -> std::io::Result<()> {
        self.ingest(Stream::Stdout, chunk)
    }

    /// Feed an ANSI-stripped chunk from stderr.
    pub fn ingest_stderr(&mut self, chunk: &str) -> std::io::Result<()> {
        self.ingest(Stream::Stderr, chunk)
    }

    fn ingest(&mut self, stream: Stream, chunk: &str) -> std::io::Result<()> {
        // Always mirror the raw bytes into raw.log first — that's the
        // unfiltered fallback when boundary parsing misses something. Cheap.
        if let Some(f) = self.raw_log.as_mut() {
            f.write_all(chunk.as_bytes())?;
        }
        if self.framework == Framework::Other {
            // Unknown framework → no per-test routing, raw.log already has it.
            return Ok(());
        }
        // Accumulate into the right per-stream buffer, then drain complete
        // lines into a local Vec so we release the borrow before processing.
        let mut lines: Vec<String> = Vec::new();
        {
            let buf = match stream {
                Stream::Stdout => &mut self.buf_stdout,
                Stream::Stderr => &mut self.buf_stderr,
            };
            buf.push_str(chunk);
            while let Some(nl) = buf.find('\n') {
                let line: String = buf.drain(..=nl).collect();
                lines.push(line);
            }
        }
        for line in lines {
            self.process_line(stream, line)?;
        }
        Ok(())
    }

    fn process_line(&mut self, stream: Stream, line: String) -> std::io::Result<()> {
        // 1) Trailing-context carry: the N lines right after a fail marker
        //    typically hold the framework's reason ("^ this test timed out").
        if self.carry_lines_left > 0 {
            if let Some(safe_dir) = self.last_failed_safe.clone() {
                self.write_lines_to_test(&safe_dir, vec![(stream, line)])?;
            }
            self.carry_lines_left -= 1;
            return Ok(());
        }

        // 2) `# Unhandled error between tests` block — bun defers rejections
        //    from a timed-out test until the next event-loop turn, so the
        //    real error + stack lands AFTER the marker, sometimes after the
        //    next test has started its setup. Attribute the whole block to
        //    `last_failed_safe`.
        if self.in_unhandled_block {
            if let Some(safe_dir) = self.last_failed_safe.clone() {
                self.write_lines_to_test(&safe_dir, vec![(stream, line.clone())])?;
            }
            let t = line.trim();
            if t.len() >= 6 && t.chars().all(|c| c == '-') {
                if self.unhandled_separator_seen {
                    // Closing separator — exit block.
                    self.in_unhandled_block = false;
                    self.unhandled_separator_seen = false;
                } else {
                    self.unhandled_separator_seen = true;
                }
            }
            return Ok(());
        }
        if line.trim() == "# Unhandled error between tests" {
            if let Some(safe_dir) = self.last_failed_safe.clone() {
                self.write_lines_to_test(&safe_dir, vec![(stream, line)])?;
                self.in_unhandled_block = true;
                self.unhandled_separator_seen = false;
                return Ok(());
            }
            // No prior failure → fall through to normal handling (pending).
        }

        // Framework file headers (`src/foo.test.ts:` etc.) are structural
        // framing, not test execution. Bun prints headers for every scanned
        // file even when filtering — they shouldn't pollute the first
        // matching test's pending buffer. Route them through raw.log only.
        if is_file_header(&line) {
            return Ok(());
        }

        if let Some((name, status, duration_ms)) = self.parse_marker(&line) {
            // Marker line itself belongs to the just-finishing test.
            let safe_dir = self
                .artifacts
                .create_for(&name)
                .map(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string()
                })
                .unwrap_or_else(|_| "unknown".into());
            // Flush every pending line + the marker line itself into the dir.
            let pending = std::mem::take(&mut self.pending);
            self.write_lines_to_test(&safe_dir, pending)?;
            self.write_lines_to_test(&safe_dir, vec![(stream, line)])?;

            // Record / update result.
            let row = TestRow {
                test_id: name.clone(),
                safe_dir: safe_dir.clone(),
                status,
                duration_ms,
            };
            let is_new = !self.by_safe.contains_key(&safe_dir);
            match self.by_safe.get(&safe_dir).copied() {
                Some(idx) => self.results[idx] = row, // last marker wins (retries)
                None => {
                    self.by_safe.insert(safe_dir.clone(), self.results.len());
                    self.results.push(row);
                }
            }

            // Live append into summary.md so `tail -f` shows progress.
            // Only on the first marker for a given test id to avoid duplicate
            // rows when frameworks emit multiple completion markers.
            if is_new {
                self.append_summary_row(&name, &safe_dir, status, duration_ms)?;
            }
            // Failures get a section in failures.md the moment they happen —
            // agents reading mid-run can dive straight to the failed test.
            if matches!(status, Status::Fail | Status::Stalled) && is_new {
                self.append_failure_section(&name, &safe_dir, status, duration_ms)?;
                // Frameworks (bun) print failure context AFTER the marker.
                // Carry the next 2 lines into this test's dir (covers
                // `^ … timed out` plus a trailing blank), and remember the
                // safe_dir so a later `# Unhandled error between tests`
                // block can be attributed back to the right test.
                self.last_failed_safe = Some(safe_dir.clone());
                self.carry_lines_left = 2;
            }
        } else {
            // Not a marker — accumulate against the next test boundary.
            self.pending.push((stream, line));
        }
        Ok(())
    }

    fn write_lines_to_test(
        &mut self,
        safe_dir: &str,
        lines: Vec<(Stream, String)>,
    ) -> std::io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let dir = self.run_dir.join("tests").join(safe_dir);
        std::fs::create_dir_all(&dir)?;
        // Open append handles per stream; minimal cost vs caching handles
        // and we'd otherwise keep file descriptors per test for the run.
        let stdout_path = dir.join(Stream::Stdout.filename());
        let stderr_path = dir.join(Stream::Stderr.filename());
        let mut f_stdout = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stdout_path)?;
        let mut f_stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&stderr_path)?;
        for (stream, line) in lines {
            match stream {
                Stream::Stdout => f_stdout.write_all(line.as_bytes())?,
                Stream::Stderr => f_stderr.write_all(line.as_bytes())?,
            }
        }
        Ok(())
    }

    /// Try to extract `(name, status, duration_ms)` from a single line for
    /// the current framework. Returns `None` for non-marker lines.
    fn parse_marker(&self, line: &str) -> Option<(String, Status, Option<u64>)> {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        match self.framework {
            Framework::Bun => parse_bun_marker(trimmed),
            Framework::Vitest | Framework::Jest => parse_vitest_marker(trimmed),
            Framework::Playwright => parse_playwright_marker(trimmed),
            Framework::Mocha => parse_mocha_marker(trimmed),
            Framework::Other => None,
        }
    }

    fn append_summary_row(
        &mut self,
        test_id: &str,
        safe_dir: &str,
        status: Status,
        duration_ms: Option<u64>,
    ) -> std::io::Result<()> {
        if let Some(f) = self.summary_log.as_mut() {
            let duration = duration_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "—".into());
            // Human-readable name = link text (greppable for "what was that
            // test about?"), sanitized path = target. They look similar but
            // serve different needs: name preserves punctuation and case;
            // path is the on-disk dir to follow.
            writeln!(
                f,
                "| [{}](tests/{}/) | {} | {} |",
                escape_md(test_id),
                safe_dir,
                duration,
                status.as_str(),
            )?;
            f.sync_data()?;
        }
        Ok(())
    }

    fn append_failure_section(
        &mut self,
        test_id: &str,
        safe_dir: &str,
        status: Status,
        duration_ms: Option<u64>,
    ) -> std::io::Result<()> {
        if let Some(f) = self.failures_log.as_mut() {
            let duration = duration_ms
                .map(|ms| format!(" — {ms} ms"))
                .unwrap_or_default();
            writeln!(
                f,
                "## {} ({}){}",
                escape_md(test_id),
                status.as_str(),
                duration
            )?;
            writeln!(f)?;
            writeln!(
                f,
                "[stdout](tests/{0}/stdout.log) · [stderr](tests/{0}/stderr.log)",
                safe_dir
            )?;
            writeln!(f)?;
            f.sync_data()?;
        }
        Ok(())
    }

    /// Flush any incomplete lines and write the totals footer.
    pub fn finalize(&mut self) -> std::io::Result<()> {
        // Drain stuck partial lines into pending as "stream X" entries.
        if !self.buf_stdout.is_empty() {
            let tail = std::mem::take(&mut self.buf_stdout);
            self.pending.push((Stream::Stdout, tail));
        }
        if !self.buf_stderr.is_empty() {
            let tail = std::mem::take(&mut self.buf_stderr);
            self.pending.push((Stream::Stderr, tail));
        }
        // Anything still pending was trailing chatter after the last marker —
        // park it under a synthetic "post-run" dir so it's still on disk.
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            let dir = self
                .artifacts
                .create_for("__post-run")
                .ok()
                .and_then(|p| {
                    p.file_name()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "__post-run".to_string());
            self.write_lines_to_test(&dir, pending)?;
        }
        // Footer with totals — table rows above were appended live; this
        // closes the file with a stable summary line.
        let total = self.results.len();
        let passed = self.results.iter().filter(|r| r.status == Status::Pass).count();
        let failed = self.results.iter().filter(|r| r.status == Status::Fail).count();
        let stalled = self
            .results
            .iter()
            .filter(|r| r.status == Status::Stalled)
            .count();
        let skipped = self
            .results
            .iter()
            .filter(|r| r.status == Status::Skipped)
            .count();
        if let Some(f) = self.summary_log.as_mut() {
            writeln!(f)?;
            writeln!(
                f,
                "**Totals:** {passed} pass, {failed} fail, {stalled} stalled, {skipped} skipped (of {total} tests)"
            )?;
            f.sync_data()?;
        }
        if let Some(f) = self.failures_log.as_mut() {
            if failed == 0 && stalled == 0 {
                writeln!(f, "No failures.")?;
            } else {
                writeln!(f)?;
                writeln!(
                    f,
                    "**Totals:** {failed} fail, {stalled} stalled (of {total} tests)"
                )?;
            }
            f.sync_data()?;
        }
        Ok(())
    }

    /// Path to the raw mirrored log (everything, unfiltered).
    pub fn raw_log_path(&self) -> &Path {
        &self.raw_log_path
    }

    /// Path to the live summary file — agents read this first.
    pub fn summary_path(&self) -> &Path {
        &self.summary_path
    }

    /// Path to the live failures index.
    pub fn failures_path(&self) -> &Path {
        &self.failures_path
    }
}

fn rank(s: Status) -> u8 {
    match s {
        Status::Fail => 0,
        Status::Stalled => 1,
        Status::Skipped => 2,
        Status::Pass => 3,
    }
}

fn escape_md(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// Recognize bun/vitest/playwright file-banner lines like:
///   `src/middleware/auth.test.ts:`
///   `apps/web/src/foo.spec.tsx:`
/// These are framework framing emitted BEFORE any test runs from that file,
/// and end with a trailing `:`. We deliberately do NOT match arbitrary
/// trailing-colon lines (e.g. pino's "Subscribed to event channel:") — the
/// `.test.`/`.spec.`/`_test.` infix is the discriminator.
fn is_file_header(line: &str) -> bool {
    let t = line.trim_end_matches(['\n', '\r']).trim();
    if !t.ends_with(':') {
        return false;
    }
    let core = &t[..t.len() - 1];
    if core.is_empty() {
        return false;
    }
    // Path-shaped: must contain a `/`, can't have spaces (file paths never
    // do in this codebase), must include a test-file marker.
    if !core.contains('/') || core.contains(' ') {
        return false;
    }
    core.contains(".test.") || core.contains(".spec.") || core.contains("_test.")
}

// ── Per-framework marker parsers ─────────────────────────────────────────

/// Bun: `(pass) my test [1.23ms]` / `(fail) my test [0.12ms]` / `(timeout) my test`
fn parse_bun_marker(line: &str) -> Option<(String, Status, Option<u64>)> {
    let trimmed = line.trim_start();
    let (status, rest) = if let Some(r) = trimmed.strip_prefix("(pass) ") {
        (Status::Pass, r)
    } else if let Some(r) = trimmed.strip_prefix("(fail) ") {
        (Status::Fail, r)
    } else if let Some(r) = trimmed.strip_prefix("(timeout) ") {
        (Status::Stalled, r)
    } else if let Some(r) = trimmed.strip_prefix("(skip) ") {
        (Status::Skipped, r)
    } else if let Some(r) = trimmed.strip_prefix("(todo) ") {
        (Status::Skipped, r)
    } else {
        return None;
    };
    let (name, duration_ms) = split_trailing_duration(rest);
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), status, duration_ms))
}

/// Vitest / Jest: `✓ src/foo.test.ts > my test 1.2ms`  (verbose reporter)
///                `× src/foo.test.ts > my test 1.2ms`
fn parse_vitest_marker(line: &str) -> Option<(String, Status, Option<u64>)> {
    let trimmed = line.trim_start();
    let (status, rest) = if let Some(r) = trimmed.strip_prefix("✓ ") {
        (Status::Pass, r)
    } else if let Some(r) = trimmed.strip_prefix("× ") {
        (Status::Fail, r)
    } else if let Some(r) = trimmed.strip_prefix("✗ ") {
        (Status::Fail, r)
    } else if let Some(r) = trimmed.strip_prefix("↓ ") {
        (Status::Skipped, r)
    } else {
        return None;
    };
    let (name, duration_ms) = split_trailing_duration(rest);
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), status, duration_ms))
}

/// Playwright list reporter:
///   `  ✓  1 [chromium] › passing.spec.ts:3:5 › addition works (2ms)`
///   `  ✘  3 [chromium] › failing.spec.ts:3:5 › first failure (3ms)`
fn parse_playwright_marker(line: &str) -> Option<(String, Status, Option<u64>)> {
    let trimmed = line.trim_start();
    let (status, rest) = if let Some(r) = trimmed.strip_prefix("✓ ").or_else(|| trimmed.strip_prefix("✓\t")) {
        (Status::Pass, r)
    } else if let Some(r) = trimmed
        .strip_prefix("✘ ")
        .or_else(|| trimmed.strip_prefix("✗ "))
    {
        (Status::Fail, r)
    } else if let Some(r) = trimmed.strip_prefix("- ") {
        // playwright "did not run" — surfaces when -x bails out
        (Status::Skipped, r)
    } else {
        return None;
    };
    // Strip the leading test index (`1 `, `42 `) if present.
    let after_index = rest
        .find(char::is_whitespace)
        .filter(|&i| rest[..i].chars().all(|c| c.is_ascii_digit()))
        .map(|i| rest[i + 1..].trim_start())
        .unwrap_or(rest);
    // Strip `[project]` prefix.
    let after_project = if after_index.starts_with('[') {
        match after_index.find(']') {
            Some(i) => after_index[i + 1..].trim_start(),
            None => after_index,
        }
    } else {
        after_index
    };
    let (name, duration_ms) = split_trailing_paren_duration(after_project);
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), status, duration_ms))
}

/// Mocha: `  ✓ my test (1ms)` / `  1) my test`
fn parse_mocha_marker(line: &str) -> Option<(String, Status, Option<u64>)> {
    let trimmed = line.trim_start();
    if let Some(r) = trimmed.strip_prefix("✓ ") {
        let (name, duration_ms) = split_trailing_paren_duration(r);
        if name.is_empty() {
            return None;
        }
        return Some((name.to_string(), Status::Pass, duration_ms));
    }
    // Mocha numbers failing tests: `  1) my test`. We don't get duration here.
    // Skip — failures are picked up via the post-run summary, not here.
    None
}

/// Strip a trailing `[1.23ms]` / `[1.2s]` token and return `(name, ms)`.
fn split_trailing_duration(s: &str) -> (&str, Option<u64>) {
    let s = s.trim_end();
    if let Some(open) = s.rfind('[') {
        if s.ends_with(']') {
            let inner = &s[open + 1..s.len() - 1];
            if let Some(ms) = parse_duration_ms(inner) {
                return (s[..open].trim_end(), Some(ms));
            }
        }
    }
    // bun also emits `1.2ms` without brackets in some reporter modes.
    if let Some(idx) = s.rfind(' ') {
        if let Some(ms) = parse_duration_ms(&s[idx + 1..]) {
            return (s[..idx].trim_end(), Some(ms));
        }
    }
    (s, None)
}

/// Strip a trailing `(1.23ms)` and return `(name, ms)`.
fn split_trailing_paren_duration(s: &str) -> (&str, Option<u64>) {
    let s = s.trim_end();
    if let Some(open) = s.rfind('(') {
        if s.ends_with(')') {
            let inner = &s[open + 1..s.len() - 1];
            if let Some(ms) = parse_duration_ms(inner) {
                return (s[..open].trim_end(), Some(ms));
            }
        }
    }
    (s, None)
}

/// Parse `1.23ms` / `1.2s` / `500ms` / `1500` → ms.
fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, unit) = if let Some(n) = s.strip_suffix("ms") {
        (n, "ms")
    } else if let Some(n) = s.strip_suffix('s') {
        (n, "s")
    } else {
        (s, "ms")
    };
    let v: f64 = num.parse().ok()?;
    Some(match unit {
        "s" => (v * 1000.0) as u64,
        _ => v as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> tempfile::TempDir {
        let dir = tempfile::Builder::new()
            .prefix(name)
            .tempdir()
            .expect("tempdir");
        dir
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap_or_default()
    }

    fn list_test_dirs(run_dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(run_dir.join("tests")) {
            for entry in rd.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn file_headers_do_not_leak_into_per_test_dirs() {
        // Bun prints `src/X.test.ts:` for every scanned file even when the
        // -t filter only matches one. Those structural headers should land
        // in raw.log only — they're not "context" for the matching test.
        let dir = tmpdir("file-headers");
        let mut r = LogRouter::new(dir.path(), "bun").unwrap();
        // Lots of scanned files…
        r.ingest_stderr("\nsrc/middleware/auth.test.ts:\n").unwrap();
        r.ingest_stderr("\nsrc/lib/random-token.test.ts:\n").unwrap();
        r.ingest_stderr("\nsrc/tests/integration/mail.test.ts:\n").unwrap();
        // …then the one test that actually ran.
        r.ingest_stderr("(pass) the only test [3.21ms]\n").unwrap();
        r.finalize().unwrap();

        let dirs = list_test_dirs(dir.path());
        let only = dirs
            .iter()
            .find(|d| d.contains("the_only_test"))
            .expect("test dir");
        let stderr = read(&dir.path().join("tests").join(only).join("stderr.log"));
        assert!(
            !stderr.contains("auth.test.ts:"),
            "file header leaked into per-test dir: {stderr}"
        );
        assert!(
            !stderr.contains("random-token.test.ts:"),
            "file header leaked: {stderr}"
        );
        // raw.log still has them.
        let raw = read(&dir.path().join("raw.log"));
        assert!(raw.contains("auth.test.ts:"));
        assert!(raw.contains("random-token.test.ts:"));
    }

    #[test]
    fn is_file_header_distinguishes_paths_from_log_lines() {
        assert!(is_file_header("src/foo.test.ts:\n"));
        assert!(is_file_header("apps/web/bar.spec.tsx:"));
        assert!(is_file_header("internal/pkg_test.go:"));
        // Not a file header — pino-style log line that ends with ':'
        assert!(!is_file_header(
            "{\"msg\":\"Subscribed to event channel\"}:"
        ));
        // Not a file header — has spaces
        assert!(!is_file_header("some message with spaces.test.ts:"));
        // Not a file header — no path separator
        assert!(!is_file_header("foo.test.ts:"));
    }

    #[test]
    fn bun_routes_chatter_to_passing_test_dir() {
        let dir = tmpdir("bun-pass");
        let mut r = LogRouter::new(dir.path(), "bun").unwrap();
        // Application logs on stdout, framework markers on stderr.
        r.ingest_stderr("src/a.test.ts:\n").unwrap();
        r.ingest_stdout("{\"msg\":\"pino chatter\"}\n").unwrap();
        r.ingest_stderr("(pass) test one [1.23ms]\n").unwrap();
        r.finalize().unwrap();

        let test_dirs = list_test_dirs(dir.path());
        assert!(
            test_dirs.iter().any(|d| d.contains("test_one")),
            "expected a test dir for 'test one', got: {:?}",
            test_dirs
        );
        let test_dir = test_dirs.iter().find(|d| d.contains("test_one")).unwrap();
        let test_path = dir.path().join("tests").join(test_dir);
        let stdout = read(&test_path.join("stdout.log"));
        let stderr = read(&test_path.join("stderr.log"));
        assert!(stdout.contains("pino chatter"), "stdout: {:?}", stdout);
        assert!(stderr.contains("(pass) test one"), "stderr: {:?}", stderr);

        // summary.md exists and carries both the human name (greppable) and
        // the path (the click target).
        let summary = read(&dir.path().join("summary.md"));
        assert!(summary.contains("test one"), "summary: {summary}");
        assert!(
            summary.contains("tests/test_one/"),
            "summary should carry the path: {summary}"
        );
        assert!(summary.contains("pass"), "summary: {summary}");
        // failures.md exists and is empty of failures.
        let failures = read(&dir.path().join("failures.md"));
        assert!(failures.contains("No failures"), "failures: {failures}");
    }

    #[test]
    fn bun_post_marker_timeout_context_attributed_to_failed_test() {
        // Bun emits `^ this test timed out` AFTER the (fail) marker — and
        // later, a `# Unhandled error between tests` block with the real
        // stack lands even after the NEXT test has started its setup. Both
        // pieces of context must end up in the failed test's stderr.log.
        let dir = tmpdir("bun-deferred");
        let mut r = LogRouter::new(dir.path(), "bun").unwrap();
        r.ingest_stderr("src/a.test.ts:\n").unwrap();
        // Test A: timeout
        r.ingest_stderr("(fail) test A [5000.02ms]\n").unwrap();
        r.ingest_stderr("  ^ this test timed out after 5000ms.\n").unwrap();
        // Next test's setup pino chatter (stdout) — must NOT capture into A
        r.ingest_stdout("{\"setup for test B\":true}\n").unwrap();
        // Deferred rejection block — still belongs to A
        r.ingest_stderr("\n").unwrap();
        r.ingest_stderr("# Unhandled error between tests\n").unwrap();
        r.ingest_stderr("-------------------------------\n").unwrap();
        r.ingest_stderr("error: App execution timed out\n").unwrap();
        r.ingest_stderr("  at executeTool (file.ts:849:21)\n").unwrap();
        r.ingest_stderr("-------------------------------\n").unwrap();
        // Then B runs and passes
        r.ingest_stderr("(pass) test B [1ms]\n").unwrap();
        r.finalize().unwrap();

        let dirs = list_test_dirs(dir.path());
        let a_dir = dirs.iter().find(|d| d.contains("test_A")).expect("A dir");
        let a_stderr = read(&dir.path().join("tests").join(a_dir).join("stderr.log"));
        // Marker + timeout reason + deferred block all in A's stderr.
        assert!(a_stderr.contains("(fail) test A"), "A stderr: {a_stderr}");
        assert!(
            a_stderr.contains("^ this test timed out"),
            "A should own the trailing timeout context: {a_stderr}"
        );
        assert!(
            a_stderr.contains("App execution timed out"),
            "A should own the deferred Unhandled-error block: {a_stderr}"
        );
        assert!(
            a_stderr.contains("at executeTool (file.ts:849:21)"),
            "A should own the deferred stack: {a_stderr}"
        );
        // B's pino chatter must NOT have leaked into A.
        assert!(
            !a_stderr.contains("setup for test B"),
            "A should not own B's setup chatter"
        );
    }

    #[test]
    fn bun_failure_dir_gets_full_context() {
        let dir = tmpdir("bun-fail");
        let mut r = LogRouter::new(dir.path(), "bun").unwrap();
        r.ingest_stderr("src/a.test.ts:\n").unwrap();
        r.ingest_stdout("{\"msg\":\"before fail\"}\n").unwrap();
        r.ingest_stderr("error: assertion failed\n").unwrap();
        r.ingest_stderr("(fail) broken test [4.5ms]\n").unwrap();
        r.finalize().unwrap();

        let test_dirs = list_test_dirs(dir.path());
        let broken = test_dirs
            .iter()
            .find(|d| d.contains("broken_test"))
            .expect("test dir for broken_test");
        let p = dir.path().join("tests").join(broken);
        assert!(read(&p.join("stdout.log")).contains("before fail"));
        assert!(read(&p.join("stderr.log")).contains("assertion failed"));
        assert!(read(&p.join("stderr.log")).contains("(fail) broken test"));

        let failures = read(&dir.path().join("failures.md"));
        assert!(failures.contains("broken test"), "failures: {failures}");
        assert!(failures.contains("stdout.log"));
        assert!(failures.contains("stderr.log"));
    }

    #[test]
    fn vitest_pass_and_fail_routed_correctly() {
        let dir = tmpdir("vitest");
        let mut r = LogRouter::new(dir.path(), "vitest").unwrap();
        r.ingest_stdout("noise A\n").unwrap();
        r.ingest_stderr("✓ src/m.test.ts > adds 1.2ms\n").unwrap();
        r.ingest_stdout("noise B\n").unwrap();
        r.ingest_stderr("× src/m.test.ts > subtracts 3.4ms\n").unwrap();
        r.finalize().unwrap();

        let test_dirs = list_test_dirs(dir.path());
        let adds = test_dirs
            .iter()
            .find(|d| d.contains("adds"))
            .expect("adds dir");
        let subs = test_dirs
            .iter()
            .find(|d| d.contains("subtracts"))
            .expect("subtracts dir");

        assert!(read(&dir.path().join("tests").join(adds).join("stdout.log")).contains("noise A"));
        assert!(read(&dir.path().join("tests").join(subs).join("stdout.log")).contains("noise B"));

        let summary = read(&dir.path().join("summary.md"));
        assert!(summary.contains("adds"), "summary: {summary}");
        assert!(summary.contains("subtracts"), "summary: {summary}");
        // Rows append in completion order — `adds` ran before `subtracts`.
        // For triage by status, agents read failures.md, not the summary order.
        let adds_idx = summary.find("adds").unwrap();
        let subs_idx = summary.find("subtracts").unwrap();
        assert!(adds_idx < subs_idx, "live append preserves completion order");
        // failures.md is the curated view: only the failing one.
        let failures = read(&dir.path().join("failures.md"));
        assert!(failures.contains("subtracts"), "failures: {failures}");
        assert!(
            !failures.contains("| adds |"),
            "passing test should not appear in failures.md"
        );
    }

    #[test]
    fn playwright_markers_routed() {
        let dir = tmpdir("playwright");
        let mut r = LogRouter::new(dir.path(), "playwright").unwrap();
        r.ingest_stdout("setup chatter\n").unwrap();
        r.ingest_stdout(
            "  ✓  1 [chromium] › passing.spec.ts:3:5 › addition works (2ms)\n",
        )
        .unwrap();
        r.ingest_stdout("more chatter\n").unwrap();
        r.ingest_stdout(
            "  ✘  3 [chromium] › failing.spec.ts:3:5 › first failure (3ms)\n",
        )
        .unwrap();
        r.finalize().unwrap();

        let dirs = list_test_dirs(dir.path());
        assert!(
            dirs.iter().any(|d| d.contains("addition_works")),
            "expected addition_works dir, got: {dirs:?}"
        );
        assert!(
            dirs.iter().any(|d| d.contains("first_failure")),
            "expected first_failure dir, got: {dirs:?}"
        );

        let summary = read(&dir.path().join("summary.md"));
        assert!(summary.contains("addition works"), "summary: {summary}");
        assert!(summary.contains("first failure"), "summary: {summary}");
        let failures = read(&dir.path().join("failures.md"));
        assert!(failures.contains("first failure"));
    }

    #[test]
    fn unknown_framework_dumps_raw_only() {
        let dir = tmpdir("unknown");
        let mut r = LogRouter::new(dir.path(), "ferris9000").unwrap();
        r.ingest_stdout("anything goes here\n").unwrap();
        r.ingest_stderr("more anything\n").unwrap();
        r.finalize().unwrap();

        // raw.log contains everything.
        let raw = read(&dir.path().join("raw.log"));
        assert!(raw.contains("anything goes here"));
        assert!(raw.contains("more anything"));
        // No per-test dirs.
        let dirs = list_test_dirs(dir.path());
        assert!(dirs.is_empty(), "no test dirs expected, got: {dirs:?}");
        // summary.md still emitted with header + footer; just no rows.
        let summary = read(&dir.path().join("summary.md"));
        assert!(summary.contains("Test Run"));
        assert!(summary.contains("**Totals:**"), "footer should include totals");
    }

    #[test]
    fn partial_lines_across_chunks_attribute_correctly() {
        let dir = tmpdir("partial");
        let mut r = LogRouter::new(dir.path(), "bun").unwrap();
        r.ingest_stdout("half-line").unwrap();
        r.ingest_stdout(" continuation\n").unwrap();
        r.ingest_stderr("(pass) the test [1ms]\n").unwrap();
        r.finalize().unwrap();

        let dirs = list_test_dirs(dir.path());
        let d = dirs.iter().find(|d| d.contains("the_test")).expect("dir");
        let stdout = read(&dir.path().join("tests").join(d).join("stdout.log"));
        assert!(
            stdout.contains("half-line continuation"),
            "stdout: {stdout:?}"
        );
    }

    #[test]
    fn marker_duration_parsing() {
        assert_eq!(parse_duration_ms("1.5ms"), Some(1));
        assert_eq!(parse_duration_ms("1500ms"), Some(1500));
        assert_eq!(parse_duration_ms("1.5s"), Some(1500));
        assert_eq!(parse_duration_ms("500"), Some(500));
        assert_eq!(parse_duration_ms("nope"), None);
    }

    #[test]
    fn marker_extraction_each_framework() {
        let bun = parse_bun_marker("(pass) hello world [12.5ms]").unwrap();
        assert_eq!(bun.0, "hello world");
        assert_eq!(bun.1, Status::Pass);
        assert_eq!(bun.2, Some(12));

        let vitest = parse_vitest_marker("✓ src/a > b 1.5ms").unwrap();
        assert_eq!(vitest.0, "src/a > b");
        assert_eq!(vitest.1, Status::Pass);

        let pw = parse_playwright_marker(
            "  ✓  1 [chromium] › a.spec.ts:1:1 › my test (10ms)",
        )
        .unwrap();
        assert!(pw.0.contains("my test"), "got: {:?}", pw.0);
        assert_eq!(pw.1, Status::Pass);
    }
}
