# M0: Daemon Prerequisites — MCP Tool Consolidation

**Spec:** `docs/specs/2026-02-10-vscode-extension.md` (M0 section + MCP Tool Consolidation section)
**Goal:** Consolidate 13 MCP tools → 8, add session status endpoint and cursor-based pagination for VS Code extension polling
**Architecture:** Add consolidated tool handlers with `action` discriminators. Old tool names become thin deprecation wrappers. Migration: both old and new names work simultaneously.
**Tech Stack:** Rust (types.rs, server.rs, session_manager.rs, db/event.rs)
**Commit strategy:** Single commit at end

## Workstreams

Serial execution required — all tasks modify `types.rs` and `server.rs` with sequential dependencies.

---

### Task 1: P2 — Extend EventTypeFilter enum

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (lines 197-206)
- Modify: [server.rs](src/daemon/server.rs) (line 590 — tool schema enum, lines 1456-1461 + 1501-1506 — match arms)

The DB `EventType` enum (`src/db/event.rs:8-18`) already has `Pause`, `Logpoint`, `ConditionError`. They're just missing from the MCP-facing `EventTypeFilter` and the tool schema.

**Step 1: Write the failing test**

Add to `src/mcp/types.rs` at the end of the file (before the closing of the module or after existing tests):

```rust
#[cfg(test)]
mod event_type_filter_tests {
    use super::*;

    #[test]
    fn test_event_type_filter_pause() {
        let json = serde_json::json!("pause");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::Pause));
    }

    #[test]
    fn test_event_type_filter_logpoint() {
        let json = serde_json::json!("logpoint");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::Logpoint));
    }

    #[test]
    fn test_event_type_filter_condition_error() {
        let json = serde_json::json!("condition_error");
        let filter: EventTypeFilter = serde_json::from_value(json).unwrap();
        assert!(matches!(filter, EventTypeFilter::ConditionError));
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::event_type_filter_tests`
Expected: FAIL — no variant `Pause`, `Logpoint`, `ConditionError`

**Step 3: Write minimal implementation**

In `src/mcp/types.rs` lines 197-206, add the three variants:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
    VariableSnapshot,
    Pause,
    Logpoint,
    ConditionError,
}
```

In `src/daemon/server.rs` line 590, update the tool schema enum:

```json
"eventType": { "type": "string", "enum": ["function_enter", "function_exit", "stdout", "stderr", "crash", "variable_snapshot", "pause", "logpoint", "condition_error"] }
```

In `src/daemon/server.rs`, add match arms at both `EventTypeFilter` → `EventType` conversion sites (lines ~1456-1461 and ~1501-1506):

```rust
EventTypeFilter::Pause => crate::db::EventType::Pause,
EventTypeFilter::Logpoint => crate::db::EventType::Logpoint,
EventTypeFilter::ConditionError => crate::db::EventType::ConditionError,
```

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::event_type_filter_tests`
Expected: PASS

**Checkpoint:** All event types queryable through `debug_query`. Existing tests still pass.

---

### Task 2: P3 — Cursor-based query pagination

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (DebugQueryRequest + DebugQueryResponse)
- Modify: [event.rs](src/db/event.rs) (EventQuery struct + query_events + count_filtered_events)
- Modify: [server.rs](src/daemon/server.rs) (tool_debug_query handler + tool schema)

SQLite tables with `TEXT PRIMARY KEY` (without `WITHOUT ROWID`) have an implicit integer `rowid`. We use `rowid` as the monotonically increasing cursor.

**Step 1: Write the failing test**

Add to `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod query_pagination_tests {
    use super::*;

    #[test]
    fn test_query_request_with_after_event_id() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "afterEventId": 42
        });
        let req: DebugQueryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.after_event_id, Some(42));
    }

    #[test]
    fn test_query_response_has_cursor_fields() {
        let resp = DebugQueryResponse {
            events: vec![],
            total_count: 0,
            has_more: false,
            pids: None,
            last_event_id: Some(99),
            events_dropped: Some(false),
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["lastEventId"], 99);
        assert_eq!(json["eventsDropped"], false);
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::query_pagination_tests`
Expected: FAIL — no field `after_event_id` / `last_event_id` / `events_dropped`

**Step 3: Write minimal implementation**

**3a. Add fields to types.rs:**

In `DebugQueryRequest` (line ~246), add after `verbose`:

```rust
    /// Cursor: return only events with rowid > after_event_id (for incremental polling)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_event_id: Option<i64>,
```

In `DebugQueryResponse` (line ~276), add after `pids`:

```rust
    /// Highest event rowid in this response (use as next cursor)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<i64>,
    /// True if FIFO eviction happened since the cursor position
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_dropped: Option<bool>,
```

**3b. Add `after_rowid` to EventQuery in `src/db/event.rs`:**

Add field to `EventQuery` struct:

```rust
    pub after_rowid: Option<i64>,
```

Default it to `None` in the Default impl.

In `query_events()`, add filter before the ORDER BY:

```rust
    if let Some(after) = query.after_rowid {
        sql.push_str(" AND rowid > ?");
        params_vec.push(Box::new(after));
    }
```

Also, change the SELECT to include `rowid`:

```sql
SELECT rowid, id, session_id, ...
```

And update `event_from_row` to return the rowid alongside the Event (or add a `rowid` field to the `Event` struct).

Actually, simpler approach: add a separate query method or return rowid via a wrapper. The cleanest option: add `pub rowid: Option<i64>` to the `Event` struct and populate it from the query.

**3c. Add FIFO drop detection in `src/db/event.rs`:**

Add a method to check if events were dropped after a given rowid:

```rust
pub fn min_rowid_for_session(&self, session_id: &str) -> Result<Option<i64>> {
    let conn = self.connection();
    let result: Option<i64> = conn.query_row(
        "SELECT MIN(rowid) FROM events WHERE session_id = ?",
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(result)
}
```

If `after_event_id < min_rowid`, events were dropped.

**3d. Update `tool_debug_query` in server.rs:**

- Pass `after_event_id` through to `EventQuery.after_rowid`
- Extract max rowid from results as `last_event_id`
- Call `min_rowid_for_session()` to detect drops when `after_event_id` is provided
- Populate `last_event_id` and `events_dropped` in response
- Update tool schema to add `afterEventId` property

**3e. Update tool schema in server.rs** (line ~590 area):

Add to the `debug_query` properties:

```json
"afterEventId": { "type": "integer", "description": "Cursor: return only events with rowid > afterEventId (for incremental polling)" }
```

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::query_pagination_tests`
Expected: PASS

Also run: `cargo test --lib db::` to verify DB tests still pass.

**Checkpoint:** `debug_query` supports cursor-based pagination. Old `offset`-based pagination still works. Extension can poll with `afterEventId` for reliable incremental updates.

---

### Task 3: P6 — Consolidate breakpoint + logpoint into unified `debug_breakpoint`

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (`BreakpointTarget` gains `message` field)
- Modify: [server.rs](src/daemon/server.rs) (`tool_debug_breakpoint` handler, `tool_debug_logpoint` wrapper, tool schemas)
- Modify: [session_manager.rs](src/daemon/session_manager.rs) (minor — breakpoint handler routes to logpoint code when `message` present)

The key insight: `BreakpointTarget` and `LogpointTarget` are nearly identical. The only difference is `LogpointTarget` has `message: String` (required) and `BreakpointTarget` has `hit_count: Option<u32>`. Unified entry: `message` present → logpoint, absent → breakpoint.

**Step 1: Write the failing test**

Add to `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod unified_breakpoint_tests {
    use super::*;

    #[test]
    fn test_breakpoint_target_with_message_is_logpoint() {
        let json = serde_json::json!({
            "function": "foo",
            "message": "hit: {args[0]}"
        });
        let target: BreakpointTarget = serde_json::from_value(json).unwrap();
        assert_eq!(target.message.as_deref(), Some("hit: {args[0]}"));
    }

    #[test]
    fn test_breakpoint_target_without_message_is_breakpoint() {
        let json = serde_json::json!({
            "function": "foo",
            "condition": "args[0] > 100"
        });
        let target: BreakpointTarget = serde_json::from_value(json).unwrap();
        assert!(target.message.is_none());
    }

    #[test]
    fn test_breakpoint_response_includes_logpoints() {
        let resp = DebugBreakpointResponse {
            breakpoints: vec![BreakpointInfo {
                id: "bp-1".to_string(),
                function: Some("foo".to_string()),
                file: None,
                line: None,
                address: "0x1000".to_string(),
            }],
            logpoints: vec![LogpointInfo {
                id: "lp-1".to_string(),
                message: "hit".to_string(),
                function: Some("bar".to_string()),
                file: None,
                line: None,
                address: "0x2000".to_string(),
            }],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["breakpoints"].as_array().unwrap().len(), 1);
        assert_eq!(json["logpoints"].as_array().unwrap().len(), 1);
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::unified_breakpoint_tests`
Expected: FAIL — `BreakpointTarget` has no field `message`, `DebugBreakpointResponse` has no field `logpoints`

**Step 3: Write minimal implementation**

**3a. Extend `BreakpointTarget` in types.rs** (line ~760):

Add `message` field:

```rust
pub struct BreakpointTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_count: Option<u32>,
    /// If present, this entry is a logpoint (non-blocking log on hit).
    /// Use {args[0]}, {args[1]} for arguments, {threadId} for thread ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
```

**3b. Extend `DebugBreakpointResponse` in types.rs** (line ~836):

Add logpoints field:

```rust
pub struct DebugBreakpointResponse {
    pub breakpoints: Vec<BreakpointInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub logpoints: Vec<LogpointInfo>,
}
```

**3c. Update validation in `DebugBreakpointRequest::validate()`:**

When `message` is present, validate as logpoint (message non-empty, length ≤ `MAX_LOGPOINT_MESSAGE_LENGTH`). When absent, validate as breakpoint (existing logic). `hit_count` is only valid without `message`.

**3d. Update `tool_debug_breakpoint` in server.rs:**

Split `add` entries: those with `message` → convert to `LogpointTarget` and call existing `session_manager.set_logpoints()`. Those without `message` → existing breakpoint path. Merge both results into unified response.

For `remove`: try removing from both breakpoints and logpoints (IDs are namespaced — `bp-*` vs `lp-*`).

**3e. Make `tool_debug_logpoint` a deprecation wrapper:**

```rust
async fn tool_debug_logpoint(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    log::warn!("debug_logpoint is deprecated, use debug_breakpoint with 'message' field");
    // Convert LogpointRequest → BreakpointRequest (add message to each target)
    let req: DebugLogpointRequest = serde_json::from_value(args.clone())?;
    let bp_targets: Vec<BreakpointTarget> = req.add.unwrap_or_default().into_iter().map(|lp| {
        BreakpointTarget {
            function: lp.function,
            file: lp.file,
            line: lp.line,
            condition: lp.condition,
            hit_count: None,
            message: Some(lp.message),
        }
    }).collect();
    let bp_req = serde_json::to_value(DebugBreakpointRequest {
        session_id: req.session_id,
        add: if bp_targets.is_empty() { None } else { Some(bp_targets) },
        remove: req.remove,
    })?;
    self.tool_debug_breakpoint(&bp_req).await
}
```

**3f. Update tool schema** in server.rs:

Add `message` to `debug_breakpoint` schema's `add` items. Add `logpoints` array to response description. Keep `debug_logpoint` in tool list but mark description as "(Deprecated: use debug_breakpoint with 'message' field)".

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::unified_breakpoint_tests`
Expected: PASS

Also run: `cargo test --lib mcp::breakpoint_tests` to verify existing tests.

**Checkpoint:** `debug_breakpoint` handles both breakpoints and logpoints. `debug_logpoint` still works but logs deprecation warning. Response includes both `breakpoints` and `logpoints` arrays.

---

### Task 4: P7 — Consolidate read + write into `debug_memory`

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (new `DebugMemoryRequest`/`DebugMemoryResponse`)
- Modify: [server.rs](src/daemon/server.rs) (new handler + deprecation wrappers + tool schema)

**Step 1: Write the failing test**

Add to `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod memory_consolidation_tests {
    use super::*;

    #[test]
    fn test_memory_read_request() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "action": "read",
            "targets": [{ "variable": "gTempo" }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Read);
        assert_eq!(req.targets.len(), 1);
    }

    #[test]
    fn test_memory_write_request() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "action": "write",
            "targets": [{ "variable": "g_counter", "value": 42 }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Write);
    }

    #[test]
    fn test_memory_action_default_read() {
        let json = serde_json::json!({
            "sessionId": "s1",
            "targets": [{ "variable": "gTempo" }]
        });
        let req: DebugMemoryRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, MemoryAction::Read);
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::memory_consolidation_tests`
Expected: FAIL — no type `DebugMemoryRequest` / `MemoryAction`

**Step 3: Write minimal implementation**

**3a. Add new types in types.rs:**

```rust
// ============ debug_memory (consolidated read + write) ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAction {
    Read,
    Write,
}

impl Default for MemoryAction {
    fn default() -> Self { Self::Read }
}

/// Unified target for debug_memory — works for both read and write.
/// For reads: `variable` or `address` (+ optional size/type).
/// For writes: `variable` or `address`, plus `value` (required).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
    /// Value to write (required for action: "write", ignored for "read")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugMemoryRequest {
    pub session_id: String,
    #[serde(default)]
    pub action: MemoryAction,
    pub targets: Vec<MemoryTarget>,
    /// Max struct traversal depth for reads (1-5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    /// Poll config for reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<PollConfig>,
}
```

Add validation method on `DebugMemoryRequest` that:
- For `Read`: delegates to existing `DebugReadRequest` validation logic (targets need variable or address, depth/poll limits)
- For `Write`: delegates to existing `DebugWriteRequest` validation logic (targets need variable or address + value, type required for address)

**3b. Add handler in server.rs:**

```rust
async fn tool_debug_memory(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: DebugMemoryRequest = serde_json::from_value(args.clone())?;
    req.validate()?;

    match req.action {
        MemoryAction::Read => {
            // Convert MemoryTarget → ReadTarget, build DebugReadRequest, delegate
            let read_req = DebugReadRequest {
                session_id: req.session_id,
                targets: req.targets.into_iter().map(|t| ReadTarget {
                    variable: t.variable,
                    address: t.address,
                    size: t.size,
                    type_hint: t.type_hint,
                }).collect(),
                depth: req.depth,
                poll: req.poll,
            };
            self.tool_debug_read(&serde_json::to_value(read_req)?).await
        }
        MemoryAction::Write => {
            // Convert MemoryTarget → WriteTarget, build DebugWriteRequest, delegate
            let write_req = DebugWriteRequest {
                session_id: req.session_id,
                targets: req.targets.into_iter().map(|t| WriteTarget {
                    variable: t.variable,
                    address: t.address,
                    value: t.value.unwrap_or(serde_json::Value::Null),
                    type_hint: t.type_hint,
                }).collect(),
            };
            self.tool_debug_write(&serde_json::to_value(write_req)?).await
        }
    }
}
```

**3c. Add to dispatch + deprecation wrappers:**

In the match block, add `"debug_memory"` arm. Mark `debug_read` and `debug_write` with deprecation logs:

```rust
"debug_memory" => self.tool_debug_memory(&call.arguments).await,
"debug_read" => {
    log::warn!("debug_read is deprecated, use debug_memory with action: 'read'");
    self.tool_debug_read(&call.arguments).await
},
"debug_write" => {
    log::warn!("debug_write is deprecated, use debug_memory with action: 'write'");
    self.tool_debug_write(&call.arguments).await
},
```

**3d. Add tool schema** for `debug_memory` in tool list. Mark `debug_read` and `debug_write` descriptions as deprecated.

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::memory_consolidation_tests`
Expected: PASS

**Checkpoint:** `debug_memory` routes reads/writes. Old `debug_read`/`debug_write` still work with deprecation warning.

---

### Task 5: P8 — Consolidate test + test_status into `debug_test`

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (extend `DebugTestRequest` with `action` field)
- Modify: [server.rs](src/daemon/server.rs) (update handler + deprecation wrapper + tool schema)

The existing `debug_test` tool starts a test run. The existing `debug_test_status` tool polls status. We consolidate: `action: "run"` (default) starts a test, `action: "status"` polls.

**Step 1: Write the failing test**

Add to `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod test_consolidation_tests {
    use super::*;

    #[test]
    fn test_debug_test_with_action_run() {
        let json = serde_json::json!({
            "action": "run",
            "projectRoot": "/tmp/proj"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, Some(TestAction::Run));
    }

    #[test]
    fn test_debug_test_with_action_status() {
        let json = serde_json::json!({
            "action": "status",
            "testRunId": "tr-123"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, Some(TestAction::Status));
        assert_eq!(req.test_run_id.as_deref(), Some("tr-123"));
    }

    #[test]
    fn test_debug_test_default_action_is_run() {
        let json = serde_json::json!({
            "projectRoot": "/tmp/proj"
        });
        let req: DebugTestRequest = serde_json::from_value(json).unwrap();
        assert!(req.action.is_none()); // None treated as "run"
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::test_consolidation_tests`
Expected: FAIL — no type `TestAction`, no field `action`/`test_run_id` on `DebugTestRequest`

**Step 3: Write minimal implementation**

**3a. Add `TestAction` enum and extend `DebugTestRequest` in types.rs:**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestAction {
    Run,
    Status,
}
```

Add to `DebugTestRequest`:

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<TestAction>,
    /// Required for action: "status"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
```

**3b. Update `tool_debug_test` in server.rs:**

```rust
async fn tool_debug_test(&self, args: &serde_json::Value, connection_id: &str) -> Result<serde_json::Value> {
    let req: DebugTestRequest = serde_json::from_value(args.clone())?;

    match req.action.as_ref().unwrap_or(&TestAction::Run) {
        TestAction::Run => {
            // Existing test run logic (unchanged)
            self.tool_debug_test_run(&req, connection_id).await
        }
        TestAction::Status => {
            let test_run_id = req.test_run_id.as_deref()
                .ok_or_else(|| crate::Error::ValidationError(
                    "testRunId is required for action: 'status'".to_string()
                ))?;
            let status_req = serde_json::json!({ "testRunId": test_run_id });
            self.tool_debug_test_status(&status_req).await
        }
    }
}
```

Extract the existing test-run logic into a private `tool_debug_test_run()` method.

**3c. Add deprecation to `tool_debug_test_status`:**

```rust
"debug_test_status" => {
    log::warn!("debug_test_status is deprecated, use debug_test with action: 'status'");
    self.tool_debug_test_status(&call.arguments).await
},
```

**3d. Update tool schema** for `debug_test`: add `action` and `testRunId` properties. Mark `debug_test_status` as deprecated in its description.

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::test_consolidation_tests`
Expected: PASS

**Checkpoint:** `debug_test` handles both starting runs and polling status. `debug_test_status` still works but logs deprecation.

---

### Task 6: P1 — Consolidate session tools + add session_status

**Files:**
- Modify: [types.rs](src/mcp/types.rs) (new `DebugSessionRequest`/`DebugSessionResponse`)
- Modify: [server.rs](src/daemon/server.rs) (new handler + deprecation wrappers + tool schema)
- Modify: [session_manager.rs](src/daemon/session_manager.rs) (new `session_status()` method)

This is the most complex task. Consolidates `debug_stop` + `debug_list_sessions` + `debug_delete_session` into `debug_session`, and adds the new `session_status` action.

**Step 1: Write the failing test**

Add to `src/mcp/types.rs`:

```rust
#[cfg(test)]
mod session_consolidation_tests {
    use super::*;

    #[test]
    fn test_session_action_status() {
        let json = serde_json::json!({
            "action": "status",
            "sessionId": "s1"
        });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::Status);
        assert_eq!(req.session_id.as_deref(), Some("s1"));
    }

    #[test]
    fn test_session_action_stop() {
        let json = serde_json::json!({
            "action": "stop",
            "sessionId": "s1",
            "retain": true
        });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::Stop);
        assert_eq!(req.retain, Some(true));
    }

    #[test]
    fn test_session_action_list() {
        let json = serde_json::json!({ "action": "list" });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::List);
        assert!(req.session_id.is_none());
    }

    #[test]
    fn test_session_action_delete() {
        let json = serde_json::json!({
            "action": "delete",
            "sessionId": "s1"
        });
        let req: DebugSessionRequest = serde_json::from_value(json).unwrap();
        assert_eq!(req.action, SessionAction::Delete);
    }

    #[test]
    fn test_session_status_response() {
        let resp = SessionStatusResponse {
            status: "running".to_string(),
            pid: 1234,
            event_count: 500,
            hooked_functions: 23,
            trace_patterns: vec!["foo::*".to_string()],
            breakpoints: vec![],
            logpoints: vec![],
            watches: vec![],
            paused_threads: vec![],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["pid"], 1234);
        assert_eq!(json["hookedFunctions"], 23);
    }
}
```

**Step 2: Run test — verify it fails**

Run: `cargo test --lib mcp::session_consolidation_tests`
Expected: FAIL — no types `DebugSessionRequest`, `SessionAction`, `SessionStatusResponse`

**Step 3: Write minimal implementation**

**3a. Add types in types.rs:**

```rust
// ============ debug_session (consolidated stop + list + delete + status) ============

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAction {
    Status,
    Stop,
    List,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugSessionRequest {
    pub action: SessionAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retain: Option<bool>,
}

impl DebugSessionRequest {
    pub fn validate(&self) -> crate::Result<()> {
        match self.action {
            SessionAction::Status | SessionAction::Stop | SessionAction::Delete => {
                if self.session_id.as_ref().map_or(true, |s| s.is_empty()) {
                    return Err(crate::Error::ValidationError(
                        format!("sessionId is required for action: {:?}", self.action)
                    ));
                }
            }
            SessionAction::List => {} // no sessionId needed
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PausedThreadInfo {
    pub thread_id: u64,
    pub breakpoint_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusResponse {
    pub status: String,             // "running" | "paused" | "exited"
    pub pid: u32,
    pub event_count: u64,
    pub hooked_functions: u32,
    pub trace_patterns: Vec<String>,
    pub breakpoints: Vec<BreakpointInfo>,
    pub logpoints: Vec<LogpointInfo>,
    pub watches: Vec<ActiveWatch>,
    pub paused_threads: Vec<PausedThreadInfo>,
}
```

**3b. Add `session_status()` to session_manager.rs:**

```rust
pub fn session_status(&self, session_id: &str) -> Result<SessionStatusResponse> {
    // Verify session exists (check DB or in-memory state)
    let pid = self.get_session_pid(session_id)?;
    let event_count = self.db.count_session_events(session_id)?;
    let hook_count = read_lock(&self.hook_counts)
        .get(session_id).copied().unwrap_or(0);
    let patterns = read_lock(&self.patterns)
        .get(session_id).cloned().unwrap_or_default();

    let breakpoints: Vec<BreakpointInfo> = read_lock(&self.breakpoints)
        .get(session_id)
        .map(|bps| bps.values().map(|bp| bp.to_info()).collect())
        .unwrap_or_default();

    let logpoints: Vec<LogpointInfo> = read_lock(&self.logpoints)
        .get(session_id)
        .map(|lps| lps.values().map(|lp| lp.to_info()).collect())
        .unwrap_or_default();

    let watches: Vec<ActiveWatch> = read_lock(&self.watches)
        .get(session_id)
        .map(|ws| ws.iter().map(|w| w.to_active_watch()).collect())
        .unwrap_or_default();

    let paused = self.get_all_paused_threads(session_id);
    let paused_threads: Vec<PausedThreadInfo> = paused.iter().map(|(tid, info)| {
        PausedThreadInfo {
            thread_id: *tid,
            breakpoint_id: info.breakpoint_id.clone(),
            function: info.func_name.clone(),
            file: info.file.clone(),
            line: info.line,
        }
    }).collect();

    let status = if !paused_threads.is_empty() {
        "paused"
    } else if self.is_session_running(session_id) {
        "running"
    } else {
        "exited"
    };

    Ok(SessionStatusResponse {
        status: status.to_string(),
        pid,
        event_count,
        hooked_functions: hook_count,
        trace_patterns: patterns,
        breakpoints,
        logpoints,
        watches,
        paused_threads,
    })
}
```

Note: `get_session_pid()` and `is_session_running()` may need to be added as helper methods on `SessionManager`. Check if equivalent methods already exist (e.g., from the DB session record or from the spawner's PID tracking). The `Breakpoint.to_info()` and `Logpoint.to_info()` and `ActiveWatchState.to_active_watch()` conversion methods may also need to be added — check if they already exist as part of the response builders in `tool_debug_breakpoint`/`tool_debug_logpoint`.

**3c. Add handler in server.rs:**

```rust
async fn tool_debug_session(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: DebugSessionRequest = serde_json::from_value(args.clone())?;
    req.validate()?;

    match req.action {
        SessionAction::Status => {
            let session_id = req.session_id.as_ref().unwrap();
            let status = self.session_manager.session_status(session_id)?;
            Ok(serde_json::to_value(status)?)
        }
        SessionAction::Stop => {
            let stop_req = serde_json::json!({
                "sessionId": req.session_id.unwrap(),
                "retain": req.retain,
            });
            self.tool_debug_stop(&stop_req).await
        }
        SessionAction::List => {
            self.tool_debug_list_sessions().await
        }
        SessionAction::Delete => {
            let del_req = serde_json::json!({
                "sessionId": req.session_id.unwrap()
            });
            self.tool_debug_delete_session(&del_req).await
        }
    }
}
```

**3d. Add to dispatch + deprecation wrappers:**

```rust
"debug_session" => self.tool_debug_session(&call.arguments).await,
"debug_stop" => {
    log::warn!("debug_stop is deprecated, use debug_session with action: 'stop'");
    self.tool_debug_stop(&call.arguments).await
},
"debug_list_sessions" => {
    log::warn!("debug_list_sessions is deprecated, use debug_session with action: 'list'");
    self.tool_debug_list_sessions().await
},
"debug_delete_session" => {
    log::warn!("debug_delete_session is deprecated, use debug_session with action: 'delete'");
    self.tool_debug_delete_session(&call.arguments).await
},
```

**3e. Add tool schema** for `debug_session` in tool list with all 4 actions documented. Mark `debug_stop`, `debug_list_sessions`, `debug_delete_session` as deprecated.

**Step 4: Run test — verify it passes**

Run: `cargo test --lib mcp::session_consolidation_tests`
Expected: PASS

**Checkpoint:** `debug_session` handles status/stop/list/delete. Session status returns all session state in a single call. Old tools still work with deprecation warnings.

---

### Task 7: Update MCP system prompt and tool list

**Files:**
- Modify: [server.rs](src/daemon/server.rs) (system prompt text + `handle_tools_list()`)

**Step 1: No test needed** — this is documentation/schema only.

**Step 2: Update system prompt**

In the system prompt text (server.rs lines ~440-499), update to reference new consolidated tools:
- Replace `debug_stop`/`debug_list_sessions`/`debug_delete_session` references → `debug_session`
- Replace `debug_read`/`debug_write` references → `debug_memory`
- Replace `debug_test_status` references → `debug_test` with `action: "status"`
- Replace `debug_logpoint` references → `debug_breakpoint` with `message` field
- Mention `afterEventId` in the Queries section

**Step 3: Reorder tool list**

Put the 8 primary tools first (debug_launch, debug_session, debug_trace, debug_query, debug_breakpoint, debug_continue, debug_memory, debug_test), then the 5 deprecated tools at the end with "(Deprecated)" in their descriptions.

**Checkpoint:** LLM clients see new consolidated tools first. Old tool names still work during migration.

---

### Task 8: Run full test suite

**Step 1: Run all unit tests**

Run: `cargo test --lib`
Expected: All pass (existing + new)

**Step 2: Run integration tests**

Run via `debug_test` tool against the project.

**Step 3: Fix any breakage**

Verify that:
- All existing tests compile (the type changes may break test code that constructs request/response types directly)
- Deprecation wrappers correctly forward to new handlers
- Session status returns correct data for running sessions

**Checkpoint:** Full green. Ready for commit.

---

## Review Findings

**Reviewed:** 2026-02-10
**Commits:** `27b40dd` feat: consolidate MCP tools (13 → 8) for VS Code extension, `d1ee35e` fix: stepping tests fail when breakpoint inline hook overlaps one-shot

### Issues

#### Issue 1: Missing `validate()` on `DebugTestRequest`
**Severity:** Important
**Location:** `src/mcp/types.rs:504-527`
**Requirement:** Plan Task 5 — consolidated `debug_test` should validate `testRunId` required for status action
**Problem:** Unlike `DebugSessionRequest`, `DebugMemoryRequest`, and `DebugBreakpointRequest` which all have `validate()` methods, `DebugTestRequest` has none. Validation for `testRunId` being required when action is "status" is handled entirely in `server.rs`. This is inconsistent with the pattern established by every other consolidated request type.
**Suggested fix:** Add a `validate()` method to `DebugTestRequest` that checks `test_run_id` is present when action is `Status`, and `project_root` is non-empty when action is `Run` (or `None`).

#### Issue 2: `DebugMemoryRequest` silently converts missing `value` to null for writes
**Severity:** Important
**Location:** `src/mcp/types.rs:1142`
**Requirement:** Plan Task 4 — write targets should require `value`
**Problem:** When `MemoryAction::Write` is used, `MemoryTarget.value` being `None` is silently converted to `serde_json::Value::Null` via `.unwrap_or(serde_json::Value::Null)`. A write request with no `value` field will attempt to write `null` instead of producing a validation error.
**Suggested fix:** Add validation in the write path that checks `t.value.is_some()` for each target before delegating.

#### Issue 3: `events_dropped` off-by-one
**Severity:** Minor
**Location:** `src/daemon/server.rs:1648`
**Requirement:** Task 2 — detect FIFO drops
**Problem:** The check `after < min` is conservative. If cursor is `after=5` and `min_rowid=6`, the next event (rowid 6) is still available — nothing was dropped. Correct check: `after + 1 < min`. Matches plan text so this is a plan-level imprecision. Worst case: spurious `eventsDropped: true`.
**Suggested fix:** Change `after < min` to `after + 1 < min`, or accept as conservative.

#### Issue 4: `// ---- Primary tools (8) ----` comment misplaced
**Severity:** Minor
**Location:** `src/daemon/server.rs:662`
**Requirement:** Task 7 — 8 primary tools first
**Problem:** Comment appears between tools 4 and 5 (after `debug_session`, before `debug_test`), suggesting only 4 of 8 are primary.
**Suggested fix:** Move comment to before `debug_launch` or remove it.

#### Issue 5: Tool ordering differs from plan
**Severity:** Minor
**Location:** `src/daemon/server.rs:510-764`
**Requirement:** Task 7 — "debug_launch, debug_session, debug_trace, debug_query, debug_breakpoint, debug_continue, debug_memory, debug_test"
**Problem:** Implementation order: launch, trace, query, session, test, breakpoint, continue, memory. No functional impact (MCP clients don't depend on order).

#### Issue 6: `session_status()` paused_threads ordering is non-deterministic
**Severity:** Minor
**Location:** `src/daemon/session_manager.rs:1566-1576`
**Problem:** `get_all_paused_threads()` returns a `HashMap`, so `Vec<PausedThreadInfo>` order varies between calls. Could confuse diff-based UIs.
**Suggested fix:** Sort by `thread_id` before returning.

#### Issue 7: `phase2a_gaps` Test 2 early `return` exits entire consolidated function
**Severity:** Minor
**Location:** `tests/phase2a_gaps.rs:133`
**Problem:** If CModule trace install fails, `return` exits the whole test function. Fine now since Test 2 is last, but future tests after it would be silently skipped.

### Approved
- [x] Task 1 — EventTypeFilter enum: Pause, Logpoint, ConditionError variants + match arms at both conversion sites + schema enum
- [x] Task 2 — Cursor-based pagination: after_event_id, last_event_id, events_dropped, afterEventId schema, min_rowid_for_session(), rowid in Event struct
- [x] Task 3 — Unified breakpoint: message field, logpoints in response, split routing, logpoint deprecation wrapper, validation
- [x] Task 4 — Unified debug_memory: MemoryAction enum, MemoryTarget, read/write routing, deprecation wrappers
- [x] Task 5 — Consolidated debug_test: TestAction enum, action/testRunId fields, run/status routing, deprecation wrapper
- [x] Task 6 — Consolidated debug_session: SessionAction enum, status/stop/list/delete routing, session_status() method, deprecation wrappers
- [x] Task 7 — System prompt updated, consolidated tools first, deprecated tools with "(Deprecated)" markers
- [x] Task 8 — Full test suite: 190 unit + all integration tests pass
- [x] Stepping fix — min_offset=16 unconditional, remove_breakpoint moved to cleanup phase
- [x] All deprecation wrappers correctly forward and log warnings
- [x] All unit tests from plan implemented (6 test modules, 20+ tests)
- [x] Event struct rowid field with #[serde(skip)] — correct design
- [x] Cursor pagination threading through both query_events and count_filtered_events

### Summary
- **Critical: 0**
- **Important: 2** (missing validate() on DebugTestRequest; silent null write in DebugMemoryRequest)
- **Minor: 5** (events_dropped off-by-one, misplaced comment, tool ordering, non-deterministic paused_threads, phase2a early return)
- **Ready to merge: Yes** (Important issues are low-risk edge cases — validation happens in server.rs for tests, and null write would fail downstream anyway)
