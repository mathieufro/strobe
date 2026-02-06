# Event Limit Analysis

## Considerations for Optimal Limit

### 1. Use Cases
- **Regular tracing**: ~1-100 events/sec (API calls, business logic)
- **High-frequency tracing**: ~1k-50k events/sec (rendering loops, timers)
- **Audio threads**: ~48k-192k events/sec (audio callbacks at sample rate)

### 2. Trade-offs

#### Database Performance
- **Queries**: SQLite handles 100k-500k rows efficiently with indexed queries
- **Inserts**: Batch inserts (100 events) remain fast up to ~1M rows
- **Cleanup**: DELETE with subquery becomes slower >500k rows
- **Indexes**: timestamp_ns index size grows ~10 bytes/event

#### Memory Usage
- **Event struct**: ~200-500 bytes per event in memory (during batch)
- **Ring buffer**: Only batches (100 events) in flight at once
- **SQLite cache**: Configured via PRAGMA cache_size

#### Disk Space
- **Storage**: ~200-800 bytes per event on disk (varies with JSON fields)
- **50k events**: ~10-40 MB
- **100k events**: ~20-80 MB
- **500k events**: ~100-400 MB
- **1M events**: ~200-800 MB

### 3. Practical Scenarios

#### Scenario A: Audio DSP Debugging
- Tracing 2-3 audio callback functions
- Sample rate: 48kHz
- Duration: Want to capture 10-30 seconds
- **Need**: 50k × 10s = 500k events minimum

#### Scenario B: UI Event Loop
- Tracing render/update cycles
- Frame rate: 60 FPS × 10 functions
- Duration: Want several minutes of history
- **Need**: 600 events/sec × 300s = 180k events

#### Scenario C: API Request Tracing
- Low frequency: 1-10 events per request
- Sustained load: 100 requests/sec
- Duration: Want full request history for debugging
- **Need**: 1000 events/sec × 600s = 600k events

### 4. Recommendation Matrix

| Limit | Use Case | Pros | Cons |
|-------|----------|------|------|
| 50k | Light tracing, quick debugging | Fast queries, small DB | Too small for audio/high-freq |
| 100k | Moderate tracing | Good balance | Still tight for sustained audio |
| 200k | Heavy tracing, audio (4s) | Enough for most scenarios | Larger DB size |
| 500k | Audio debugging (10s) | Comfortable audio capture | Query slowdown begins |
| 1M | Sustained high-frequency | Maximum headroom | Noticeable query lag |

## Proposed Default

### Option 1: Conservative (100k)
```rust
const DEFAULT_MAX_EVENTS_PER_SESSION: usize = 100_000;
```
- 2x current limit
- Handles audio for ~2 seconds
- DB size: ~20-80 MB typical
- Query performance: excellent

### Option 2: Comfortable (200k)
```rust
const DEFAULT_MAX_EVENTS_PER_SESSION: usize = 200_000;
```
- 4x current limit
- Handles audio for ~4 seconds
- DB size: ~40-160 MB typical
- Query performance: good
- **Recommended for general use**

### Option 3: Audio-focused (500k)
```rust
const DEFAULT_MAX_EVENTS_PER_SESSION: usize = 500_000;
```
- 10x current limit
- Handles audio for ~10 seconds
- DB size: ~100-400 MB typical
- Query performance: acceptable
- **Best for audio/DSP work**

## Adaptive Approach

Could also implement adaptive limits based on event frequency:
```rust
fn get_max_events_per_session() -> usize {
    std::env::var("STROBE_MAX_EVENTS_PER_SESSION")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            // Auto-detect: use larger limit if high-frequency patterns detected
            if detecting_audio_patterns() {
                500_000
            } else {
                200_000
            }
        })
}
```

## User Guidance

Should document in MCP instructions:
- Default limit works for most cases
- For audio/DSP: `export STROBE_MAX_EVENTS_PER_SESSION=500000`
- For API tracing: default is fine
- Monitor cleanup warnings - if seeing frequent cleanup, increase limit
