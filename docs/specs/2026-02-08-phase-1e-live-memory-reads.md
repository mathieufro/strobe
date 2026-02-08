# Phase 1e: Live Memory Reads (`debug_read`)

> Non-blocking, on-demand memory reads from a running process via Frida. No breakpoints, no tracing required.

## Motivation

Current observation tools are **event-driven** — watches fire at function entry/exit, traces capture call history. There's no way to ask "what is the value of `gTempo` right now?" without first setting up a trace on a function that happens to execute.

`debug_read` fills this gap: point-in-time memory snapshots on a live process, independent of function hooks. Combined with polling mode, snapshots interleave with trace events in the timeline, showing causal relationships between variable changes and function calls.

## MCP Tool: `debug_read`

### Request

```
debug_read({
  sessionId: string,                // Required

  targets: [                        // 1-16 read targets
    { variable: "gTempo" },                              // DWARF-resolved
    { variable: "gClock->counter" },                     // Pointer chain
    { address: "0x7ff800", size: 64, type: "bytes" },    // Raw address
  ],

  depth?: number,       // Struct traversal depth (default 1, max 5)

  poll?: {
    intervalMs: number, // Min 50, max 5000
    durationMs: number, // Min 100, max 30000
  }
})
```

**Target types:**

| Field | DWARF needed | Description |
|-------|-------------|-------------|
| `variable` | Yes | Variable name or pointer chain (`gClock->counter`). Type, size, address resolved from DWARF. |
| `address` + `size` + `type` | No | Raw memory read. `type` is one of: `i8/u8/i16/u16/i32/u32/i64/u64/f32/f64/pointer/bytes`. |

### Response (one-shot)

```json
{
  "results": [
    {
      "target": "gTempo",
      "address": "0x10000a3c0",
      "type": "f64",
      "value": 120.0,
      "size": 8
    },
    {
      "target": "gClock->counter",
      "address": "0x10000b200",
      "type": "u32",
      "value": 4817,
      "size": 4
    },
    {
      "target": "0x7ff800",
      "type": "bytes",
      "size": 64,
      "file": "/tmp/strobe/reads/session-abc-1707400000.bin",
      "preview": "4d 54 68 64 00 00 00 06 00 01 00 02..."
    }
  ]
}
```

### Response (struct, depth 1)

```json
{
  "results": [
    {
      "target": "gMidiBuffer",
      "address": "0x10000a3c0",
      "type": "MidiBuffer",
      "size": 24,
      "fields": {
        "size": { "type": "u32", "value": 128 },
        "capacity": { "type": "u32", "value": 512 },
        "data": { "type": "u8*", "value": "0x7ff8001234" },
        "owner": { "type": "AudioEngine", "value": "<struct>" }
      }
    }
  ]
}
```

Nested structs beyond `depth` show `"<struct>"` as value. Increase `depth` to expand.

### Response (poll)

```json
{
  "polling": true,
  "variable_count": 2,
  "intervalMs": 100,
  "durationMs": 2000,
  "expectedSamples": 20,
  "eventType": "variable_snapshot",
  "hint": "Use debug_query({ eventType: 'variable_snapshot' }) to see results"
}
```

Poll returns immediately. Events stream into the timeline asynchronously.

### Response (bytes)

Buffer dumps (type `bytes`) write raw data to a file. The response includes the file path and a hex preview of the first 32 bytes. This prevents large buffers from flooding the MCP response.

Files are written to `/tmp/strobe/reads/<sessionId>-<timestamp>.bin`.

## Timeline Integration

### Event type: `variable_snapshot`

Poll samples are stored as events in the existing timeline, interleaved with function traces:

```
t=0ms     variable_snapshot  { "gTempo": 120.0, "gBufferSize": 0 }
t=12ms    function_enter     midi::processBlock
t=13ms    function_exit      midi::processBlock (ret: 3)
t=100ms   variable_snapshot  { "gTempo": 120.0, "gBufferSize": 3 }
t=105ms   function_enter     audio::renderBlock
t=106ms   function_exit      audio::renderBlock
t=200ms   variable_snapshot  { "gTempo": 120.5, "gBufferSize": 0 }
```

All targets in one poll tick produce a **single event** (read at the same instant). This keeps event count proportional to `durationMs / intervalMs`, not multiplied by target count.

### Storage mapping

| events column | value |
|---------------|-------|
| `event_type` | `"variable_snapshot"` |
| `function_name` | `NULL` |
| `arguments` | JSON object: `{ "gTempo": 120.0, "gCounter": 42 }` |
| `text` | `NULL` |
| `duration_ns` | `NULL` |
| `thread_id` | Frida JS thread ID |

### Querying

```
debug_query({ sessionId, eventType: "variable_snapshot" })
```

No new filter params on `debug_query`. Max poll generates 600 events (30s / 50ms). Default query limit of 500 handles this; pagination covers the rest.

## Architecture

### DWARF resolution on host (Option A)

The host resolves all variable targets into flat "read recipes" before sending to the agent. The agent never interprets DWARF — it just reads memory at computed addresses.

**Read recipe (host → agent):**

```json
{
  "label": "gClock->counter",
  "address": "0x10000b180",
  "size": 4,
  "typeKind": "uint",
  "derefDepth": 1,
  "derefOffset": 8
}
```

**Struct recipe (host → agent, depth 1):**

```json
{
  "label": "gMidiBuffer",
  "address": "0x10000a3c0",
  "struct": true,
  "fields": [
    { "name": "size", "offset": 0, "size": 4, "typeKind": "uint" },
    { "name": "capacity", "offset": 4, "size": 4, "typeKind": "uint" },
    { "name": "data", "offset": 8, "size": 8, "typeKind": "pointer" },
    { "name": "owner", "offset": 16, "size": 8, "typeKind": "struct", "typeName": "AudioEngine" }
  ]
}
```

For depth > 1, the host recursively expands nested struct fields before sending. The agent sees a flat field list regardless of depth.

### One-shot flow

```
LLM
  → debug_read({ sessionId, targets: [{ variable: "gTempo" }] })
server.rs
  → validate request
  → resolve DWARF: variable name → address, size, type, deref chain
  → build flat read recipes
session_manager
  → send ReadMemory command to frida worker thread
spawner.rs (worker thread)
  → post_message_raw({ type: "read_memory", recipes: [...] })
  → block on response channel (5s timeout)
agent.ts
  → apply ASLR slide to each address
  → validate each address (Process.findRangeByAddress)
  → read memory, format values
  → send({ type: "read_response", results: [...] })
spawner.rs (message handler)
  → parse response, signal channel
  → worker unblocks, returns result
server.rs
  → format DebugReadResponse
LLM
  ← { results: [...] }
```

### Poll flow

```
LLM
  → debug_read({ ..., poll: { intervalMs: 100, durationMs: 2000 } })
server.rs
  → resolve DWARF, build recipes (same as one-shot)
spawner.rs
  → post_message_raw({ type: "read_memory", recipes, poll: { ... } })
  → return immediately (no blocking)
agent.ts
  → start setInterval(100ms)
  → each tick: read all targets, send({ type: "events", events: [variable_snapshot] })
  → after 2000ms: clearInterval, send({ type: "poll_complete", sampleCount: 20 })
server.rs
  ← { polling: true, expectedSamples: 20, ... }
LLM
  → debug_query({ eventType: "variable_snapshot" })
  ← interleaved timeline
```

### Agent message protocol

**Host → Agent:**

```json
{
  "type": "read_memory",
  "recipes": [
    { "label": "gTempo", "address": "0x...", "size": 8, "typeKind": "float", "derefDepth": 0, "derefOffset": 0 },
    { "label": "gMidiBuffer", "address": "0x...", "struct": true, "fields": [...] }
  ],
  "poll": { "intervalMs": 100, "durationMs": 2000 }
}
```

`poll` field absent = one-shot read.

**Agent → Host (one-shot):**

```json
{
  "type": "read_response",
  "results": [
    { "label": "gTempo", "value": 120.0 },
    { "label": "gMidiBuffer", "fields": { "size": 128, "capacity": 512 } },
    { "label": "gBuf", "error": "Address 0x... not readable" }
  ]
}
```

**Agent → Host (poll ticks):**

Reuses existing event stream:
```json
{
  "type": "events",
  "events": [{
    "eventType": "variable_snapshot",
    "data": { "gTempo": 120.0, "gCounter": 42 }
  }]
}
```

**Agent → Host (poll done):**

```json
{ "type": "poll_complete", "sampleCount": 20 }
```

## Error Handling

| Condition | Behavior |
|-----------|----------|
| Variable not found in DWARF | Error before reaching agent: `"Variable 'gFoo' not found in debug symbols"` |
| Process exited | Error: `"Process exited — session still queryable but reads unavailable"` |
| Address not readable | Per-target error: `{ "target": "gBuf", "error": "Address not readable" }` |
| Null pointer in deref chain | Per-target error: `{ "target": "gClock->counter", "error": "Null pointer at gClock" }` |
| Struct field at depth > limit | Field shows `"<struct>"`, no error |
| Timeout (agent unresponsive) | 5s timeout: `"Memory read timed out"` |
| Poll on exited process | Tick fails → poll stops → `poll_complete` with actual sample count |

Per-target errors: one bad variable doesn't kill the whole read. 2 values + 1 error is a valid response.

## Validation Limits

| Parameter | Min | Max | Default |
|-----------|-----|-----|---------|
| `targets` length | 1 | 16 | — |
| `depth` | 1 | 5 | 1 |
| `poll.intervalMs` | 50 | 5000 | — |
| `poll.durationMs` | 100 | 30000 | — |
| `size` (bytes type) | 1 | 65536 | — |

## DWARF Requirements

### Existing infrastructure (reused)

- `DwarfParser::resolve_watch_expression()` — variable name → `WatchRecipe` (address, size, type, deref chain)
- `DwarfParser::variables_by_name` — fast variable lookup index
- `struct_members` with lazy resolution — field layouts loaded on demand
- ASLR image base extraction from `__TEXT` segment

### New DWARF work: struct expansion

`resolve_watch_expression()` currently returns a single `WatchRecipe` for the final value in a deref chain. For struct traversal, we need a new method:

```rust
pub fn expand_struct(
    &self,
    type_name: &str,
    depth: usize,
) -> Result<Vec<StructFieldRecipe>>
```

This recursively expands struct members up to `depth`, producing flat field recipes with computed offsets. Uses the existing `struct_members` lazy cache.

## Files to Modify

| File | Changes |
|------|---------|
| `src/mcp/types.rs` | `DebugReadRequest`, `DebugReadResponse`, `ReadTarget`, validation |
| `src/daemon/server.rs` | Tool registration, `tool_debug_read()`, DWARF recipe building |
| `src/daemon/session_manager.rs` | `read_memory()` method, forwarding to spawner |
| `src/frida_collector/spawner.rs` | `ReadMemory` session command, `post_message_raw`, response handling |
| `src/dwarf/parser.rs` | `expand_struct()` for struct field enumeration |
| `agent/src/agent.ts` | `recv('read_memory')` handler, `handleReadMemory()`, poll via `setInterval` |
| `src/db/mod.rs` | `variable_snapshot` event type support (if not already handled by generic event storage) |
| `docs/CURRENT-SPEC.md` | Add `debug_read` tool documentation |
| `docs/FEATURES.md` | Add Phase 1e section |
