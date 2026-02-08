# Live Memory Reads (`debug_read`) Implementation Plan

**Spec:** `docs/specs/2026-02-08-phase-1e-live-memory-reads.md`
**Goal:** Add on-demand memory reads from running processes via `debug_read` MCP tool — one-shot reads, struct expansion, polling with timeline integration.
**Architecture:** Host resolves DWARF → flat read recipes → agent reads memory at computed addresses (with ASLR slide). Poll mode uses `setInterval` in agent, streaming `variable_snapshot` events through existing event pipeline.
**Tech Stack:** Rust (daemon/types/DWARF), TypeScript (Frida agent), SQLite (event storage)
**Commit strategy:** Single commit at end

## Workstreams

- **Stream A (Types + DWARF):** Tasks 1, 2 — no Frida/agent dependencies
- **Stream B (Agent):** Task 3 — no Rust dependencies beyond message protocol
- **Serial:** Tasks 4, 5, 6, 7 — depend on A and B

---

### Task 1: MCP Types — `DebugReadRequest` and `DebugReadResponse`

**Files:**
- Modify: `src/mcp/types.rs`

**Step 1: Write the failing test**

Add at the bottom of `src/mcp/types.rs`, before the closing of the file:

```rust
#[cfg(test)]
mod read_tests {
    use super::*;

    #[test]
    fn test_debug_read_request_validation_empty_targets() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_too_many_targets() {
        let targets: Vec<ReadTarget> = (0..17).map(|i| ReadTarget {
            variable: Some(format!("var{}", i)),
            address: None,
            size: None,
            type_hint: None,
        }).collect();
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets,
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_valid() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_debug_read_request_validation_poll_limits() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: None,
            poll: Some(PollConfig {
                interval_ms: 10,  // below min 50
                duration_ms: 2000,
            }),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_validation_depth_limits() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: Some("gTempo".to_string()),
                address: None,
                size: None,
                type_hint: None,
            }],
            depth: Some(10),  // above max 5
            poll: None,
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_debug_read_request_raw_address_requires_size_and_type() {
        let req = DebugReadRequest {
            session_id: "s1".to_string(),
            targets: vec![ReadTarget {
                variable: None,
                address: Some("0x7ff800".to_string()),
                size: None,  // missing
                type_hint: None,  // missing
            }],
            depth: None,
            poll: None,
        };
        assert!(req.validate().is_err());
    }
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib mcp::types::read_tests -- --no-capture`
Expected: FAIL — types don't exist yet

**Step 3: Write minimal implementation**

Add to `src/mcp/types.rs` after the `DebugStopResponse` section (around line 294):

```rust
// ============ debug_read ============

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTarget {
    /// DWARF variable name or pointer chain (e.g. "gClock->counter")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    /// Raw hex address (e.g. "0x7ff800")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Size in bytes (required for raw address reads)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// Type hint for raw address reads: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer/bytes
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PollConfig {
    pub interval_ms: u32,
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadRequest {
    pub session_id: String,
    pub targets: Vec<ReadTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll: Option<PollConfig>,
}

// Validation limits for debug_read
pub const MAX_READ_TARGETS: usize = 16;
pub const MAX_READ_DEPTH: u32 = 5;
pub const MIN_POLL_INTERVAL_MS: u32 = 50;
pub const MAX_POLL_INTERVAL_MS: u32 = 5000;
pub const MIN_POLL_DURATION_MS: u32 = 100;
pub const MAX_POLL_DURATION_MS: u32 = 30000;
pub const MAX_RAW_READ_SIZE: u32 = 65536;
const VALID_TYPE_HINTS: &[&str] = &[
    "i8", "u8", "i16", "u16", "i32", "u32", "i64", "u64",
    "f32", "f64", "pointer", "bytes",
];

impl DebugReadRequest {
    pub fn validate(&self) -> crate::Result<()> {
        if self.targets.is_empty() {
            return Err(crate::Error::ValidationError(
                "targets must not be empty".to_string()
            ));
        }
        if self.targets.len() > MAX_READ_TARGETS {
            return Err(crate::Error::ValidationError(
                format!("Too many targets ({}, max {})", self.targets.len(), MAX_READ_TARGETS)
            ));
        }
        if let Some(depth) = self.depth {
            if depth < 1 || depth > MAX_READ_DEPTH {
                return Err(crate::Error::ValidationError(
                    format!("depth must be between 1 and {}", MAX_READ_DEPTH)
                ));
            }
        }
        if let Some(ref poll) = self.poll {
            if poll.interval_ms < MIN_POLL_INTERVAL_MS || poll.interval_ms > MAX_POLL_INTERVAL_MS {
                return Err(crate::Error::ValidationError(
                    format!("poll.intervalMs must be between {} and {}", MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS)
                ));
            }
            if poll.duration_ms < MIN_POLL_DURATION_MS || poll.duration_ms > MAX_POLL_DURATION_MS {
                return Err(crate::Error::ValidationError(
                    format!("poll.durationMs must be between {} and {}", MIN_POLL_DURATION_MS, MAX_POLL_DURATION_MS)
                ));
            }
        }
        for target in &self.targets {
            if target.variable.is_none() && target.address.is_none() {
                return Err(crate::Error::ValidationError(
                    "Each target must have either 'variable' or 'address'".to_string()
                ));
            }
            if target.address.is_some() {
                if target.size.is_none() || target.type_hint.is_none() {
                    return Err(crate::Error::ValidationError(
                        "Raw address targets require 'size' and 'type'".to_string()
                    ));
                }
                if let Some(size) = target.size {
                    if size == 0 || size > MAX_RAW_READ_SIZE {
                        return Err(crate::Error::ValidationError(
                            format!("size must be between 1 and {}", MAX_RAW_READ_SIZE)
                        ));
                    }
                }
                if let Some(ref type_hint) = target.type_hint {
                    if !VALID_TYPE_HINTS.contains(&type_hint.as_str()) {
                        return Err(crate::Error::ValidationError(
                            format!("Invalid type '{}'. Valid: {}", type_hint, VALID_TYPE_HINTS.join(", "))
                        ));
                    }
                }
            }
            if let Some(ref var) = target.variable {
                validate_watch_field(var, "variable")?;
            }
        }
        Ok(())
    }
}

/// A single read result in the debug_read response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// File path for bytes-type reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Hex preview for bytes-type reads
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

/// Response for one-shot debug_read
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadResponse {
    pub results: Vec<ReadResult>,
}

/// Response for poll-mode debug_read
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugReadPollResponse {
    pub polling: bool,
    pub variable_count: usize,
    pub interval_ms: u32,
    pub duration_ms: u32,
    pub expected_samples: u32,
    pub event_type: String,
    pub hint: String,
}
```

Also add `VariableSnapshot` to the `EventTypeFilter` enum (around line 194):

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
}
```

And add `ReadFailed` to the `ErrorCode` enum:

```rust
ReadFailed,
```

**Step 4: Run test — verify it passes**
Run: `cargo test --lib mcp::types::read_tests -- --no-capture`
Expected: PASS

**Checkpoint:** Types compile and validation works. No runtime behavior yet.

---

### Task 2: DWARF — Struct Expansion (`expand_struct`)

**Files:**
- Modify: `src/dwarf/parser.rs`
- Modify: `src/dwarf/function.rs` (add `StructFieldRecipe`)

**Step 1: Write the failing test**

Add to `src/dwarf/function.rs`:

```rust
/// A flat recipe for reading a struct field at runtime.
/// Used by debug_read to send field layouts to the agent.
#[derive(Debug, Clone)]
pub struct StructFieldRecipe {
    pub name: String,
    pub offset: u64,
    pub size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
    /// True if this field is itself a struct beyond the depth limit
    pub is_truncated_struct: bool,
}
```

Add test in `src/dwarf/parser.rs` (in the existing `pattern_tests` module or a new module):

```rust
#[cfg(test)]
mod struct_expansion_tests {
    use super::*;
    use crate::dwarf::{TypeKind, StructFieldRecipe};

    #[test]
    fn test_expand_struct_from_members() {
        // Unit test: given a set of StructMembers, verify expand_struct produces correct flat recipes
        let members = vec![
            StructMember {
                name: "size".to_string(),
                offset: 0,
                byte_size: 4,
                type_kind: TypeKind::Integer { signed: false },
                type_name: Some("uint32_t".to_string()),
                is_pointer: false,
                pointed_struct_members: None,
            },
            StructMember {
                name: "data".to_string(),
                offset: 8,
                byte_size: 8,
                type_kind: TypeKind::Pointer,
                type_name: Some("pointer".to_string()),
                is_pointer: true,
                pointed_struct_members: None,
            },
        ];

        let recipes = DwarfParser::struct_members_to_recipes(&members, 1);
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0].name, "size");
        assert_eq!(recipes[0].offset, 0);
        assert_eq!(recipes[0].size, 4);
        assert_eq!(recipes[1].name, "data");
        assert_eq!(recipes[1].offset, 8);
        assert!(!recipes[0].is_truncated_struct);
    }
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test --lib dwarf::parser::struct_expansion_tests -- --no-capture`
Expected: FAIL — `StructFieldRecipe` and `struct_members_to_recipes` don't exist yet

**Step 3: Write minimal implementation**

Add the `StructFieldRecipe` type (shown above) to `src/dwarf/function.rs` and add `pub use function::StructFieldRecipe;` to `src/dwarf/mod.rs`.

Add to `src/dwarf/parser.rs`, as a public method on `DwarfParser`:

```rust
/// Convert cached StructMembers to flat field recipes for the agent.
/// This is a pure transformation — no DWARF re-parsing needed.
pub fn struct_members_to_recipes(members: &[StructMember], depth: usize) -> Vec<StructFieldRecipe> {
    members.iter().map(|m| {
        // If this field is a struct (not a primitive, not a pointer) and we're at depth limit,
        // mark it as truncated
        let is_struct_field = !matches!(m.type_kind, TypeKind::Integer { .. } | TypeKind::Float | TypeKind::Pointer);
        let is_truncated = is_struct_field && depth <= 1;

        StructFieldRecipe {
            name: m.name.clone(),
            offset: m.offset,
            size: m.byte_size,
            type_kind: m.type_kind.clone(),
            type_name: m.type_name.clone(),
            is_truncated_struct: is_truncated,
        }
    }).collect()
}

/// Resolve a variable to a read recipe, optionally expanding struct fields.
/// Returns the WatchRecipe for the variable plus optional struct field recipes.
/// This reuses existing resolve_watch_expression for the base recipe.
pub fn resolve_read_target(
    &self,
    variable: &str,
    depth: u32,
) -> Result<(WatchRecipe, Option<Vec<StructFieldRecipe>>)> {
    let recipe = self.resolve_watch_expression(variable)?;

    // For pointer variables with struct members, expand if depth > 0
    if matches!(recipe.type_kind, TypeKind::Pointer) && recipe.deref_chain.is_empty() {
        // Try to lazily resolve struct members
        if self.lazy_resolve_struct_members(variable).is_ok() {
            let cache = self.struct_members.lock().unwrap();
            if let Some(members) = cache.get(variable) {
                let field_recipes = Self::struct_members_to_recipes(members, depth as usize);
                return Ok((recipe, Some(field_recipes)));
            }
        }
    }

    Ok((recipe, None))
}
```

**Step 4: Run test — verify it passes**
Run: `cargo test --lib dwarf::parser::struct_expansion_tests -- --no-capture`
Expected: PASS

**Checkpoint:** DWARF can produce read recipes with optional struct field expansion.

---

### Task 3: Agent — `read_memory` Message Handler

**Files:**
- Modify: `agent/src/agent.ts`

**Step 1: Define the agent-side protocol**

Add to `agent/src/agent.ts`, new interfaces and handler:

```typescript
interface ReadRecipe {
  label: string;
  address: string;  // hex
  size: number;
  typeKind: string;  // "int", "uint", "float", "pointer", "bytes"
  derefDepth: number;
  derefOffset: number;
  struct?: boolean;
  fields?: Array<{
    name: string;
    offset: number;
    size: number;
    typeKind: string;
    typeName?: string;
    isTruncatedStruct?: boolean;
  }>;
}

interface ReadMemoryMessage {
  recipes: ReadRecipe[];
  poll?: {
    intervalMs: number;
    durationMs: number;
  };
}
```

**Step 2: Implement `handleReadMemory` in `StrobeAgent`**

Add these methods to the `StrobeAgent` class:

```typescript
handleReadMemory(message: ReadMemoryMessage): void {
  const slide = this.tracer.getSlide();

  if (message.poll) {
    // Poll mode: start interval, return immediately
    this.startReadPoll(message.recipes, slide, message.poll);
    return;
  }

  // One-shot mode: read all targets, send response
  const results = message.recipes.map(recipe => this.readSingleTarget(recipe, slide));
  send({ type: 'read_response', results });
}

private readSingleTarget(recipe: ReadRecipe, slide: NativePointer): any {
  try {
    const baseAddr = ptr(recipe.address).add(slide);

    // Handle struct reads
    if (recipe.struct && recipe.fields) {
      // Dereference the pointer first
      const structPtr = baseAddr.readPointer();
      if (structPtr.isNull()) {
        return { label: recipe.label, error: `Null pointer at ${recipe.label}` };
      }
      const fields: Record<string, any> = {};
      for (const field of recipe.fields) {
        if (field.isTruncatedStruct) {
          fields[field.name] = '<struct>';
          continue;
        }
        try {
          const fieldAddr = structPtr.add(field.offset);
          fields[field.name] = this.readTypedValue(fieldAddr, field.size, field.typeKind);
        } catch (e: any) {
          fields[field.name] = `<error: ${e.message}>`;
        }
      }
      return { label: recipe.label, fields };
    }

    // Handle deref chain (e.g. gClock->counter)
    if (recipe.derefDepth > 0) {
      const ptrVal = baseAddr.readPointer();
      if (ptrVal.isNull()) {
        return { label: recipe.label, error: `Null pointer at ${recipe.label.split('->')[0]}` };
      }
      const finalAddr = ptrVal.add(recipe.derefOffset);
      const value = this.readTypedValue(finalAddr, recipe.size, recipe.typeKind);
      return { label: recipe.label, value };
    }

    // Simple direct read
    if (recipe.typeKind === 'bytes') {
      const bytes = baseAddr.readByteArray(recipe.size);
      if (!bytes) return { label: recipe.label, error: 'Failed to read bytes' };
      return { label: recipe.label, value: _arrayBufferToHex(bytes), isBytes: true };
    }

    const value = this.readTypedValue(baseAddr, recipe.size, recipe.typeKind);
    return { label: recipe.label, value };
  } catch (e: any) {
    return { label: recipe.label, error: `Address not readable: ${e.message}` };
  }
}

private readTypedValue(addr: NativePointer, size: number, typeKind: string): any {
  // Validate address is readable
  const range = Process.findRangeByAddress(addr);
  if (!range || !range.protection.includes('r')) {
    throw new Error(`Address ${addr} not readable`);
  }

  switch (typeKind) {
    case 'float':
      return size === 4 ? addr.readFloat() : addr.readDouble();
    case 'int':
      switch (size) {
        case 1: return addr.readS8();
        case 2: return addr.readS16();
        case 4: return addr.readS32();
        case 8: return addr.readS64().toNumber();
        default: return addr.readS32();
      }
    case 'uint':
      switch (size) {
        case 1: return addr.readU8();
        case 2: return addr.readU16();
        case 4: return addr.readU32();
        case 8: return addr.readU64().toNumber();
        default: return addr.readU32();
      }
    case 'pointer':
      return addr.readPointer().toString();
    default:
      return addr.readU64().toNumber();
  }
}

private startReadPoll(
  recipes: ReadRecipe[],
  slide: NativePointer,
  poll: { intervalMs: number; durationMs: number }
): void {
  const startTime = Date.now();
  let sampleCount = 0;

  const timer = setInterval(() => {
    const elapsed = Date.now() - startTime;
    if (elapsed >= poll.durationMs) {
      clearInterval(timer);
      send({ type: 'poll_complete', sampleCount });
      return;
    }

    // Read all targets and combine into single snapshot
    const data: Record<string, any> = {};
    for (const recipe of recipes) {
      const result = this.readSingleTarget(recipe, slide);
      if (result.error) {
        data[recipe.label] = `<error: ${result.error}>`;
      } else if (result.fields) {
        data[recipe.label] = result.fields;
      } else {
        data[recipe.label] = result.value;
      }
    }

    sampleCount++;
    send({
      type: 'events',
      events: [{
        id: `${this.sessionId}-snap-${sampleCount}`,
        timestampNs: this.getTimestampNs(),
        threadId: Process.getCurrentThreadId(),
        eventType: 'variable_snapshot',
        data,
      }],
    });
  }, poll.intervalMs);
}
```

**Step 3: Register the message handler**

Add at the bottom of `agent/src/agent.ts`, next to the existing `recv('hooks', ...)` and `recv('watches', ...)`:

```typescript
function onReadMemoryMessage(message: ReadMemoryMessage): void {
  recv('read_memory', onReadMemoryMessage);
  agent.handleReadMemory(message);
}
recv('read_memory', onReadMemoryMessage);
```

**Step 4: Add `getSlide()` to CModuleTracer**

The `CModuleTracer` already computes the ASLR slide in `setImageBase()`. Expose it:

In `agent/src/cmodule-tracer.ts`, add a getter:

```typescript
getSlide(): NativePointer {
  return this.slide;
}
```

(Verify the `slide` field exists and is set in `setImageBase`. If it's private, just add the public getter.)

**Step 5: Build agent**
Run: `cd agent && npm run build && cd .. && touch src/frida_collector/spawner.rs`

**Checkpoint:** Agent can handle `read_memory` messages with one-shot, struct, deref, and poll modes.

---

### Task 4: Spawner — `ReadMemory` Session Command

**Files:**
- Modify: `src/frida_collector/spawner.rs`

**Step 1: Add the ReadMemory command variant**

Add to the `SessionCommand` enum (around line 400):

```rust
ReadMemory {
    recipes_json: String,
    response: oneshot::Sender<Result<serde_json::Value>>,
},
```

**Step 2: Handle ReadMemory in session_worker**

In the `session_worker` function's match (around line 717), add:

```rust
SessionCommand::ReadMemory { recipes_json, response } => {
    let result = handle_read_memory(raw_ptr, &hooks_ready, &session_id, &recipes_json);
    let _ = response.send(result);
}
```

**Step 3: Implement `handle_read_memory`**

Add new function:

```rust
/// Handle ReadMemory on a session worker thread.
fn handle_read_memory(
    script_ptr: *mut frida_sys::_FridaScript,
    hooks_ready: &HooksReadySignal,
    session_id: &str,
    recipes_json: &str,
) -> Result<serde_json::Value> {
    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = hooks_ready.lock().unwrap();
        *guard = Some(signal_tx);
    }

    unsafe {
        post_message_raw(script_ptr, recipes_json)
            .map_err(|e| crate::Error::Frida(format!("Failed to send read_memory: {}", e)))?;
    }

    // For one-shot reads, wait for response (5s timeout)
    // For poll, return immediately (handled via events pipeline)
    let msg: serde_json::Value = serde_json::from_str(recipes_json)
        .map_err(|e| crate::Error::Frida(format!("Invalid recipes JSON: {}", e)))?;

    if msg.get("poll").is_some() {
        // Poll mode — return immediately, events stream through normal pipeline
        return Ok(serde_json::json!({ "polling": true }));
    }

    // One-shot — wait for read_response via hooks_ready signal
    // We repurpose the signal: agent sends read_response, message handler
    // captures it and signals the worker
    match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(_) => {
            // The actual response is captured by the message handler
            // We need a different mechanism — use a dedicated channel
            Ok(serde_json::json!({ "completed": true }))
        }
        Err(_) => Err(crate::Error::Frida("Memory read timed out (5s)".to_string())),
    }
}
```

**Actually — the hooks_ready signal is insufficient for read_response since it only carries a u64 count.** We need a dedicated response channel for read results. Let me revise:

**Step 3 (revised): Add read_response channel to AgentMessageHandler and SessionCommand**

Add a new field to the message handler context. The cleanest approach: add a `read_response` oneshot channel that the message handler fills when it receives a `read_response` message.

In the `SessionCommand::ReadMemory` variant, include a response oneshot. The session worker creates a `std::sync::mpsc::channel` for read responses, stores the sender in a shared slot (like `hooks_ready`), and the message handler fills it.

Add a new shared signal type:

```rust
type ReadResponseSignal = Arc<Mutex<Option<std::sync::mpsc::Sender<serde_json::Value>>>>;
```

Add `read_response: ReadResponseSignal` to `AgentMessageHandler` struct. In `handle_payload`, add:

```rust
"read_response" => {
    if let Ok(mut guard) = self.read_response.lock() {
        if let Some(tx) = guard.take() {
            let _ = tx.send(payload.clone());
        }
    }
}
```

Update `handle_read_memory` to use this channel:

```rust
fn handle_read_memory(
    script_ptr: *mut frida_sys::_FridaScript,
    read_response: &ReadResponseSignal,
    recipes_json: &str,
) -> Result<serde_json::Value> {
    let msg: serde_json::Value = serde_json::from_str(recipes_json)
        .map_err(|e| crate::Error::Frida(format!("Invalid recipes JSON: {}", e)))?;

    if msg.get("poll").is_some() {
        // Poll mode — send message, return immediately
        unsafe {
            post_message_raw(script_ptr, recipes_json)
                .map_err(|e| crate::Error::Frida(format!("Failed to send read_memory: {}", e)))?;
        }
        return Ok(serde_json::json!({ "polling": true }));
    }

    // One-shot: arm the response channel, send message, wait
    let (signal_tx, signal_rx) = std::sync::mpsc::channel();
    {
        let mut guard = read_response.lock().unwrap();
        *guard = Some(signal_tx);
    }

    unsafe {
        post_message_raw(script_ptr, recipes_json)
            .map_err(|e| crate::Error::Frida(format!("Failed to send read_memory: {}", e)))?;
    }

    match signal_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(response) => Ok(response),
        Err(_) => Err(crate::Error::Frida("Memory read timed out (5s)".to_string())),
    }
}
```

Update `session_worker` signature to receive `read_response: ReadResponseSignal`, and thread it through from the spawn site.

**Step 4: Add `read_memory` method to `FridaSpawner`**

```rust
pub async fn read_memory(&self, session_id: &str, recipes_json: String) -> Result<serde_json::Value> {
    let (response_tx, response_rx) = oneshot::channel();

    let worker_tx = self.session_workers.get(session_id)
        .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

    worker_tx.send(SessionCommand::ReadMemory {
        recipes_json,
        response: response_tx,
    }).map_err(|_| crate::Error::Frida("Session worker died".to_string()))?;

    response_rx.await
        .map_err(|_| crate::Error::Frida("Session worker response lost".to_string()))?
}
```

**Checkpoint:** Spawner can relay read_memory commands to agent and receive one-shot responses.

---

### Task 5: Session Manager — `read_memory` Forwarding

**Files:**
- Modify: `src/daemon/session_manager.rs`

Add method to `SessionManager`:

```rust
/// Send a read_memory command to the Frida agent and return the response.
pub async fn read_memory(
    &self,
    session_id: &str,
    recipes_json: String,
) -> Result<serde_json::Value> {
    let mut guard = self.frida_spawner.write().await;
    let spawner = guard.as_mut()
        .ok_or_else(|| crate::Error::Frida("No Frida spawner available".to_string()))?;

    spawner.read_memory(session_id, recipes_json).await
}
```

**Checkpoint:** Session manager can relay reads to the spawner.

---

### Task 6: Event Pipeline — `variable_snapshot` Event Type

**Files:**
- Modify: `src/db/event.rs` — add `VariableSnapshot` variant to `EventType`
- Modify: `src/frida_collector/spawner.rs` — handle `variable_snapshot` in `parse_event`
- Modify: `src/daemon/server.rs` — handle `VariableSnapshot` in query filter and `format_event`

**Step 1: Add event type variant**

In `src/db/event.rs`, add `VariableSnapshot` to the `EventType` enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    FunctionEnter,
    FunctionExit,
    Stdout,
    Stderr,
    Crash,
    VariableSnapshot,
}
```

Update `as_str()`:
```rust
Self::VariableSnapshot => "variable_snapshot",
```

Update `from_str()`:
```rust
"variable_snapshot" => Some(Self::VariableSnapshot),
```

**Step 2: Handle in parse_event**

In `src/frida_collector/spawner.rs`, update the `parse_event` function to handle `variable_snapshot`:

Add to the match in `parse_event` (around line 1014):

```rust
"variable_snapshot" => EventType::VariableSnapshot,
```

And add a branch before the function_enter/exit handling:

```rust
if event_type == EventType::VariableSnapshot {
    return Some(Event {
        id: json.get("id").and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}-snap-{}", session_id, chrono::Utc::now().timestamp_millis())),
        session_id: session_id.to_string(),
        timestamp_ns: json.get("timestampNs")?.as_i64()?,
        thread_id: json.get("threadId")?.as_i64()?,
        event_type,
        // Store snapshot data in the arguments field (JSON object)
        arguments: json.get("data").cloned(),
        pid,
        ..Event::default()
    });
}
```

**Step 3: Handle in server.rs query and format_event**

In `format_event` (around line 33), add a branch for VariableSnapshot:

```rust
if event.event_type == crate::db::EventType::VariableSnapshot {
    return serde_json::json!({
        "id": event.id,
        "timestamp_ns": event.timestamp_ns,
        "eventType": "variable_snapshot",
        "threadId": event.thread_id,
        "pid": event.pid,
        "data": event.arguments,  // snapshot data stored in arguments column
    });
}
```

In the `tool_debug_query` method, update the `EventTypeFilter` match (around line 1198):

```rust
EventTypeFilter::VariableSnapshot => crate::db::EventType::VariableSnapshot,
```

**Checkpoint:** `variable_snapshot` events flow through the entire pipeline and are queryable.

---

### Task 7: Server — `tool_debug_read` Dispatch and Recipe Building

**Files:**
- Modify: `src/daemon/server.rs`
- Modify: `src/error.rs` (add `ReadFailed` variant)

**Step 1: Add error variant**

In `src/error.rs`, add:

```rust
#[error("READ_FAILED: {0}")]
ReadFailed(String),
```

In `src/mcp/types.rs`, update the `From<crate::Error> for McpError` impl:

```rust
crate::Error::ReadFailed(_) => ErrorCode::ReadFailed,
```

**Step 2: Register the tool in `handle_tools_list`**

Add to the `tools` vec in `handle_tools_list` (around line 663):

```rust
McpTool {
    name: "debug_read".to_string(),
    description: "Read memory from a running process. Supports DWARF-resolved variables, pointer chains, struct expansion, raw addresses, and polling mode for timeline integration.".to_string(),
    input_schema: serde_json::json!({
        "type": "object",
        "properties": {
            "sessionId": { "type": "string" },
            "targets": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "variable": { "type": "string", "description": "Variable name or pointer chain (e.g. 'gClock->counter')" },
                        "address": { "type": "string", "description": "Hex address for raw memory reads" },
                        "size": { "type": "integer", "description": "Size in bytes (required for raw address)" },
                        "type": { "type": "string", "description": "Type: i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer/bytes" }
                    }
                },
                "description": "1-16 read targets"
            },
            "depth": { "type": "integer", "description": "Struct traversal depth (default 1, max 5)", "minimum": 1, "maximum": 5 },
            "poll": {
                "type": "object",
                "properties": {
                    "intervalMs": { "type": "integer", "description": "Poll interval in ms (50-5000)", "minimum": 50, "maximum": 5000 },
                    "durationMs": { "type": "integer", "description": "Poll duration in ms (100-30000)", "minimum": 100, "maximum": 30000 }
                }
            }
        },
        "required": ["sessionId", "targets"]
    }),
},
```

**Step 3: Add tool dispatch**

In `handle_tools_call` match (around line 673), add:

```rust
"debug_read" => self.tool_debug_read(&call.arguments).await,
```

**Step 4: Implement `tool_debug_read`**

```rust
async fn tool_debug_read(&self, args: &serde_json::Value) -> Result<serde_json::Value> {
    let req: crate::mcp::DebugReadRequest = serde_json::from_value(args.clone())?;
    req.validate()?;

    // Verify session exists and is running
    let session = self.require_session(&req.session_id)?;
    if session.status != crate::db::SessionStatus::Running {
        return Err(crate::Error::ReadFailed(
            "Process exited — session still queryable but reads unavailable".to_string()
        ));
    }

    let depth = req.depth.unwrap_or(1);

    // Build read recipes from targets
    let mut recipes: Vec<serde_json::Value> = Vec::new();
    let mut response_results: Vec<crate::mcp::ReadResult> = Vec::new();

    // Get DWARF parser for variable resolution
    let dwarf = self.session_manager.get_dwarf(&req.session_id).await?;

    for target in &req.targets {
        if let Some(ref var_name) = target.variable {
            // DWARF-resolved variable
            let dwarf = match dwarf.as_ref() {
                Some(d) => d,
                None => {
                    response_results.push(crate::mcp::ReadResult {
                        target: var_name.clone(),
                        error: Some("No debug symbols available".to_string()),
                        ..Default::default()
                    });
                    continue;
                }
            };

            match dwarf.resolve_read_target(var_name, depth) {
                Ok((recipe, struct_fields)) => {
                    let type_kind_str = match recipe.type_kind {
                        crate::dwarf::TypeKind::Integer { signed } => {
                            if signed { "int" } else { "uint" }
                        }
                        crate::dwarf::TypeKind::Float => "float",
                        crate::dwarf::TypeKind::Pointer => "pointer",
                        crate::dwarf::TypeKind::Unknown => "uint",
                    };

                    let mut recipe_json = serde_json::json!({
                        "label": var_name,
                        "address": format!("0x{:x}", recipe.base_address),
                        "size": recipe.final_size,
                        "typeKind": type_kind_str,
                        "derefDepth": recipe.deref_chain.len(),
                        "derefOffset": recipe.deref_chain.first().copied().unwrap_or(0),
                    });

                    // Add struct fields if expanded
                    if let Some(fields) = struct_fields {
                        recipe_json["struct"] = serde_json::json!(true);
                        let fields_json: Vec<serde_json::Value> = fields.iter().map(|f| {
                            let tk = match f.type_kind {
                                crate::dwarf::TypeKind::Integer { signed } => {
                                    if signed { "int" } else { "uint" }
                                }
                                crate::dwarf::TypeKind::Float => "float",
                                crate::dwarf::TypeKind::Pointer => "pointer",
                                crate::dwarf::TypeKind::Unknown => "uint",
                            };
                            serde_json::json!({
                                "name": f.name,
                                "offset": f.offset,
                                "size": f.size,
                                "typeKind": tk,
                                "typeName": f.type_name,
                                "isTruncatedStruct": f.is_truncated_struct,
                            })
                        }).collect();
                        recipe_json["fields"] = serde_json::json!(fields_json);
                    }

                    recipes.push(recipe_json);
                }
                Err(e) => {
                    response_results.push(crate::mcp::ReadResult {
                        target: var_name.clone(),
                        error: Some(e.to_string()),
                        ..Default::default()
                    });
                }
            }
        } else if let Some(ref addr) = target.address {
            // Raw address read
            let size = target.size.unwrap_or(4);
            let type_hint = target.type_hint.clone().unwrap_or("bytes".to_string());

            recipes.push(serde_json::json!({
                "label": addr,
                "address": addr,
                "size": size,
                "typeKind": type_hint,
                "derefDepth": 0,
                "derefOffset": 0,
            }));
        }
    }

    if recipes.is_empty() && !response_results.is_empty() {
        // All targets had errors — return immediately
        return Ok(serde_json::to_value(crate::mcp::DebugReadResponse {
            results: response_results,
        })?);
    }

    // Build the message to send to agent
    let mut msg = serde_json::json!({
        "type": "read_memory",
        "recipes": recipes,
    });

    if let Some(ref poll) = req.poll {
        msg["poll"] = serde_json::json!({
            "intervalMs": poll.interval_ms,
            "durationMs": poll.duration_ms,
        });
    }

    let msg_str = serde_json::to_string(&msg)?;

    // Send to agent via session manager
    let agent_response = self.session_manager.read_memory(&req.session_id, msg_str).await?;

    // Handle poll mode
    if req.poll.is_some() {
        let poll = req.poll.as_ref().unwrap();
        let expected = poll.duration_ms / poll.interval_ms;
        let response = crate::mcp::DebugReadPollResponse {
            polling: true,
            variable_count: recipes.len(),
            interval_ms: poll.interval_ms,
            duration_ms: poll.duration_ms,
            expected_samples: expected,
            event_type: "variable_snapshot".to_string(),
            hint: "Use debug_query({ eventType: 'variable_snapshot' }) to see results".to_string(),
        };
        return Ok(serde_json::to_value(response)?);
    }

    // Handle one-shot response — merge agent results with any pre-computed errors
    if let Some(results) = agent_response.get("results").and_then(|v| v.as_array()) {
        for result in results {
            let label = result.get("label").and_then(|v| v.as_str()).unwrap_or("?");
            let mut read_result = crate::mcp::ReadResult {
                target: label.to_string(),
                ..Default::default()
            };

            if let Some(err) = result.get("error").and_then(|v| v.as_str()) {
                read_result.error = Some(err.to_string());
            } else if let Some(fields) = result.get("fields") {
                read_result.fields = Some(fields.clone());
            } else if let Some(value) = result.get("value") {
                // Handle bytes type: write to file
                if result.get("isBytes").and_then(|v| v.as_bool()).unwrap_or(false) {
                    if let Some(hex) = value.as_str() {
                        let dir = "/tmp/strobe/reads";
                        let _ = std::fs::create_dir_all(dir);
                        let filename = format!("{}-{}.bin", req.session_id, chrono::Utc::now().timestamp());
                        let filepath = format!("{}/{}", dir, filename);
                        if let Ok(bytes) = hex_to_bytes(hex) {
                            let _ = std::fs::write(&filepath, &bytes);
                            read_result.file = Some(filepath);
                            // Preview first 32 bytes as hex
                            let preview_bytes = &bytes[..bytes.len().min(32)];
                            read_result.preview = Some(
                                preview_bytes.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join(" ")
                            );
                        }
                    }
                } else {
                    read_result.value = Some(value.clone());
                }
            }

            response_results.push(read_result);
        }
    }

    Ok(serde_json::to_value(crate::mcp::DebugReadResponse {
        results: response_results,
    })?)
}
```

Add a helper function in `server.rs`:

```rust
fn hex_to_bytes(hex: &str) -> std::result::Result<Vec<u8>, String> {
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i.min(hex.len()).min(i + 2)], 16)
                .map_err(|e| format!("Invalid hex: {}", e))
        })
        .collect()
}
```

Add `Default` impl for `ReadResult` in `src/mcp/types.rs`:

```rust
impl Default for ReadResult {
    fn default() -> Self {
        Self {
            target: String::new(),
            address: None,
            type_name: None,
            value: None,
            size: None,
            fields: None,
            error: None,
            file: None,
            preview: None,
        }
    }
}
```

**Step 5: Update debugging instructions**

In the `debugging_instructions()` method, add a section about `debug_read` to the instructions string:

```
## Live Memory Reads

Read variables from a running process without setting up traces:
- `debug_read({ sessionId, targets: [{ variable: "gTempo" }] })` — read a global
- `debug_read({ sessionId, targets: [{ variable: "gClock->counter" }] })` — follow pointer chain
- `debug_read({ sessionId, targets: [...], depth: 2 })` — expand struct fields
- `debug_read({ sessionId, targets: [...], poll: { intervalMs: 100, durationMs: 2000 } })` — sample over time
- Poll results: `debug_query({ eventType: "variable_snapshot" })`
```

**Step 6: Update the `EventTypeFilter` match in `tool_debug_query`**

In the `tool_debug_query` method's event type match, ensure `VariableSnapshot` is handled:

```rust
EventTypeFilter::VariableSnapshot => crate::db::EventType::VariableSnapshot,
```

**Checkpoint:** Full end-to-end debug_read works — one-shot variable reads, struct expansion, raw address reads, polling with timeline integration.

---

### Task 8: Build and Verify

**Step 1: Build agent**
```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
```

**Step 2: Build Rust**
```bash
cargo build 2>&1
```
Fix any compilation errors.

**Step 3: Run unit tests**
```bash
cargo test --lib 2>&1
```

**Step 4: Run integration tests**
```bash
cargo test 2>&1
```

**Checkpoint:** Everything compiles and tests pass. Feature is ready for manual testing.
