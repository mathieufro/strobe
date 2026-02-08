/**
 * CModule-based high-performance tracer.
 *
 * Uses a native CModule with a shared ring buffer to record function
 * enter/leave events at near-zero per-call overhead. A JS timer drains
 * the ring buffer every 10ms and forwards structured event JSON to the
 * daemon via send().
 */

import { ObjectSerializer, TypeInfo } from './object-serializer.js';

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export type HookMode = 'full' | 'light';

/** Callback for per-function rate checking. Returns true if the event should be recorded. */
export type RateCheckFn = (funcId: number) => boolean;

export interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

interface TraceEvent {
  id: string;
  sessionId: string;
  timestampNs: number;
  threadId: number;
  threadName?: string | null;
  parentEventId: string | null;
  eventType: 'function_enter' | 'function_exit';
  functionName: string;
  functionNameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
  arguments?: string[];
  returnValue?: string;
  durationNs?: number;
  sampled?: boolean;
  watchValues?: Record<string, number | string>;
}

interface WatchConfig {
  label: string;
  size: number;
  typeKind: 'int' | 'uint' | 'float' | 'pointer';
  isGlobal: boolean;
  onFuncIds: Set<number>;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RING_CAPACITY = 16384;
const ENTRY_SIZE = 80;
const HEADER_SIZE = 128;
const RING_BUFFER_SIZE = HEADER_SIZE + RING_CAPACITY * ENTRY_SIZE;

// Adaptive sampling thresholds
const HIGH_THRESHOLD = Math.floor(RING_CAPACITY * 0.5);
const LOW_THRESHOLD  = Math.floor(RING_CAPACITY * 0.1);
const HIGH_CYCLES_TRIGGER = 2;
const LOW_CYCLES_TRIGGER  = 5;
const MAX_SAMPLE_INTERVAL = 256;
const MIN_SAMPLE_INTERVAL = 1;

const DRAIN_INTERVAL_MS = 10;

// ---------------------------------------------------------------------------
// CModule C source
// ---------------------------------------------------------------------------
// Single CModule with onEnter + onLeave. Per-hook mode (full vs light) is
// encoded in the data pointer's low bit:
//   data = (func_id << 1) | is_light
// The shift limits func_id to 2^30 (1 billion) to prevent signed 32-bit overflow
// (2^31 - 1 = 2,147,483,647). In practice, hook cap of 100 means we never approach this.
// In onEnter: light hooks check sampling, full hooks don't.
// In onLeave: light hooks are skipped entirely (enter-only).

const CMODULE_SOURCE = `
#include <gum/guminterceptor.h>
#include <glib.h>

extern unsigned long long mach_absolute_time(void);

extern volatile gint write_idx;
extern volatile gint overflow_count;
extern volatile gint sample_interval;
extern volatile gint global_counter;
extern guint8 *ring_data;

extern volatile gint watch_count;
extern guint64 watch_addrs[4];
extern guint8 watch_sizes[4];
extern guint8 watch_deref_depths[4];
extern guint64 watch_deref_offsets[4];

#define RING_CAPACITY 16384
#define ENTRY_SIZE 80

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
  guint8  watch_entry_count;
  guint8  _pad;
  guint64 watch0;
  guint64 watch1;
  guint64 watch2;
  guint64 watch3;
} TraceEntry;

static void write_entry(guint32 func_id, GumInvocationContext *ic,
                         guint8 etype, guint8 samp,
                         guint64 a0, guint64 a1, guint64 rv) {
  gint pos = g_atomic_int_add(&write_idx, 1);
  guint32 slot = ((guint32)pos) % RING_CAPACITY;
  TraceEntry *e = (TraceEntry *)(ring_data + slot * ENTRY_SIZE);

  e->timestamp  = mach_absolute_time();
  e->func_id    = func_id;
  e->thread_id  = gum_invocation_context_get_thread_id(ic);
  e->depth      = gum_invocation_context_get_depth(ic);
  e->event_type = etype;
  e->sampled    = samp;
  e->_pad       = 0;
  e->arg0       = a0;
  e->arg1       = a1;
  e->retval     = rv;

  /* Read watch values */
  guint32 wc = (guint32)g_atomic_int_add(&watch_count, 0);

  // Early exit optimization: if no watches, zero out slots and skip
  if (wc == 0) {
    e->watch_entry_count = 0;
    for (guint32 w = 0; w < 4; w++) {
      *((guint64*)(((guint8*)e) + 48 + w * 8)) = 0;
    }
  } else {
    if (wc > 4) wc = 4;
    e->watch_entry_count = (guint8)wc;

    guint32 w;
    for (w = 0; w < wc; w++) {
    guint64 addr = watch_addrs[w];
    guint8 dd = watch_deref_depths[w];
    guint8 sz = watch_sizes[w];
    guint64 val = 0;

    if (addr != 0) {
      if (dd > 0) {
        guint64 ptr_val = *(volatile guint64*)(gpointer)addr;
        if (ptr_val != 0) {
          addr = ptr_val + watch_deref_offsets[w];
        } else {
          addr = 0;
        }
      }
      if (addr != 0) {
        // Check alignment before reading to avoid crashes on ARM64
        if ((sz == 2 && (addr % 2) != 0) ||
            (sz == 4 && (addr % 4) != 0) ||
            (sz == 8 && (addr % 8) != 0)) {
          val = 0; // Unaligned address, skip read
        } else {
          if (sz == 1) val = *(volatile guint8*)(gpointer)addr;
          else if (sz == 2) val = *(volatile guint16*)(gpointer)addr;
          else if (sz == 4) val = *(volatile guint32*)(gpointer)addr;
          else val = *(volatile guint64*)(gpointer)addr;
        }
      }
    }
      *((guint64*)(((guint8*)e) + 48 + w * 8)) = val;
    }
    for (; w < 4; w++) {
      *((guint64*)(((guint8*)e) + 48 + w * 8)) = 0;
    }
  }
}

void onEnter(GumInvocationContext *ic) {
  gsize raw = (gsize)gum_invocation_context_get_listener_function_data(ic);
  guint32 func_id = (guint32)(raw >> 1);
  guint8 is_light = (guint8)(raw & 1);

  if (is_light) {
    gint interval = g_atomic_int_add(&sample_interval, 0);
    if (interval > 1) {
      gint count = g_atomic_int_add(&global_counter, 1);
      if ((count % interval) != 0) return;
    }
    write_entry(func_id, ic, 0, interval > 1 ? 1 : 0,
      (guint64)gum_invocation_context_get_nth_argument(ic, 0),
      (guint64)gum_invocation_context_get_nth_argument(ic, 1),
      0);
  } else {
    write_entry(func_id, ic, 0, 0,
      (guint64)gum_invocation_context_get_nth_argument(ic, 0),
      (guint64)gum_invocation_context_get_nth_argument(ic, 1),
      0);
  }
}

void onLeave(GumInvocationContext *ic) {
  gsize raw = (gsize)gum_invocation_context_get_listener_function_data(ic);
  guint8 is_light = (guint8)(raw & 1);
  if (is_light) return;

  guint32 func_id = (guint32)(raw >> 1);
  write_entry(func_id, ic, 1, 0, 0, 0,
    (guint64)gum_invocation_context_get_return_value(ic));
}
`;

// ---------------------------------------------------------------------------
// CModuleTracer
// ---------------------------------------------------------------------------

export class CModuleTracer {
  // Ring buffer shared memory
  private ringBuffer: NativePointer;
  // Pointers into the header (used as extern symbols for the CModule)
  private writeIdxPtr: NativePointer;
  private readIdxPtr: NativePointer;
  private overflowCountPtr: NativePointer;
  private sampleIntervalPtr: NativePointer;
  private globalCounterPtr: NativePointer;
  // Watch table pointers in header
  private watchCountPtr: NativePointer;
  private watchAddrsPtr: NativePointer;
  private watchSizesPtr: NativePointer;
  private watchDerefDepthsPtr: NativePointer;
  private watchDerefOffsetsPtr: NativePointer;
  // Pointer to the data region after the header
  private ringDataPtr: NativePointer;
  // Pointer-to-pointer for ring_data extern (CModule needs guint8 *)
  private ringDataPtrHolder: NativePointer;

  // CModule instance
  private cm: CModule | null = null;

  // Function registry: func_id -> metadata
  private funcRegistry: Map<number, FunctionTarget> = new Map();
  private nextFuncId: number = 1;

  // Hook tracking: address string -> { listener, funcId }
  private hooks: Map<string, { listener: InvocationListener; funcId: number; funcName: string }> = new Map();

  // ASLR
  private aslrSlide: NativePointer = ptr(0);
  private imageBaseSet: boolean = false;

  // Session
  private sessionId: string = '';
  private eventIdCounter: number = 0;

  // Timestamp conversion
  private ticksToNs: number = 1.0;

  // Adaptive sampling state
  private highCycleCount: number = 0;
  private lowCycleCount: number = 0;
  private currentSampleInterval: number = 1;

  // Drain timer handle
  private drainTimer: ReturnType<typeof setInterval> | null = null;

  // Callback for emitting events to the daemon
  private onEvents: (events: TraceEvent[]) => void;

  // Per-thread depth stacks for parent tracking during drain
  // Map<threadId, Array<{ eventId: string; depth: number; timestampNs: number }>>
  private threadStacks: Map<number, Array<{ eventId: string; depth: number; timestampNs: number }>> = new Map();

  // Watch configurations (up to 4 CModule watches)
  private watchConfigs: (WatchConfig | null)[] = [null, null, null, null];
  // JS expression watches (unlimited)
  private exprWatches: Array<{
    label: string;
    expr: string;
    compiledFn: () => any;
    isGlobal: boolean;
    onFuncIds: Set<number>;
  }> = [];

  // Object serializer for deep argument inspection
  private objectSerializer: ObjectSerializer | null = null;

  // Rate check callback for hot function detection
  private rateCheck: RateCheckFn | null = null;

  // Thread name cache: threadId -> name
  private threadNames: Map<number, string | null> = new Map();

  constructor(onEvents: (events: any[]) => void) {
    this.onEvents = onEvents;

    // --- Allocate ring buffer shared memory ---
    this.ringBuffer = Memory.alloc(RING_BUFFER_SIZE);

    // Zero out the header
    this.ringBuffer.writeByteArray(new ArrayBuffer(HEADER_SIZE));

    // Header field pointers
    this.writeIdxPtr       = this.ringBuffer;            // offset 0
    this.readIdxPtr        = this.ringBuffer.add(4);     // offset 4
    this.overflowCountPtr  = this.ringBuffer.add(8);     // offset 8
    this.sampleIntervalPtr = this.ringBuffer.add(12);    // offset 12
    this.globalCounterPtr  = this.ringBuffer.add(16);    // offset 16

    // Watch table in header (offsets 24-103)
    this.watchCountPtr       = this.ringBuffer.add(24);
    this.watchAddrsPtr       = this.ringBuffer.add(32);   // 4 × 8 bytes
    this.watchSizesPtr       = this.ringBuffer.add(64);   // 4 × 1 byte
    this.watchDerefDepthsPtr = this.ringBuffer.add(68);   // 4 × 1 byte
    this.watchDerefOffsetsPtr = this.ringBuffer.add(72);  // 4 × 8 bytes = 32 bytes

    // Initialize watch_count to 0
    this.watchCountPtr.writeU32(0);

    // Data region starts at offset HEADER_SIZE
    this.ringDataPtr = this.ringBuffer.add(HEADER_SIZE);

    // Initialize sample_interval to 1
    this.sampleIntervalPtr.writeU32(1);

    // Allocate a pointer-to-pointer for ring_data extern symbol.
    // CModule declares `extern guint8 *ring_data;` — it's a pointer variable
    // whose *value* is the address of the data region.
    this.ringDataPtrHolder = Memory.alloc(Process.pointerSize);
    this.ringDataPtrHolder.writePointer(this.ringDataPtr);

    // --- Resolve mach_absolute_time ---
    // Use Process.getModuleByName (Frida 17.x — static Module.getExportByName was removed)
    const libSystem = Process.getModuleByName('libSystem.B.dylib');
    const machAbsTimePtr = libSystem.getExportByName('mach_absolute_time');

    // --- Compute ticksToNs ---
    this.initTimebaseInfo();

    // --- Create CModule ---
    this.cm = new CModule(CMODULE_SOURCE, {
      mach_absolute_time: machAbsTimePtr,
      write_idx:            this.writeIdxPtr,
      overflow_count:       this.overflowCountPtr,
      sample_interval:      this.sampleIntervalPtr,
      global_counter:       this.globalCounterPtr,
      ring_data:            this.ringDataPtrHolder,
      watch_count:          this.watchCountPtr,
      watch_addrs:          this.watchAddrsPtr,
      watch_sizes:          this.watchSizesPtr,
      watch_deref_depths:   this.watchDerefDepthsPtr,
      watch_deref_offsets:  this.watchDerefOffsetsPtr,
    });

    // --- Start drain timer ---
    this.drainTimer = setInterval(() => this.drain(), DRAIN_INTERVAL_MS);
  }

  // -----------------------------------------------------------------------
  // Public API
  // -----------------------------------------------------------------------

  setImageBase(imageBase: string): void {
    if (this.imageBaseSet) return;
    const staticBase = ptr(imageBase);
    const runtimeBase = Process.mainModule!.base;
    this.aslrSlide = runtimeBase.sub(staticBase);
    this.imageBaseSet = true;
  }

  setSessionId(sessionId: string): void {
    this.sessionId = sessionId;
  }

  setSerializationDepth(depth: number): void {
    const clamped = Math.max(1, Math.min(depth, 10));
    this.objectSerializer = new ObjectSerializer(clamped);
  }

  setRateCheck(fn: RateCheckFn): void {
    this.rateCheck = fn;
  }

  installHook(func: FunctionTarget, mode: HookMode = 'full'): boolean {
    if (this.hooks.has(func.address)) {
      return true; // Already hooked
    }
    if (!this.cm) return false;

    const funcId = this.nextFuncId++;

    // funcId << 1 must not overflow signed 32-bit.
    // JS << operates on int32, so (funcId << 1) overflows sign bit at 2^30.
    // Guard at 2^29 to ensure (funcId << 1) | 1 stays positive.
    if (funcId >= (1 << 29)) {
      return false;
    }

    this.funcRegistry.set(funcId, func);

    // Adjust address for ASLR: runtime addr = static addr + slide
    const addr = ptr(func.address).add(this.aslrSlide);

    try {
      // Encode mode in data pointer: data = (funcId << 1) | is_light
      // The CModule's onEnter/onLeave decode this to get func_id and mode.
      const isLight = mode === 'light' ? 1 : 0;
      const data = ptr((funcId << 1) | isLight);

      // Pass CModule directly — Frida uses its exported onEnter/onLeave natively.
      // Type definitions don't include the CModule overload, but it works at runtime.
      const listener = Interceptor.attach(addr, this.cm as any, data);

      this.hooks.set(func.address, { listener, funcId, funcName: func.name });
      return true;
    } catch (_e) {
      // Silently skip functions that can't be hooked
      this.funcRegistry.delete(funcId);
      return false;
    }
  }

  removeHook(address: string): void {
    const entry = this.hooks.get(address);
    if (entry) {
      entry.listener.detach();
      this.funcRegistry.delete(entry.funcId);
      this.hooks.delete(address);
    }
  }

  activeHookCount(): number {
    return this.hooks.size;
  }

  removeAll(): void {
    for (const entry of this.hooks.values()) {
      entry.listener.detach();
    }
    this.hooks.clear();
    this.funcRegistry.clear();
    this.nextFuncId = 1;
    this.threadStacks.clear();

    // Clear watch state
    this.watchCountPtr.writeU32(0);
    this.watchConfigs = [null, null, null, null];
    this.exprWatches = [];
  }

  updateWatches(watches: Array<{
    address: string; size: number; label: string;
    derefDepth: number; derefOffset: number;
    typeKind: string; isGlobal: boolean; onFuncIds?: number[]; onPatterns?: string[];
  }>): void {
    if (watches.length > 4) throw new Error('Max 4 CModule watches');

    // Atomic disable
    this.watchCountPtr.writeU32(0);

    for (let i = 0; i < 4; i++) {
      if (i < watches.length) {
        const w = watches[i];
        const runtimeAddr = ptr(w.address).add(this.aslrSlide);

        // Validate address is readable
        const range = Process.findRangeByAddress(runtimeAddr);
        if (!range || !range.protection.includes('r')) {
          throw new Error(`Watch address ${runtimeAddr} not readable`);
        }

        this.watchAddrsPtr.add(i * 8).writeU64(uint64(runtimeAddr.toString()));
        this.watchSizesPtr.add(i).writeU8(w.size);
        this.watchDerefDepthsPtr.add(i).writeU8(w.derefDepth);
        this.watchDerefOffsetsPtr.add(i * 8).writeU64(uint64(w.derefOffset.toString()));

        // Resolve patterns to funcIds by matching against installed hooks
        let resolvedFuncIds: Set<number>;
        if (w.onPatterns && w.onPatterns.length > 0) {
          resolvedFuncIds = this.matchPatternsToFuncIds(w.onPatterns);
        } else {
          resolvedFuncIds = w.onFuncIds ? new Set(w.onFuncIds) : new Set();
        }

        this.watchConfigs[i] = {
          label: w.label,
          size: w.size,
          typeKind: w.typeKind as WatchConfig['typeKind'],
          // Treat as global if no patterns/funcIds provided or empty set
          isGlobal: w.isGlobal || resolvedFuncIds.size === 0,
          onFuncIds: resolvedFuncIds,
        };
      } else {
        this.watchAddrsPtr.add(i * 8).writeU64(uint64(0));
        this.watchSizesPtr.add(i).writeU8(0);
        this.watchDerefDepthsPtr.add(i).writeU8(0);
        this.watchDerefOffsetsPtr.add(i * 8).writeU64(uint64(0));
        this.watchConfigs[i] = null;
      }
    }

    // Atomic enable
    this.watchCountPtr.writeU32(watches.length);
  }

  // Match patterns against installed hook function names and return matching funcIds
  private matchPatternsToFuncIds(patterns: string[]): Set<number> {
    const matchedIds = new Set<number>();

    // Iterate through all installed hooks
    for (const [_address, entry] of this.hooks) {
      const funcName = entry.funcName;

      // Check if function name matches any pattern
      for (const pattern of patterns) {
        if (this.matchPattern(funcName, pattern)) {
          matchedIds.add(entry.funcId);
          break; // Found a match, no need to check other patterns
        }
      }
    }

    return matchedIds;
  }

  // Simple pattern matching: supports * wildcards and ** for deep matching
  private matchPattern(name: string, pattern: string): boolean {
    // Exact match
    if (name === pattern) return true;

    // @file: pattern - match source file substring
    if (pattern.startsWith('@file:')) {
      // We don't have source file info at runtime, so treat as no match
      // In practice, @file patterns should be resolved at hook installation time
      return false;
    }

    // Pattern with wildcards
    if (pattern.includes('*')) {
      // Step 1: Replace ** with a temporary marker (deep wildcard)
      let regexPattern = pattern.replace(/\*\*/g, '\x00DEEP\x00');

      // Step 2: Escape regex special chars (but not single *)
      const charsToEscape = /[\\.\+\?\^\$\{\}\(\)\|\[\]]/g;
      regexPattern = regexPattern.replace(charsToEscape, '\\$&');

      // Step 3: Convert single * to regex (matches one or more non-colon chars)
      regexPattern = regexPattern.replace(/\*/g, '[^:]+');

      // Step 4: Restore ** marker as .* (matches anything including ::)
      regexPattern = regexPattern.replace(/\x00DEEP\x00/g, '.*');

      const regex = new RegExp(`^${regexPattern}$`);
      return regex.test(name);
    }

    return false;
  }

  updateExprWatches(exprs: Array<{
    expr: string; label: string; isGlobal: boolean; onFuncIds: number[];
  }>): void {
    this.exprWatches = exprs.map(e => ({
      label: e.label,
      expr: e.expr,
      compiledFn: new Function('return ' + e.expr) as () => any,
      // Treat as global if isGlobal is true OR onFuncIds is null/undefined/empty
      isGlobal: e.isGlobal || !e.onFuncIds || e.onFuncIds.length === 0,
      onFuncIds: e.onFuncIds ? new Set(e.onFuncIds) : new Set(),
    }));
  }

  // -----------------------------------------------------------------------
  // Timestamp helpers
  // -----------------------------------------------------------------------

  private initTimebaseInfo(): void {
    // On Apple Silicon, ticks == nanoseconds (ratio 1:1).
    // On Intel, we need mach_timebase_info to convert.
    try {
      const timebaseInfoPtr = Process.getModuleByName('libSystem.B.dylib').getExportByName('mach_timebase_info');
      if (timebaseInfoPtr) {
        // struct mach_timebase_info { uint32_t numer; uint32_t denom; }
        const infoStruct = Memory.alloc(8);
        const machTimebaseInfo = new NativeFunction(timebaseInfoPtr, 'int', ['pointer']);
        machTimebaseInfo(infoStruct);
        const numer = infoStruct.readU32();
        const denom = infoStruct.add(4).readU32();
        if (denom !== 0) {
          this.ticksToNs = numer / denom;
        }
      }
    } catch (_e) {
      // Fall back to ratio 1.0
      this.ticksToNs = 1.0;
    }
  }

  // -----------------------------------------------------------------------
  // Ring buffer drain
  // -----------------------------------------------------------------------

  private drain(): void {
    // Issue 2: bail if sessionId not yet set (setSessionId() hasn't been called)
    if (!this.sessionId) return;

    // Periodic cleanup: clear thread stacks every 50k events to prevent
    // unbounded growth from missed function exits (exception unwinding, ring overflow)
    if (this.eventIdCounter % 50000 === 0) {
      this.threadStacks.clear();
    }

    const writeIdx = this.writeIdxPtr.readU32();
    const readIdx  = this.readIdxPtr.readU32();

    if (writeIdx === readIdx) return; // nothing to drain

    // Issue 3: force unsigned 32-bit subtraction to handle U32 wraparound
    let count = (writeIdx - readIdx) >>> 0;

    // Detect overflow: if count > RING_CAPACITY, we lost entries
    if (count > RING_CAPACITY) {
      // Skip ahead — only read the most recent RING_CAPACITY entries
      count = RING_CAPACITY;
    }

    const events: TraceEvent[] = [];

    for (let i = 0; i < count; i++) {
      const idx = (readIdx + i) % RING_CAPACITY;
      const entryPtr = this.ringDataPtr.add(idx * ENTRY_SIZE);

      // Write-complete marker check disabled — TinyCC doesn't support
      // __atomic_store_n or __sync_synchronize reliably on ARM64.
      // The 10ms drain interval provides sufficient visibility window.

      // Read TraceEntry fields
      const timestamp  = entryPtr.readU64().toNumber();
      const arg0       = entryPtr.add(8).readU64();
      const arg1       = entryPtr.add(16).readU64();
      const retval     = entryPtr.add(24).readU64();
      const funcId     = entryPtr.add(32).readU32();
      const threadId   = entryPtr.add(36).readU32();
      const depth      = entryPtr.add(40).readU32();
      const eventType  = entryPtr.add(44).readU8();
      const sampled    = entryPtr.add(45).readU8();
      const watchEntryCount = entryPtr.add(46).readU8();

      const func = this.funcRegistry.get(funcId);
      if (!func) continue;

      // Hot function detection: check if this call should be recorded
      if (this.rateCheck) {
        const shouldRecord = this.rateCheck(funcId);
        if (!shouldRecord) continue;
      }

      // Resolve thread name (cached)
      let threadName: string | null | undefined;
      if (this.threadNames.has(threadId)) {
        threadName = this.threadNames.get(threadId);
      } else {
        try {
          const found = Process.enumerateThreads()
            .find(t => t.id === threadId)?.name || null;
          this.threadNames.set(threadId, found);
          threadName = found;
        } catch {
          threadName = null;
          this.threadNames.set(threadId, null);
        }
      }

      const eventId = this.generateEventId();
      const timestampNs = Math.round(timestamp * this.ticksToNs);

      // Determine parent event using per-thread depth stacks
      let stack = this.threadStacks.get(threadId);
      if (!stack) {
        stack = [];
        this.threadStacks.set(threadId, stack);
      }

      let parentEventId: string | null = null;

      if (eventType === 0) {
        // function_enter
        // Pop any entries at depth >= current (handles missed exits)
        while (stack.length > 0 && stack[stack.length - 1].depth >= depth) {
          stack.pop();
        }
        // Parent is top of stack (the caller)
        parentEventId = stack.length > 0 ? stack[stack.length - 1].eventId : null;
        // Push ourselves (with timestamp for durationNs computation)
        stack.push({ eventId, depth, timestampNs });

        const event: TraceEvent = {
          id: eventId,
          sessionId: this.sessionId,
          timestampNs,
          threadId,
          threadName,
          parentEventId,
          eventType: 'function_enter',
          functionName: func.name,
          functionNameRaw: func.nameRaw,
          sourceFile: func.sourceFile,
          lineNumber: func.lineNumber,
          arguments: this.serializeArguments(arg0, arg1),
        };
        if (sampled) event.sampled = true;

        // Read watch values
        if (watchEntryCount > 0 || this.exprWatches.length > 0) {
          const watchValues: Record<string, number | string> = {};

          // CModule watches
          for (let w = 0; w < watchEntryCount && w < 4; w++) {
            const cfg = this.watchConfigs[w];
            if (!cfg) continue;
            if (!cfg.isGlobal && !cfg.onFuncIds.has(funcId)) continue;

            const raw = entryPtr.add(48 + w * 8).readU64();
            watchValues[cfg.label] = this.formatWatchValue(raw, cfg);
          }

          // JS expression watches
          for (const ew of this.exprWatches) {
            if (!ew.isGlobal && !ew.onFuncIds.has(funcId)) continue;
            try { watchValues[ew.label] = ew.compiledFn(); }
            catch { watchValues[ew.label] = '<error>'; }
          }

          if (Object.keys(watchValues).length > 0) {
            event.watchValues = watchValues;
          }
        }

        events.push(event);

      } else {
        // function_exit
        // Find and pop our enter event from the stack
        let enterEventId: string | null = null;
        let durationNs: number | undefined;
        if (stack.length > 0 && stack[stack.length - 1].depth === depth) {
          const enterEntry = stack.pop()!;
          enterEventId = enterEntry.eventId;
          // Issue 7: compute durationNs from enter timestamp
          durationNs = timestampNs - enterEntry.timestampNs;
          if (durationNs < 0) durationNs = undefined; // clock skew safety
        }
        // Parent is now the top of stack (our caller)
        parentEventId = enterEventId;

        const event: TraceEvent = {
          id: eventId,
          sessionId: this.sessionId,
          timestampNs,
          threadId,
          threadName,
          parentEventId,
          eventType: 'function_exit',
          functionName: func.name,
          functionNameRaw: func.nameRaw,
          sourceFile: func.sourceFile,
          lineNumber: func.lineNumber,
          returnValue: '0x' + retval.toString(16),
          durationNs,
        };
        if (sampled) event.sampled = true;
        events.push(event);
      }
    }

    // Advance read index
    this.readIdxPtr.writeU32(writeIdx);

    // Emit events
    if (events.length > 0) {
      this.onEvents(events);
    }

    // Adaptive sampling
    this.adaptSampling(count);
  }

  // -----------------------------------------------------------------------
  // Adaptive sampling
  // -----------------------------------------------------------------------

  private adaptSampling(drainedCount: number): void {
    if (drainedCount >= HIGH_THRESHOLD) {
      this.highCycleCount++;
      this.lowCycleCount = 0;
      if (this.highCycleCount >= HIGH_CYCLES_TRIGGER) {
        this.currentSampleInterval = Math.min(
          this.currentSampleInterval * 2,
          MAX_SAMPLE_INTERVAL
        );
        this.sampleIntervalPtr.writeU32(this.currentSampleInterval);
        this.highCycleCount = 0;
      }
    } else if (drainedCount <= LOW_THRESHOLD) {
      this.lowCycleCount++;
      this.highCycleCount = 0;
      if (this.lowCycleCount >= LOW_CYCLES_TRIGGER) {
        this.currentSampleInterval = Math.max(
          Math.floor(this.currentSampleInterval / 2),
          MIN_SAMPLE_INTERVAL
        );
        this.sampleIntervalPtr.writeU32(this.currentSampleInterval);
        this.lowCycleCount = 0;
      }
    } else {
      this.highCycleCount = 0;
      this.lowCycleCount = 0;
    }
  }

  // -----------------------------------------------------------------------
  // Helpers
  // -----------------------------------------------------------------------

  private formatWatchValue(raw: UInt64, cfg: WatchConfig): number | string {
    if (cfg.typeKind === 'float') {
      const buf = new ArrayBuffer(8);
      const view = new DataView(buf);
      if (cfg.size === 4) {
        view.setUint32(0, raw.toNumber(), true);
        return view.getFloat32(0, true);
      } else {
        view.setBigUint64(0, BigInt(raw.toString()), true);
        return view.getFloat64(0, true);
      }
    }
    if (cfg.typeKind === 'int') {
      const n = raw.toNumber();
      if (cfg.size === 1) return (n << 24) >> 24;
      if (cfg.size === 2) return (n << 16) >> 16;
      if (cfg.size === 4) return n | 0;
      return n;
    }
    return raw.toNumber();
  }

  private serializeArguments(arg0: UInt64, arg1: UInt64): string[] {
    if (!this.objectSerializer) {
      return ['0x' + arg0.toString(16), '0x' + arg1.toString(16)];
    }

    const results: string[] = [];
    for (const rawArg of [arg0, arg1]) {
      const addr = ptr(rawArg.toString());
      // Without DWARF type info for arguments, treat as generic pointer
      const typeInfo: TypeInfo = {
        typeKind: 'pointer',
        byteSize: 8,
        typeName: 'void*',
      };
      try {
        const serialized = this.objectSerializer.serialize(addr, typeInfo);
        results.push(typeof serialized === 'string' ? serialized : JSON.stringify(serialized));
      } catch (e) {
        results.push('0x' + rawArg.toString(16));
      }
      this.objectSerializer.reset();
    }
    return results;
  }

  private generateEventId(): string {
    const sid = this.sessionId || 'uninitialized';
    return `${sid}-${++this.eventIdCounter}`;
  }
}
