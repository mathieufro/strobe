# CModule High-Performance Tracing

**Date:** 2026-02-06
**Status:** Approved
**Goal:** Replace JS Interceptor callbacks with native CModule callbacks + ring buffer to support 1000+ hooked functions without killing target process performance
**Commit strategy:** Single commit at the end

## Problem

Every hooked function call triggers 2 JS context switches (enter + leave). Each does syscalls (`getCurrentThreadId`, `Date.now`), string allocations (event IDs, serialization of 10 args), object allocations (TraceEvent), Map lookups (call stack), and potentially `send()` IPC. At 69 hooked functions in `@file:layout_manager`, the target becomes sluggish. Broad patterns like `@file:` or `@usercode` are unusable.

## Architecture

```
BEFORE:
  func called → Frida trampoline → JS onEnter (syscalls, allocs, serialize) → JS onLeave → native
  Per-call cost: ~4-6 microseconds (JS VM entry/exit + work)

AFTER:
  func called → Frida trampoline → C onEnter (48 bytes to ring buffer) → [C onLeave if full mode] → native
  JS timer (every 10ms) → drain ring buffer → resolve func_ids → batch send()
  Per-call cost: ~200-500 nanoseconds (no JS, no syscalls, no allocs)
```

### Key concepts

**Function registry**: Each hooked function gets a numeric `func_id` (uint16). The agent maintains a registry mapping `func_id → {name, nameRaw, sourceFile, lineNumber}`. Ring buffer entries use `func_id` (2 bytes) instead of full metadata (~200 bytes JSON). Metadata is resolved during JS drain.

**Hook modes**: Two modes, selected per-pattern by the Rust side based on pattern type:
- **Full**: `onEnter` + `onLeave`, captures 2 args + retval. For specific patterns where every call matters.
- **Light**: `onEnter` only, captures 2 args, subject to adaptive sampling. For broad patterns.

**Pattern classification** (Rust-side, deterministic):
- `foo::bar` (exact match) → Full
- `foo::*` (single-level glob) → Full
- `foo::**` (deep glob) → Light
- `@file:X` → Light
- `@usercode` → Light
- Override: if a pattern resolves to ≤10 functions regardless of syntax → Full

**Adaptive sampling** (agent-side, for Light-mode hooks only):
- Global `sample_interval` variable in CModule shared memory (uint32, initially 1)
- Light onEnter: `if (atomic_inc(&counter) % sample_interval != 0) return;`
- JS drain monitors ring buffer fill rate, adjusts sample_interval: 1→2→4→8→16→...→256
- Hysteresis: 2 consecutive high-pressure cycles to increase, 5 low-pressure cycles to decrease
- Full-mode hooks are NEVER sampled

**Parent tracking**: CModule uses `gum_invocation_context_get_depth()` instead of maintaining per-thread JS stacks. JS drain reconstructs parent-child relationships from `(thread_id, depth, timestamp)` ordering during drain.

## Ring Buffer Layout

```
Shared memory (Memory.alloc):
  Offset 0:   write_idx       (uint32, atomic, CModule writes)
  Offset 4:   read_idx        (uint32, JS writes)
  Offset 8:   overflow_count  (uint32, atomic, CModule increments on overflow)
  Offset 12:  sample_interval (uint32, JS writes, CModule reads)
  Offset 16:  global_counter  (uint32, atomic, for sampling modulo)
  Offset 20:  padding         (12 bytes, align to 32)
  Offset 32:  entries[0..N]   (N × 48 bytes)

TraceEntry (48 bytes):
  uint64_t timestamp     // mach_absolute_time()
  uint64_t arg0          // raw pointer value
  uint64_t arg1          // raw pointer value
  uint64_t retval        // raw pointer (leave only, 0 for enter)
  uint32_t func_id       // index into function registry
  uint32_t thread_id     // from GumInvocationContext
  uint32_t depth         // from gum_invocation_context_get_depth()
  uint8_t  event_type    // 0=enter, 1=leave
  uint8_t  sampled       // 1=this event was sampled (not every call recorded)
  uint16_t _pad          // align to 48
```

Ring capacity: 16384 entries = 768KB. At 10ms drain interval with 100K events/sec, the buffer holds ~160ms of events with margin.

## Changes

### Stream A: Agent CModule (agent-side only, no Rust changes)

#### Task 1: `agent/src/cmodule-tracer.ts` — NEW (~180 lines)

Core CModule management class. Creates the C code, compiles it, manages ring buffer and function registry.

```typescript
// Key exports:
export interface FuncRegistryEntry {
  funcId: number;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

export type HookMode = 'full' | 'light';

export class CModuleTracer {
  private ringMem: NativePointer;     // shared memory
  private cmFull: CModule;            // onEnter + onLeave
  private cmLight: CModule;           // onEnter only (with sampling)
  private registry: Map<number, FuncRegistryEntry>;
  private nextFuncId: number = 0;
  private hooks: Map<string, InvocationListener>;  // address → listener
  private drainTimer: any;
  private aslrSlide: NativePointer = ptr(0);
  private imageBaseSet: boolean = false;

  // Adaptive sampling state
  private consecutiveHigh: number = 0;
  private consecutiveLow: number = 0;

  constructor(private onEvents: (events: any[]) => void) { ... }
  setImageBase(imageBase: string): void { ... }
  installHook(func: FunctionTarget, mode: HookMode): boolean { ... }
  removeHook(address: string): void { ... }
  activeHookCount(): number { ... }
  removeAll(): void { ... }
  dispose(): void { ... }
  private drain(): void { ... }          // reads ring buffer, resolves func_ids, calls onEvents
  private adaptSampling(entries: number): void { ... }
}
```

**CModule C source** (embedded as template string):

```c
#include <gum/guminterceptor.h>
#include <glib.h>

extern unsigned long long mach_absolute_time(void);

extern volatile gint write_idx;
extern volatile gint overflow_count;
extern volatile gint sample_interval;
extern volatile gint global_counter;
extern guint8 *ring_data;

#define RING_CAPACITY 16384
#define ENTRY_SIZE 48

typedef struct {
  guint64 timestamp;
  guint64 arg0;
  guint64 arg1;
  guint64 retval;
  guint32 func_id;
  guint32 thread_id;
  guint32 depth;
  guint8  event_type;
  guint8  sampled;
  guint16 _pad;
} TraceEntry;

static void write_entry(GumInvocationContext *ic, guint8 etype, guint8 samp) {
  gint pos = g_atomic_int_add(&write_idx, 1);
  gint rp = g_atomic_int_get(&((volatile gint *)ring_data)[-3]); // read_idx at offset 4
  // Actually read_idx is at shared memory offset 4, not in ring_data.
  // We'll handle overflow check differently — just wrap and let JS detect.
  gint slot = pos % RING_CAPACITY;
  TraceEntry *e = (TraceEntry *)(ring_data + slot * ENTRY_SIZE);

  e->timestamp  = mach_absolute_time();
  e->func_id    = (guint32)(gsize)gum_invocation_context_get_listener_function_data(ic);
  e->thread_id  = gum_invocation_context_get_thread_id(ic);
  e->depth      = gum_invocation_context_get_depth(ic);
  e->event_type = etype;
  e->sampled    = samp;
  e->_pad       = 0;
}

// --- Full mode (enter + leave) ---
void fullOnEnter(GumInvocationContext *ic) {
  write_entry(ic, 0, 0);
  TraceEntry *e = (TraceEntry *)(ring_data +
    ((g_atomic_int_get(&write_idx) - 1) % RING_CAPACITY) * ENTRY_SIZE);
  e->arg0 = (guint64)gum_invocation_context_get_nth_argument(ic, 0);
  e->arg1 = (guint64)gum_invocation_context_get_nth_argument(ic, 1);
  e->retval = 0;
}

void fullOnLeave(GumInvocationContext *ic) {
  write_entry(ic, 1, 0);
  TraceEntry *e = (TraceEntry *)(ring_data +
    ((g_atomic_int_get(&write_idx) - 1) % RING_CAPACITY) * ENTRY_SIZE);
  e->arg0 = 0;
  e->arg1 = 0;
  e->retval = (guint64)gum_invocation_context_get_return_value(ic);
}

// --- Light mode (enter only, with sampling) ---
void lightOnEnter(GumInvocationContext *ic) {
  gint interval = g_atomic_int_get(&sample_interval);
  gint count = g_atomic_int_add(&global_counter, 1);
  if (interval > 1 && (count % interval) != 0) return;

  write_entry(ic, 0, interval > 1 ? 1 : 0);
  TraceEntry *e = (TraceEntry *)(ring_data +
    ((g_atomic_int_get(&write_idx) - 1) % RING_CAPACITY) * ENTRY_SIZE);
  e->arg0 = (guint64)gum_invocation_context_get_nth_argument(ic, 0);
  e->arg1 = (guint64)gum_invocation_context_get_nth_argument(ic, 1);
  e->retval = 0;
}
```

Note: `fullOnEnter`/`fullOnLeave` and `lightOnEnter` share the same CModule (one compilation). Different functions are attached to different callbacks.

**Ring buffer drain** (`drain()` method):

```typescript
drain(): void {
  const wp = this.ringMem.readU32();         // write_idx
  const rp = this.ringMem.add(4).readU32();  // read_idx
  if (wp === rp) return;

  // Cap drain to prevent huge batches
  const count = Math.min(wp - rp, 16384);
  const events: any[] = [];

  // Per-thread stacks for parent reconstruction
  const stacks = new Map<number, string[]>();

  for (let i = rp; i < rp + count; i++) {
    const slot = i % 16384;
    const base = this.ringMem.add(32 + slot * 48);

    const funcId   = base.add(32).readU32();
    const threadId = base.add(36).readU32();
    const depth    = base.add(40).readU32();
    const etype    = base.add(44).readU8();
    const sampled  = base.add(45).readU8();

    const meta = this.registry.get(funcId);
    if (!meta) continue;

    // Parent reconstruction from depth
    let stack = stacks.get(threadId) || [];
    let parentId: string | null = null;

    const eventId = `${this.sessionId}-${this.eventCounter++}`;

    if (etype === 0) { // enter
      parentId = stack.length > 0 ? stack[stack.length - 1] : null;
      while (stack.length >= depth) stack.pop(); // handle missed exits
      stack.push(eventId);
    } else { // leave
      stack.pop();
      parentId = eventId; // link to matching enter (approximate)
    }
    stacks.set(threadId, stack);

    const event: any = {
      id: eventId,
      sessionId: this.sessionId,
      timestampNs: /* convert mach_absolute_time to session-relative ns */,
      threadId,
      parentEventId: parentId,
      eventType: etype === 0 ? 'function_enter' : 'function_exit',
      functionName: meta.name,
      functionNameRaw: meta.nameRaw,
      sourceFile: meta.sourceFile,
      lineNumber: meta.lineNumber,
    };

    // Only include args/retval as raw hex (deferred serialization)
    if (etype === 0) {
      event.arguments = [
        '0x' + base.add(8).readU64().toString(16),
        '0x' + base.add(16).readU64().toString(16),
      ];
    } else {
      event.returnValue = '0x' + base.add(24).readU64().toString(16);
    }

    events.push(event);
  }

  // Advance read pointer
  this.ringMem.add(4).writeU32(rp + count);

  // Adaptive sampling
  this.adaptSampling(count);

  if (events.length > 0) {
    this.onEvents(events);
  }
}
```

**Adaptive sampling** (`adaptSampling()` method):

```typescript
private adaptSampling(entriesThisCycle: number): void {
  const HIGH_THRESHOLD = 16384 * 0.5;  // 50% full per cycle = too fast
  const LOW_THRESHOLD = 16384 * 0.1;   // 10% = comfortably low
  const UP_CYCLES = 2;    // react fast to pressure
  const DOWN_CYCLES = 5;  // wait before relaxing

  const currentInterval = this.ringMem.add(12).readU32();

  if (entriesThisCycle > HIGH_THRESHOLD) {
    this.consecutiveHigh++;
    this.consecutiveLow = 0;
    if (this.consecutiveHigh >= UP_CYCLES && currentInterval < 256) {
      const newInterval = Math.min(currentInterval * 2, 256);
      this.ringMem.add(12).writeU32(newInterval);
      send({ type: 'log', message: `Adaptive sampling: interval ${currentInterval} → ${newInterval}` });
      this.consecutiveHigh = 0;
    }
  } else if (entriesThisCycle < LOW_THRESHOLD) {
    this.consecutiveLow++;
    this.consecutiveHigh = 0;
    if (this.consecutiveLow >= DOWN_CYCLES && currentInterval > 1) {
      const newInterval = Math.max(Math.floor(currentInterval / 2), 1);
      this.ringMem.add(12).writeU32(newInterval);
      send({ type: 'log', message: `Adaptive sampling: interval ${currentInterval} → ${newInterval}` });
      this.consecutiveLow = 0;
    }
  } else {
    this.consecutiveHigh = 0;
    this.consecutiveLow = 0;
  }
}
```

#### Task 2: Modify `agent/src/hooks.ts` — Support CModule hooks

Replace the single `HookInstaller` with a unified interface that delegates to `CModuleTracer` for all hooks. The old JS `Interceptor.attach` path is removed entirely (CModule handles both full and light modes).

**Changes:**
- Remove the `HookInstaller` class body, replace with a thin wrapper around `CModuleTracer`
- Or: keep `HookInstaller` as the public API but internally delegate to CModuleTracer
- `installHook(func, mode)` now accepts a `mode` parameter

Simplest approach: `HookInstaller` becomes a facade:

```typescript
export class HookInstaller {
  private tracer: CModuleTracer;
  private imageBaseSet: boolean = false;

  constructor(onEvents: (events: any[]) => void) {
    this.tracer = new CModuleTracer(onEvents);
  }

  setImageBase(imageBase: string): void {
    this.tracer.setImageBase(imageBase);
  }

  installHook(func: FunctionTarget, mode: HookMode = 'full'): boolean {
    return this.tracer.installHook(func, mode);
  }

  removeHook(address: string): void {
    this.tracer.removeHook(address);
  }

  activeHookCount(): number {
    return this.tracer.activeHookCount();
  }

  removeAll(): void {
    this.tracer.removeAll();
  }
}
```

#### Task 3: Modify `agent/src/agent.ts` — Wire CModule events

**Changes:**
- Remove `eventBuffer`, `flushInterval`, `maxBufferSize`, `callStacks`, `eventIdCounter` — all moved to CModuleTracer
- Remove `onEnter()` and `onLeave()` methods — CModule handles these natively
- Keep `installOutputCapture()` — still JS-based (write() interception is low-frequency)
- `handleMessage()` now passes `mode` from the hooks message to `installHook()`
- Events from CModuleTracer go directly to `send({ type: 'events', events })`

The message protocol changes:
```json
// New hooks message format (Rust → Agent):
{
  "type": "hooks",
  "action": "add",
  "functions": [...],
  "imageBase": "0x...",
  "mode": "full" | "light"   // NEW
}
```

---

### Stream B: Rust Pattern Classification

#### Task 4: Modify `src/frida_collector/hooks.rs` — Pattern classification

Add `HookMode` enum and classification logic:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookMode {
    Full,   // enter + leave, no sampling
    Light,  // enter only, adaptive sampling
}

impl HookManager {
    /// Classify a pattern's hook mode based on syntax.
    pub fn classify_pattern(pattern: &str) -> HookMode {
        // Deep globs → Light
        if pattern.contains("**") {
            return HookMode::Light;
        }
        // File patterns → Light
        if pattern.starts_with("@file:") {
            return HookMode::Light;
        }
        // @usercode → Light
        if pattern == "@usercode" {
            return HookMode::Light;
        }
        // Everything else (exact, single-glob) → Full
        HookMode::Full
    }

    /// Override: if a pattern resolved to very few functions, upgrade to Full
    pub fn classify_with_count(pattern: &str, match_count: usize) -> HookMode {
        let mode = Self::classify_pattern(pattern);
        if mode == HookMode::Light && match_count <= 10 {
            return HookMode::Full;
        }
        mode
    }
}
```

#### Task 5: Modify `src/frida_collector/spawner.rs` — Send mode per batch

**Changes to `FridaCommand::AddPatterns`:**

```rust
AddPatterns {
    session_id: String,
    functions: Vec<FunctionTarget>,
    image_base: u64,
    mode: HookMode,  // NEW
    response: oneshot::Sender<Result<u32>>,
}
```

**Changes to `add_patterns()` method:**

Split patterns by mode and send separate AddPatterns commands:

```rust
pub async fn add_patterns(&mut self, session_id: &str, patterns: &[String]) -> Result<u32> {
    let session = self.sessions.get_mut(session_id)
        .ok_or_else(|| crate::Error::SessionNotFound(session_id.to_string()))?;

    session.hook_manager.add_patterns(patterns);
    let dwarf = session.dwarf_handle.clone().get().await?;

    // Group functions by mode
    let mut full_funcs: Vec<FunctionTarget> = Vec::new();
    let mut light_funcs: Vec<FunctionTarget> = Vec::new();

    for pattern in patterns {
        let matches: Vec<&FunctionInfo> = resolve_pattern(&dwarf, pattern, &session.project_root);
        let mode = HookManager::classify_with_count(pattern, matches.len());

        let target = if mode == HookMode::Full { &mut full_funcs } else { &mut light_funcs };
        for func in matches {
            target.push(FunctionTarget::from(func));
        }
    }

    let image_base = session.image_base;
    let mut total_hooks = 0u32;

    // Send full-mode batch
    if !full_funcs.is_empty() {
        total_hooks += self.send_add_patterns(session_id, full_funcs, image_base, HookMode::Full).await?;
    }

    // Send light-mode batch
    if !light_funcs.is_empty() {
        total_hooks += self.send_add_patterns(session_id, light_funcs, image_base, HookMode::Light).await?;
    }

    Ok(total_hooks)
}
```

**Changes to worker `FridaCommand::AddPatterns` handler:**

Add `mode` to the JSON hooks message:

```rust
let hooks_msg = serde_json::json!({
    "type": "hooks",
    "action": "add",
    "functions": func_list,
    "imageBase": format!("0x{:x}", image_base),
    "mode": match mode {
        HookMode::Full => "full",
        HookMode::Light => "light",
    },
});
```

---

### Serial: Integration

#### Task 6: Build, test, and verify

1. `cd agent && npm run build` — rebuild agent with CModule tracer
2. `cargo build` — rebuild daemon (picks up new agent.js)
3. `cargo test` — all existing tests pass
4. Manual verification:
   - Launch erae with no patterns → ~1s, stdout/stderr captured
   - `debug_trace({ add: ["@file:layout_manager"] })` → light mode, 69 hooks
   - Observe: app remains responsive with 69 hooks
   - `debug_trace({ add: ["specific::function"] })` → full mode
   - `debug_query({ eventType: "function_enter" })` → events present
   - Add many broad patterns → adaptive sampling kicks in (check daemon logs)

## Thread Safety

- CModule C code runs on target process threads concurrently
- Ring buffer uses `g_atomic_int_add` for `write_idx` — lock-free MPSC
- `read_idx` written only by JS drain timer (single-threaded)
- `sample_interval` written by JS, read by C — atomic read guarantees visibility
- Function registry (`Map<number, entry>`) only mutated during `installHook()` (JS thread), read during `drain()` (also JS thread) — no concurrent access

## Error Handling

- CModule compilation fails → fall back to JS Interceptor callbacks (log warning, degrade gracefully)
- Ring buffer overflow → increment `overflow_count`, newest events overwrite oldest (circular)
- `mach_absolute_time` not found → fall back to JS `Date.now()` timestamps
- Invalid entries in ring buffer → skip during drain (null func_id check)

## Performance Expectations

| Metric | Before (JS) | After (CModule) |
|--------|-------------|-----------------|
| Per-call overhead | ~4-6 µs | ~200-500 ns |
| Max hooks (responsive) | ~50-100 | ~1000-5000 |
| IPC events/batch | per-call send risk | 10ms batched drain |
| Args captured | 10 args, serialized | 2 raw pointers, deferred |
| Sampling | none (all or nothing) | adaptive 1-256x for broad patterns |

## Future Optimizations (not in this plan)

- Binary event format via `send(data, ArrayBuffer)` instead of JSON — eliminates JSON serialization in agent and parsing in Rust
- `Interceptor.replaceFast()` for absolute hottest functions
- Stalker-based tracing for comprehensive "trace all calls" mode
- Configurable args capture count (0, 2, 4, 10) per pattern
