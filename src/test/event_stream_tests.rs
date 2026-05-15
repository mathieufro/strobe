use super::event_stream::{EventStream, FailureDetail, TestEvent};
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn writes_run_started_event_with_metadata() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let mut stream = EventStream::create(&path).unwrap();

    stream
        .emit(TestEvent::RunStarted {
            run_id: "abc123".into(),
            project_root: "/repo".into(),
            suite: "unit".into(),
            framework: "bun".into(),
            mode: "first-fail".into(),
            started_at_ms: 1_700_000_000_000,
        })
        .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let line = contents.lines().next().unwrap();
    let event: Value = serde_json::from_str(line).unwrap();

    assert_eq!(event["type"], "run_started");
    assert_eq!(event["run_id"], "abc123");
    assert_eq!(event["framework"], "bun");
    assert_eq!(event["mode"], "first-fail");
}

#[test]
fn writes_test_failed_event_with_code_context() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let mut stream = EventStream::create(&path).unwrap();

    stream
        .emit(TestEvent::TestFailed {
            test_id: "auth-service.test.ts::login::rejects bad password".into(),
            file: "apps/api/src/tests/auth-service.test.ts".into(),
            line: 42,
            duration_ms: 120,
            error: FailureDetail {
                message: "expected 401 got 200".into(),
                kind: "AssertionError".into(),
                stack_frames: vec![],
                expected: Some("401".into()),
                actual: Some("200".into()),
                code_context: vec!["  expect(res.status).toBe(401)".into()],
            },
            artifact_dir: "tests/auth-service__login__rejects_bad_password".into(),
        })
        .unwrap();

    let line = std::fs::read_to_string(&path).unwrap();
    let event: Value = serde_json::from_str(line.lines().next().unwrap()).unwrap();
    assert_eq!(event["type"], "test_failed");
    assert_eq!(event["error"]["expected"], "401");
    assert_eq!(event["error"]["actual"], "200");
    assert!(event["artifact_dir"]
        .as_str()
        .unwrap()
        .contains("rejects_bad_password"));
}

#[test]
fn flushes_each_event_immediately_for_tailing() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let mut stream = EventStream::create(&path).unwrap();

    stream
        .emit(TestEvent::TestPassed {
            test_id: "t1".into(),
            file: "a.ts".into(),
            line: 1,
            duration_ms: 5,
            artifact_dir: "tests/t1".into(),
        })
        .unwrap();

    // No drop / no explicit flush — must be readable now.
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("test_passed"));
}

#[test]
fn omits_expected_actual_when_none() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let mut stream = EventStream::create(&path).unwrap();

    stream
        .emit(TestEvent::TestFailed {
            test_id: "t".into(),
            file: "f.ts".into(),
            line: 1,
            duration_ms: 1,
            error: FailureDetail {
                message: "boom".into(),
                kind: "Error".into(),
                stack_frames: vec![],
                expected: None,
                actual: None,
                code_context: vec![],
            },
            artifact_dir: "tests/t".into(),
        })
        .unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    let event: Value = serde_json::from_str(contents.lines().next().unwrap()).unwrap();
    assert!(event["error"].get("expected").is_none());
    assert!(event["error"].get("actual").is_none());
}
