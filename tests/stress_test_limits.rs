use strobe::db::{Database, Event, EventType};
use std::time::Instant;
use tempfile::tempdir;

/// Stress test to find optimal event limit
/// Tests insert, query, and cleanup performance at various limits
#[test]
#[ignore] // Run with: cargo test --release stress_test_limits -- --ignored --nocapture
fn stress_test_limits() {
    let limits_to_test = vec![
        10_000,
        50_000,
        100_000,
        200_000,
        500_000,
        1_000_000,
    ];

    println!("\n=== Event Limit Stress Test ===\n");
    println!("Testing insert, query, and cleanup performance at various limits");
    println!("Simulating high-frequency tracing (audio callbacks at ~48kHz)\n");

    for limit in limits_to_test {
        println!("--- Testing limit: {} events ---", format_number(limit));
        test_limit(limit);
        println!();
    }
}

fn test_limit(max_events: usize) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("stress.db");
    let db = Database::open(&db_path).unwrap();

    db.create_session("stress-test", "/bin/test", "/home", 1234).unwrap();

    // Phase 1: Fill to limit (simulate normal operation)
    println!("  Phase 1: Filling to {} events...", format_number(max_events));
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
    println!("    ✓ Filled in {:.2}s ({:.0} events/sec)", fill_time.as_secs_f64(), events_per_sec);

    // Phase 2: Test queries at limit
    println!("  Phase 2: Query performance at limit...");

    // Query recent events (typical use case)
    let start = Instant::now();
    let _results = db.query_events("stress-test", |q| q.limit(50)).unwrap();
    let query_time = start.elapsed();
    println!("    ✓ Query 50 recent: {:.2}ms", query_time.as_secs_f64() * 1000.0);

    // Query with filter (more expensive)
    let start = Instant::now();
    let _results = db.query_events("stress-test", |q| {
        q.function_contains("audio").limit(100)
    }).unwrap();
    let filter_time = start.elapsed();
    println!("    ✓ Query 100 filtered: {:.2}ms", filter_time.as_secs_f64() * 1000.0);

    // Count query (full table scan)
    let start = Instant::now();
    let count = db.count_events("stress-test").unwrap();
    let count_time = start.elapsed();
    println!("    ✓ Count query: {:.2}ms (count={})", count_time.as_secs_f64() * 1000.0, count);

    // Phase 3: Test insert with limit (cleanup overhead)
    println!("  Phase 3: Insert with cleanup (limit enforcement)...");

    let new_batch: Vec<Event> = (0..1000).map(|i| {
        create_event("stress-test", max_events + i)
    }).collect();

    let start = Instant::now();
    let stats = db.insert_events_with_limit(&new_batch, max_events).unwrap();
    let cleanup_time = start.elapsed();

    println!("    ✓ Insert 1000 + cleanup: {:.2}ms", cleanup_time.as_secs_f64() * 1000.0);
    println!("    ✓ Events deleted: {}", stats.events_deleted);
    println!("    ✓ Events inserted: {}", stats.events_inserted);

    // Phase 4: Sustained high-frequency load
    println!("  Phase 4: Sustained load (5 seconds @ 10k events/sec)...");
    let start = Instant::now();
    let mut total_inserted = 0;
    let mut total_deleted = 0;
    let mut global_seq = max_events + 1000; // Start after previous events

    while start.elapsed().as_secs() < 5 {
        let batch: Vec<Event> = (0..100).map(|i| {
            create_event("stress-test", global_seq + i)
        }).collect();
        global_seq += 100;

        let stats = db.insert_events_with_limit(&batch, max_events).unwrap();
        total_inserted += stats.events_inserted;
        total_deleted += stats.events_deleted;
    }

    let sustained_time = start.elapsed();
    let sustained_rate = total_inserted as f64 / sustained_time.as_secs_f64();

    println!("    ✓ Sustained: {:.0} events/sec", sustained_rate);
    println!("    ✓ Total inserted: {}", format_number(total_inserted as usize));
    println!("    ✓ Total deleted: {}", format_number(total_deleted as usize));

    // Phase 5: Database size
    let metadata = std::fs::metadata(&db_path).unwrap();
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
    println!("  Phase 5: Database size: {:.2} MB", size_mb);

    // Calculate approximate size per event
    let final_count = db.count_events("stress-test").unwrap();
    let bytes_per_event = (metadata.len() as f64) / (final_count as f64);
    println!("    ✓ Bytes per event: {:.0}", bytes_per_event);
    println!("    ✓ Estimated size at limit: {:.2} MB", bytes_per_event * max_events as f64 / (1024.0 * 1024.0));
}

fn create_event(session_id: &str, seq: usize) -> Event {
    // Alternate between different event types to simulate realistic workload
    let (event_type, function_name) = match seq % 4 {
        0 => (EventType::FunctionEnter, "audio::process_callback"),
        1 => (EventType::FunctionExit, "audio::process_callback"),
        2 => (EventType::FunctionEnter, "midi::handle_note"),
        _ => (EventType::FunctionExit, "midi::handle_note"),
    };

    Event {
        id: format!("{}-evt-{}", session_id, seq),
        session_id: session_id.to_string(),
        timestamp_ns: seq as i64 * 20833, // ~48kHz = 20.8μs per sample
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
    }
}

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.insert(0, ',');
        }
        result.insert(0, c);
    }
    result
}
