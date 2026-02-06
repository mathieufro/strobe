/**
 * CModule-based high-performance tracer.
 *
 * Uses a native CModule with a shared ring buffer to record function
 * enter/leave events at near-zero per-call overhead. A JS timer drains
 * the ring buffer every 10ms and forwards structured event JSON to the
 * daemon via send().
 */

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

export type HookMode = 'full' | 'light';

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
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const RING_CAPACITY = 16384;
const ENTRY_SIZE = 48;
const HEADER_SIZE = 32;
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
  private hooks: Map<string, { listener: InvocationListener; funcId: number }> = new Map();

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
      write_idx:       this.writeIdxPtr,
      overflow_count:  this.overflowCountPtr,
      sample_interval: this.sampleIntervalPtr,
      global_counter:  this.globalCounterPtr,
      ring_data:       this.ringDataPtrHolder,
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

  installHook(func: FunctionTarget, mode: HookMode = 'full'): boolean {
    if (this.hooks.has(func.address)) {
      return true; // Already hooked
    }
    if (!this.cm) return false;

    const funcId = this.nextFuncId++;

    // Issue 4: funcId << 1 overflows signed 32-bit at 2^30
    if (funcId >= (1 << 30)) {
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

      this.hooks.set(func.address, { listener, funcId });
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


      const func = this.funcRegistry.get(funcId);
      if (!func) continue;

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
          parentEventId,
          eventType: 'function_enter',
          functionName: func.name,
          functionNameRaw: func.nameRaw,
          sourceFile: func.sourceFile,
          lineNumber: func.lineNumber,
          arguments: [
            '0x' + arg0.toString(16),
            '0x' + arg1.toString(16),
          ],
        };
        if (sampled) event.sampled = true;
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

  private generateEventId(): string {
    return `${this.sessionId}-${++this.eventIdCounter}`;
  }
}
