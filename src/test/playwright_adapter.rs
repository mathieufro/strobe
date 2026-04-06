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

pub struct PlaywrightAdapter;

impl TestAdapter for PlaywrightAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Direct config in project root
        if has_playwright_config(project_root) {
            return if has_competing_framework(project_root) { 80 } else { 95 };
        }

        // Monorepo: check if any workspace has a playwright config
        if find_playwright_workspace(project_root).is_some() {
            // Always return 80 in monorepos — bun:test or vitest likely handles unit/integration,
            // so Playwright should only be used when explicitly requested.
            return 80;
        }

        0
    }

    fn name(&self) -> &str { "playwright" }

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
        Ok(TestCommand {
            program: "bun".to_string(),
            args: vec![
                "node_modules/@playwright/test/cli.js".to_string(),
                "test".to_string(),
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
                test_name.to_string(),
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
            summary: TestSummary { passed: 0, failed: 0, skipped: 0, stuck: None, duration_ms: 0 },
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
                    if json_str.is_empty() || !json_str.starts_with('{') { continue; }
                    let json_end = json_str.find('\n').unwrap_or(json_str.len());
                    let json = &json_str[..json_end];
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
                        let event = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
                        let name = v.get("n").and_then(|n| n.as_str()).unwrap_or("").to_string();
                        let dur = v.get("d").and_then(|d| d.as_u64()).unwrap_or(0);
                        match event {
                            "pass" => {
                                total.summary.passed += 1;
                                total.all_tests.push(TestDetail {
                                    name, status: TestStatus::Pass, duration_ms: dur,
                                    stdout: None, stderr: None, message: None,
                                });
                            }
                            "fail" => {
                                let file = v.get("f").and_then(|f| f.as_str()).map(|s| s.to_string());
                                let line = v.get("l").and_then(|l| l.as_u64()).map(|l| l as u32);
                                let msg = v.get("m").and_then(|m| m.as_str())
                                    .unwrap_or("Test failed").to_string();
                                total.summary.failed += 1;
                                total.failures.push(TestFailure {
                                    name: name.clone(), file: file.clone(), line,
                                    message: msg.clone(),
                                    rerun: None, suggested_traces: vec![],
                                });
                                total.all_tests.push(TestDetail {
                                    name, status: TestStatus::Fail, duration_ms: dur,
                                    stdout: None, stderr: None, message: Some(msg),
                                });
                            }
                            "skip" => {
                                total.summary.skipped += 1;
                                total.all_tests.push(TestDetail {
                                    name, status: TestStatus::Skip, duration_ms: dur,
                                    stdout: None, stderr: None, message: None,
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
            let stem = Path::new(file).file_stem()
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
    if !pkg.contains("\"workspaces\"") { return None; }
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
    find_playwright_workspace(project_root)
        .map(|ws| ws.to_string_lossy().into_owned())
}

use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use super::TestProgress;

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
            let name = v.get("n").and_then(|n| n.as_str()).unwrap_or("").to_string();

            match event {
                "module_start" => {
                    if p.phase == super::TestPhase::Compiling {
                        p.phase = super::TestPhase::Running;
                    }
                }
                "start" => { p.start_test(name); }
                "pass"  => { p.passed += 1; p.finish_test(&name); }
                "fail"  => { p.failed += 1; p.finish_test(&name); }
                "skip"  => { p.skipped += 1; p.finish_test(&name); }
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
        ).unwrap();
        let adapter = PlaywrightAdapter;
        // Should return lower confidence when vitest is present
        assert_eq!(adapter.detect(dir.path(), None), 80);
    }

    #[test]
    fn test_suite_command_e2e() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PlaywrightAdapter.suite_command(dir.path(), Some(TestLevel::E2e), &Default::default()).unwrap();
        assert_eq!(cmd.program, "bun");
        assert!(cmd.args.contains(&"test".to_string()));
        // No --reporter on CLI — config handles reporters via STROBE_REPORTER env
        assert!(cmd.env.contains_key("STROBE_REPORTER"));
        assert!(cmd.env.contains_key("STROBE_PROGRESS_FILE"));
    }

    #[test]
    fn test_suite_command_unit_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = PlaywrightAdapter.suite_command(dir.path(), Some(TestLevel::Unit), &Default::default());
        assert!(result.is_err());
    }

    #[test]
    fn test_single_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = PlaywrightAdapter.single_test_command(dir.path(), "login page").unwrap();
        assert!(cmd.args.contains(&"--grep".to_string()));
        assert!(cmd.args.contains(&"login page".to_string()));
    }

    #[test]
    fn test_detect_monorepo_workspace() {
        let dir = tempfile::tempdir().unwrap();
        // Root has workspaces but no playwright config
        std::fs::write(dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#).unwrap();
        // Web workspace has playwright config
        let web = dir.path().join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("playwright.config.ts"), "export default {}").unwrap();

        let adapter = PlaywrightAdapter;
        let conf = adapter.detect(dir.path(), None);
        assert_eq!(conf, 80, "monorepo with playwright workspace should detect at 80");
    }

    #[test]
    fn test_suite_command_monorepo_cwd() {
        let dir = tempfile::tempdir().unwrap();
        // Root with workspaces, no playwright config at root
        std::fs::write(dir.path().join("package.json"),
            r#"{"workspaces": ["apps/*"]}"#).unwrap();
        // Web workspace with playwright config
        let web = dir.path().join("apps/web");
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("playwright.config.ts"), "export default {}").unwrap();

        let cmd = PlaywrightAdapter.suite_command(dir.path(), Some(TestLevel::E2e), &Default::default()).unwrap();
        assert!(cmd.cwd.is_some(), "should set cwd for monorepo");
        assert!(cmd.cwd.as_ref().unwrap().ends_with("apps/web"),
            "cwd should point to web workspace, got: {:?}", cmd.cwd);
    }

    #[test]
    fn test_suite_command_no_cwd_when_config_at_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("playwright.config.ts"), "export default {}").unwrap();

        let cmd = PlaywrightAdapter.suite_command(dir.path(), Some(TestLevel::E2e), &Default::default()).unwrap();
        assert!(cmd.cwd.is_none(), "should not set cwd when config is at project root");
    }

    #[test]
    fn test_update_progress_reads_file() {
        use super::super::TestProgress;

        // Reset offset and write a test progress file
        reset_progress();
        std::fs::write(PROGRESS_FILE, concat!(
            "STROBE_TEST:{\"e\":\"module_start\",\"n\":\"test.ts\"}\n",
            "STROBE_TEST:{\"e\":\"start\",\"n\":\"test one\"}\n",
            "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test one\",\"d\":100}\n",
            "STROBE_TEST:{\"e\":\"start\",\"n\":\"test two\"}\n",
            "STROBE_TEST:{\"e\":\"fail\",\"n\":\"test two\",\"d\":50}\n",
        )).unwrap();

        let progress = Arc::new(Mutex::new(TestProgress::new()));

        // Call update_progress — should read file and update counts
        update_progress("", &progress);

        let p = progress.lock().unwrap();
        assert_eq!(p.passed, 1, "should have 1 passed, got {}", p.passed);
        assert_eq!(p.failed, 1, "should have 1 failed, got {}", p.failed);
        assert!(p.has_custom_reporter, "should set custom reporter flag");
        assert_eq!(p.phase, super::super::TestPhase::Running, "module_start should transition to Running");

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
        std::fs::write(PROGRESS_FILE, concat!(
            "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test alpha\",\"d\":100}\n",
            "STROBE_TEST:{\"e\":\"pass\",\"n\":\"test beta\",\"d\":200}\n",
            "STROBE_TEST:{\"e\":\"fail\",\"n\":\"test gamma\",\"d\":50}\n",
        )).unwrap();

        // Call parse_output with empty stdout — should fall back to file
        let adapter = PlaywrightAdapter;
        let result = adapter.parse_output("", "", 1);

        assert_eq!(result.summary.passed, 2, "should find 2 passed from file, got {}", result.summary.passed);
        assert_eq!(result.summary.failed, 1, "should find 1 failed from file, got {}", result.summary.failed);
        assert_eq!(result.all_tests.len(), 3, "should have 3 tests total");
        assert_eq!(result.failures.len(), 1, "should have 1 failure");

        let _ = std::fs::remove_file(PROGRESS_FILE);
    }
}
