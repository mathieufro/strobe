use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use walkdir::WalkDir;

use super::adapter::*;
use super::TestProgress;

pub struct BunAdapter;

impl TestAdapter for BunAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Direct bunfig.toml — strong signal for bun:test
        if project_root.join("bunfig.toml").exists() {
            return 90;
        }

        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            // Monorepo: check if any workspace has bunfig.toml
            if pkg.contains("\"workspaces\"") {
                if has_bun_test_workspace(project_root, &pkg) {
                    return 90;
                }
                // Monorepo with workspaces but no bun:test workspace — don't
                // claim based on bun.lock alone (it's the package manager, not test runner)
                if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                    return 0;
                }
            } else {
                // Non-monorepo: defer to vitest/jest if present (existing behavior)
                if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
                    if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") {
                        return 90;
                    }
                    return 0;
                }
            }
        }

        if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
            return 85;
        }

        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") {
                return 90;
            }
            if pkg.contains("\"bun\"") {
                return 75;
            }
        }
        0
    }

    fn name(&self) -> &str {
        "bun"
    }

    fn suite_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let mut args = vec!["test".to_string()];
        let mut cwd: Option<String> = None;

        // Try orchestrator script first (handles monorepo suite configs)
        let orchestrator = load_orchestrator(project_root);
        if let Some(ref suites) = orchestrator {
            let suite_key = match level {
                Some(TestLevel::Unit) => "unit",
                Some(TestLevel::Integration) => "integration",
                Some(TestLevel::E2e) => "e2e",
                None => "all",
            };
            if let Some(suite) = suites.get(suite_key) {
                args.extend(suite.dirs.iter().cloned());
                if !suite.cwd.is_empty() {
                    let joined = project_root.join(&suite.cwd);
                    // If project_root already ends with the suite cwd (e.g. both are
                    // "apps/api"), avoid doubling the path. The orchestrator script
                    // specifies cwd relative to the monorepo root, but project_root
                    // may already BE that subdirectory.
                    cwd = Some(
                        if joined.exists() {
                            joined
                        } else if project_root.ends_with(&suite.cwd) {
                            project_root.to_path_buf()
                        } else {
                            // Try resolving from monorepo root (parent dirs)
                            let mut ancestor = project_root.to_path_buf();
                            loop {
                                if !ancestor.pop() {
                                    break joined;
                                }
                                let candidate = ancestor.join(&suite.cwd);
                                if candidate.exists() {
                                    break candidate;
                                }
                            }
                        }
                        .to_string_lossy()
                        .into_owned(),
                    );
                }
            }
        }

        // If no orchestrator or no matching suite, try workspace detection
        if cwd.is_none() {
            if let Some(ws) = find_bun_workspace(project_root) {
                cwd = Some(ws.to_string_lossy().into_owned());
            }
        }

        // If no orchestrator, fall back to level→dir extraction from package.json
        if orchestrator.is_none() {
            if let Some(level) = level {
                let level_key = match level {
                    TestLevel::Unit => "test:unit",
                    TestLevel::Integration => "test:integration",
                    TestLevel::E2e => "test:e2e",
                };
                let pkg_path = cwd
                    .as_deref()
                    .map(|c| Path::new(c).join("package.json"))
                    .unwrap_or_else(|| project_root.join("package.json"));
                if let Ok(pkg) = std::fs::read_to_string(&pkg_path) {
                    if let Some(script) = extract_script_value(&pkg, level_key) {
                        let parts: Vec<&str> = script.split_whitespace().collect();
                        if parts.len() >= 2 && parts[0] == "bun" && parts[1] == "test" {
                            for part in &parts[2..] {
                                if !part.starts_with('-') {
                                    args.push(part.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }

        let remove_env = bun_remove_env(&cwd, project_root);
        tracing::info!(
            args = ?args,
            cwd = ?cwd,
            has_orchestrator = orchestrator.is_some(),
            "bun suite_command"
        );
        Ok(TestCommand {
            program: "bun".to_string(),
            args,
            env: HashMap::new(),
            cwd,
            remove_env,
        })
    }

    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        // Explicit file path — has extension or path separator
        let is_file_path = test_name.contains('/')
            || test_name.ends_with(".ts")
            || test_name.ends_with(".tsx")
            || test_name.ends_with(".js")
            || test_name.ends_with(".jsx")
            || test_name.contains(".test.")
            || test_name.contains(".spec.");

        if is_file_path {
            let (cwd, relative_path) = resolve_workspace_path(project_root, test_name);
            let remove_env = bun_remove_env(&cwd, project_root);
            return Ok(TestCommand {
                program: "bun".to_string(),
                args: vec!["test".to_string(), relative_path],
                env: HashMap::new(),
                cwd,
                remove_env,
            });
        }

        // Fuzzy file resolution: try to find test files matching the stem.
        // This lets LLMs pass just a file stem like "session-lifecycle" instead
        // of the full path "src/__tests__/session-lifecycle.test.ts".
        if let Some((cwd, files)) = find_test_files_by_stem(project_root, test_name) {
            let remove_env = bun_remove_env(&cwd, project_root);
            let mut args = vec!["test".to_string()];
            args.extend(files);
            return Ok(TestCommand {
                program: "bun".to_string(),
                args,
                env: HashMap::new(),
                cwd,
                remove_env,
            });
        }

        // Name pattern — need workspace cwd for bunfig.toml discovery.
        // Escape regex metacharacters so the pattern is a literal substring match.
        let cwd = find_bun_workspace(project_root).map(|ws| ws.to_string_lossy().into_owned());
        let remove_env = bun_remove_env(&cwd, project_root);
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "test".to_string(),
                "--test-name-pattern".to_string(),
                escape_regex(test_name),
            ],
            env: HashMap::new(),
            cwd,
            remove_env,
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, _exit_code: i32) -> TestResult {
        // Bun writes test output to stderr (default reporter).
        // Wrappers may redirect child stderr to stdout — try stderr first, fall back to stdout.
        let result = parse_bun_output(stderr);
        let has_structured_result = !result.all_tests.is_empty()
            || !result.failures.is_empty()
            || result.summary.passed > 0
            || result.summary.failed > 0
            || result.summary.skipped > 0;
        if has_structured_result {
            return result;
        }
        parse_bun_output(stdout)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            let module = stem.trim_end_matches(".test").trim_end_matches(".spec");
            traces.push(format!("@file:{}", stem));
            traces.push(format!("{}.*", module));
        }
        traces
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 60_000,
            Some(TestLevel::Integration) => 180_000,
            Some(TestLevel::E2e) => 300_000,
            None => 120_000,
        }
    }

    /// Detect pretest scripts from package.json. Checks the project root first
    /// (monorepo scripts live at root), then the workspace package.json.
    /// Looks for `pretest:<level>` → `pretest` in that order.
    fn pretest_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
    ) -> Option<TestCommand> {
        let level_key = match level {
            Some(TestLevel::Unit) => Some("pretest:unit"),
            Some(TestLevel::Integration) => Some("pretest:integration"),
            Some(TestLevel::E2e) => Some("pretest:e2e"),
            None => None,
        };

        // Search root package.json first, then workspace
        let candidates = [
            project_root.join("package.json"),
            find_bun_workspace(project_root)
                .map(|ws| ws.join("package.json"))
                .unwrap_or_default(),
        ];

        for pkg_path in &candidates {
            let pkg = match std::fs::read_to_string(pkg_path) {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Try level-specific pretest first, then generic pretest
            let script_keys: Vec<&str> = level_key
                .into_iter()
                .chain(std::iter::once("pretest"))
                .collect();
            for key in script_keys {
                if extract_script_value(&pkg, key).is_some() {
                    let cwd = pkg_path.parent().map(|p| p.to_string_lossy().into_owned());
                    return Some(TestCommand {
                        program: "bun".to_string(),
                        args: vec!["run".to_string(), key.to_string()],
                        env: HashMap::new(),
                        cwd,
                        remove_env: vec![],
                    });
                }
            }
        }
        None
    }
}

/// Parse JUnit XML (bun:test --reporter=junit) into TestResult.
pub(crate) fn parse_junit_xml(xml: &str) -> TestResult {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures = Vec::new();
    let mut all_tests = Vec::new();

    // State for current testcase
    let mut in_testcase = false;
    let mut tc_name = String::new();
    let mut tc_classname = String::new();
    let mut tc_duration_ms = 0u64;
    let mut tc_failure_msg = String::new();
    let mut tc_failure_body = String::new();
    let mut tc_skipped = false;
    let mut tc_failed = false;
    let mut reading_failure = false;

    // Helper closure to finalize a testcase
    let finalize_testcase = |passed: &mut u32,
                             failed: &mut u32,
                             skipped: &mut u32,
                             failures: &mut Vec<TestFailure>,
                             all_tests: &mut Vec<TestDetail>,
                             tc_name: &str,
                             tc_classname: &str,
                             tc_duration_ms: u64,
                             tc_skipped: bool,
                             tc_failed: bool,
                             tc_failure_msg: &str,
                             tc_failure_body: &str| {
        if tc_skipped {
            *skipped += 1;
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Skip,
                duration_ms: tc_duration_ms,
                stdout: None,
                stderr: None,
                message: None,
            });
        } else if tc_failed {
            *failed += 1;
            let message = if !tc_failure_body.is_empty() {
                tc_failure_body.to_string()
            } else {
                tc_failure_msg.to_string()
            };
            let file = if !tc_classname.is_empty() {
                Some(tc_classname.to_string())
            } else {
                None
            };

            failures.push(TestFailure {
                name: tc_name.to_string(),
                file: file.clone(),
                line: None,
                message: message.clone(),
                rerun: None,
                suggested_traces: vec![],
            });
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Fail,
                duration_ms: tc_duration_ms,
                stdout: None,
                stderr: None,
                message: Some(message),
            });
        } else {
            *passed += 1;
            all_tests.push(TestDetail {
                name: tc_name.to_string(),
                status: TestStatus::Pass,
                duration_ms: tc_duration_ms,
                stdout: None,
                stderr: None,
                message: None,
            });
        }
    };

    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) => {
                match e.local_name().as_ref() {
                    b"testcase" => {
                        // Self-closing <testcase .../> — a passing test
                        let name = get_attr(e, "name");
                        let classname = get_attr(e, "classname");
                        let secs = get_attr(e, "time");
                        let dur = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                        finalize_testcase(
                            &mut passed,
                            &mut failed,
                            &mut skipped,
                            &mut failures,
                            &mut all_tests,
                            &name,
                            &classname,
                            dur,
                            false,
                            false,
                            "",
                            "",
                        );
                    }
                    b"skipped" if in_testcase => {
                        tc_skipped = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Start(ref e)) => match e.local_name().as_ref() {
                b"testcase" => {
                    in_testcase = true;
                    tc_name = get_attr(e, "name");
                    tc_classname = get_attr(e, "classname");
                    let secs = get_attr(e, "time");
                    tc_duration_ms = (secs.parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                    tc_failure_msg.clear();
                    tc_failure_body.clear();
                    tc_skipped = false;
                    tc_failed = false;
                    reading_failure = false;
                }
                b"failure" if in_testcase => {
                    tc_failed = true;
                    tc_failure_msg = get_attr(e, "message");
                    reading_failure = true;
                }
                _ => {}
            },
            Ok(Event::Text(ref e)) => {
                if reading_failure {
                    tc_failure_body = e.unescape().unwrap_or_default().to_string();
                }
            }
            Ok(Event::End(ref e)) => match e.local_name().as_ref() {
                b"failure" => {
                    reading_failure = false;
                }
                b"testcase" => {
                    finalize_testcase(
                        &mut passed,
                        &mut failed,
                        &mut skipped,
                        &mut failures,
                        &mut all_tests,
                        &tc_name,
                        &tc_classname,
                        tc_duration_ms,
                        tc_skipped,
                        tc_failed,
                        &tc_failure_msg,
                        &tc_failure_body,
                    );
                    in_testcase = false;
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let total_duration: u64 = all_tests.iter().map(|t| t.duration_ms).sum();

    TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped,
            stuck: None,
            duration_ms: total_duration,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

/// Extract a script value from package.json by key name.
/// Looks for `"<key>": "<value>"` in the raw JSON string.
fn extract_script_value(pkg_json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\"", key);
    let idx = pkg_json.find(&pattern)?;
    let after_key = &pkg_json[idx + pattern.len()..];
    // Skip whitespace and colon
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    // Extract quoted value
    let after_quote = after_ws.strip_prefix('"')?;
    let end = after_quote.find('"')?;
    Some(after_quote[..end].to_string())
}

/// Parse Bun's default test output into TestResult.
/// Bun writes per-test markers (✓/✗/-) to stderr with durations and failure details.
pub(crate) fn parse_bun_output(output: &str) -> TestResult {
    let cleaned_output = strip_ansi_sequences(output);
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut skipped = 0u32;
    let mut failures: Vec<TestFailure> = Vec::new();
    let mut all_tests: Vec<TestDetail> = Vec::new();
    let mut summary_passed = 0u32;
    let mut summary_failed = 0u32;
    let mut summary_skipped = 0u32;

    // Current file header (e.g., "src/auth.test.ts")
    let mut current_file: Option<String> = None;

    // Failure collection state: (test_name, file, message_lines)
    // For ✗ format: error comes AFTER the marker
    let mut failure_ctx: Option<(String, Option<String>, Vec<String>)> = None;

    // Pending error lines — Bun v1.3+ puts error details BEFORE the (fail) marker.
    // We collect non-marker, non-header lines and attach them to the next (fail).
    let mut pending_error: Vec<String> = Vec::new();

    for line in cleaned_output.lines() {
        let trimmed = line.trim();

        if let Some((kind, count)) = parse_summary_counter_line(trimmed) {
            match kind {
                SummaryKind::Pass => summary_passed = count,
                SummaryKind::Fail => summary_failed = count,
                SummaryKind::Skip | SummaryKind::Todo => summary_skipped += count,
            }
            continue;
        }

        // File header: "path/to/file.test.ts:" (line ending with colon, looks like a test file)
        if is_file_header(trimmed) {
            flush_failure(&mut failure_ctx, &mut failures);
            pending_error.clear();
            current_file = Some(trimmed.trim_end_matches(':').to_string());
            continue;
        }

        // Pass: ✓ Test Name [1.23ms]  or  (pass) Test Name [1.23ms]
        if trimmed.starts_with('✓')
            || trimmed.starts_with('\u{2713}')
            || trimmed.starts_with("(pass)")
        {
            flush_failure(&mut failure_ctx, &mut failures);
            pending_error.clear();
            let (name, dur) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                passed += 1;
                all_tests.push(TestDetail {
                    name,
                    status: TestStatus::Pass,
                    duration_ms: dur,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
            }
            continue;
        }

        // Fail: ✗ Test Name [0.12ms]  or  (fail) Test Name [0.12ms]
        if trimmed.starts_with('✗')
            || trimmed.starts_with('\u{2717}')
            || trimmed.starts_with('\u{2718}')
            || trimmed.starts_with("(fail)")
        {
            flush_failure(&mut failure_ctx, &mut failures);
            let (name, dur) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                failed += 1;
                // Bun v1.3+: error lines come BEFORE (fail) marker — use pending_error.
                // Old ✗ format: error lines come AFTER — start empty, collect below.
                let pre_lines = std::mem::take(&mut pending_error);
                failure_ctx = Some((name.clone(), current_file.clone(), pre_lines));
                all_tests.push(TestDetail {
                    name,
                    status: TestStatus::Fail,
                    duration_ms: dur,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
            }
            continue;
        }

        // Skip: (skip) Test Name  or  - Test Name [skip]  or  » Test Name
        if trimmed.starts_with("(skip)") || trimmed.starts_with("(todo)") {
            flush_failure(&mut failure_ctx, &mut failures);
            pending_error.clear();
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                skipped += 1;
                all_tests.push(TestDetail {
                    name,
                    status: TestStatus::Skip,
                    duration_ms: 0,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
            }
            continue;
        }
        // Only check - / » outside failure context — diff output has '-' prefixed lines
        if failure_ctx.is_none()
            && (trimmed.starts_with("- ")
                || trimmed.starts_with('»')
                || trimmed.starts_with('\u{00bb}'))
            && trimmed.len() > 2
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() && name != "-" {
                flush_failure(&mut failure_ctx, &mut failures);
                pending_error.clear();
                skipped += 1;
                all_tests.push(TestDetail {
                    name,
                    status: TestStatus::Skip,
                    duration_ms: 0,
                    stdout: None,
                    stderr: None,
                    message: None,
                });
            }
            continue;
        }

        // Collect non-marker lines
        if !trimmed.is_empty() {
            // If we have an active failure context (✗ format: error after marker),
            // append to that. Otherwise buffer as pending error (Bun v1.3+: error before marker).
            if let Some((_, _, ref mut msg_lines)) = failure_ctx {
                msg_lines.push(trimmed.to_string());
            } else {
                // Don't buffer summary lines like " 498 pass", "Ran 525 tests..."
                if !is_summary_line(trimmed) {
                    pending_error.push(trimmed.to_string());
                }
            }
        }
    }

    flush_failure(&mut failure_ctx, &mut failures);

    if all_tests.is_empty() {
        passed = summary_passed;
        failed = summary_failed;
        skipped = summary_skipped;
    }

    let total_duration = all_tests.iter().map(|t| t.duration_ms).sum();
    TestResult {
        summary: TestSummary {
            passed,
            failed,
            skipped,
            stuck: None,
            duration_ms: total_duration,
        },
        failures,
        stuck: vec![],
        all_tests,
    }
}

fn strip_ansi_sequences(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if matches!(chars.peek(), Some('[')) {
                chars.next();
                while let Some(next) = chars.next() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            continue;
        }
        out.push(ch);
    }

    out
}

enum SummaryKind {
    Pass,
    Fail,
    Skip,
    Todo,
}

fn parse_summary_counter_line(line: &str) -> Option<(SummaryKind, u32)> {
    let trimmed = line.trim();
    let mut parts = trimmed.split_whitespace();
    let count = parts.next()?.parse::<u32>().ok()?;
    let kind = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    match kind {
        "pass" => Some((SummaryKind::Pass, count)),
        "fail" => Some((SummaryKind::Fail, count)),
        "skip" => Some((SummaryKind::Skip, count)),
        "todo" => Some((SummaryKind::Todo, count)),
        _ => None,
    }
}

/// Check if a line is a summary/footer line that should not be buffered as error context.
fn is_summary_line(line: &str) -> bool {
    let trimmed = line.trim();
    // " 498 pass", " 2 fail", " 1 skip", " 2 todo", "Ran 525 tests...", "N expect() calls"
    if trimmed.starts_with("Ran ") && trimmed.contains(" tests ") {
        return true;
    }
    if trimmed.ends_with(" expect() calls") {
        return true;
    }
    // "  N pass", "  N fail", "  N skip", "  N todo"
    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    if parts.len() == 2 {
        if parts[0].parse::<u32>().is_ok() {
            let suffix = parts[1].trim();
            if matches!(suffix, "pass" | "fail" | "skip" | "todo") {
                return true;
            }
        }
    }
    false
}

/// Check if a line is a file header like "src/auth.test.ts:"
fn is_file_header(line: &str) -> bool {
    line.ends_with(':')
        && !line.starts_with(' ')
        && (line.contains(".test.")
            || line.contains(".spec.")
            || line.contains(".ts:")
            || line.contains(".js:"))
}

/// Extract test name and duration from a marker line like "✓ Test Name [1.23ms]"
/// Also handles "(pass) Test Name [1.23ms]" format from Bun v1.3+.
fn parse_test_marker_line(line: &str) -> (String, u64) {
    // Strip known text-based markers first: (pass), (fail), (skip)
    let stripped = line.trim();
    let after_text_marker = if let Some(rest) = stripped.strip_prefix("(pass)") {
        rest
    } else if let Some(rest) = stripped.strip_prefix("(fail)") {
        rest
    } else if let Some(rest) = stripped.strip_prefix("(skip)") {
        rest
    } else {
        stripped
    };

    // Strip leading marker character(s) (✓/✗/-/»/etc.) and whitespace
    let after_marker = after_text_marker
        .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '[')
        .trim_start();

    // Extract duration from trailing [Xms] or [Xs]
    let (name_part, dur) = if let Some(bracket_start) = after_marker.rfind('[') {
        let bracket_content = &after_marker[bracket_start + 1..].trim_end_matches(']');
        let duration = parse_duration_bracket(bracket_content);
        (after_marker[..bracket_start].trim(), duration)
    } else {
        (after_marker.trim(), 0u64)
    };

    (name_part.to_string(), dur)
}

/// Parse duration from bracket content: "1.23ms" → 1, "0.45ms" → 0 (rounded), "1.50s" → 1500
fn parse_duration_bracket(s: &str) -> u64 {
    let s = s.trim();
    if let Some(ms_str) = s.strip_suffix("ms") {
        ms_str.parse::<f64>().unwrap_or(0.0).round() as u64
    } else if let Some(s_str) = s.strip_suffix('s') {
        (s_str.parse::<f64>().unwrap_or(0.0) * 1000.0).round() as u64
    } else {
        0
    }
}

/// Flush accumulated failure context into the failures vec.
/// Extracts file path and line number from stack trace lines.
fn flush_failure(
    ctx: &mut Option<(String, Option<String>, Vec<String>)>,
    failures: &mut Vec<TestFailure>,
) {
    if let Some((name, file_from_header, msg_lines)) = ctx.take() {
        if msg_lines.is_empty() {
            failures.push(TestFailure {
                name,
                file: file_from_header,
                line: None,
                message: String::new(),
                rerun: None,
                suggested_traces: vec![],
            });
            return;
        }

        let message = msg_lines.join("\n");

        // Extract file:line from first "at" line that isn't node_modules
        let (file, line) = extract_location_from_stack(&msg_lines, &file_from_header);

        failures.push(TestFailure {
            name,
            file,
            line,
            message,
            rerun: None,
            suggested_traces: vec![],
        });
    }
}

/// Extract file path and line number from stack trace lines.
/// Prefers the first non-node_modules frame. Falls back to file header.
fn extract_location_from_stack(
    lines: &[String],
    file_from_header: &Option<String>,
) -> (Option<String>, Option<u32>) {
    for line in lines {
        let trimmed = line.trim();
        if !trimmed.starts_with("at ") {
            continue;
        }
        // Patterns: "at /abs/path:line:col" or "at func (path:line:col)"
        let path_part = if let Some(paren_start) = trimmed.rfind('(') {
            &trimmed[paren_start + 1..trimmed.len() - trimmed.ends_with(')') as usize]
        } else {
            &trimmed[3..] // skip "at "
        };
        if path_part.contains("node_modules") || path_part.contains("node:internal") {
            continue;
        }
        // Parse "path:line:col"
        let parts: Vec<&str> = path_part.rsplitn(3, ':').collect();
        if parts.len() >= 3 {
            let file_path = parts[2];
            let line_num = parts[1].parse::<u32>().ok();
            // Make path relative if absolute
            let relative = if file_path.starts_with('/') {
                // Try stripping common prefixes
                file_path
                    .rsplit_once("/src/")
                    .map(|(_, rest)| format!("src/{}", rest))
                    .unwrap_or_else(|| file_path.to_string())
            } else {
                file_path.to_string()
            };
            return (Some(relative), line_num);
        }
    }
    (file_from_header.clone(), None)
}

/// Check if any workspace directory contains bunfig.toml (bun:test).
fn has_bun_test_workspace(project_root: &Path, pkg_json: &str) -> bool {
    let workspace_dirs = find_workspace_dirs(project_root, pkg_json);
    workspace_dirs
        .iter()
        .any(|ws| ws.join("bunfig.toml").exists())
}

/// Resolve workspace glob patterns to concrete directory paths.
/// Supports simple patterns like "apps/*" and "packages/*".
pub(crate) fn find_workspace_dirs(project_root: &Path, pkg_json: &str) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    // Extract workspaces array — simple string matching
    let ws_start = match pkg_json.find("\"workspaces\"") {
        Some(i) => i,
        None => return dirs,
    };
    let bracket_start = match pkg_json[ws_start..].find('[') {
        Some(i) => ws_start + i,
        None => return dirs,
    };
    let bracket_end = match pkg_json[bracket_start..].find(']') {
        Some(i) => bracket_start + i,
        None => return dirs,
    };
    let array_content = &pkg_json[bracket_start + 1..bracket_end];

    for item in array_content.split(',') {
        let pattern = item.trim().trim_matches('"').trim_matches('\'');
        if pattern.is_empty() {
            continue;
        }

        if pattern.ends_with("/*") {
            // Glob: "apps/*" → list subdirs of apps/
            let parent = project_root.join(pattern.trim_end_matches("/*"));
            if let Ok(entries) = std::fs::read_dir(&parent) {
                for entry in entries.flatten() {
                    if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        dirs.push(entry.path());
                    }
                }
            }
        } else {
            // Direct: "packages/shared"
            let ws = project_root.join(pattern);
            if ws.is_dir() {
                dirs.push(ws);
            }
        }
    }
    dirs
}

/// Find the primary bun:test workspace directory in a monorepo.
/// Returns the first workspace with bunfig.toml, or None.
pub(crate) fn find_bun_workspace(project_root: &Path) -> Option<std::path::PathBuf> {
    let pkg = std::fs::read_to_string(project_root.join("package.json")).ok()?;
    if !pkg.contains("\"workspaces\"") {
        return None;
    }
    let dirs = find_workspace_dirs(project_root, &pkg);
    dirs.into_iter().find(|ws| ws.join("bunfig.toml").exists())
}

/// Build remove_env list: only strip DATABASE_URL when a .env.test file exists
/// in the test cwd (Bun auto-loads it, and inherited env would override).
fn bun_remove_env(cwd: &Option<String>, project_root: &Path) -> Vec<String> {
    let has_env_test = cwd
        .as_deref()
        .map(|c| Path::new(c).join(".env.test").exists())
        .unwrap_or_else(|| project_root.join(".env.test").exists());
    if has_env_test {
        vec!["DATABASE_URL".to_string()]
    } else {
        vec![]
    }
}

/// Escape regex metacharacters so `--test-name-pattern` treats the input as a
/// literal substring. Bun uses JS regex — unescaped parens/brackets in test
/// names like `"handles (edge case)"` would otherwise cause match failures.
fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(
            c,
            '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Try to find test files whose stem matches `query`.
/// e.g. query="session-lifecycle" finds "src/__tests__/session-lifecycle.test.ts".
///
/// Returns `(cwd, relative_paths)` on match. Prefers exact stem matches over
/// partial contains matches (only used when unambiguous — single result).
fn find_test_files_by_stem(
    project_root: &Path,
    query: &str,
) -> Option<(Option<String>, Vec<String>)> {
    let (search_root, cwd) = if let Some(ws) = find_bun_workspace(project_root) {
        let cwd_str = ws.to_string_lossy().into_owned();
        (ws, Some(cwd_str))
    } else {
        (project_root.to_path_buf(), None)
    };

    let mut exact = Vec::new();
    let mut partial = Vec::new();

    for entry in WalkDir::new(&search_root)
        .max_depth(10)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.') && name != "node_modules" && name != "dist" && name != "build"
        })
        .flatten()
    {
        if !entry.file_type().is_file() {
            continue;
        }

        let file_name = entry.file_name().to_string_lossy();

        // Extract test stem: "auth.test.ts" -> "auth", "auth.spec.tsx" -> "auth"
        let stem = if let Some(pos) = file_name.find(".test.") {
            &file_name[..pos]
        } else if let Some(pos) = file_name.find(".spec.") {
            &file_name[..pos]
        } else {
            continue;
        };

        let relative = entry
            .path()
            .strip_prefix(&search_root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .into_owned();

        if stem == query {
            exact.push(relative);
        } else if stem.contains(query) {
            partial.push(relative);
        }
    }

    if !exact.is_empty() {
        Some((cwd, exact))
    } else if partial.len() == 1 {
        // Only use partial match when unambiguous
        Some((cwd, partial))
    } else {
        None
    }
}

/// Resolve a test file path to a workspace-relative path + cwd.
/// If the path starts with a known workspace prefix, strips it and sets cwd.
fn resolve_workspace_path(project_root: &Path, test_path: &str) -> (Option<String>, String) {
    // Try to find workspace from path prefix
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if pkg.contains("\"workspaces\"") {
            let ws_dirs = find_workspace_dirs(project_root, &pkg);
            for ws in &ws_dirs {
                if let Ok(relative_ws) = ws.strip_prefix(project_root) {
                    let prefix = relative_ws.to_string_lossy();
                    let prefix_with_slash = format!("{}/", prefix);
                    if test_path.starts_with(&*prefix_with_slash) {
                        let stripped = &test_path[prefix_with_slash.len()..];
                        return (
                            Some(ws.to_string_lossy().into_owned()),
                            stripped.to_string(),
                        );
                    }
                }
            }
        }
    }

    // No workspace match — check if file exists relative to bun workspace
    if let Some(ws) = find_bun_workspace(project_root) {
        if ws.join(test_path).exists() {
            return (
                Some(ws.to_string_lossy().into_owned()),
                test_path.to_string(),
            );
        }
    }

    // No workspace context — pass path as-is
    (None, test_path.to_string())
}

/// Parsed suite config from a test orchestrator script.
#[derive(Debug, Clone)]
pub(crate) struct OrchestratorSuite {
    /// Directories/files to pass to `bun test` (empty = all tests)
    pub dirs: Vec<String>,
    /// Workspace-relative cwd (e.g., "apps/api")
    pub cwd: String,
}

/// Parse suite configs from a test orchestrator TypeScript file.
/// Looks for a SUITES-like object mapping suite names to {cmd, cwd} entries.
/// Handles both single-line and multi-line `cmd` array literals.
pub(crate) fn parse_suites_from_ts(content: &str) -> Option<HashMap<String, OrchestratorSuite>> {
    let mut suites = HashMap::new();
    let mut current_name: Option<String> = None;
    let mut current_dirs: Vec<String> = Vec::new();
    let mut current_cwd: Option<String> = None;
    let mut brace_depth = 0i32;
    let mut in_suites_block = false;

    // State for accumulating multi-line cmd arrays (e.g. cmd: [\n"bun",\n"test",\n...])
    let mut in_cmd_array = false;
    let mut cmd_accumulator = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Detect start of SUITES object
        if !in_suites_block {
            if (trimmed.contains("SUITES") || trimmed.contains("suites"))
                && (trimmed.contains("Record<")
                    || trimmed.contains(": {")
                    || trimmed.contains("= {"))
            {
                in_suites_block = true;
                brace_depth = 1;
            }
            continue;
        }

        // Accumulate multi-line cmd array until closing bracket
        if in_cmd_array {
            cmd_accumulator.push(' ');
            cmd_accumulator.push_str(trimmed);
            if trimmed.contains(']') {
                in_cmd_array = false;
                let all_strings = extract_quoted_strings(&cmd_accumulator);
                let is_bun_test = all_strings.first().map(|s| s == "bun").unwrap_or(false)
                    && all_strings.get(1).map(|s| s == "test").unwrap_or(false);
                current_dirs = if is_bun_test {
                    all_strings
                        .into_iter()
                        .filter(|s| s != "bun" && s != "test" && !s.starts_with("--"))
                        .collect()
                } else {
                    vec![]
                };
                cmd_accumulator.clear();
            }
            continue;
        }

        // Track brace depth
        brace_depth += trimmed.chars().filter(|&c| c == '{').count() as i32;
        brace_depth -= trimmed.chars().filter(|&c| c == '}').count() as i32;

        if brace_depth <= 0 && in_suites_block && !suites.is_empty() {
            // Flush last suite
            if let Some(name) = current_name.take() {
                suites.insert(
                    name,
                    OrchestratorSuite {
                        dirs: std::mem::take(&mut current_dirs),
                        cwd: current_cwd.take().unwrap_or_default(),
                    },
                );
            }
            break;
        }

        // Suite name: `name: {` or `"name": {`
        if trimmed.ends_with(": {") || trimmed.ends_with(":{") || trimmed.ends_with(": {,") {
            // Flush previous suite
            if let Some(name) = current_name.take() {
                suites.insert(
                    name,
                    OrchestratorSuite {
                        dirs: std::mem::take(&mut current_dirs),
                        cwd: current_cwd.take().unwrap_or_default(),
                    },
                );
            }
            let name_part = trimmed.split(':').next().unwrap_or("").trim();
            let name = name_part.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace());
            if !name.is_empty() {
                current_name = Some(name.to_string());
            }
            continue;
        }

        // cmd array — single-line or start of multi-line
        if trimmed.starts_with("cmd:") || trimmed.starts_with("cmd :") {
            if trimmed.contains('[') && trimmed.contains(']') {
                // Single-line: cmd: ["bun", "test", "src/services", ...]
                let all_strings = extract_quoted_strings(trimmed);
                let is_bun_test = all_strings.first().map(|s| s == "bun").unwrap_or(false)
                    && all_strings.get(1).map(|s| s == "test").unwrap_or(false);
                current_dirs = if is_bun_test {
                    all_strings
                        .into_iter()
                        .filter(|s| s != "bun" && s != "test" && !s.starts_with("--"))
                        .collect()
                } else {
                    vec![]
                };
            } else if trimmed.contains('[') {
                // Multi-line: cmd: [\n  "bun",\n  "test",\n  ...  ]
                in_cmd_array = true;
                cmd_accumulator = trimmed.to_string();
            }
            continue;
        }

        // cwd: `cwd: path.join(ROOT, "apps/api")` or `cwd: ROOT`
        if trimmed.starts_with("cwd:") || trimmed.starts_with("cwd :") {
            current_cwd = extract_cwd_path(trimmed);
            continue;
        }
    }

    // Flush last
    if let Some(name) = current_name.take() {
        suites.insert(
            name,
            OrchestratorSuite {
                dirs: std::mem::take(&mut current_dirs),
                cwd: current_cwd.take().unwrap_or_default(),
            },
        );
    }

    if suites.is_empty() {
        None
    } else {
        Some(suites)
    }
}

/// Extract all double/single-quoted strings from a line.
fn extract_quoted_strings(line: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let quote = c;
            let mut s = String::new();
            for c in chars.by_ref() {
                if c == quote {
                    break;
                }
                s.push(c);
            }
            if !s.is_empty() {
                strings.push(s);
            }
        }
    }
    strings
}

/// Extract cwd path from a line like `cwd: path.join(ROOT, "apps/api")`
fn extract_cwd_path(line: &str) -> Option<String> {
    // Pattern: path.join(ROOT, "relative/path") → "relative/path"
    if line.contains("path.join") {
        let strings = extract_quoted_strings(line);
        return strings.last().cloned();
    }
    // Pattern: `cwd: ROOT` → "" (project root)
    if line.contains("ROOT") {
        return Some(String::new());
    }
    // Direct string: `cwd: "apps/api"`
    let strings = extract_quoted_strings(line);
    strings.first().cloned()
}

/// Find and parse the test orchestrator script referenced in package.json.
pub(crate) fn load_orchestrator(project_root: &Path) -> Option<HashMap<String, OrchestratorSuite>> {
    let pkg = std::fs::read_to_string(project_root.join("package.json")).ok()?;

    // Find script path from any test:* script that references a .ts file
    for key in &["test:unit", "test:integration", "test:e2e", "test"] {
        if let Some(script) = extract_script_value(&pkg, key) {
            let parts: Vec<&str> = script.split_whitespace().collect();
            // Pattern: "bun run scripts/test-run.ts <args>"
            if parts.len() >= 3 && parts[0] == "bun" && parts[1] == "run" {
                let script_path = parts[2];
                if script_path.ends_with(".ts") || script_path.ends_with(".js") {
                    if let Ok(content) = std::fs::read_to_string(project_root.join(script_path)) {
                        return parse_suites_from_ts(&content);
                    }
                }
            }
        }
    }
    None
}

/// Incremental progress tracker for Bun's default test output.
/// Called from the DB event polling loop in mod.rs with text chunks.
pub fn update_progress(text: &str, progress: &Arc<Mutex<TestProgress>>) {
    let mut p = progress.lock().unwrap();

    for line in text.lines() {
        let trimmed = line.trim();

        // File header → transition to Running
        if is_file_header(trimmed) {
            if p.phase == super::TestPhase::Compiling {
                p.phase = super::TestPhase::Running;
            }
            continue;
        }

        // Pass
        if trimmed.starts_with('✓')
            || trimmed.starts_with('\u{2713}')
            || trimmed.starts_with("(pass)")
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                p.passed += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
            continue;
        }

        // Fail
        if trimmed.starts_with('✗')
            || trimmed.starts_with('\u{2717}')
            || trimmed.starts_with('\u{2718}')
            || trimmed.starts_with("(fail)")
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                p.failed += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
            continue;
        }

        // Skip
        if trimmed.starts_with("(skip)") {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() {
                p.skipped += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
            continue;
        }
        if (trimmed.starts_with("- ")
            || trimmed.starts_with('»')
            || trimmed.starts_with('\u{00bb}'))
            && trimmed.len() > 2
        {
            let (name, _) = parse_test_marker_line(trimmed);
            if !name.is_empty() && name != "-" {
                p.skipped += 1;
                p.start_test(name.clone());
                p.finish_test(&name);
            }
        }
    }
}

/// Extract an attribute value from an XML element.
fn get_attr(e: &quick_xml::events::BytesStart, name: &str) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == name.as_bytes())
        .and_then(|a| a.unescape_value().ok().map(|s| s.to_string()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUNIT_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="0" time="0.050">
  <testsuite name="calc.test.ts" tests="2" failures="0" time="0.040">
    <testcase name="Math > adds two numbers" classname="calc.test.ts" time="0.005"/>
    <testcase name="Math > subs two numbers" classname="calc.test.ts" time="0.003"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="1" time="0.060">
  <testsuite name="calc.test.ts" tests="2" failures="1" time="0.050">
    <testcase name="Math > multiplies" classname="calc.test.ts" time="0.008">
      <failure message="Expected 6, got 5" type="AssertionError">
AssertionError: Expected 6, got 5
    at &lt;anonymous&gt; (calc.test.ts:12:7)
      </failure>
    </testcase>
    <testcase name="Math > adds" classname="calc.test.ts" time="0.005"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_SKIP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="1" failures="0" time="0.010">
  <testsuite name="todo.test.ts" tests="1" failures="0" time="0.005">
    <testcase name="todo test" classname="todo.test.ts" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn test_detect_bun() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = BunAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("bun.lockb"), b"").unwrap();
        assert!(
            adapter.detect(dir.path(), None) >= 85,
            "bun.lockb → high confidence"
        );
    }

    #[test]
    fn test_detect_bun_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "bun test"}}"#,
        )
        .unwrap();
        let adapter = BunAdapter;
        assert!(adapter.detect(dir.path(), None) >= 80);
    }

    #[test]
    fn test_parse_passing_junit() {
        // parse_junit_xml is still used by other adapters — test it directly
        let result = parse_junit_xml(JUNIT_PASS);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing_junit() {
        let result = parse_junit_xml(JUNIT_FAIL);
        assert_eq!(result.summary.failed, 1);
        let f = &result.failures[0];
        assert_eq!(f.name, "Math > multiplies");
        assert!(f.message.contains("Expected 6"));
        assert!(f.file.as_deref().unwrap_or("").ends_with("calc.test.ts"));
    }

    #[test]
    fn test_parse_skipped_junit() {
        let result = parse_junit_xml(JUNIT_SKIP);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.summary.passed, 0);
    }

    #[test]
    fn test_parse_xml_entities_unescaped() {
        let result = parse_junit_xml(JUNIT_FAIL);
        let msg = &result.failures[0].message;
        assert!(
            msg.contains("<anonymous>"),
            "XML entities should be decoded, got: {}",
            msg
        );
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter
            .suite_command(dir.path(), None, &Default::default())
            .unwrap();
        assert_eq!(cmd.program, "bun");
        assert!(cmd.args.contains(&"test".to_string()));
    }

    #[test]
    fn test_suite_command_with_level() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "bun test", "test:e2e": "bun test src/tests/e2e"}}"#,
        )
        .unwrap();
        let cmd = BunAdapter
            .suite_command(dir.path(), Some(TestLevel::E2e), &Default::default())
            .unwrap();
        assert!(
            cmd.args.contains(&"src/tests/e2e".to_string()),
            "E2E level should add paths from package.json script, got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_extract_script_value() {
        let pkg = r#"{"scripts": {"test:e2e": "bun test src/tests/e2e", "test": "bun test"}}"#;
        assert_eq!(
            extract_script_value(pkg, "test:e2e"),
            Some("bun test src/tests/e2e".to_string())
        );
        assert_eq!(
            extract_script_value(pkg, "test"),
            Some("bun test".to_string())
        );
        assert_eq!(extract_script_value(pkg, "test:missing"), None);
    }

    // --- Bun v1.3+ (pass)/(fail) format tests ---

    // Bun v1.3+: error details come BEFORE the (fail) marker
    const BUN_PAREN_OUTPUT: &str = "\
bun test v1.3.6 (d530ed99)

src/services/auth.test.ts:
(pass) Auth > validates token [0.50ms]

error: expect(received).toBe(expected)

Expected: 401
Received: 200

      at /project/src/services/auth.test.ts:45:12
(fail) Auth > rejects expired [0.12ms]

src/services/todo.test.ts:
(pass) Todo > creates [1.00ms]
(skip) Todo > deletes [skip]

 2 pass
 1 fail
 1 skip
";

    #[test]
    fn test_parse_bun_paren_format() {
        let result = parse_bun_output(BUN_PAREN_OUTPUT);
        assert_eq!(result.summary.passed, 2, "should count (pass) markers");
        assert_eq!(result.summary.failed, 1, "should count (fail) markers");
        assert_eq!(result.summary.skipped, 1, "should count (skip) markers");
        assert_eq!(result.all_tests.len(), 4);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].name, "Auth > rejects expired");
        assert!(
            result.failures[0].message.contains("Expected: 401"),
            "should capture pre-marker error, got: {}",
            result.failures[0].message
        );
        assert!(
            result.failures[0].message.contains("Received: 200"),
            "should capture full error"
        );
        assert_eq!(
            result.failures[0].file.as_deref(),
            Some("src/services/auth.test.ts")
        );
        assert_eq!(result.failures[0].line, Some(45));
    }

    #[test]
    fn test_bun_update_progress_paren_format() {
        use super::super::{TestPhase, TestProgress};
        use std::sync::{Arc, Mutex};

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        update_progress(
            "src/auth.test.ts:\n(pass) Auth > passes [1.00ms]\n(fail) Auth > fails [0.10ms]\n",
            &progress,
        );

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1, "should count (pass)");
        assert_eq!(p.failed, 1, "should count (fail)");
        assert_eq!(
            p.phase,
            TestPhase::Running,
            "file header should transition to Running"
        );
    }

    // --- File path tests ---

    #[test]
    fn test_single_test_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter
            .single_test_command(dir.path(), "src/middleware/auth.test.ts")
            .unwrap();
        assert!(
            !cmd.args.contains(&"--test-name-pattern".to_string()),
            "file path should not use --test-name-pattern"
        );
        assert!(
            cmd.args
                .contains(&"src/middleware/auth.test.ts".to_string()),
            "file path should be passed directly to bun test"
        );
    }

    #[test]
    fn test_single_test_name_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter
            .single_test_command(dir.path(), "should validate token")
            .unwrap();
        assert!(
            cmd.args.contains(&"--test-name-pattern".to_string()),
            "name pattern should use --test-name-pattern"
        );
        assert!(cmd.args.contains(&"should validate token".to_string()));
    }

    #[test]
    fn test_single_test_workspace_path() {
        let dir = tempfile::tempdir().unwrap();
        // Set up monorepo
        let api = dir.path().join("apps/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("bunfig.toml"), "[test]").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#,
        )
        .unwrap();

        // Path includes workspace prefix
        let cmd = BunAdapter
            .single_test_command(dir.path(), "apps/api/src/middleware/auth.test.ts")
            .unwrap();
        assert!(
            cmd.args
                .contains(&"src/middleware/auth.test.ts".to_string()),
            "should strip workspace prefix, got: {:?}",
            cmd.args
        );
        assert!(
            cmd.cwd.as_ref().unwrap().ends_with("apps/api"),
            "should set cwd to workspace dir, got: {:?}",
            cmd.cwd
        );
    }

    #[test]
    fn test_single_test_path_no_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Non-monorepo with bunfig.toml
        std::fs::write(dir.path().join("bunfig.toml"), "[test]").unwrap();

        let cmd = BunAdapter
            .single_test_command(dir.path(), "src/middleware/auth.test.ts")
            .unwrap();
        assert!(cmd
            .args
            .contains(&"src/middleware/auth.test.ts".to_string()));
    }

    // --- Fuzzy file stem matching tests ---

    #[test]
    fn test_single_test_fuzzy_stem_exact() {
        let dir = tempfile::tempdir().unwrap();
        // Create a test file nested in the project
        let test_dir = dir.path().join("src/__tests__");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("session-lifecycle.test.ts"), "").unwrap();

        let cmd = BunAdapter
            .single_test_command(dir.path(), "session-lifecycle")
            .unwrap();
        assert!(
            !cmd.args.contains(&"--test-name-pattern".to_string()),
            "fuzzy stem match should not use --test-name-pattern"
        );
        assert!(
            cmd.args
                .iter()
                .any(|a| a.ends_with("session-lifecycle.test.ts")),
            "should find test file by stem, got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_single_test_fuzzy_stem_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Set up monorepo with a test file in the workspace
        let api = dir.path().join("apps/api");
        let test_dir = api.join("src/__tests__");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(api.join("bunfig.toml"), "[test]").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#,
        )
        .unwrap();
        std::fs::write(test_dir.join("auth-service.test.ts"), "").unwrap();

        let cmd = BunAdapter
            .single_test_command(dir.path(), "auth-service")
            .unwrap();
        assert!(
            cmd.args.iter().any(|a| a.contains("auth-service.test.ts")),
            "should find test file in workspace by stem, got: {:?}",
            cmd.args
        );
        assert!(
            cmd.cwd.as_ref().unwrap().ends_with("apps/api"),
            "should set cwd to workspace dir"
        );
    }

    #[test]
    fn test_single_test_fuzzy_stem_no_match_falls_to_pattern() {
        let dir = tempfile::tempdir().unwrap();
        // No test files exist — should fall through to --test-name-pattern
        let cmd = BunAdapter
            .single_test_command(dir.path(), "nonexistent")
            .unwrap();
        assert!(
            cmd.args.contains(&"--test-name-pattern".to_string()),
            "no matching file should fall through to name pattern"
        );
    }

    #[test]
    fn test_single_test_fuzzy_stem_ambiguous_falls_to_pattern() {
        let dir = tempfile::tempdir().unwrap();
        // Create two files that partially match — ambiguous, should fall through
        let test_dir = dir.path().join("src");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("auth-login.test.ts"), "").unwrap();
        std::fs::write(test_dir.join("auth-signup.test.ts"), "").unwrap();

        // "auth" partially matches both stems but neither is exact
        let cmd = BunAdapter.single_test_command(dir.path(), "auth").unwrap();
        assert!(
            cmd.args.contains(&"--test-name-pattern".to_string()),
            "ambiguous partial match should fall through to name pattern, got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_single_test_fuzzy_stem_unique_partial() {
        let dir = tempfile::tempdir().unwrap();
        let test_dir = dir.path().join("src");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("auth-login.test.ts"), "").unwrap();
        std::fs::write(test_dir.join("todo-crud.test.ts"), "").unwrap();

        // "auth" partially matches only one stem — unambiguous
        let cmd = BunAdapter.single_test_command(dir.path(), "auth").unwrap();
        assert!(
            !cmd.args.contains(&"--test-name-pattern".to_string()),
            "unique partial match should resolve to file, got: {:?}",
            cmd.args
        );
        assert!(
            cmd.args.iter().any(|a| a.contains("auth-login.test.ts")),
            "should find the matching file"
        );
    }

    #[test]
    fn test_single_test_fuzzy_spec_file() {
        let dir = tempfile::tempdir().unwrap();
        let test_dir = dir.path().join("tests");
        std::fs::create_dir_all(&test_dir).unwrap();
        std::fs::write(test_dir.join("utils.spec.ts"), "").unwrap();

        let cmd = BunAdapter.single_test_command(dir.path(), "utils").unwrap();
        assert!(
            cmd.args.iter().any(|a| a.contains("utils.spec.ts")),
            "should find .spec. files too, got: {:?}",
            cmd.args
        );
    }

    // --- Regex escaping tests ---

    #[test]
    fn test_single_test_name_pattern_escapes_regex() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter
            .single_test_command(dir.path(), "handles (edge case)")
            .unwrap();
        assert!(cmd.args.contains(&"--test-name-pattern".to_string()));
        assert!(
            cmd.args.contains(&r"handles \(edge case\)".to_string()),
            "should escape regex metacharacters, got: {:?}",
            cmd.args
        );
    }

    #[test]
    fn test_escape_regex() {
        assert_eq!(escape_regex("plain text"), "plain text");
        assert_eq!(escape_regex("a.b"), r"a\.b");
        assert_eq!(escape_regex("foo(bar)"), r"foo\(bar\)");
        assert_eq!(escape_regex("[test]"), r"\[test\]");
        assert_eq!(escape_regex("a*b+c?"), r"a\*b\+c\?");
        assert_eq!(escape_regex("^start$"), r"\^start\$");
        assert_eq!(escape_regex("a|b"), r"a\|b");
        assert_eq!(escape_regex("a{1,2}"), r"a\{1,2\}");
    }

    // --- Orchestrator tests ---

    #[test]
    fn test_parse_orchestrator_suites() {
        let content = r#"
const ROOT = path.resolve(import.meta.dir, "..")

const SUITES: Record<string, { cmd: string[]; cwd: string }> = {
  all: {
    cmd: ["bun", "test"],
    cwd: path.join(ROOT, "apps/api"),
  },
  unit: {
    cmd: ["bun", "test", "src/services", "src/middleware", "src/lib", "src/db"],
    cwd: path.join(ROOT, "apps/api"),
  },
  integration: {
    cmd: ["bun", "test", "src/routes", "src/tests/auth-service.test.ts", "src/tests/app-contract.test.ts", "src/tests/full-integration.test.ts"],
    cwd: path.join(ROOT, "apps/api"),
  },
  e2e: {
    cmd: ["bun", "test", "src/tests/e2e"],
    cwd: path.join(ROOT, "apps/api"),
  },
  "e2e-parallel": {
    cmd: ["bun", "run", "scripts/test-e2e-parallel.ts", "--workers=4"],
    cwd: ROOT,
  },
};
"#;
        let suites = parse_suites_from_ts(content).unwrap();
        assert!(
            suites.len() >= 4,
            "should parse at least 4 suites, got {}",
            suites.len()
        );

        let unit = &suites["unit"];
        assert_eq!(
            unit.dirs,
            vec!["src/services", "src/middleware", "src/lib", "src/db"]
        );
        assert_eq!(unit.cwd, "apps/api");

        let integration = &suites["integration"];
        assert!(integration.dirs.contains(&"src/routes".to_string()));
        assert!(integration
            .dirs
            .contains(&"src/tests/auth-service.test.ts".to_string()));

        let e2e = &suites["e2e"];
        assert_eq!(e2e.dirs, vec!["src/tests/e2e"]);
        assert_eq!(e2e.cwd, "apps/api");

        let all = &suites["all"];
        assert!(
            all.dirs.is_empty(),
            "all suite should have no dirs (runs everything)"
        );
        assert_eq!(all.cwd, "apps/api");
    }

    #[test]
    fn test_parse_orchestrator_multiline_cmd() {
        // Exact pattern from a real orchestrator: comments between suite name and cmd,
        // multi-line cmd array, mix of single-line and multi-line suites.
        let content = r#"
const SUITES: Record<string, { cmd: string[]; cwd: string }> = {
  unit: {
    cmd: ["bun", "test", "src/modules", "src/lib"],
    cwd: path.join(ROOT, "apps/api"),
  },
  integration: {
    // Runs all cross-module integration tests in src/tests/ (excluding e2e/).
    // auth-service.test.ts requires exclusive DB state — serialized naturally.
    cmd: [
      "bun",
      "test",
      "src/tests/auth-service.test.ts",
      "src/tests/auth-login-error.test.ts",
      "src/tests/event-bus.test.ts",
    ],
    cwd: path.join(ROOT, "apps/api"),
  },
  e2e: {
    cmd: ["bun", "test", "src/tests/e2e"],
    cwd: path.join(ROOT, "apps/api"),
  },
};
"#;
        let suites = parse_suites_from_ts(content).unwrap();
        assert_eq!(suites.len(), 3);

        // Multi-line cmd correctly parsed (with comments before cmd:)
        let integration = &suites["integration"];
        assert_eq!(
            integration.dirs,
            vec![
                "src/tests/auth-service.test.ts",
                "src/tests/auth-login-error.test.ts",
                "src/tests/event-bus.test.ts",
            ]
        );
        assert_eq!(integration.cwd, "apps/api");

        // Single-line still works
        let unit = &suites["unit"];
        assert_eq!(unit.dirs, vec!["src/modules", "src/lib"]);
    }

    #[test]
    fn test_parse_orchestrator_real_world_full() {
        // Full real-world orchestrator with all edge cases:
        // - Comments between suite name and cmd
        // - Multi-line cmd arrays
        // - Single-line cmd arrays
        // - Custom runner suites (bun run, not bun test)
        // - Back-compat aliases
        let content = r#"
const ROOT = path.resolve(import.meta.dir, "..")
const SUITES: Record<string, { cmd: string[]; cwd: string }> = {
  all: {
    cmd: ["bun", "test"],
    cwd: path.join(ROOT, "apps/api"),
  },
  unit: {
    cmd: ["bun", "test", "src/modules", "src/infra", "src/middleware", "src/lib", "src/db"],
    cwd: path.join(ROOT, "apps/api"),
  },
  integration: {
    // Runs all cross-module integration tests in src/tests/ (excluding e2e/).
    // auth-service.test.ts requires exclusive DB state (bootstrapRegister needs
    // zero orgs) — serialized naturally since bun test runs sequentially within a dir.
    cmd: [
      "bun",
      "test",
      "src/tests/auth-service.test.ts",
      "src/tests/auth-login-error.test.ts",
      "src/tests/app-contract.test.ts",
      "src/tests/contacts.test.ts",
      "src/tests/db-import-enforcement.test.ts",
      "src/tests/event-bus.test.ts",
      "src/tests/event-subscribers.test.ts",
      "src/tests/full-integration.test.ts",
      "src/tests/jobs-integration.test.ts",
    ],
    cwd: path.join(ROOT, "apps/api"),
  },
  e2e: {
    cmd: ["bun", "test", "src/tests/e2e"],
    cwd: path.join(ROOT, "apps/api"),
  },
  "e2e-parallel": {
    cmd: ["bun", "run", "scripts/test-e2e-parallel.ts", "--workers=4"],
    cwd: ROOT,
  },
  playwright: {
    cmd: ["bunx", "--bun", "playwright", "test"],
    cwd: path.join(ROOT, "apps/web"),
  },
  "e2e-web": {
    cmd: ["bunx", "--bun", "playwright", "test"],
    cwd: path.join(ROOT, "apps/web"),
  },
};
"#;
        let suites = parse_suites_from_ts(content).unwrap();

        // integration: multi-line cmd with comments before it
        let integration = &suites["integration"];
        assert_eq!(
            integration.dirs.len(),
            9,
            "integration should have 9 specific files, got: {:?}",
            integration.dirs
        );
        assert!(integration
            .dirs
            .contains(&"src/tests/auth-service.test.ts".to_string()));
        assert!(integration
            .dirs
            .contains(&"src/tests/jobs-integration.test.ts".to_string()));
        assert_eq!(integration.cwd, "apps/api");

        // unit: single-line cmd
        let unit = &suites["unit"];
        assert_eq!(
            unit.dirs,
            vec![
                "src/modules",
                "src/infra",
                "src/middleware",
                "src/lib",
                "src/db"
            ]
        );

        // e2e: single-line cmd
        let e2e = &suites["e2e"];
        assert_eq!(e2e.dirs, vec!["src/tests/e2e"]);

        // e2e-parallel: custom runner (not bun test) → empty dirs
        let parallel = &suites["e2e-parallel"];
        assert!(
            parallel.dirs.is_empty(),
            "custom runner should have no dirs"
        );

        // all: bun test with no dirs
        let all = &suites["all"];
        assert!(all.dirs.is_empty());
    }

    #[test]
    fn test_parse_orchestrator_empty() {
        assert!(parse_suites_from_ts("").is_none());
        assert!(parse_suites_from_ts("const x = 5;").is_none());
    }

    #[test]
    fn test_suite_command_with_orchestrator() {
        let dir = tempfile::tempdir().unwrap();

        // Set up monorepo with orchestrator
        let scripts = dir.path().join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(
            scripts.join("test-run.ts"),
            r#"
const SUITES = {
  unit: {
    cmd: ["bun", "test", "src/services", "src/lib"],
    cwd: path.join(ROOT, "apps/api"),
  },
};
"#,
        )
        .unwrap();

        std::fs::write(dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"], "scripts": {"test:unit": "bun run scripts/test-run.ts unit"}}"#
        ).unwrap();

        let api = dir.path().join("apps/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("bunfig.toml"), "[test]").unwrap();

        let cmd = BunAdapter
            .suite_command(dir.path(), Some(TestLevel::Unit), &Default::default())
            .unwrap();
        assert_eq!(cmd.program, "bun");
        assert!(
            cmd.args.contains(&"src/services".to_string()),
            "should include dirs from orchestrator, got: {:?}",
            cmd.args
        );
        assert!(cmd.args.contains(&"src/lib".to_string()));
        assert!(cmd.cwd.is_some(), "should set cwd");
        assert!(
            cmd.cwd.as_ref().unwrap().ends_with("apps/api"),
            "cwd should be workspace dir"
        );
    }

    // --- Detection tests ---

    #[test]
    fn test_detect_bunfig_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bunfig.toml"), "[test]\ntimeout = 30000").unwrap();
        let adapter = BunAdapter;
        assert!(
            adapter.detect(dir.path(), None) >= 90,
            "bunfig.toml should give high confidence"
        );
    }

    #[test]
    fn test_detect_monorepo_bun_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Root has vitest in deps (from web app) + workspaces
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"], "devDependencies": {"vitest": "^3"}}"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("bun.lock"), "").unwrap();
        // API workspace has bunfig.toml
        let api = dir.path().join("apps/api");
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(api.join("bunfig.toml"), "[test]\ntimeout = 30000").unwrap();

        let adapter = BunAdapter;
        let conf = adapter.detect(dir.path(), None);
        assert!(
            conf >= 85,
            "monorepo with bun:test workspace should detect despite vitest in root, got {}",
            conf
        );
    }

    #[test]
    fn test_detect_monorepo_no_bun_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Root has vitest + workspaces but NO bun:test workspace
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"], "devDependencies": {"vitest": "^3"}}"#,
        )
        .unwrap();
        let web = dir.path().join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("vitest.config.ts"), "export default {}").unwrap();

        let adapter = BunAdapter;
        let conf = adapter.detect(dir.path(), None);
        assert_eq!(
            conf, 0,
            "monorepo with only vitest workspaces should return 0"
        );
    }

    // --- Progress tracker tests ---

    #[test]
    fn test_bun_update_progress_pass() {
        use super::super::{TestPhase, TestProgress};
        use std::sync::{Arc, Mutex};

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        update_progress("src/auth.test.ts:\n✓ Auth > passes [1.00ms]\n", &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1, "should count 1 pass");
        assert_eq!(p.failed, 0);
        assert_eq!(
            p.phase,
            TestPhase::Running,
            "file header should transition to Running"
        );
    }

    #[test]
    fn test_bun_update_progress_mixed() {
        use super::super::{TestPhase, TestProgress};
        use std::sync::{Arc, Mutex};

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        // First chunk: file header + some results
        update_progress(
            "src/math.test.ts:\n✓ adds [1.00ms]\n✗ divides [0.10ms]\n",
            &progress,
        );

        // Second chunk: more results from another file
        update_progress(
            "src/todo.test.ts:\n✓ exists [0.30ms]\n- skipped [skip]\n",
            &progress,
        );

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 2);
        assert_eq!(p.failed, 1);
        assert_eq!(p.skipped, 1);
    }

    #[test]
    fn test_bun_update_progress_ignores_non_test_lines() {
        use super::super::TestProgress;
        use std::sync::{Arc, Mutex};

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        update_progress("bun test v1.2.0 (abc123def)\n\n 2 pass\n 1 fail\n\nRan 3 tests across 1 files. [10.00ms]\n", &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 0, "summary lines should not increment counters");
        assert_eq!(p.failed, 0);
    }

    // --- Native Bun output parser tests ---

    const BUN_NATIVE_PASS: &str = "\
bun test v1.2.0 (abc123def)

src/math.test.ts:
✓ Math > adds two numbers [1.23ms]
✓ Math > subtracts [0.45ms]

 2 pass
 0 fail
 2 expect() calls

Ran 2 tests across 1 files. [10.00ms]
";

    const BUN_NATIVE_FAIL: &str = "\
bun test v1.2.0 (abc123def)

src/auth.test.ts:
✓ Auth > validates token [0.50ms]
✗ Auth > rejects expired token [0.12ms]

error: expect(received).toBe(expected)

Expected: 401
Received: 200

      at /project/src/auth.test.ts:45:12
      at processTicksAndRejections (node:internal/process/task_queues:95:5)

 1 pass
 1 fail
 2 expect() calls

Ran 2 tests across 1 files. [5.00ms]
";

    const BUN_NATIVE_MIXED: &str = "\
bun test v1.2.0 (abc123def)

src/math.test.ts:
✓ Math > adds [1.00ms]
✗ Math > divides [0.10ms]

error: expect(received).toBe(expected)

Expected: 2
Received: NaN

      at /project/src/math.test.ts:20:5

src/todo.test.ts:
✓ Todo > exists [0.30ms]
- Todo > not yet [skip]

 2 pass
 1 fail
 1 skip
 4 expect() calls

Ran 4 tests across 2 files. [15.00ms]
";

    const BUN_NATIVE_MULTI_FAILURE: &str = "\
bun test v1.2.0 (abc123def)

src/routes/users.test.ts:
✓ GET /users > returns list [2.00ms]
✗ POST /users > validates email [0.50ms]

error: expect(received).toContain(expected)

Expected string to contain: \"@\"
Received: \"invalid\"

      at /project/src/routes/users.test.ts:30:10

✗ DELETE /users > requires auth [0.10ms]

error: expect(received).toBe(expected)

Expected: 401
Received: 200

      at /project/src/routes/users.test.ts:55:8

 1 pass
 2 fail
 3 expect() calls

Ran 3 tests across 1 files. [8.00ms]
";

    const BUN_NATIVE_DIFF_FAILURE: &str = "\
bun test v1.2.0 (abc123def)

src/snapshot.test.ts:
✓ Snapshot > matches basic [0.50ms]
✗ Snapshot > matches complex [0.20ms]

error: expect(received).toEqual(expected)

- Expected
+ Received

  Object {
-   \"status\": 401,
+   \"status\": 200,
  }

      at /project/src/snapshot.test.ts:25:8

 1 pass
 1 fail
 2 expect() calls

Ran 2 tests across 1 files. [5.00ms]
";

    #[test]
    fn test_parse_bun_native_passing() {
        let result = parse_bun_output(BUN_NATIVE_PASS);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
        assert_eq!(result.all_tests[0].name, "Math > adds two numbers");
        assert_eq!(result.all_tests[0].status, TestStatus::Pass);
        assert!(result.all_tests[0].duration_ms > 0);
    }

    #[test]
    fn test_parse_bun_native_failure() {
        let result = parse_bun_output(BUN_NATIVE_FAIL);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Auth > rejects expired token");
        assert!(
            f.message.contains("Expected: 401"),
            "message should contain expected value, got: {}",
            f.message
        );
        assert!(
            f.message.contains("Received: 200"),
            "message should contain received value"
        );
        assert_eq!(f.file.as_deref(), Some("src/auth.test.ts"));
        assert_eq!(f.line, Some(45));
    }

    #[test]
    fn test_parse_bun_native_mixed() {
        let result = parse_bun_output(BUN_NATIVE_MIXED);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.all_tests.len(), 4);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].name, "Math > divides");
        assert_eq!(result.failures[0].file.as_deref(), Some("src/math.test.ts"));
    }

    #[test]
    fn test_parse_bun_native_multi_failure() {
        let result = parse_bun_output(BUN_NATIVE_MULTI_FAILURE);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 2);
        assert_eq!(result.failures.len(), 2);
        assert_eq!(result.failures[0].name, "POST /users > validates email");
        assert_eq!(result.failures[0].line, Some(30));
        assert_eq!(result.failures[1].name, "DELETE /users > requires auth");
        assert_eq!(result.failures[1].line, Some(55));
    }

    #[test]
    fn test_parse_bun_native_empty() {
        let result = parse_bun_output("");
        assert_eq!(result.summary.passed, 0);
        assert_eq!(result.summary.failed, 0);
        assert!(result.all_tests.is_empty());
    }

    #[test]
    fn test_parse_bun_summary_only_with_ansi() {
        let output = "\n\u{1b}[0m\u{1b}[32m 1 pass\u{1b}[0m\n \u{1b}[0m\u{1b}[2m1608 filtered out\u{1b}[0m\n\u{1b}[0m\u{1b}[2m 0 fail\u{1b}[0m\n 1 expect() calls\nRan 1 test across 172 files. \u{1b}[0m\u{1b}[2m[\u{1b}[1m364.00ms\u{1b}[0m\u{1b}[2m]\u{1b}[0m\n";
        let result = parse_bun_output(output);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 0);
        assert_eq!(result.summary.skipped, 0);
        assert!(result.all_tests.is_empty());
    }

    #[test]
    fn test_parse_output_prefers_stderr() {
        // Bun writes test output to stderr; stdout may have user console.log
        let adapter = BunAdapter;
        let result = adapter.parse_output("console.log output", BUN_NATIVE_PASS, 0);
        assert_eq!(result.summary.passed, 2, "should parse stderr, not stdout");
    }

    #[test]
    fn test_parse_output_falls_back_to_stdout() {
        // Wrappers may redirect child stderr to stdout
        let adapter = BunAdapter;
        let result = adapter.parse_output(BUN_NATIVE_PASS, "", 0);
        assert_eq!(
            result.summary.passed, 2,
            "should fall back to stdout when stderr empty"
        );
    }

    #[test]
    fn test_parse_bun_native_diff_failure_no_false_skip() {
        // Diff-style assertion output with '-'-prefixed lines must NOT be counted as skipped tests
        let result = parse_bun_output(BUN_NATIVE_DIFF_FAILURE);
        assert_eq!(result.summary.passed, 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(
            result.summary.skipped, 0,
            "diff lines starting with '-' must not count as skipped"
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].name, "Snapshot > matches complex");
        assert!(
            result.failures[0].message.contains("Expected"),
            "full diff should be in message"
        );
        assert!(
            result.failures[0].message.contains("401"),
            "diff content should be preserved"
        );
    }
}
