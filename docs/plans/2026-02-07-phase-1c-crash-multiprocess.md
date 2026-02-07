# Phase 1c: Crash & Multi-Process Implementation Plan

**Spec:** `docs/FEATURES.md` (Phase 1c section)
**Goal:** Handle crashes gracefully with full context (signal, stack, registers, locals), track execution across fork/exec, and add time/duration/PID query filters.
**Architecture:** Agent-side exception handler (`Process.setExceptionHandler`) for crash capture; DWARF location expression evaluation for local variables; frida-sys spawn gating for fork/exec; SQL query enhancements.
**Tech Stack:** Frida GumJS, frida-sys, gimli (DWARF), rusqlite
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Schema & Queries):** Tasks 1, 2 — foundational changes needed by both crash and fork/exec
- **Stream B (Crash Capture):** Tasks 3, 4, 5 — agent exception handler, daemon storage, DWARF locals
- **Stream C (Fork/Exec):** Tasks 6, 7 — spawn gating, multi-PID sessions
- **Stream D (Testing):** Task 8 — stress test C binary (can be built first, used throughout)

**Dependencies:** Task 8 (stress test binary) can be built first as it has no code dependencies. A must complete before B and C. B and C can run in parallel.
Within B: 3 → 4 → 5 (sequential)
Within C: 6 → 7 (sequential)

---

### Task 1: Add PID to Events Schema & Struct

**Files:**
- Modify: `src/db/schema.rs` (add column migration)
- Modify: `src/db/event.rs` (Event struct, insert, query)
- Modify: `src/frida_collector/spawner.rs` (set PID on events)

**Step 1: Add `pid` column to events table**

In `src/db/schema.rs`, after the `thread_name` column migration, add:

```rust
// Add pid column (idempotent for existing DBs)
match conn.execute("ALTER TABLE events ADD COLUMN pid INTEGER", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
```

Add index for PID queries:
```rust
conn.execute(
    "CREATE INDEX IF NOT EXISTS idx_events_pid ON events(session_id, pid)",
    [],
)?;
```

**Step 2: Add `pid` field to Event struct**

In `src/db/event.rs`, add to `Event`:
```rust
pub pid: Option<u32>,
```

Update `insert_event`, `insert_events_batch`, and `insert_events_with_limit` to include `pid` in the INSERT statement and params.

Update `query_events` to read `pid` from the result row.

**Step 3: Stamp PID on events from agent**

In `src/frida_collector/spawner.rs`:

In `AgentMessageHandler`, add a `pid: u32` field. Set it when creating the handler from `WorkerSession::pid`.

In `parse_event`, add a `pid` parameter and set `event.pid = Some(pid)`.

In `raw_on_output`, set `pid: Some(ctx.pid)` on the output Event.

**Step 4: Add PID to EventQuery**

In `src/db/event.rs`, add to `EventQuery`:
```rust
pub pid_equals: Option<u32>,
```

In `Database::query_events`, add SQL clause:
```rust
if let Some(pid) = query.pid_equals {
    sql.push_str(" AND pid = ?");
    params_vec.push(Box::new(pid as i64));
}
```

**Checkpoint:** Events are stored with PID. Existing code works (PID is optional/nullable). New events from agent and output capture have PID set.

---

### Task 2: Enhanced Query Filters

**Files:**
- Modify: `src/db/event.rs` (EventQuery, SQL generation)
- Modify: `src/mcp/types.rs` (DebugQueryRequest, new filter types)
- Modify: `src/daemon/server.rs` (tool schema, query handler, tool description)

**Step 1: Add time range and duration to EventQuery**

In `src/db/event.rs`, add to `EventQuery`:
```rust
pub timestamp_from_ns: Option<i64>,
pub timestamp_to_ns: Option<i64>,
pub min_duration_ns: Option<i64>,
```

Add SQL clauses in `Database::query_events`:
```rust
if let Some(from) = query.timestamp_from_ns {
    sql.push_str(" AND timestamp_ns >= ?");
    params_vec.push(Box::new(from));
}
if let Some(to) = query.timestamp_to_ns {
    sql.push_str(" AND timestamp_ns <= ?");
    params_vec.push(Box::new(to));
}
if let Some(min_dur) = query.min_duration_ns {
    sql.push_str(" AND duration_ns IS NOT NULL AND duration_ns >= ?");
    params_vec.push(Box::new(min_dur));
}
```

**Step 2: Add MCP types for new filters**

In `src/mcp/types.rs`, add to `DebugQueryRequest`:
```rust
/// Filter events from this timestamp (nanoseconds from session start).
/// Also accepts relative strings: "-5s", "-1m", "-500ms"
#[serde(skip_serializing_if = "Option::is_none")]
pub time_from: Option<serde_json::Value>,

/// Filter events up to this timestamp (nanoseconds from session start).
/// Also accepts relative strings.
#[serde(skip_serializing_if = "Option::is_none")]
pub time_to: Option<serde_json::Value>,

/// Find functions that took at least this many nanoseconds
#[serde(skip_serializing_if = "Option::is_none")]
pub min_duration_ns: Option<i64>,

/// Filter by process ID (for multi-process sessions)
#[serde(skip_serializing_if = "Option::is_none")]
pub pid: Option<u32>,
```

**Step 3: Relative time parser**

Add a helper function in `src/daemon/server.rs`:
```rust
/// Parse a time value that can be either:
/// - An integer (absolute timestamp_ns)
/// - A string like "-5s", "-1m", "-500ms" (relative to latest event)
fn resolve_time_value(value: &serde_json::Value, latest_ns: i64) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => {
            let s = s.trim();
            if !s.starts_with('-') {
                return s.parse::<i64>().ok();
            }
            let (num_str, multiplier) = if s.ends_with("ms") {
                (&s[1..s.len()-2], 1_000_000i64)
            } else if s.ends_with('s') {
                (&s[1..s.len()-1], 1_000_000_000i64)
            } else if s.ends_with('m') {
                (&s[1..s.len()-1], 60_000_000_000i64)
            } else {
                return None;
            };
            let num: i64 = num_str.parse().ok()?;
            Some(latest_ns - num * multiplier)
        }
        _ => None,
    }
}
```

**Step 4: Wire up in query handler**

In `tool_debug_query` in `src/daemon/server.rs`, resolve time filters before building the query:
```rust
// Resolve relative time values
let latest_ns = if req.time_from.is_some() || req.time_to.is_some() {
    // Get max timestamp for this session
    self.session_manager.db().get_latest_timestamp(&req.session_id)?
} else {
    0
};
let timestamp_from_ns = req.time_from.as_ref()
    .and_then(|v| resolve_time_value(v, latest_ns));
let timestamp_to_ns = req.time_to.as_ref()
    .and_then(|v| resolve_time_value(v, latest_ns));
```

Add `get_latest_timestamp` to `Database` in `src/db/event.rs`:
```rust
pub fn get_latest_timestamp(&self, session_id: &str) -> Result<i64> {
    let conn = self.connection();
    let ts: i64 = conn.query_row(
        "SELECT COALESCE(MAX(timestamp_ns), 0) FROM events WHERE session_id = ?",
        params![session_id],
        |row| row.get(0),
    )?;
    Ok(ts)
}
```

Apply filters in the query builder closure:
```rust
if let Some(from) = timestamp_from_ns {
    q.timestamp_from_ns = Some(from);
}
if let Some(to) = timestamp_to_ns {
    q.timestamp_to_ns = Some(to);
}
if let Some(dur) = req.min_duration_ns {
    q.min_duration_ns = Some(dur);
}
if let Some(pid) = req.pid {
    q.pid_equals = Some(pid);
}
```

**Step 5: Update tool schema**

In `handle_tools_list`, add to `debug_query` schema:
```json
"timeFrom": {
    "description": "Filter from this time. Integer (absolute ns) or string (\"-5s\", \"-1m\", \"-500ms\")"
},
"timeTo": {
    "description": "Filter to this time. Integer (absolute ns) or string (\"-5s\", \"-1m\", \"-500ms\")"
},
"minDurationNs": {
    "type": "integer",
    "description": "Minimum function duration in nanoseconds (find slow functions)"
},
"pid": {
    "type": "integer",
    "description": "Filter by process ID (for multi-process sessions)"
}
```

Update the `debug_query` tool description to mention time range and duration filters.

Also update the debugging instructions in `debugging_instructions()` to document the new query capabilities.

**Step 6: Include PID in query output**

In `tool_debug_query`, add `"pid": e.pid` to both verbose and summary event JSON output for all event types (function traces, stdout/stderr, and crash).

**Checkpoint:** Can query events by time range (both absolute and relative), find slow functions by duration, and filter by PID. All events carry PID.

---

### Task 3: Crash Capture (Agent-Side)

**Files:**
- Modify: `agent/src/agent.ts` (exception handler, crash event creation)
- Rebuild: `agent/dist/agent.js`

**Step 1: Add exception handler in agent initialization**

In `agent/src/agent.ts`, in the `StrobeAgent` class, add an `installExceptionHandler()` method called during initialization:

```typescript
private installExceptionHandler(): void {
    Process.setExceptionHandler((details) => {
        const crashEvent = this.buildCrashEvent(details);
        send({ type: 'events', events: [crashEvent] });

        // Return false to let the OS handle the crash (terminate the process)
        // The event will be flushed because send() is synchronous
        return false;
    });
}

private buildCrashEvent(details: ExceptionDetails): any {
    const timestamp = this.getTimestampNs();
    const eventId = `${this.sessionId}-crash-${Date.now()}`;

    // Build stack trace using Thread.backtrace
    let backtrace: any[] = [];
    try {
        const frames = Thread.backtrace(details.context, Backtracer.ACCURATE);
        backtrace = frames.map((addr: NativePointer) => {
            const sym = DebugSymbol.fromAddress(addr);
            return {
                address: addr.toString(),
                moduleName: sym.moduleName,
                name: sym.name,
                fileName: sym.fileName,
                lineNumber: sym.lineNumber,
            };
        });
    } catch (e) {
        // Backtrace may fail in some crash scenarios
    }

    // Capture register state from crash context
    const registers: Record<string, string> = {};
    const ctx = details.context as any;
    // ARM64 registers
    if (Process.arch === 'arm64') {
        for (let i = 0; i <= 28; i++) {
            const regName = `x${i}`;
            if (ctx[regName]) registers[regName] = ctx[regName].toString();
        }
        if (ctx.fp) registers.fp = ctx.fp.toString();
        if (ctx.lr) registers.lr = ctx.lr.toString();
        if (ctx.sp) registers.sp = ctx.sp.toString();
        if (ctx.pc) registers.pc = ctx.pc.toString();
    }
    // x86_64 registers
    else if (Process.arch === 'x64') {
        for (const reg of ['rax','rbx','rcx','rdx','rsi','rdi','rbp','rsp',
                           'r8','r9','r10','r11','r12','r13','r14','r15','rip']) {
            if (ctx[reg]) registers[reg] = ctx[reg].toString();
        }
    }

    // Read stack frame memory around frame pointer (for local variable resolution)
    let frameMemory: string | null = null;
    let frameBase: string | null = null;
    try {
        const fp = Process.arch === 'arm64' ? ctx.fp : ctx.rbp;
        if (fp && !fp.isNull()) {
            frameBase = fp.toString();
            // Read 512 bytes below and 128 bytes above FP
            const readBase = fp.sub(512);
            const data = readBase.readByteArray(640);
            if (data) {
                frameMemory = _arrayBufferToHex(data);
            }
        }
    } catch (e) {
        // Frame memory read may fail
    }

    // Memory access details (for access violations)
    let memoryAccess: any = undefined;
    if (details.memory) {
        memoryAccess = {
            operation: details.memory.operation,
            address: details.memory.address.toString(),
        };
    }

    return {
        id: eventId,
        timestampNs: timestamp,
        threadId: Process.getCurrentThreadId(),
        threadName: null,
        eventType: 'crash',
        pid: Process.id,
        signal: details.type,
        faultAddress: details.address.toString(),
        registers: registers,
        backtrace: backtrace,
        frameMemory: frameMemory,
        frameBase: frameBase,
        memoryAccess: memoryAccess,
    };
}
```

Add hex encoding helper:
```typescript
function _arrayBufferToHex(buffer: ArrayBuffer): string {
    const bytes = new Uint8Array(buffer);
    let hex = '';
    for (let i = 0; i < bytes.length; i++) {
        hex += bytes[i].toString(16).padStart(2, '0');
    }
    return hex;
}
```

Call `installExceptionHandler()` from the `initialize` message handler, after session setup.

**Step 2: Rebuild agent**

```bash
cd agent && npm run build
touch ../src/frida_collector/spawner.rs  # Force Cargo to pick up new agent.js
```

**Checkpoint:** Agent intercepts crashes and sends crash events with signal, registers, backtrace, and frame memory.

---

### Task 4: Crash Capture (Daemon-Side)

**Files:**
- Modify: `src/db/event.rs` (EventType::Crash, crash fields)
- Modify: `src/db/schema.rs` (new columns)
- Modify: `src/frida_collector/spawner.rs` (parse crash events)
- Modify: `src/mcp/types.rs` (EventTypeFilter::Crash)
- Modify: `src/daemon/server.rs` (crash event format in queries, tool schema)

**Step 1: Add EventType::Crash**

In `src/db/event.rs`:
```rust
pub enum EventType {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
}

// Update as_str/from_str:
Self::Crash => "crash",
"crash" => Some(Self::Crash),
```

**Step 2: Add crash columns to schema**

In `src/db/schema.rs`, add migrations:
```rust
// Crash-related columns
match conn.execute("ALTER TABLE events ADD COLUMN signal TEXT", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
match conn.execute("ALTER TABLE events ADD COLUMN fault_address TEXT", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
match conn.execute("ALTER TABLE events ADD COLUMN registers JSON", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
match conn.execute("ALTER TABLE events ADD COLUMN backtrace JSON", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
match conn.execute("ALTER TABLE events ADD COLUMN locals JSON", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
```

**Step 3: Add crash fields to Event struct**

In `src/db/event.rs`, add to `Event`:
```rust
pub signal: Option<String>,
pub fault_address: Option<String>,
pub registers: Option<serde_json::Value>,
pub backtrace: Option<serde_json::Value>,
pub locals: Option<serde_json::Value>,
```

Update all INSERT statements to include the new columns.
Update `query_events` to read the new columns.
Set all crash fields to `None` for non-crash events in `parse_event`.

**Step 4: Parse crash events from agent**

In `src/frida_collector/spawner.rs`, update `parse_event` to handle `"crash"` event type:

```rust
"crash" => EventType::Crash,
```

For crash events, extract:
```rust
if event_type == EventType::Crash {
    return Some(Event {
        id: ...,
        session_id: session_id.to_string(),
        timestamp_ns: json.get("timestampNs")?.as_i64()?,
        thread_id: json.get("threadId")?.as_i64()?,
        thread_name: None,
        parent_event_id: None,
        event_type,
        function_name: String::new(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: None,
        sampled: None,
        watch_values: None,
        pid: json.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32),
        signal: json.get("signal").and_then(|v| v.as_str()).map(|s| s.to_string()),
        fault_address: json.get("faultAddress").and_then(|v| v.as_str()).map(|s| s.to_string()),
        registers: json.get("registers").cloned(),
        backtrace: json.get("backtrace").cloned(),
        locals: None, // Populated later by DWARF resolution
    });
}
```

**Step 5: Add Crash to EventTypeFilter**

In `src/mcp/types.rs`:
```rust
pub enum EventTypeFilter {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
}
```

**Step 6: Format crash events in query output**

In `src/daemon/server.rs` `tool_debug_query`, add crash event formatting:
```rust
if e.event_type == crate::db::EventType::Crash {
    return serde_json::json!({
        "id": e.id,
        "timestamp_ns": e.timestamp_ns,
        "eventType": "crash",
        "pid": e.pid,
        "threadId": e.thread_id,
        "signal": e.signal,
        "faultAddress": e.fault_address,
        "registers": e.registers,
        "backtrace": e.backtrace,
        "locals": e.locals,
    });
}
```

**Step 7: Update tool schema**

Add `"crash"` to the `eventType` enum in the `debug_query` tool schema.

**Checkpoint:** Crash events are captured, stored, and queryable. `debug_query({ eventType: "crash" })` returns full crash context with signal, registers, and backtrace.

---

### Task 5: Local Variables in Crash Frame (DWARF)

**Files:**
- Modify: `src/dwarf/parser.rs` (parse locals from subprograms, evaluate location expressions)
- Modify: `src/dwarf/function.rs` (LocalVariableInfo struct)
- Modify: `src/daemon/session_manager.rs` (resolve locals on crash)
- Modify: `src/frida_collector/spawner.rs` (trigger local resolution, store frame memory)

**Step 1: Add LocalVariableInfo struct**

In `src/dwarf/function.rs`:
```rust
/// A local variable or parameter in a function, with its DWARF location.
#[derive(Debug, Clone)]
pub struct LocalVariableInfo {
    pub name: String,
    pub byte_size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
    /// Location: either a simple expression or a location list
    pub location: LocalVarLocation,
}

#[derive(Debug, Clone)]
pub enum LocalVarLocation {
    /// Frame-base relative: value is at [frame_base + offset]
    FrameBaseRelative(i64),
    /// In a register
    Register(u16),
    /// Register + offset: value is at [register_value + offset]
    RegisterOffset(u16, i64),
    /// Fixed address
    Address(u64),
    /// Complex expression we can't evaluate
    Complex,
}
```

**Step 2: Parse local variables from subprograms**

Add to `DwarfParser`:
```rust
/// Parse local variables for the function containing the given PC address.
/// Returns (function_name, frame_base_location, locals).
pub fn locals_at_pc(&self, pc: u64) -> Option<(String, LocalVarLocation, Vec<LocalVariableInfo>)> {
    // Find the function containing this PC
    let func = self.functions.iter().find(|f| f.contains_address(pc))?;

    // Re-parse the DWARF to find locals in this specific function.
    // This is expensive but only done on crash (rare).
    // We need access to the raw DWARF data, which means we need to
    // store/re-open the binary path.
    //
    // For now, return None — Task 5b will implement the full re-parse.
    // The crash event is still useful with signal/registers/backtrace.
    None
}
```

The full implementation requires re-opening the DWARF file and doing a targeted parse. Add a `binary_path` field to `DwarfParser` so it can be re-opened:

```rust
pub struct DwarfParser {
    pub functions: Vec<FunctionInfo>,
    pub(crate) functions_by_name: HashMap<String, Vec<usize>>,
    pub variables: Vec<VariableInfo>,
    pub(crate) variables_by_name: HashMap<String, Vec<usize>>,
    pub(crate) struct_members: HashMap<String, Vec<StructMember>>,
    pub image_base: u64,
    /// Path to the binary (or dSYM) for re-parsing on demand
    binary_path: Option<std::path::PathBuf>,
}
```

Set it during parse:
```rust
parser.binary_path = Some(path.to_path_buf());
```

**Step 3: Implement locals parsing for a specific function**

Add a new method that re-opens the DWARF file and parses only the function containing the crash PC:

```rust
pub fn parse_locals_at_pc(&self, crash_pc: u64) -> Result<Vec<LocalVariableInfo>> {
    let binary_path = self.binary_path.as_ref()
        .ok_or_else(|| Error::Frida("No binary path for DWARF re-parse".into()))?;

    let file = std::fs::File::open(binary_path)?;
    let mmap = unsafe { memmap2::Mmap::map(&file)? };
    let object = object::File::parse(&*mmap)
        .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

    let endian = if object.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };

    let load_section = |id: gimli::SectionId| -> std::result::Result<std::borrow::Cow<[u8]>, gimli::Error> {
        let name = id.name();
        let data = object.section_by_name(name)
            .or_else(|| object.section_by_name(&name.replace(".debug_", "__debug_")))
            .and_then(|section| section.data().ok())
            .unwrap_or(&[]);
        Ok(std::borrow::Cow::Borrowed(data))
    };

    let dwarf_cow = gimli::Dwarf::load(&load_section)
        .map_err(|e| Error::Frida(format!("DWARF load: {}", e)))?;
    let dwarf = dwarf_cow.borrow(|s| gimli::EndianSlice::new(s.as_ref(), endian));

    let mut locals = Vec::new();

    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let unit = match dwarf.unit(header) {
            Ok(u) => u,
            Err(_) => continue,
        };

        let mut entries = unit.entries();
        let mut in_target_func = false;
        let mut target_depth: isize = 0;
        let mut current_depth: isize = 0;

        while let Ok(Some((delta, entry))) = entries.next_dfs() {
            current_depth += delta;

            // Left the target function
            if in_target_func && current_depth <= target_depth {
                break; // Done — found all locals for this function
            }

            match entry.tag() {
                gimli::DW_TAG_subprogram => {
                    // Check if this function contains crash_pc
                    let low_pc = entry.attr_value(gimli::DW_AT_low_pc).ok().flatten()
                        .and_then(|v| dwarf.attr_address(&unit, v).ok().flatten());
                    let high_pc = entry.attr_value(gimli::DW_AT_high_pc).ok().flatten()
                        .map(|v| match v {
                            gimli::AttributeValue::Udata(offset) => low_pc.map(|lp| lp + offset),
                            _ => dwarf.attr_address(&unit, v).ok().flatten(),
                        })
                        .flatten();

                    if let (Some(lp), Some(hp)) = (low_pc, high_pc) {
                        if crash_pc >= lp && crash_pc < hp {
                            in_target_func = true;
                            target_depth = current_depth;
                        }
                    }
                }
                gimli::DW_TAG_variable | gimli::DW_TAG_formal_parameter if in_target_func => {
                    if let Some(local) = Self::parse_local_variable(&dwarf, &unit, entry) {
                        locals.push(local);
                    }
                }
                _ => {}
            }
        }

        if !locals.is_empty() {
            break; // Found the function, no need to continue
        }
    }

    Ok(locals)
}

fn parse_local_variable<R: gimli::Reader>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    entry: &gimli::DebuggingInformationEntry<R>,
) -> Option<LocalVariableInfo> {
    let name = entry.attr_value(gimli::DW_AT_name).ok()??
        .and_then(|v| dwarf.attr_string(unit, v).ok())
        .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));
    // Simpler: just get the string directly
    let name = entry.attr_value(gimli::DW_AT_name).ok().flatten()
        .and_then(|v| dwarf.attr_string(unit, v).ok())
        .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()))?;

    // Parse location
    let location = match entry.attr_value(gimli::DW_AT_location).ok().flatten() {
        Some(gimli::AttributeValue::Exprloc(expr)) => {
            let mut ops = expr.operations(unit.encoding());
            match ops.next().ok().flatten() {
                Some(gimli::Operation::FrameOffset { offset }) => {
                    LocalVarLocation::FrameBaseRelative(offset)
                }
                Some(gimli::Operation::Register { register }) => {
                    LocalVarLocation::Register(register.0)
                }
                Some(gimli::Operation::RegisterOffset { register, offset, .. }) => {
                    LocalVarLocation::RegisterOffset(register.0, offset)
                }
                Some(gimli::Operation::Address { address }) => {
                    LocalVarLocation::Address(address)
                }
                _ => LocalVarLocation::Complex,
            }
        }
        _ => LocalVarLocation::Complex,
    };

    // Get type info
    let (byte_size, type_kind, type_name) = Self::resolve_type_info(dwarf, unit, entry)
        .unwrap_or((0, TypeKind::Unknown, None));

    Some(LocalVariableInfo {
        name,
        byte_size,
        type_kind,
        type_name,
        location,
    })
}
```

**Step 4: Resolve locals from crash context**

Add a function to resolve local variable values from registers and frame memory:

```rust
/// Resolve local variable values using crash context.
/// `registers`: map of register name -> hex address string
/// `frame_memory`: hex-encoded bytes read from [fp-512..fp+128]
/// `frame_base`: hex address of the frame pointer
pub fn resolve_crash_locals(
    locals: &[LocalVariableInfo],
    registers: &serde_json::Value,
    frame_memory: Option<&str>,
    frame_base: Option<&str>,
    arch: &str,
) -> Vec<serde_json::Value> {
    let fp_addr = frame_base
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0);

    let frame_bytes = frame_memory
        .map(|hex| hex_to_bytes(hex))
        .unwrap_or_default();

    // Frame memory starts at fp - 512
    let frame_start = fp_addr.saturating_sub(512);

    locals.iter().filter_map(|local| {
        let value = match &local.location {
            LocalVarLocation::FrameBaseRelative(offset) => {
                // Value at fp + offset
                let addr = (fp_addr as i64 + offset) as u64;
                read_from_frame(&frame_bytes, frame_start, addr, local.byte_size)
            }
            LocalVarLocation::Register(reg_num) => {
                let reg_name = register_name(*reg_num, arch);
                registers.get(&reg_name)
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    .map(|v| format_value(v, local.byte_size, &local.type_kind))
            }
            LocalVarLocation::RegisterOffset(reg_num, offset) => {
                let reg_name = register_name(*reg_num, arch);
                let base = registers.get(&reg_name)
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())?;
                let addr = (base as i64 + offset) as u64;
                read_from_frame(&frame_bytes, frame_start, addr, local.byte_size)
            }
            LocalVarLocation::Address(addr) => {
                // Fixed address — can't read without agent help. Skip.
                None
            }
            LocalVarLocation::Complex => None,
        };

        value.map(|v| serde_json::json!({
            "name": local.name,
            "value": v,
            "type": local.type_name,
        }))
    }).collect()
}

fn read_from_frame(frame_bytes: &[u8], frame_start: u64, addr: u64, size: u8) -> Option<String> {
    if addr < frame_start {
        return None;
    }
    let offset = (addr - frame_start) as usize;
    if offset + size as usize > frame_bytes.len() {
        return None;
    }
    let bytes = &frame_bytes[offset..offset + size as usize];
    // Read as little-endian integer
    let mut val = 0u64;
    for (i, &b) in bytes.iter().enumerate() {
        val |= (b as u64) << (i * 8);
    }
    Some(format!("0x{:x}", val))
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&hex[i..i+2], 16).ok())
        .collect()
}

fn register_name(dwarf_reg: u16, arch: &str) -> String {
    match arch {
        "arm64" => match dwarf_reg {
            0..=28 => format!("x{}", dwarf_reg),
            29 => "fp".to_string(),
            30 => "lr".to_string(),
            31 => "sp".to_string(),
            _ => format!("reg{}", dwarf_reg),
        },
        "x64" => match dwarf_reg {
            0 => "rax".to_string(),
            1 => "rdx".to_string(),
            2 => "rcx".to_string(),
            3 => "rbx".to_string(),
            4 => "rsi".to_string(),
            5 => "rdi".to_string(),
            6 => "rbp".to_string(),
            7 => "rsp".to_string(),
            8..=15 => format!("r{}", dwarf_reg),
            16 => "rip".to_string(),
            _ => format!("reg{}", dwarf_reg),
        },
        _ => format!("reg{}", dwarf_reg),
    }
}

fn format_value(raw: u64, size: u8, type_kind: &TypeKind) -> String {
    match type_kind {
        TypeKind::Integer { signed: true } => {
            match size {
                1 => format!("{}", raw as i8),
                2 => format!("{}", raw as i16),
                4 => format!("{}", raw as i32),
                8 => format!("{}", raw as i64),
                _ => format!("0x{:x}", raw),
            }
        }
        TypeKind::Integer { signed: false } => format!("{}", raw),
        TypeKind::Float => {
            match size {
                4 => format!("{}", f32::from_bits(raw as u32)),
                8 => format!("{}", f64::from_bits(raw)),
                _ => format!("0x{:x}", raw),
            }
        }
        TypeKind::Pointer => format!("0x{:x}", raw),
        TypeKind::Unknown => format!("0x{:x}", raw),
    }
}
```

**Step 5: Wire up local resolution in crash event handling**

In `src/frida_collector/spawner.rs`, when a crash event is received, we need the DWARF parser. But `parse_event` runs in the GLib callback on the Frida thread and doesn't have access to the DWARF parser.

**Approach:** Store the frame memory and register data in the crash Event. After the event is inserted into the DB, a separate task resolves locals and updates the event. This avoids blocking the message handler.

In `src/daemon/session_manager.rs`, add a method:

```rust
/// Resolve local variables for a crash event and update it in the DB.
pub async fn resolve_crash_locals(&self, session_id: &str, event_id: &str) -> Result<()> {
    // Get the crash event
    let events = self.db.query_events(session_id, |q| {
        q.event_type(crate::db::EventType::Crash).limit(1)
    })?;
    let event = events.first().ok_or_else(|| Error::Frida("Crash event not found".into()))?;

    // Get DWARF parser
    let dwarf = match self.get_dwarf(session_id).await? {
        Some(d) => d,
        None => return Ok(()),
    };

    // Get crash PC from backtrace (first frame) or fault address
    let crash_pc_str = event.backtrace.as_ref()
        .and_then(|bt| bt.as_array())
        .and_then(|frames| frames.first())
        .and_then(|f| f.get("address"))
        .and_then(|a| a.as_str())
        .or(event.fault_address.as_deref());

    let crash_pc = crash_pc_str
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok());

    if let Some(pc) = crash_pc {
        // Adjust for ASLR: pc is runtime address, DWARF uses file addresses
        // The agent sends runtime addresses. We need to subtract the ASLR slide.
        // slide = runtime_base - file_base
        // file_pc = runtime_pc - slide = runtime_pc - (runtime_base - image_base)
        // We can compute this if we know the runtime base from Process.mainModule.base
        // For now, use the PC as-is — it may not match DWARF.
        // TODO: Send process base address with crash event for ASLR adjustment.

        if let Ok(locals_info) = dwarf.parse_locals_at_pc(pc) {
            let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
            let locals = crate::dwarf::resolve_crash_locals(
                &locals_info,
                event.registers.as_ref().unwrap_or(&serde_json::Value::Null),
                // frame_memory and frame_base come from the crash event
                // but they're not stored as separate fields — they're part of the original
                // agent message. We need to either store them or compute locals in parse_event.
                None, // TODO: store frame_memory in event
                None, // TODO: store frame_base in event
                arch,
            );
            if !locals.is_empty() {
                self.db.update_event_locals(event_id, &serde_json::Value::Array(locals))?;
            }
        }
    }
    Ok(())
}
```

Add `update_event_locals` to `Database`:
```rust
pub fn update_event_locals(&self, event_id: &str, locals: &serde_json::Value) -> Result<()> {
    let conn = self.connection();
    conn.execute(
        "UPDATE events SET locals = ? WHERE id = ?",
        params![locals.to_string(), event_id],
    )?;
    Ok(())
}
```

**Alternative simpler approach:** Instead of a separate update, resolve locals inline in `parse_event` by passing a reference to the DWARF parser. Since crash events are rare, the performance impact is negligible. Store frame_memory and frame_base as additional fields in the crash Event, or resolve them immediately.

For simplicity, store `frame_memory` and `frame_base` in the Event's `text` field (JSON-encoded) and resolve locals in a background task after DB insertion. The crash event handler in the agent message handler should:
1. Parse the crash event normally
2. Stash the frame_memory/frame_base in the Event (use the `text` field or a new field)
3. After insertion, trigger local resolution asynchronously

**Checkpoint:** Crash events include local variable values (where DWARF resolution succeeds). Variables with complex locations are omitted gracefully.

---

### Task 6: Fork/Exec Following (Frida Integration)

**Files:**
- Modify: `src/frida_collector/spawner.rs` (spawn gating, child attachment)

**Step 1: Enable spawn gating**

In `frida_worker`, after setting up the output signal handler, enable spawn gating:

```rust
// Enable spawn gating to intercept child processes
unsafe {
    let device_ptr = device_raw_ptr(&device);
    let mut error: *mut frida_sys::GError = std::ptr::null_mut();
    frida_sys::frida_device_enable_spawn_gating_sync(
        device_ptr,
        std::ptr::null_mut(), // cancellable
        &mut error,
    );
    if !error.is_null() {
        let err_msg = CStr::from_ptr((*error).message).to_str().unwrap_or("?");
        tracing::warn!("Failed to enable spawn gating: {}", err_msg);
        frida_sys::g_error_free(error);
    } else {
        tracing::info!("Spawn gating enabled — will intercept child processes");
    }
}
```

**Step 2: Register "spawn-added" signal handler**

Define a callback for the "spawn-added" signal:

```rust
/// Context for handling spawned child processes.
struct SpawnContext {
    output_registry: OutputRegistry,
    /// Maps parent PID -> session state needed to set up children
    parent_sessions: Arc<Mutex<HashMap<u32, ChildSetupInfo>>>,
}

struct ChildSetupInfo {
    session_id: String,
    event_tx: mpsc::Sender<Event>,
    start_ns: i64,
}

unsafe extern "C" fn raw_on_spawn_added(
    _device: *mut frida_sys::_FridaDevice,
    spawn: *mut frida_sys::_FridaSpawn,
    user_data: *mut c_void,
) {
    let ctx = &*(user_data as *const SpawnContext);
    let child_pid = frida_sys::frida_spawn_get_pid(spawn);

    tracing::info!("Spawn detected: child PID {}", child_pid);

    // We can't do async work here (GLib callback thread).
    // Queue the child PID for the worker to handle.
    // This requires a channel from the signal handler to the worker loop.
}
```

**Revised approach:** Since the GLib callback can't do complex work, use a channel to notify the worker loop about new child processes. Add a new FridaCommand variant:

```rust
enum FridaCommand {
    // ... existing variants ...
    ChildSpawned {
        parent_pid: u32,
        child_pid: u32,
    },
}
```

Actually, the worker loop is blocked on `cmd_rx.recv()`. We need a way to wake it up when a child is spawned. Use a separate channel for spawn notifications:

```rust
// In frida_worker, create a spawn notification channel
let (spawn_tx, spawn_rx) = std::sync::mpsc::channel::<u32>();
```

Store `spawn_tx` in user_data for the "spawn-added" callback. The worker loop polls both channels:

```rust
loop {
    // Check for spawn notifications (non-blocking)
    while let Ok(child_pid) = spawn_rx.try_recv() {
        handle_child_spawn(&device, child_pid, &sessions, &output_registry);
    }

    // Wait for commands (with timeout to check spawns periodically)
    match cmd_rx.recv_timeout(std::time::Duration::from_millis(100)) {
        Ok(cmd) => { /* handle command */ }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
    }
}
```

**Step 3: Handle child process attachment**

```rust
fn handle_child_spawn(
    device: &mut frida::Device,
    child_pid: u32,
    sessions: &HashMap<String, WorkerSession>,
    output_registry: &OutputRegistry,
) {
    // Find which session this child belongs to (match by parent PID)
    let parent_session = {
        let reg = output_registry.lock().unwrap();
        // Find a session whose PID matches a known parent
        // For simplicity, check if any registered PID is the parent of this child
        // Actually, we need to track parent-child relationships.
        // Frida's spawn-added gives us the child PID but not the parent PID directly.
        // We know which PIDs are ours (in the registry), so any new spawn
        // while our sessions are running is likely a child.

        // Find the most recently added session to associate the child with
        reg.values().next().map(|ctx| (ctx.session_id.clone(), ctx.event_tx.clone(), ctx.start_ns))
    };

    let (session_id, event_tx, start_ns) = match parent_session {
        Some(info) => info,
        None => {
            tracing::debug!("No active session for child PID {}, resuming without attaching", child_pid);
            let _ = device.resume(child_pid);
            return;
        }
    };

    tracing::info!("Attaching to child process {} (session: {})", child_pid, session_id);

    // Register output context for the child
    let output_ctx = Arc::new(OutputContext {
        pid: child_pid,
        session_id: session_id.clone(),
        event_tx: event_tx.clone(),
        event_counter: AtomicU64::new(0),
        start_ns,
    });
    if let Ok(mut reg) = output_registry.lock() {
        reg.insert(child_pid, output_ctx);
    }

    // Attach to child
    match device.attach(child_pid) {
        Ok(frida_session) => {
            let raw_session = unsafe { session_raw_ptr(&frida_session) };
            std::mem::forget(frida_session);

            // Create and load agent script in child
            match unsafe { create_script_raw(raw_session, AGENT_CODE) } {
                Ok(script_ptr) => {
                    let hooks_ready: HooksReadySignal = Arc::new(Mutex::new(None));
                    let handler = AgentMessageHandler {
                        event_tx: event_tx.clone(),
                        session_id: session_id.clone(),
                        hooks_ready: hooks_ready.clone(),
                        pid: child_pid,
                    };
                    unsafe {
                        let _ = register_handler_raw(script_ptr, handler);
                        if let Err(e) = load_script_raw(script_ptr) {
                            tracing::error!("Failed to load script in child {}: {}", child_pid, e);
                            return;
                        }
                    }

                    // Initialize agent in child
                    let init_msg = serde_json::json!({
                        "type": "initialize",
                        "sessionId": session_id,
                    });
                    unsafe {
                        let _ = post_message_raw(script_ptr, &serde_json::to_string(&init_msg).unwrap());
                    }

                    tracing::info!("Agent loaded in child process {}", child_pid);
                }
                Err(e) => {
                    tracing::error!("Failed to create script in child {}: {}", child_pid, e);
                }
            }
        }
        Err(e) => {
            tracing::error!("Failed to attach to child {}: {}", child_pid, e);
        }
    }

    // Resume the child process
    let _ = device.resume(child_pid);
}
```

**Step 4: Register "spawn-added" signal**

In `frida_worker`, after enabling spawn gating:

```rust
// Set up spawn-added signal handler
let spawn_tx_clone = spawn_tx.clone();
unsafe {
    let device_ptr = device_raw_ptr(&device);
    let signal_name = CString::new("spawn-added").unwrap();

    // We need to pass spawn_tx through user_data
    let tx_ptr = Box::into_raw(Box::new(spawn_tx_clone));

    let callback = Some(std::mem::transmute::<
        *mut c_void,
        unsafe extern "C" fn(),
    >(raw_on_spawn_added as *mut c_void));

    frida_sys::g_signal_connect_data(
        device_ptr as *mut _,
        signal_name.as_ptr(),
        callback,
        tx_ptr as *mut c_void,
        None,
        0,
    );
}
```

With the callback:
```rust
unsafe extern "C" fn raw_on_spawn_added(
    _device: *mut frida_sys::_FridaDevice,
    spawn: *mut frida_sys::_FridaSpawn,
    user_data: *mut c_void,
) {
    let tx = &*(user_data as *const std::sync::mpsc::Sender<u32>);
    let child_pid = frida_sys::frida_spawn_get_pid(spawn);
    tracing::info!("Spawn signal: child PID {}", child_pid);
    let _ = tx.send(child_pid);
}
```

**Step 5: Clean up children on session stop**

In `FridaCommand::Stop` handler, kill all child PIDs associated with the session:

```rust
// Remove all PIDs for this session from output registry
if let Ok(mut reg) = output_registry.lock() {
    let pids_to_remove: Vec<u32> = reg.iter()
        .filter(|(_, ctx)| ctx.session_id == session_id)
        .map(|(&pid, _)| pid)
        .collect();
    for pid in &pids_to_remove {
        reg.remove(pid);
    }
    // Kill all associated processes
    for pid in pids_to_remove {
        let is_alive = libc::kill(pid as i32, 0) == 0;
        if is_alive {
            tracing::info!("Killing child process {}", pid);
            device.kill(pid).unwrap_or_else(|e|
                tracing::warn!("Failed to kill child PID {}: {:?}", pid, e));
        }
    }
}
```

**Checkpoint:** Child processes spawned via fork/exec are automatically instrumented. Their stdout/stderr and trace events flow into the parent session, tagged with child PID.

---

### Task 7: Multi-PID Session Management

**Files:**
- Modify: `src/daemon/session_manager.rs` (track child PIDs)
- Modify: `src/db/session.rs` (optional: store child PIDs)
- Modify: `src/daemon/server.rs` (expose PIDs in responses)

**Step 1: Track child PIDs in session manager**

In `src/daemon/session_manager.rs`, add to `SessionManager`:
```rust
/// Child PIDs per session (parent PID is in the Session struct)
child_pids: Arc<RwLock<HashMap<String, Vec<u32>>>>,
```

Initialize in `new()`:
```rust
child_pids: Arc::new(RwLock::new(HashMap::new())),
```

Add methods:
```rust
pub fn add_child_pid(&self, session_id: &str, pid: u32) {
    self.child_pids.write().unwrap()
        .entry(session_id.to_string())
        .or_default()
        .push(pid);
}

pub fn get_all_pids(&self, session_id: &str) -> Vec<u32> {
    let mut pids = vec![];
    if let Ok(Some(session)) = self.get_session(session_id) {
        pids.push(session.pid);
    }
    if let Some(children) = self.child_pids.read().unwrap().get(session_id) {
        pids.extend(children);
    }
    pids
}
```

Clean up in `stop_session`:
```rust
self.child_pids.write().unwrap().remove(id);
```

**Step 2: Expose PIDs in launch response**

In `DebugLaunchResponse`, the PID is already included. For fork/exec, the child PIDs appear dynamically. Add PIDs to query metadata:

In `DebugQueryResponse`, add:
```rust
/// All process IDs in this session (parent + children)
#[serde(skip_serializing_if = "Option::is_none")]
pub pids: Option<Vec<u32>>,
```

In `tool_debug_query`, populate:
```rust
let pids = self.session_manager.get_all_pids(&req.session_id);
let response = DebugQueryResponse {
    events: event_values,
    total_count,
    has_more,
    pids: if pids.len() > 1 { Some(pids) } else { None },
};
```

**Step 3: Notify session manager of child PIDs**

The spawn handler in the worker needs to notify the session manager. Since the worker runs on a separate thread and the session manager is on the tokio runtime, use a channel:

Add a new field to `FridaSpawner`:
```rust
child_pid_tx: Option<mpsc::Sender<(String, u32)>>,
```

When `FridaSpawner::new()` is called, optionally accept a channel for child PID notifications. The session manager sets up this channel when creating the spawner.

In `SessionManager::spawn_with_frida`, after spawning:
```rust
// Set up child PID notification channel
let child_pids = Arc::clone(&self.child_pids);
let (child_tx, mut child_rx) = mpsc::channel::<(String, u32)>(100);

tokio::spawn(async move {
    while let Some((session_id, child_pid)) = child_rx.recv().await {
        child_pids.write().unwrap()
            .entry(session_id)
            .or_default()
            .push(child_pid);
    }
});
```

**Checkpoint:** Sessions track all PIDs (parent + children). Query responses include PID list when multiple processes are involved. PID filter allows focusing on specific processes.

---

## Verification

After all tasks are complete, verify the following scenarios:

### Scenario A: Crash Debugging
```
1. debug_launch (no patterns)
2. User triggers a crash (null pointer dereference, etc.)
3. debug_query({ eventType: "crash" })
   → Returns: signal, faultAddress, registers, backtrace, locals
4. debug_query({ eventType: "stderr" })
   → Returns: any ASAN/crash output
5. LLM has full crash context to identify root cause
```

### Scenario B: Multi-Process Tracking
```
1. debug_launch (app that forks workers)
2. debug_query({ eventType: "stdout" })
   → Events from parent AND children, each tagged with pid
3. debug_query({ pid: <child_pid> })
   → Only events from that specific child
4. Response includes pids: [parent, child1, child2, ...]
```

### Scenario C: Enhanced Queries
```
1. debug_launch + debug_trace (add patterns)
2. debug_query({ timeFrom: "-5s" })
   → Only events from last 5 seconds
3. debug_query({ minDurationNs: 1000000 })
   → Only functions that took >1ms
4. debug_query({ timeFrom: "-10s", timeTo: "-5s", function: { contains: "process" } })
   → Combined filters
```

---

### Task 8: Write stress_test_phase1c Binary

**Files:**
- Create: `tests/stress_test_phase1c/main.c`
- Create: `tests/stress_test_phase1c/Makefile`

A C program (not Rust — fork/exec, crash signals, and DWARF locals are more natural in C). Compiled with `-g -O0` for full debug symbols, no optimization (preserves locals on stack).

**Design:** The binary runs in one of several modes selected by CLI argument:

```
./stress_test_phase1c crash-null      # NULL pointer dereference (SIGSEGV)
./stress_test_phase1c crash-abort     # abort() (SIGABRT)
./stress_test_phase1c crash-stack     # Stack overflow (deep recursion)
./stress_test_phase1c fork-workers    # Fork N child processes that do work
./stress_test_phase1c fork-exec       # Fork + exec a child command
./stress_test_phase1c slow-functions  # Functions with varied durations (1ms-500ms)
./stress_test_phase1c mixed           # All of the above in sequence (default)
```

**Step 1: Crash scenarios with local variables**

```c
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <signal.h>
#include <sys/wait.h>

// Global variables (for watch variable testing)
static int g_crash_count = 0;
static float g_temperature = 98.6f;
static const char* g_app_state = "running";

// ========== Crash Scenarios ==========

// Has interesting locals for DWARF resolution testing
void crash_null_deref(void) {
    int local_counter = 42;
    float local_ratio = 3.14159f;
    char local_buffer[64];
    strcpy(local_buffer, "about to crash");
    int* ptr = NULL;

    // These locals should be visible in crash frame:
    // local_counter=42, local_ratio=3.14159, local_buffer="about to crash"
    printf("[CRASH] About to dereference NULL (counter=%d, ratio=%.2f)\n",
           local_counter, local_ratio);
    fflush(stdout);

    g_crash_count++;
    *ptr = local_counter;  // SIGSEGV
}

void crash_abort(void) {
    int error_code = -1;
    const char* reason = "intentional abort for testing";

    printf("[CRASH] About to abort (error_code=%d, reason=%s)\n",
           error_code, reason);
    fflush(stdout);

    g_crash_count++;
    abort();  // SIGABRT
}

static int recurse_depth = 0;

void crash_stack_overflow(int depth) {
    char frame_padding[4096];  // Eat stack space
    memset(frame_padding, depth & 0xFF, sizeof(frame_padding));
    recurse_depth = depth;

    if (depth % 100 == 0) {
        printf("[CRASH] Recursion depth: %d\n", depth);
        fflush(stdout);
    }

    crash_stack_overflow(depth + 1);  // Eventually SIGSEGV (stack overflow)
}
```

**Step 2: Fork/exec scenarios**

```c
// ========== Fork/Exec Scenarios ==========

void do_child_work(int child_id, int iterations) {
    printf("[CHILD %d] PID=%d started, doing %d iterations\n",
           child_id, getpid(), iterations);

    for (int i = 0; i < iterations; i++) {
        // Simulate work with varied durations
        volatile double result = 0;
        for (int j = 0; j < (child_id + 1) * 10000; j++) {
            result += j * 0.001;
        }

        if (i % 10 == 0) {
            printf("[CHILD %d] iteration %d/%d (result=%.2f)\n",
                   child_id, i, iterations, result);
        }
    }

    printf("[CHILD %d] PID=%d finished\n", child_id, getpid());
}

void fork_workers(int num_workers) {
    printf("[PARENT] PID=%d forking %d workers\n", getpid(), num_workers);

    pid_t children[16];
    int n = num_workers < 16 ? num_workers : 16;

    for (int i = 0; i < n; i++) {
        pid_t pid = fork();
        if (pid == 0) {
            // Child process
            do_child_work(i, 50);
            _exit(0);
        } else if (pid > 0) {
            children[i] = pid;
            printf("[PARENT] Forked child %d with PID %d\n", i, pid);
        } else {
            perror("fork");
        }
    }

    // Wait for all children
    for (int i = 0; i < n; i++) {
        int status;
        waitpid(children[i], &status, 0);
        printf("[PARENT] Child %d (PID %d) exited with status %d\n",
               i, children[i], WEXITSTATUS(status));
    }
}

void fork_exec(void) {
    printf("[PARENT] PID=%d forking + exec\n", getpid());

    pid_t pid = fork();
    if (pid == 0) {
        // Child: exec a simple command
        execlp("echo", "echo", "Hello from child process!", NULL);
        perror("exec failed");
        _exit(1);
    } else if (pid > 0) {
        int status;
        waitpid(pid, &status, 0);
        printf("[PARENT] Exec child (PID %d) exited with status %d\n",
               pid, WEXITSTATUS(status));
    }
}
```

**Step 3: Functions with varied durations**

```c
// ========== Slow Functions (for duration query testing) ==========

// Each function has a different, known duration
void fast_function(void) {
    // ~0 ns — just a counter increment
    volatile int x = 0;
    for (int i = 0; i < 100; i++) x += i;
}

void medium_function(void) {
    // ~1-5ms
    volatile double result = 0;
    for (int i = 0; i < 100000; i++) {
        result += i * 0.001;
    }
    printf("[TIMING] medium_function result=%.2f\n", result);
}

void slow_function(void) {
    // ~50ms
    usleep(50000);
    printf("[TIMING] slow_function done\n");
}

void very_slow_function(void) {
    // ~500ms
    usleep(500000);
    printf("[TIMING] very_slow_function done\n");
}

void run_slow_functions(void) {
    printf("[TIMING] Running functions with varied durations...\n");

    for (int round = 0; round < 5; round++) {
        fast_function();
        fast_function();
        fast_function();
        medium_function();
        slow_function();
        if (round == 2) very_slow_function();
    }

    printf("[TIMING] Done\n");
}
```

**Step 4: Main dispatch**

```c
int main(int argc, char* argv[]) {
    const char* mode = (argc > 1) ? argv[1] : "mixed";

    printf("[STRESS TEST 1C] PID=%d mode=%s\n", getpid(), mode);

    if (strcmp(mode, "crash-null") == 0) {
        crash_null_deref();
    } else if (strcmp(mode, "crash-abort") == 0) {
        crash_abort();
    } else if (strcmp(mode, "crash-stack") == 0) {
        crash_stack_overflow(0);
    } else if (strcmp(mode, "fork-workers") == 0) {
        fork_workers(3);
    } else if (strcmp(mode, "fork-exec") == 0) {
        fork_exec();
    } else if (strcmp(mode, "slow-functions") == 0) {
        run_slow_functions();
    } else if (strcmp(mode, "mixed") == 0) {
        // Non-crashing scenarios first
        run_slow_functions();
        fork_workers(2);
        fork_exec();
        // Crash last (terminates process)
        crash_null_deref();
    } else {
        fprintf(stderr, "Unknown mode: %s\n", mode);
        fprintf(stderr, "Usage: %s [crash-null|crash-abort|crash-stack|fork-workers|fork-exec|slow-functions|mixed]\n", argv[0]);
        return 1;
    }

    return 0;
}
```

**Step 5: Makefile**

```makefile
CC = clang
CFLAGS = -g -O0 -Wall -Wextra
TARGET = stress_test_phase1c

all: $(TARGET)

$(TARGET): main.c
	$(CC) $(CFLAGS) -o $@ $<

clean:
	rm -f $(TARGET) $(TARGET).dSYM

.PHONY: all clean
```

**Verification matrix:**

| Mode | Tests Feature | Expected Strobe Behavior |
|------|--------------|-------------------------|
| `crash-null` | Crash capture + locals | Crash event with signal=SIGSEGV, registers, backtrace, locals (local_counter=42, local_ratio=3.14) |
| `crash-abort` | Crash capture | Crash event with signal=SIGABRT |
| `crash-stack` | Crash capture (edge case) | Crash event with signal=SIGSEGV (stack overflow), deep backtrace |
| `fork-workers` | Fork following | 3 child PIDs appear, stdout from children tagged with child PID |
| `fork-exec` | Exec following | Child PID spawned, "Hello from child process!" in stdout |
| `slow-functions` | Duration queries | `debug_query({ minDurationNs: 50000000 })` finds slow_function and very_slow_function |
| `mixed` | All of the above | Full integration test |

**Checkpoint:** A purpose-built C binary exercises all Phase 1c features. Each mode can be run independently for focused testing, or `mixed` mode validates the full integration.

---

## Build & Test

```bash
# Build stress test
cd tests/stress_test_phase1c && make && cd ../..

# Build agent
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs

# Build daemon
cargo build

# Run tests
cargo test

# Integration test with stress_test_phase1c
# 1. Crash capture:
#    debug_launch({ command: "tests/stress_test_phase1c/stress_test_phase1c", args: ["crash-null"] })
#    debug_query({ eventType: "crash" })
#
# 2. Fork following:
#    debug_launch({ command: "tests/stress_test_phase1c/stress_test_phase1c", args: ["fork-workers"] })
#    debug_query({ eventType: "stdout" })  -- should see events from multiple PIDs
#
# 3. Duration queries:
#    debug_launch({ command: "tests/stress_test_phase1c/stress_test_phase1c", args: ["slow-functions"] })
#    debug_trace({ add: ["slow_function", "very_slow_function", "medium_function", "fast_function"] })
#    debug_query({ minDurationNs: 50000000 })
```
