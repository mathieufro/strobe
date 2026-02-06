# Runtime Event Limit Configuration

## Summary

Added the ability to configure per-session event limits dynamically through the `debug_trace` MCP endpoint, complementing the existing environment variable configuration.

## Changes

### 1. MCP Protocol Updates (src/mcp/types.rs)
- Added `event_limit: Option<usize>` field to `DebugTraceRequest`
- Added `event_limit: usize` field to `DebugTraceResponse` to report current limit

### 2. SessionManager State (src/daemon/session_manager.rs)
- Added `event_limits: Arc<RwLock<HashMap<String, usize>>>` to store per-session limits
- Made `DEFAULT_MAX_EVENTS_PER_SESSION` public for access from server.rs
- Added `set_event_limit()` and `get_event_limit()` methods
- Initialize per-session limit in `create_session()` from environment variable
- Clean up limit in `stop_session()`
- Updated database writer task to read limit dynamically from shared state

### 3. Server Handler (src/daemon/server.rs)
- Updated `tool_debug_trace()` to handle `event_limit` parameter
- Include current `event_limit` in `DebugTraceResponse`
- Updated MCP tool description for `debug_trace` to document `eventLimit` parameter
- Added "Event Storage Limits" section to debugging instructions

### 4. Documentation (memory/MEMORY.md)
- Updated Event Storage Limits section with runtime configuration info
- Added performance guidelines for different limit values

## Usage

### Set limit when configuring a running session:
```typescript
debug_trace({
  sessionId: "myapp-2026-02-06-15h30",
  eventLimit: 500000,  // Increase to 500k for audio debugging
  add: ["audio::process"]
})
```

### Response includes current limit:
```json
{
  "activePatterns": ["audio::process"],
  "hookedFunctions": 1,
  "eventLimit": 500000,
  "warnings": []
}
```

## Configuration Priority

1. **Runtime (highest priority)**: `debug_trace({ sessionId, eventLimit })`
2. **Environment variable**: `STROBE_MAX_EVENTS_PER_SESSION=500000`
3. **Default**: 200,000 events

## Performance Guidelines

| Limit | Query Speed | DB Size | Use Case |
|-------|-------------|---------|----------|
| 200k  | <10ms       | ~56MB   | Default, general purpose |
| 500k  | ~28ms       | ~140MB  | Audio/DSP debugging (10s at 48kHz) |
| 1M+   | >300ms      | >280MB  | Avoid unless necessary |

## Implementation Details

- Event limits are stored in a shared `Arc<RwLock<HashMap>>` accessible to database writer tasks
- Database writer reads the current limit on each batch insert
- Cleanup happens asynchronously and never blocks event generation
- All operations are thread-safe and atomic
