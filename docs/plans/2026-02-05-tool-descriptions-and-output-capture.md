# Tool Descriptions & Process Output Capture

**Goal:** Fix misleading MCP tool descriptions so the LLM follows the correct trace→launch→query workflow, and capture stdout/stderr into the unified event timeline.
**Architecture:** Intercept `write(2)` syscall in the Frida agent for fd 1/2, send output as events through the existing event pipeline, extend the events table and query to support `stdout`/`stderr` event types.
**Commit strategy:** Single commit at the end.

## Workstreams

- **Stream A (Tool descriptions):** Task 1 — pure Rust, no dependencies
- **Stream B (Output capture):** Tasks 2, 3, 4, 5 — agent → daemon → db → query pipeline
- **Serial:** Task 6 (rebuild agent + Rust, integration verification)

---

### Task 1: Rewrite MCP tool descriptions

**Files:**
- Modify: `src/daemon/server.rs:174-251` (handle_tools_list)

**Step 1: Update `debug_trace` description and schema**

Replace lines 191-202 with:
```rust
McpTool {
    name: "debug_trace".to_string(),
    description: "Configure trace patterns. IMPORTANT: Call BEFORE debug_launch (without sessionId) to set which functions to trace — patterns are applied when the process spawns. Can also be called WITH sessionId to add/remove patterns on a running session.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string", "description": "Session ID. Omit to set pending patterns for the next debug_launch. Provide to modify a running session." },
            "add": { "type": "array", "items": { "type": "string" }, "description": "Patterns to start tracing (e.g. \"mymodule::*\", \"*::init\", \"@usercode\")" },
            "remove": { "type": "array", "items": { "type": "string" }, "description": "Patterns to stop tracing" }
        }
    }),
},
```

**Step 2: Update `debug_launch` description**

Replace lines 176-190 with:
```rust
McpTool {
    name: "debug_launch".to_string(),
    description: "Launch a binary with Frida attached. Applies any pending trace patterns set via debug_trace (without sessionId). If no patterns were set, no functions will be traced — call debug_trace first. Process stdout/stderr are captured and queryable as events.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "command": { "type": "string", "description": "Path to executable" },
            "args": { "type": "array", "items": { "type": "string" }, "description": "Command line arguments" },
            "cwd": { "type": "string", "description": "Working directory" },
            "projectRoot": { "type": "string", "description": "Root directory for user code detection" },
            "env": { "type": "object", "description": "Additional environment variables" }
        },
        "required": ["command", "projectRoot"]
    }),
},
```

**Step 3: Update `debug_query` description and schema**

Replace lines 203-239 with:
```rust
McpTool {
    name: "debug_query".to_string(),
    description: "Query the unified execution timeline: function traces AND process stdout/stderr. Returns events in chronological order. Filter by eventType to get only traces or only output.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string" },
            "eventType": { "type": "string", "enum": ["function_enter", "function_exit", "stdout", "stderr"] },
            "function": {
                "type": "object",
                "properties": {
                    "equals": { "type": "string" },
                    "contains": { "type": "string" },
                    "matches": { "type": "string" }
                }
            },
            "sourceFile": {
                "type": "object",
                "properties": {
                    "equals": { "type": "string" },
                    "contains": { "type": "string" }
                }
            },
            "returnValue": {
                "type": "object",
                "properties": {
                    "equals": {},
                    "isNull": { "type": "boolean" }
                }
            },
            "limit": { "type": "integer", "default": 50, "maximum": 500 },
            "offset": { "type": "integer" },
            "verbose": { "type": "boolean", "default": false }
        },
        "required": ["sessionId"]
    }),
},
```

**Checkpoint:** Tool descriptions are clear about the trace→launch→query workflow.

---

### Task 2: Add stdout/stderr interception in Frida agent

**Files:**
- Modify: `agent/src/agent.ts`

**Step 1: Add output event type to TraceEvent interface**

After the `TraceEvent` interface (line 31), add:
```typescript
interface OutputEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  eventType: 'stdout' | 'stderr';
  text: string;
}
```

**Step 2: Add write() interception in StrobeAgent constructor**

At the end of the constructor (after the setInterval call, ~line 52), add:
```typescript
// Intercept write(2) for stdout/stderr capture
this.installOutputCapture();
```

**Step 3: Add installOutputCapture method to StrobeAgent**

Add after the `getTimestampNs()` method:
```typescript
private installOutputCapture(): void {
  const self = this;
  const writePtr = Module.getExportByName(null, 'write');
  if (!writePtr) return;

  Interceptor.attach(writePtr, {
    onEnter(args) {
      const fd = args[0].toInt32();
      if (fd !== 1 && fd !== 2) return;

      const buf = args[1];
      const count = args[2].toInt32();
      if (count <= 0 || count > 1048576) return; // Skip empty or >1MB writes

      let text: string;
      try {
        text = buf.readUtf8String(count) ?? '';
      } catch {
        try {
          text = buf.readCString(count) ?? '';
        } catch {
          return; // Can't read buffer, skip
        }
      }

      if (text.length === 0) return;

      const event: OutputEvent = {
        id: self.generateEventId(),
        sessionId: self.sessionId,
        timestampNs: self.getTimestampNs(),
        threadId: Process.getCurrentThreadId(),
        eventType: fd === 1 ? 'stdout' : 'stderr',
        text,
      };

      self.bufferOutputEvent(event);
    }
  });
}

private bufferOutputEvent(event: OutputEvent): void {
  // Reuse the same event buffer and flush mechanism
  this.eventBuffer.push(event as any);

  if (this.eventBuffer.length >= this.maxBufferSize) {
    this.flush();
  }
}
```

**Step 4: Rebuild agent**

```bash
cd agent && npm run build
```

**Checkpoint:** Agent intercepts write(2) and sends output events through existing pipeline.

---

### Task 3: Handle output events in daemon spawner

**Files:**
- Modify: `src/db/event.rs:6-11` (EventType enum)
- Modify: `src/db/event.rs:30-45` (Event struct)
- Modify: `src/frida_collector/spawner.rs:383-405` (parse_event)
- Modify: `src/mcp/types.rs:48-53` (EventTypeFilter)

**Step 1: Extend EventType enum**

In `src/db/event.rs`, add variants:
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FunctionEnter => "function_enter",
            Self::FunctionExit => "function_exit",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "function_enter" => Some(Self::FunctionEnter),
            "function_exit" => Some(Self::FunctionExit),
            "stdout" => Some(Self::Stdout),
            "stderr" => Some(Self::Stderr),
            _ => None,
        }
    }
}
```

**Step 2: Add text field to Event struct**

In `src/db/event.rs` Event struct, add after `duration_ns`:
```rust
pub text: Option<String>,
```

**Step 3: Extend parse_event in spawner.rs**

In `src/frida_collector/spawner.rs`, update `parse_event` to handle output events:
```rust
fn parse_event(session_id: &str, json: &serde_json::Value) -> Option<Event> {
    let event_type = match json.get("eventType")?.as_str()? {
        "function_enter" => EventType::FunctionEnter,
        "function_exit" => EventType::FunctionExit,
        "stdout" => EventType::Stdout,
        "stderr" => EventType::Stderr,
        _ => return None,
    };

    // Output events have a simpler structure
    if event_type == EventType::Stdout || event_type == EventType::Stderr {
        return Some(Event {
            id: json.get("id")?.as_str()?.to_string(),
            session_id: session_id.to_string(),
            timestamp_ns: json.get("timestampNs")?.as_i64()?,
            thread_id: json.get("threadId")?.as_i64()?,
            parent_event_id: None,
            event_type,
            function_name: String::new(),
            function_name_raw: None,
            source_file: None,
            line_number: None,
            arguments: None,
            return_value: None,
            duration_ns: None,
            text: json.get("text").and_then(|v| v.as_str()).map(|s| s.to_string()),
        });
    }

    Some(Event {
        id: json.get("id")?.as_str()?.to_string(),
        session_id: session_id.to_string(),
        timestamp_ns: json.get("timestampNs")?.as_i64()?,
        thread_id: json.get("threadId")?.as_i64()?,
        parent_event_id: json.get("parentEventId").and_then(|v| v.as_str()).map(|s| s.to_string()),
        event_type,
        function_name: json.get("functionName")?.as_str()?.to_string(),
        function_name_raw: json.get("functionNameRaw").and_then(|v| v.as_str()).map(|s| s.to_string()),
        source_file: json.get("sourceFile").and_then(|v| v.as_str()).map(|s| s.to_string()),
        line_number: json.get("lineNumber").and_then(|v| v.as_i64()).map(|n| n as i32),
        arguments: json.get("arguments").cloned(),
        return_value: json.get("returnValue").cloned(),
        duration_ns: json.get("durationNs").and_then(|v| v.as_i64()),
        text: None,
    })
}
```

**Step 4: Extend EventTypeFilter in mcp/types.rs**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
}
```

**Checkpoint:** Daemon can parse and route stdout/stderr events from agent.

---

### Task 4: Update database schema and queries for text field

**Files:**
- Modify: `src/db/schema.rs:53-71` (events table)
- Modify: `src/db/event.rs:142-199` (insert methods)
- Modify: `src/db/event.rs:201-293` (query method)

**Step 1: Add text column to events table**

In `src/db/schema.rs`, update the CREATE TABLE events statement — add after `duration_ns INTEGER,`:
```sql
text TEXT,
```

**Step 2: Update insert_event and insert_events_batch**

Add `text` to the INSERT statements and params:
```rust
// In both insert_event and insert_events_batch, update SQL to:
"INSERT INTO events (id, session_id, timestamp_ns, thread_id, parent_event_id,
 event_type, function_name, function_name_raw, source_file, line_number,
 arguments, return_value, duration_ns, text)
 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"

// Add to params:
event.text,  // (or &event.text for batch)
```

**Step 3: Update query_events to read text column**

In the SELECT and row mapping, add text as column index 13:
```rust
// Add to SELECT:
"SELECT id, session_id, timestamp_ns, thread_id, parent_event_id,
 event_type, function_name, function_name_raw, source_file, line_number,
 arguments, return_value, duration_ns, text
 FROM events WHERE session_id = ?"

// Add to row mapping:
text: row.get(13)?,
```

**Checkpoint:** Database stores and retrieves the text field for output events.

---

### Task 5: Update query response formatting for output events

**Files:**
- Modify: `src/daemon/server.rs:401-485` (tool_debug_query)

**Step 1: Update the event_type filter mapping**

In `tool_debug_query`, extend the match for event_type filter (~line 413):
```rust
if let Some(ref et) = req.event_type {
    q = q.event_type(match et {
        EventTypeFilter::FunctionEnter => crate::db::EventType::FunctionEnter,
        EventTypeFilter::FunctionExit => crate::db::EventType::FunctionExit,
        EventTypeFilter::Stdout => crate::db::EventType::Stdout,
        EventTypeFilter::Stderr => crate::db::EventType::Stderr,
    });
}
```

**Step 2: Update event JSON formatting**

In the verbose/summary event serialization, handle output events differently:
```rust
// In both verbose and summary branches, check event type first:
let event_values: Vec<serde_json::Value> = events.iter().map(|e| {
    // Output events have a different shape
    if e.event_type == crate::db::EventType::Stdout || e.event_type == crate::db::EventType::Stderr {
        return serde_json::json!({
            "id": e.id,
            "timestamp_ns": e.timestamp_ns,
            "eventType": e.event_type.as_str(),
            "threadId": e.thread_id,
            "text": e.text,
        });
    }

    // Function trace events (existing logic)
    if verbose {
        serde_json::json!({
            "id": e.id,
            "timestamp_ns": e.timestamp_ns,
            "eventType": e.event_type.as_str(),
            "function": e.function_name,
            "functionRaw": e.function_name_raw,
            "sourceFile": e.source_file,
            "line": e.line_number,
            "duration_ns": e.duration_ns,
            "threadId": e.thread_id,
            "parentEventId": e.parent_event_id,
            "arguments": e.arguments,
            "returnValue": e.return_value,
        })
    } else {
        serde_json::json!({
            "id": e.id,
            "timestamp_ns": e.timestamp_ns,
            "eventType": e.event_type.as_str(),
            "function": e.function_name,
            "sourceFile": e.source_file,
            "line": e.line_number,
            "duration_ns": e.duration_ns,
            "returnType": e.return_value.as_ref()
                .map(|v| match v {
                    serde_json::Value::Null => "null",
                    serde_json::Value::Bool(_) => "bool",
                    serde_json::Value::Number(_) => "number",
                    serde_json::Value::String(_) => "string",
                    serde_json::Value::Array(_) => "array",
                    serde_json::Value::Object(_) => "object",
                })
                .unwrap_or("void"),
        })
    }
}).collect();
```

Note: also add `"eventType"` to the existing summary format — previously it was omitted, but now it's needed to distinguish event types.

**Checkpoint:** Query returns a unified timeline with function traces and stdout/stderr interleaved chronologically.

---

### Task 6: Update tests and verify

**Files:**
- Modify: `tests/integration.rs`
- Modify: `src/db/mod.rs` (tests)
- Modify: `src/mcp/mod.rs` (tests)

**Step 1: Update existing event tests to include text field**

In all places where `Event { ... }` is constructed in tests, add `text: None`.

**Step 2: Add output event test**

In `tests/integration.rs`:
```rust
#[test]
fn test_output_event_insertion_and_query() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = strobe::db::Database::open(&db_path).unwrap();

    db.create_session("test-session", "/bin/test", "/home", 1234).unwrap();

    // Insert stdout event
    db.insert_event(strobe::db::Event {
        id: "evt-out-1".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 1500,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::Stdout,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: Some("Hello from stdout\n".to_string()),
    }).unwrap();

    // Insert stderr event
    db.insert_event(strobe::db::Event {
        id: "evt-out-2".to_string(),
        session_id: "test-session".to_string(),
        timestamp_ns: 2500,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::Stderr,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: Some("Error: something went wrong\n".to_string()),
    }).unwrap();

    // Query all - should return both in timestamp order
    let all = db.query_events("test-session", |q| q).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].event_type, strobe::db::EventType::Stdout);
    assert_eq!(all[0].text.as_deref(), Some("Hello from stdout\n"));
    assert_eq!(all[1].event_type, strobe::db::EventType::Stderr);

    // Query filtered by event type
    let stdout_only = db.query_events("test-session", |q| {
        q.event_type(strobe::db::EventType::Stdout)
    }).unwrap();
    assert_eq!(stdout_only.len(), 1);
}
```

**Step 3: Update EventType serialization test**

```rust
#[test]
fn test_event_type_serialization() {
    use strobe::db::EventType;
    assert_eq!(EventType::Stdout.as_str(), "stdout");
    assert_eq!(EventType::Stderr.as_str(), "stderr");
    assert_eq!(EventType::from_str("stdout"), Some(EventType::Stdout));
    assert_eq!(EventType::from_str("stderr"), Some(EventType::Stderr));
}
```

**Step 4: Build and run tests**

```bash
cd agent && npm run build
cd .. && cargo build
cargo test
```

**Step 5: Manual end-to-end verification**

Delete the old database to pick up schema changes:
```bash
rm -f ~/.strobe/strobe.db
pkill -f "strobe daemon"
```

Then from a Claude Code session, the correct workflow should be:
1. `debug_trace({ add: ["*"] })` — set pending patterns (no sessionId)
2. `debug_launch({ command: "/path/to/app", projectRoot: "/path" })` — launch with patterns applied
3. `debug_query({ sessionId: "..." })` — see unified timeline with function traces AND stdout/stderr

**Checkpoint:** All tests pass, agent rebuilt, end-to-end workflow verified.
