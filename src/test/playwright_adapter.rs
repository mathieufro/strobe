use std::collections::HashMap;
use std::path::Path;

use super::adapter::*;
use super::bun_adapter::find_workspace_dirs;

/// Custom Playwright reporter that streams per-test events to stderr.
/// Written to a temp file and passed via `--reporter=<path>`.
const REPORTER_JS: &str = include_str!("reporters/playwright-reporter.mjs");

/// Write the custom reporter to a temp file, returning the path.
fn ensure_reporter_file() -> String {
    let path = "/tmp/.strobe-playwright-reporter.mjs";
    let _ = std::fs::write(path, REPORTER_JS);
    path.to_string()
}

pub const PROGRESS_FILE: &str = "/tmp/.strobe-playwright-progress";

fn progress_file_path() -> String {
    PROGRESS_FILE.to_string()
}

/// Escape a literal test name so Playwright's `--grep` (which compiles its
/// argument with `new RegExp(...)`) matches it verbatim. Without this, a name
/// containing regex metacharacters (`[Phase 9] … visuals + app-registry …`)
/// silently matches zero tests and the run exits clean having done nothing.
fn escape_grep_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        if matches!(
            ch,
            '.' | '*' | '+' | '?' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

pub struct PlaywrightAdapter;

impl PlaywrightAdapter {
    pub fn new() -> Self {
        PlaywrightAdapter
    }

    /// Run playwright tests and emit an NDJSON event stream into `run_dir`.
    pub async fn run(
        &self,
        project_root: &Path,
        run_dir: &Path,
        config: &RunConfig,
    ) -> std::io::Result<RunResult> {
        use super::event_stream::{EventStream, FailureDetail, TestEvent};
        use std::time::{SystemTime, UNIX_EPOCH};

        std::fs::create_dir_all(run_dir)?;
        let events_path = run_dir.join("events.ndjson");
        let mut stream = EventStream::create(&events_path)?;

        let report_path = run_dir.join("pw-report.json");
        let _ = std::fs::remove_file(&report_path);

        let run_id = format!("pw-{}", super::adapter_util::nano_id());
        let mode_str = match config.mode {
            Mode::FirstFail => "first-fail",
            Mode::CollectAll => "collect-all",
        };
        let started_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let start = std::time::Instant::now();

        stream.emit(TestEvent::RunStarted {
            run_id: run_id.clone(),
            project_root: project_root.to_string_lossy().into_owned(),
            suite: "default".into(),
            framework: "playwright".into(),
            mode: mode_str.into(),
            started_at_ms,
        })?;

        let pw_path = project_root.join("node_modules/.bin/playwright");
        let mut args: Vec<String> = vec!["test".into(), "--reporter=json".into()];
        if config.mode == Mode::FirstFail {
            args.push("-x".into());
        }
        if let Some(filter) = &config.test_filter {
            args.push(filter.clone());
        }

        let output = tokio::process::Command::new(&pw_path)
            .args(&args)
            .current_dir(project_root)
            .env("PLAYWRIGHT_JSON_OUTPUT_NAME", &report_path)
            .output()
            .await?;
        let exit_code = output.status.code().unwrap_or(-1);
        // Playwright may also write JSON to stdout when env var is set; we use the file.
        // If file is missing, fall back to stdout content.
        if !report_path.exists() {
            let _ = std::fs::write(&report_path, &output.stdout);
        }

        let cases = parse_pw_json(&report_path).unwrap_or_default();

        let mut passed = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        for case in &cases {
            let test_id = format!("{}::{}", case.file, case.title);
            let artifact_dir = super::adapter_util::artifact_dir(&case.file, &case.title);

            stream.emit(TestEvent::TestStarted {
                test_id: test_id.clone(),
                file: case.file.clone(),
                line: case.line,
            })?;

            let duration_ms = case.duration_ms;

            match case.status.as_str() {
                "skipped" | "didnotrun" => {
                    skipped += 1;
                    stream.emit(TestEvent::TestSkipped {
                        test_id,
                        file: case.file.clone(),
                        reason: case.status.clone(),
                    })?;
                }
                "failed" | "timedout" | "interrupted" => {
                    failed += 1;
                    let code_context = super::adapter_util::read_code_context(
                        project_root,
                        &case.file,
                        case.fail_line.unwrap_or(case.line),
                    )
                    .unwrap_or_default();
                    stream.emit(TestEvent::TestFailed {
                        test_id,
                        file: case.file.clone(),
                        line: case.fail_line.unwrap_or(case.line),
                        duration_ms,
                        error: FailureDetail {
                            message: strip_ansi(case.message.as_deref().unwrap_or("test failed")),
                            kind: "Error".into(),
                            stack_frames: vec![],
                            expected: case.expected.clone(),
                            actual: case.actual.clone(),
                            code_context,
                        },
                        artifact_dir,
                    })?;
                }
                _ => {
                    passed += 1;
                    stream.emit(TestEvent::TestPassed {
                        test_id,
                        file: case.file.clone(),
                        line: case.line,
                        duration_ms,
                        artifact_dir,
                    })?;
                }
            }
        }

        let total = passed + failed + skipped;
        stream.emit(TestEvent::RunComplete {
            run_id,
            exit_code,
            total,
            passed,
            failed,
            stalled: 0,
            skipped,
            duration_ms: start.elapsed().as_millis() as u64,
        })?;

        Ok(RunResult { exit_code })
    }
}

impl Default for PlaywrightAdapter {
    fn default() -> Self {
        PlaywrightAdapter::new()
    }
}

#[derive(Debug, Default)]
struct PwCase {
    file: String,
    title: String,
    line: u32,
    duration_ms: u64,
    status: String,
    message: Option<String>,
    expected: Option<String>,
    actual: Option<String>,
    fail_line: Option<u32>,
}

fn parse_pw_json(path: &Path) -> Option<Vec<PwCase>> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let suites = v.get("suites")?.as_array()?;
    let mut out = Vec::new();
    collect_suites(suites, &mut out);
    Some(out)
}

fn collect_suites(suites: &[serde_json::Value], out: &mut Vec<PwCase>) {
    for suite in suites {
        if let Some(specs) = suite.get("specs").and_then(|s| s.as_array()) {
            for spec in specs {
                let file = spec
                    .get("file")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let title = spec
                    .get("title")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let line = spec.get("line").and_then(|n| n.as_u64()).unwrap_or(0) as u32;
                let tests = match spec.get("tests").and_then(|t| t.as_array()) {
                    Some(t) => t,
                    None => continue,
                };
                for t in tests {
                    let results = match t.get("results").and_then(|r| r.as_array()) {
                        Some(r) if !r.is_empty() => r,
                        _ => {
                            out.push(PwCase {
                                file: file.clone(),
                                title: title.clone(),
                                line,
                                status: "didnotrun".into(),
                                ..Default::default()
                            });
                            continue;
                        }
                    };
                    // Emit one PwCase per result (per-attempt for retries).
                    for r in results {
                        let status = r
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("")
                            .to_string();
                        let duration_ms =
                            r.get("duration").and_then(|d| d.as_u64()).unwrap_or(0);
                        let mut case = PwCase {
                            file: file.clone(),
                            title: title.clone(),
                            line,
                            duration_ms,
                            status,
                            ..Default::default()
                        };
                        if case.status == "failed"
                            || case.status == "timedOut"
                            || case.status == "interrupted"
                        {
                            case.status = case.status.to_lowercase();
                            if let Some(errs) = r.get("errors").and_then(|e| e.as_array()) {
                                if let Some(first) = errs.first() {
                                    if let Some(msg) =
                                        first.get("message").and_then(|m| m.as_str())
                                    {
                                        let (expected, actual) =
                                            extract_expected_actual(&strip_ansi(msg));
                                        case.expected = expected;
                                        case.actual = actual;
                                        case.message = Some(msg.to_string());
                                    }
                                    if let Some(loc) = first.get("location") {
                                        if let Some(ln) =
                                            loc.get("line").and_then(|l| l.as_u64())
                                        {
                                            case.fail_line = Some(ln as u32);
                                        }
                                        if let Some(f) =
                                            loc.get("file").and_then(|f| f.as_str())
                                        {
                                            // Prefer the absolute file path from the error location.
                                            case.file = f.to_string();
                                        }
                                    }
                                }
                            }
                        }
                        out.push(case);
                    }
                }
            }
        }
        if let Some(child) = suite.get("suites").and_then(|s| s.as_array()) {
            collect_suites(child, out);
        }
    }
}

fn extract_expected_actual(msg: &str) -> (Option<String>, Option<String>) {
    let mut expected: Option<String> = None;
    let mut actual: Option<String> = None;
    for line in msg.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Expected:") {
            expected = Some(rest.trim().to_string());
        } else if let Some(rest) = t.strip_prefix("Received:") {
            actual = Some(rest.trim().to_string());
        }
    }
    (expected, actual)
}

/// Delegate to the canonical stripper in adapter_util so there's one
/// implementation to maintain and test.
fn strip_ansi(s: &str) -> String {
    super::adapter_util::strip_ansi(s)
}

impl TestAdapter for PlaywrightAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Direct config in project root
        if has_playwright_config(project_root) {
            return if has_competing_framework(project_root) {
                80
            } else {
                95
            };
        }

        // Monorepo: check if any workspace has a playwright config
        if find_playwright_workspace(project_root).is_some() {
            // Always return 80 in monorepos — bun:test or vitest likely handles unit/integration,
            // so Playwright should only be used when explicitly requested.
            return 80;
        }

        0
    }

    fn name(&self) -> &str {
        "playwright"
    }

    fn suite_command(
        &self,
        project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        // Playwright tests are E2E only
        if matches!(level, Some(TestLevel::Unit) | Some(TestLevel::Integration)) {
            return Err(crate::Error::ValidationError(
                "Playwright runs E2E tests only. Use framework='vitest' or 'bun' for unit/integration.".to_string()
            ));
        }

        let reporter_path = ensure_reporter_file();
        let progress_file = progress_file_path();
        let mut env = HashMap::new();
        env.insert("STROBE_REPORTER".to_string(), reporter_path.clone());
        env.insert("STROBE_PROGRESS_FILE".to_string(), progress_file);

        // Resolve workspace cwd — Playwright's node_modules and config live in the workspace dir.
        let cwd = resolve_playwright_cwd(project_root);

        // Invoke Playwright CLI directly via bun (not bunx) to avoid exec-replacement
        // that confuses Frida's process tracking. No --reporter on CLI — playwright.config.ts
        // detects the Strobe reporter file on disk and uses both JUnit + progress reporter.
        // `--update-snapshots=all` is appended generically by TestRunner::run
        // when env contains STROBE_UPDATE_SNAPSHOTS=1.
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "node_modules/@playwright/test/cli.js".to_string(),
                "test".to_string(),
                // Capture a full browser trace for failures (CLI overrides config).
                // The custom reporter surfaces the trace.zip path per test so it
                // lands in <run_dir>/tests/<id>/stderr.log.
                "--trace=retain-on-failure".to_string(),
            ],
            env,
            cwd,
            remove_env: vec![],
        })
    }

    fn single_test_command(
        &self,
        project_root: &Path,
        test_name: &str,
    ) -> crate::Result<TestCommand> {
        let reporter_path = ensure_reporter_file();
        let progress_file = progress_file_path();
        let mut env = HashMap::new();
        env.insert("STROBE_PROGRESS_FILE".to_string(), progress_file);

        let cwd = resolve_playwright_cwd(project_root);

        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "node_modules/@playwright/test/cli.js".to_string(),
                "test".to_string(),
                "--grep".to_string(),
                escape_grep_regex(test_name),
                // Full browser trace for failures (see suite_command).
                "--trace=retain-on-failure".to_string(),
            ],
            env,
            cwd,
            remove_env: vec![],
        })
    }

    fn parse_output(&self, stdout: &str, _stderr: &str, _exit_code: i32) -> TestResult {
        // Primary: try JUnit XML from stdout (works when Frida captures stdout correctly)
        let blocks: Vec<&str> = stdout
            .split("<?xml")
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim())
            .collect();

        let mut total = TestResult {
            summary: TestSummary {
                passed: 0,
                failed: 0,
                skipped: 0,
                stuck: None,
                duration_ms: 0,
            },
            failures: vec![],
            stuck: vec![],
            all_tests: vec![],
        };

        if !blocks.is_empty() {
            for block in blocks {
                let xml = format!("<?xml {}", block);
                let result = super::bun_adapter::parse_junit_xml(&xml);
                total.summary.passed += result.summary.passed;
                total.summary.failed += result.summary.failed;
                total.summary.skipped += result.summary.skipped;
                total.summary.duration_ms += result.summary.duration_ms;
                total.failures.extend(result.failures);
                total.all_tests.extend(result.all_tests);
            }
        }

        // Fallback: reconstruct from the progress file (handles exec-replacement cases
        // where Frida loses stdout capture after bun→node exec).
        if total.all_tests.is_empty() {
            if let Ok(content) = std::fs::read_to_string(PROGRESS_FILE) {
                for segment in content.split("STROBE_TEST:") {
                    let json_str = segment.trim();
                    if json_str.is_empty() || !json_str.starts_with('{') {
                        continue;
                    }
                    let json_end = json_str.find('\n').unwrap_or(json_str.len());
                    let json = &json_str[..json_end];
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                        let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
                        let name = v
                            .get("n")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let dur = v.get("d").and_then(|d| d.as_u64()).unwrap_or(0);
                        match event {
                            "pass" => {
                                total.summary.passed += 1;
                                total.all_tests.push(TestDetail {
                                    name,
                                    status: TestStatus::Pass,
                                    duration_ms: dur,
                                    stdout: None,
                                    stderr: None,
                                    message: None,
                                });
                            }
                            "fail" => {
                                let file =
                                    v.get("f").and_then(|f| f.as_str()).map(|s| s.to_string());
                                let line = v.get("l").and_then(|l| l.as_u64()).map(|l| l as u32);
                                let msg = v
                                    .get("m")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("Test failed")
                                    .to_string();
                                total.summary.failed += 1;
                                total.failures.push(TestFailure {
                                    name: name.clone(),
                                    file: file.clone(),
                                    line,
                                    message: msg.clone(),
                                    rerun: None,
                                    suggested_traces: vec![],
                                });
                                total.all_tests.push(TestDetail {
                                    name,
                                    status: TestStatus::Fail,
                                    duration_ms: dur,
                                    stdout: None,
                                    stderr: None,
                                    message: Some(msg),
                                });
                            }
                            "skip" => {
                                total.summary.skipped += 1;
                                total.all_tests.push(TestDetail {
                                    name,
                                    status: TestStatus::Skip,
                                    duration_ms: dur,
                                    stdout: None,
                                    stderr: None,
                                    message: None,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                total.summary.duration_ms = total.all_tests.iter().map(|t| t.duration_ms).sum();
            }
        }

        total
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            traces.push(format!("@file:{}", stem));
        }
        traces
    }

    fn default_timeout(&self, _level: Option<TestLevel>) -> u64 {
        600_000 // 10 minutes — browser startup, fixtures, network calls.
                // Large E2E suites (100+ tests) can easily exceed 5 minutes.
                // Override via .strobe/settings.json "test.timeoutMs" or debug_test timeout param.
    }
}

const PLAYWRIGHT_CONFIGS: &[&str] = &[
    "playwright.config.ts",
    "playwright.config.js",
    "playwright.config.mts",
];

/// Check if a directory contains a playwright config file.
fn has_playwright_config(dir: &Path) -> bool {
    PLAYWRIGHT_CONFIGS.iter().any(|cfg| dir.join(cfg).exists())
}

/// Check if project has vitest or jest (competing framework).
fn has_competing_framework(project_root: &Path) -> bool {
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        return pkg.contains("\"vitest\"") || pkg.contains("\"jest\"");
    }
    false
}

/// Find the workspace directory containing a playwright config in a monorepo.
/// Returns the absolute path to the workspace dir, or None.
pub(crate) fn find_playwright_workspace(project_root: &Path) -> Option<std::path::PathBuf> {
    let pkg = std::fs::read_to_string(project_root.join("package.json")).ok()?;
    if !pkg.contains("\"workspaces\"") {
        return None;
    }
    let dirs = find_workspace_dirs(project_root, &pkg);
    dirs.into_iter().find(|ws| has_playwright_config(ws))
}

/// Resolve the cwd for Playwright commands.
/// In monorepos, Playwright's config and node_modules live in a workspace dir.
/// Returns None if config is in project_root (no cwd override needed).
fn resolve_playwright_cwd(project_root: &Path) -> Option<String> {
    // Config at project root — no cwd needed
    if has_playwright_config(project_root) {
        return None;
    }
    // Monorepo: find workspace with config
    find_playwright_workspace(project_root).map(|ws| ws.to_string_lossy().into_owned())
}

use super::TestProgress;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

/// File offset tracker — how far we've read into the progress file.
static PROGRESS_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Poll the progress file for new STROBE_TEST events and update progress.
/// Called from mod.rs progress loop instead of the vitest stderr-based updater.
pub fn update_progress(_text: &str, progress: &Arc<Mutex<TestProgress>>) {
    // Read new content from the progress file since last offset
    let content = match std::fs::read_to_string(PROGRESS_FILE) {
        Ok(c) => c,
        Err(_) => return,
    };

    let offset = PROGRESS_OFFSET.load(Ordering::Relaxed);
    if content.len() <= offset {
        return; // No new data
    }

    let new_content = &content[offset..];
    PROGRESS_OFFSET.store(content.len(), Ordering::Relaxed);

    // Parse STROBE_TEST events using the same protocol as vitest
    let mut p = progress.lock().unwrap();
    for segment in new_content.split("STROBE_TEST:") {
        let json_str = segment.trim();
        if json_str.is_empty() || !json_str.starts_with('{') {
            continue;
        }
        let json_end = json_str.find('\n').unwrap_or(json_str.len());
        let json = &json_str[..json_end];

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
            p.has_custom_reporter = true;
            let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
            let name = v
                .get("n")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();

            match event {
                "module_start" => {
                    if p.phase == super::TestPhase::Compiling {
                        p.phase = super::TestPhase::Running;
                    }
                }
                "start" => {
                    p.start_test(name);
                }
                "pass" => {
                    p.passed += 1;
                    p.finish_test(&name);
                }
                "fail" => {
                    p.failed += 1;
                    p.finish_test(&name);
                }
                "skip" => {
                    p.skipped += 1;
                    p.finish_test(&name);
                }
                _ => {}
            }
        }
    }
}

/// Reset the file offset — call before each test run.
pub fn reset_progress() {
    PROGRESS_OFFSET.store(0, Ordering::Relaxed);
    let _ = std::fs::write(PROGRESS_FILE, "");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_playwright_config() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = PlaywrightAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("playwright.config.ts"), "export default {}").unwrap();
        assert_eq!(adapter.detect(dir.path(), None), 95);
    }

    #[test]
    fn test_detect_playwright_with_vitest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("playwright.config.ts"), "export default {}").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"devDependencies": {"vitest": "^3"}}"#,
        )
        .unwrap();
        let adapter = PlaywrightAdapter;
        // Should return lower confidence when vitest is present
        assert_eq!(adapter.detect(dir.path(), None), 80);
    }

    #[test]
    fn test_suite_command_e2e() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PlaywrightAdapter
            .suite_command(dir.path(), Some(TestLevel::E2e), &Default::default())
            .unwrap();
        assert_eq!(cmd.program, "bun");
        assert!(cmd.args.contains(&"test".to_string()));
        // No --reporter on CLI — config handles reporters via STROBE_REPORTER env
        assert!(cmd.env.contains_key("STROBE_REPORTER"));
        assert!(cmd.env.contains_key("STROBE_PROGRESS_FILE"));
    }

    #[test]
    fn test_suite_command_unit_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            PlaywrightAdapter.suite_command(dir.path(), Some(TestLevel::Unit), &Default::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_single_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PlaywrightAdapter
            .single_test_command(dir.path(), "login page")
            .unwrap();
        assert!(cmd.args.contains(&"--grep".to_string()));
        assert!(cmd.args.contains(&"login page".to_string()));
    }

    #[test]
    fn test_single_test_command_escapes_regex_metachars() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PlaywrightAdapter
            .single_test_command(
                dir.path(),
                "[Phase 9] S3b role list/detail visuals + app-registry scopes",
            )
            .unwrap();
        // The grep value must be a regex-escaped literal so it matches the
        // test title verbatim instead of being parsed as a regex (where `+`
        // is a quantifier and `[...]` a char class → zero matches).
        let grep_idx = cmd.args.iter().position(|a| a == "--grep").unwrap();
        let grep_val = &cmd.args[grep_idx + 1];
        assert_eq!(
            grep_val,
            r"\[Phase 9\] S3b role list/detail visuals \+ app-registry scopes"
        );
    }

    #[test]
    fn test_detect_monorepo_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Root has workspaces but no playwright config
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#,
        )
        .unwrap();
        // Web workspace has playwright config
        let web = dir.path().join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("playwright.config.ts"), "export default {}").unwrap();

        let adapter = PlaywrightAdapter;
        let conf = adapter.detect(dir.path(), None);
        assert_eq!(
            conf, 80,
            "monorepo with playwright workspace should detect at 80"
        );
    }

    #[test]
    fn test_suite_command_monorepo_cwd() {
        let dir = tempfile::tempdir().unwrap();
        // Root with workspaces, no playwright config at root
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#,
        )
        .unwrap();
        // Web workspace with playwright config
        let web = dir.path().join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("playwright.config.ts"), "export default {}").unwrap();

        let cmd = PlaywrightAdapter
            .suite_command(dir.path(), Some(TestLevel::E2e), &Default::default())
            .unwrap();
        assert!(cmd.cwd.is_some(), "should set cwd for monorepo");
        assert!(
            cmd.cwd.as_ref().unwrap().ends_with("apps/web"),
            "cwd should point to web workspace, got: {:?}",
            cmd.cwd
        );
    }

    #[test]
    fn test_suite_command_no_cwd_when_config_at_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("playwright.config.ts"), "export default {}").unwrap();

        let cmd = PlaywrightAdapter
            .suite_command(dir.path(), Some(TestLevel::E2e), &Default::default())
            .unwrap();
        assert!(
            cmd.cwd.is_none(),
            "should not set cwd when config is at project root"
        );
    }

    #[test]
    fn test_update_progress_reads_file() {
        use super::super::TestProgress;

        // Reset offset and write a test progress file
        reset_progress();
        std::fs::write(
            PROGRESS_FILE,
            concat!(
                "STROBE_TEST:{\"e\":\"module_start\",\"n\":\"test.ts\"}\n",
                "STROBE_TEST:{\"e\":\"start\",\"n\":\"test one\"}\n",
                "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test one\",\"d\":100}\n",
                "STROBE_TEST:{\"e\":\"start\",\"n\":\"test two\"}\n",
                "STROBE_TEST:{\"e\":\"fail\",\"n\":\"test two\",\"d\":50}\n",
            ),
        )
        .unwrap();

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        // Call update_progress — should read file and update counts
        update_progress("", &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1, "should have 1 passed, got {}", p.passed);
        assert_eq!(p.failed, 1, "should have 1 failed, got {}", p.failed);
        assert!(p.has_custom_reporter, "should set custom reporter flag");
        assert_eq!(
            p.phase,
            super::super::TestPhase::Running,
            "module_start should transition to Running"
        );

        // Second call with no new data — should be a no-op
        drop(p);
        update_progress("", &progress);
        let p2 = progress.lock().unwrap();
        assert_eq!(p2.passed, 1, "no-op call should not change counts");
        assert_eq!(p2.failed, 1);

        // Clean up
        let _ = std::fs::remove_file(PROGRESS_FILE);
    }

    #[test]
    fn test_parse_output_falls_back_to_progress_file() {
        // Write progress file with known content
        std::fs::write(
            PROGRESS_FILE,
            concat!(
                "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test alpha\",\"d\":100}\n",
                "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test beta\",\"d\":200}\n",
                "STROBE_TEST:{\"e\":\"fail\",\"n\":\"test gamma\",\"d\":50}\n",
            ),
        )
        .unwrap();

        // Call parse_output with empty stdout — should fall back to file
        let adapter = PlaywrightAdapter;
        let result = adapter.parse_output("", "", 1);

        assert_eq!(
            result.summary.passed, 2,
            "should find 2 passed from file, got {}",
            result.summary.passed
        );
        assert_eq!(
            result.summary.failed, 1,
            "should find 1 failed from file, got {}",
            result.summary.failed
        );
        assert_eq!(result.all_tests.len(), 3, "should have 3 tests total");
        assert_eq!(result.failures.len(), 1, "should have 1 failure");

        let _ = std::fs::remove_file(PROGRESS_FILE);
    }
}
