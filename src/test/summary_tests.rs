use super::summary;
use tempfile::tempdir;

fn write_fixture_events(run_dir: &std::path::Path) {
    let mut s = String::new();
    s.push_str(r#"{"type":"run_started","run_id":"r1","project_root":"/x","suite":"unit","framework":"bun","mode":"first-fail","started_at_ms":0}"#);
    s.push('\n');
    // auth/login — pass
    s.push_str(r#"{"type":"test_started","test_id":"auth/login","file":"auth/login.ts","line":3}"#);
    s.push('\n');
    s.push_str(r#"{"type":"test_passed","test_id":"auth/login","file":"auth/login.ts","line":3,"duration_ms":120,"artifact_dir":"tests/auth_login"}"#);
    s.push('\n');
    // auth/register — fail
    s.push_str(r#"{"type":"test_started","test_id":"auth/register","file":"auth/register.ts","line":10}"#);
    s.push('\n');
    s.push_str(r#"{"type":"test_failed","test_id":"auth/register","file":"auth/register.ts","line":10,"duration_ms":40,"error":{"message":"expected 401 got 200","kind":"AssertionError","stack_frames":[],"expected":"401","actual":"200","code_context":["  expect(res.status).toBe(401)"]},"artifact_dir":"tests/auth_register"}"#);
    s.push('\n');
    // billing/charge — stalled
    s.push_str(r#"{"type":"test_started","test_id":"billing/charge","file":"billing/charge.ts","line":5}"#);
    s.push('\n');
    s.push_str(r#"{"type":"test_stalled","test_id":"billing/charge","file":"billing/charge.ts","elapsed_ms":1200,"median_ms":240,"threshold_ms":1200,"artifact_dir":"tests/billing_charge"}"#);
    s.push('\n');
    // two more passing
    s.push_str(r#"{"type":"test_passed","test_id":"x/two","file":"x.ts","line":1,"duration_ms":2,"artifact_dir":"tests/x_two"}"#);
    s.push('\n');
    s.push_str(r#"{"type":"test_passed","test_id":"x/three","file":"x.ts","line":2,"duration_ms":3,"artifact_dir":"tests/x_three"}"#);
    s.push('\n');
    s.push_str(r#"{"type":"run_complete","run_id":"r1","exit_code":1,"total":5,"passed":3,"failed":1,"stalled":1,"skipped":0,"duration_ms":1500}"#);
    s.push('\n');
    std::fs::write(run_dir.join("events.ndjson"), s).unwrap();
}

fn write_all_pass_events(run_dir: &std::path::Path) {
    let mut s = String::new();
    s.push_str(r#"{"type":"run_started","run_id":"r1","project_root":"/x","suite":"unit","framework":"bun","mode":"first-fail","started_at_ms":0}"#);
    s.push('\n');
    for i in 0..3 {
        s.push_str(&format!(
            r#"{{"type":"test_passed","test_id":"t{}","file":"x.ts","line":1,"duration_ms":2,"artifact_dir":"tests/t{}"}}"#,
            i, i
        ));
        s.push('\n');
    }
    s.push_str(r#"{"type":"run_complete","run_id":"r1","exit_code":0,"total":3,"passed":3,"failed":0,"stalled":0,"skipped":0,"duration_ms":50}"#);
    s.push('\n');
    std::fs::write(run_dir.join("events.ndjson"), s).unwrap();
}

#[test]
fn generates_summary_table_with_links_to_artifact_dirs() {
    let run_dir = tempdir().unwrap();
    write_fixture_events(run_dir.path());

    summary::generate(run_dir.path()).unwrap();

    let summary = std::fs::read_to_string(run_dir.path().join("summary.md")).unwrap();
    assert!(
        summary.contains("| Test | Status | Duration | Artifacts |"),
        "summary: {}",
        summary
    );
    assert!(
        summary.contains("| auth/login | pass | 120 ms | [tests/auth_login](tests/auth_login) |"),
        "summary: {}",
        summary
    );
    assert!(summary.contains(" fail "));
    assert!(summary.contains(" stalled "));

    let failures = std::fs::read_to_string(run_dir.path().join("failures.md")).unwrap();
    assert!(failures.contains("## auth/register"), "failures: {}", failures);
    assert!(
        failures.contains("**Error:** expected 401 got 200"),
        "failures: {}",
        failures
    );
    assert!(failures.contains("```"));
    assert!(failures.contains("[Artifacts](tests/auth_register)"));
}

#[test]
fn no_failures_writes_placeholder() {
    let run_dir = tempdir().unwrap();
    write_all_pass_events(run_dir.path());
    summary::generate(run_dir.path()).unwrap();

    let failures = std::fs::read_to_string(run_dir.path().join("failures.md")).unwrap();
    assert!(
        failures.contains("No failures"),
        "failures.md must exist even when all tests pass"
    );
}
