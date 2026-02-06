# Event Limits and Cleanup

## Problem
- Session accumulated 5.4M events (3.2GB database)
- Daemon consumed 179% CPU trying to handle the volume
- No limits or cleanup mechanism
- System became completely unusable

## Root Cause
1. Broad JUCE pattern hooks generated massive event volume
2. No per-session or global event limits
3. All events stored in SQLite forever
4. Database operations became too slow with millions of rows

## Solution

### 1. Per-Session Event Limits
- **Default limit: 50,000 events per session**
- Configurable via environment variable `STROBE_MAX_EVENTS_PER_SESSION`
- When limit is reached, oldest events are automatically deleted (FIFO circular buffer)

### 2. Async Cleanup
- Cleanup happens in database writer task (already async)
- Use efficient DELETE + INSERT in transaction
- Never blocks event generation in Frida agent

### 3. Implementation Details

#### Database Changes (event.rs)
```rust
// Add cleanup method
pub fn cleanup_old_events(&self, session_id: &str, keep_count: usize) -> Result<u64> {
    // Delete events keeping only the most recent N
    // Uses timestamp_ns ordering (already indexed)
}

pub fn insert_events_batch_with_limit(&self,
    events: &[Event],
    max_per_session: usize
) -> Result<EventInsertStats> {
    // 1. Count current events per session
    // 2. Delete oldest if over limit
    // 3. Insert new batch
    // All in transaction
}
```

#### Session Manager Changes
- Add MAX_EVENTS_PER_SESSION constant (50,000 default)
- Modify database writer task to use new limited insert
- Log warnings when cleanup occurs

#### Sampling Improvements
- Already have adaptive sampling in CModule tracer
- Tune thresholds to be more aggressive
- Consider per-pattern sampling configuration

### 4. Safety Measures
- Stdout/stderr events never sampled (critical for debugging)
- Always keep minimum of 1,000 most recent events
- Warn user when 80% of limit reached
- Track cleanup stats (events deleted, sessions affected)

### 5. Alternative: Time-Based Retention
Future enhancement: Keep events for last N minutes instead of count-based
- More intuitive for users
- Requires timestamp-based cleanup
- Can combine with count-based limit

## Files to Modify
1. `src/db/event.rs` - Add cleanup methods
2. `src/daemon/session_manager.rs` - Use limited insert in writer task
3. `src/daemon/server.rs` - Add limit info to tool descriptions
4. `MEMORY.md` - Document limit behavior

## Testing
1. Create test with 60,000 events, verify oldest 10,000 deleted
2. Test async cleanup doesn't block insertions
3. Verify transaction atomicity (no partial writes)
4. Benchmark cleanup performance

## Rollout
1. Implement with conservative limit (50k)
2. Add metrics/logging
3. Monitor in production
4. Tune limit based on real usage
