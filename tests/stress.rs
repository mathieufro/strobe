//! Performance benchmarks for event storage.
//! Run manually: cargo test --test stress -- --ignored --nocapture

use std::time::Instant;
use strobe::db::{Database, Event, EventType};
use tempfile::tempdir;

#[test]
#[ignore] // Run with: cargo test --release --test stress -- --ignored --nocapture
fn stress_test_event_limits() {
    let limits_to_test = vec![10_000, 50_000, 100_000, 200_000, 500_000, 1_000_000];

    println!("\n=== Event Limit Stress Test ===\n");
    println!("Testing insert, query, and cleanup performance at various limits\n");

    for limit in limits_to_test {
        println!("--- Testing limit: {} events ---", limit);
        test_limit(limit);
        println!();
    }
}

fn test_limit(max_events: usize) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("stress.db");
    let db = Database::open(&db_path).unwrap();

    db.create_session("stress-test", "/bin/test", "/home", 1234)
        .unwrap();

    // Phase 1: Fill to limit
    println!("  Phase 1: Filling to {} events...", max_events);
    let start = Instant::now();
    let mut batch = Vec::with_capacity(1000);

    for i in 0..max_events {
        batch.push(create_event("stress-test", i));
        if batch.len() >= 1000 {
            db.insert_events_batch(&batch).unwrap();
            batch.clear();
        }
    }
    if !batch.is_empty() {
        db.insert_events_batch(&batch).unwrap();
    }

    let fill_time = start.elapsed();
    let events_per_sec = max_events as f64 / fill_time.as_secs_f64();
    println!(
        "    Fill: {:.2}s ({:.0} events/sec)",
        fill_time.as_secs_f64(),
        events_per_sec
    );

    // Phase 2: Query performance
    let start = Instant::now();
    let _results = db.query_events("stress-test", |q| q.limit(50)).unwrap();
    let query_time = start.elapsed();
    println!(
        "    Query 50 recent: {:.2}ms",
        query_time.as_secs_f64() * 1000.0
    );

    let start = Instant::now();
    let _results = db
        .query_events("stress-test", |q| q.function_contains("audio").limit(100))
        .unwrap();
    let filter_time = start.elapsed();
    println!(
        "    Query 100 filtered: {:.2}ms",
        filter_time.as_secs_f64() * 1000.0
    );

    // Phase 3: Insert with limit enforcement
    let new_batch: Vec<Event> = (0..1000)
        .map(|i| create_event("stress-test", max_events + i))
        .collect();

    let start = Instant::now();
    let stats = db
        .insert_events_with_limit(&new_batch, max_events)
        .unwrap();
    let cleanup_time = start.elapsed();
    println!(
        "    Insert 1000 + cleanup: {:.2}ms (deleted: {})",
        cleanup_time.as_secs_f64() * 1000.0,
        stats.events_deleted
    );

    // Phase 4: DB size
    let metadata = std::fs::metadata(&db_path).unwrap();
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
    println!("    DB size: {:.2} MB", size_mb);
}

fn create_event(session_id: &str, seq: usize) -> Event {
    let (event_type, function_name) = match seq % 4 {
        0 => (EventType::FunctionEnter, "audio::process_callback"),
        1 => (EventType::FunctionExit, "audio::process_callback"),
        2 => (EventType::FunctionEnter, "midi::handle_note"),
        _ => (EventType::FunctionExit, "midi::handle_note"),
    };

    Event {
        id: format!("{}-evt-{}", session_id, seq),
        session_id: session_id.to_string(),
        timestamp_ns: seq as i64 * 20833,
        thread_id: 1,
        thread_name: None,
        event_type: event_type.clone(),
        parent_event_id: if event_type == EventType::FunctionExit {
            Some(format!("{}-evt-{}", session_id, seq - 1))
        } else {
            None
        },
        function_name: function_name.to_string(),
        function_name_raw: Some(format!("_ZN5audio15process_callbackEv_{}", seq)),
        source_file: Some("/src/audio.cpp".to_string()),
        line_number: Some(42),
        arguments: Some(serde_json::json!([0, 1024])),
        return_value: if event_type == EventType::FunctionExit {
            Some(serde_json::json!(0))
        } else {
            None
        },
        duration_ns: if event_type == EventType::FunctionExit {
            Some(15000)
        } else {
            None
        },
        text: None,
        sampled: Some(false),
        watch_values: None,
        pid: None,
        signal: None,
        fault_address: None,
        registers: None,
        backtrace: None,
        locals: None,
        breakpoint_id: None,
        logpoint_message: None,
        exception_type: None,
        exception_message: None,
        throw_backtrace: None,
        rowid: None,
    }
}
