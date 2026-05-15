use super::adapter::{Mode, RunConfig};
use super::playwright_adapter::PlaywrightAdapter;
use tempfile::tempdir;

fn fixtures() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playwright")
}

#[tokio::test]
async fn streams_events_for_passing_suite() {
    let run_dir = tempdir().unwrap();
    let adapter = PlaywrightAdapter::new();
    let result = adapter
        .run(
            &fixtures(),
            run_dir.path(),
            &RunConfig {
                test_filter: Some("tests/passing.spec.ts".into()),
                mode: Mode::CollectAll,
            },
        )
        .await
        .unwrap();

    let ndjson = std::fs::read_to_string(run_dir.path().join("events.ndjson")).unwrap();
    let events: Vec<serde_json::Value> = ndjson
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert_eq!(events[0]["type"], "run_started");
    assert_eq!(events[0]["framework"], "playwright");
    assert!(events.iter().any(|e| e["type"] == "test_passed"));
    assert_eq!(events.last().unwrap()["type"], "run_complete");
    assert_eq!(events.last().unwrap()["failed"], 0);
    assert_eq!(result.exit_code, 0);
}

#[tokio::test]
async fn first_fail_mode_aborts() {
    let run_dir = tempdir().unwrap();
    let adapter = PlaywrightAdapter::new();
    let result = adapter
        .run(
            &fixtures(),
            run_dir.path(),
            &RunConfig {
                test_filter: Some("tests/failing.spec.ts".into()),
                mode: Mode::FirstFail,
            },
        )
        .await
        .unwrap();

    let ndjson = std::fs::read_to_string(run_dir.path().join("events.ndjson")).unwrap();
    let events: Vec<serde_json::Value> = ndjson
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    let failed: Vec<_> = events.iter().filter(|e| e["type"] == "test_failed").collect();
    assert_eq!(failed.len(), 1, "first-fail must stop after one failure");
    assert!(!failed[0]["error"]["code_context"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_ne!(result.exit_code, 0);
}
