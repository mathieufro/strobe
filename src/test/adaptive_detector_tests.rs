use super::adaptive_detector::{AdaptiveDetector, TestStats, DEFAULT_TIMEOUT_MS};
use crate::db::Database;

#[test]
fn records_green_durations_and_computes_rolling_median() {
    let db = Database::open_in_memory().unwrap();
    let stats = TestStats::new(&db);

    for ms in [
        100u64, 110, 105, 120, 115, 108, 112, 118, 100, 110, 5000, /* outlier */
    ] {
        stats.record_green("/repo", "auth/login::ok", ms).unwrap();
    }

    let median = stats.median("/repo", "auth/login::ok").unwrap();
    assert!(
        median >= 105 && median <= 115,
        "median should be ~110, got {}",
        median
    );

    let count = stats.window_size("/repo", "auth/login::ok").unwrap();
    assert_eq!(count, 10, "rolling window capped at 10");
}

#[test]
fn detects_stall_at_5x_median_with_30s_floor() {
    let db = Database::open_in_memory().unwrap();
    let stats = TestStats::new(&db);
    for _ in 0..5 {
        stats.record_green("/repo", "t1", 200).unwrap();
    }
    for _ in 0..5 {
        stats.record_green("/repo", "t2", 100_000).unwrap();
    }

    let detector = AdaptiveDetector::new(stats);

    // Below floor (5*200=1000ms < 30s floor) → uses 30s
    assert_eq!(detector.threshold_ms("/repo", "t1"), 30_000);
    assert!(!detector.is_stalled("/repo", "t1", 29_000));
    assert!(detector.is_stalled("/repo", "t1", 31_000));

    // Above floor (5*100_000=500_000) → uses 5×
    assert_eq!(detector.threshold_ms("/repo", "t2"), 500_000);
    assert!(!detector.is_stalled("/repo", "t2", 400_000));
    assert!(detector.is_stalled("/repo", "t2", 600_000));
}

#[test]
fn unknown_test_falls_back_to_fixed_timeout() {
    let stats = TestStats::new(&Database::open_in_memory().unwrap());
    let detector = AdaptiveDetector::new(stats);
    assert_eq!(
        detector.threshold_ms("/repo", "never-seen-test"),
        DEFAULT_TIMEOUT_MS
    );
}
