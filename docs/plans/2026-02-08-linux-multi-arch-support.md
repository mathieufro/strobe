# Linux & Multi-Arch Platform Abstraction — Implementation Plan

**Spec:** `docs/specs/2026-02-08-linux-multi-arch-support.md`
**Goal:** Remove 3 hardcoded macOS dependencies from the Frida agent so Strobe runs on Linux; gate dSYM lookup on macOS in Rust.
**Architecture:** New `PlatformAdapter` interface in `agent/src/platform.ts` selected at startup via `Process.platform`. Each platform provides a C timing preamble, symbol resolution, ticksToNs, and write(2) pointer. CModule body becomes platform-agnostic by calling `strobe_timestamp()`.
**Tech Stack:** TypeScript (Frida agent), Rust (`#[cfg]` gates)
**Commit strategy:** Single commit at end

## Workstreams

Serial execution required — each task builds on the previous:
- Task 1: Create `platform.ts` (interface + Darwin + Linux implementations)
- Task 2: Update `cmodule-tracer.ts` to use platform adapter
- Task 3: Update `agent.ts` to instantiate platform and use it for write(2)
- Task 4: Gate dSYM lookup in `src/dwarf/parser.rs`
- Task 5: Build + verify

---

### Task 1: Create `agent/src/platform.ts`

**Files:**
- Create: `agent/src/platform.ts`

**Step 1: Write the platform adapter module**

```typescript
// agent/src/platform.ts

/**
 * Platform abstraction for OS-specific Frida agent behavior.
 * Selected at startup via Process.platform.
 */

export interface PlatformAdapter {
  /** C source preamble implementing `static uint64_t strobe_timestamp(void)` */
  getCModuleTimingPreamble(): string;

  /** Native symbols the CModule timing preamble needs */
  getCModuleTimingSymbols(): Record<string, NativePointer>;

  /** Conversion factor: timestamp ticks -> nanoseconds */
  getTicksToNs(): number;

  /** Resolve write(2) for output capture, or null if unavailable */
  resolveWritePtr(): NativePointer | null;
}

class DarwinPlatform implements PlatformAdapter {
  private libSystem: Module;
  private ticksToNs: number = 1.0;

  constructor() {
    this.libSystem = Process.getModuleByName('libSystem.B.dylib');
    this.ticksToNs = this.computeTimebaseRatio();
  }

  getCModuleTimingPreamble(): string {
    return `
extern unsigned long long mach_absolute_time(void);
static unsigned long long strobe_timestamp(void) {
  return mach_absolute_time();
}
`;
  }

  getCModuleTimingSymbols(): Record<string, NativePointer> {
    return {
      mach_absolute_time: this.libSystem.getExportByName('mach_absolute_time')!,
    };
  }

  getTicksToNs(): number {
    return this.ticksToNs;
  }

  resolveWritePtr(): NativePointer | null {
    try {
      return this.libSystem.getExportByName('write');
    } catch {
      return null;
    }
  }

  private computeTimebaseRatio(): number {
    try {
      const timebaseInfoPtr = this.libSystem.getExportByName('mach_timebase_info');
      if (timebaseInfoPtr) {
        const infoStruct = Memory.alloc(8);
        const machTimebaseInfo = new NativeFunction(timebaseInfoPtr, 'int', ['pointer']);
        machTimebaseInfo(infoStruct);
        const numer = infoStruct.readU32();
        const denom = infoStruct.add(4).readU32();
        if (denom !== 0) {
          return numer / denom;
        }
      }
    } catch {
      // Fall back to ratio 1.0
    }
    return 1.0;
  }
}

class LinuxPlatform implements PlatformAdapter {
  getCModuleTimingPreamble(): string {
    return `
struct timespec { long tv_sec; long tv_nsec; };
extern int clock_gettime(int, struct timespec*);
static unsigned long long strobe_timestamp(void) {
  struct timespec ts;
  clock_gettime(1, &ts);  /* CLOCK_MONOTONIC = 1 */
  return (unsigned long long)ts.tv_sec * 1000000000ULL
       + (unsigned long long)ts.tv_nsec;
}
`;
  }

  getCModuleTimingSymbols(): Record<string, NativePointer> {
    // Try libc.so.6 first (glibc >= 2.17), fallback to global search
    let clockGettime: NativePointer | null = null;
    try {
      clockGettime = Process.getModuleByName('libc.so.6').getExportByName('clock_gettime');
    } catch {
      // Older glibc or musl — try global search
    }
    if (!clockGettime) {
      clockGettime = Module.findExportByName(null, 'clock_gettime');
    }
    if (!clockGettime) {
      throw new Error('Cannot resolve clock_gettime — required for Linux tracing');
    }
    return { clock_gettime: clockGettime };
  }

  getTicksToNs(): number {
    return 1.0; // CLOCK_MONOTONIC returns nanoseconds directly
  }

  resolveWritePtr(): NativePointer | null {
    try {
      return Process.getModuleByName('libc.so.6').getExportByName('write');
    } catch {
      // Fallback: global search
    }
    return Module.findExportByName(null, 'write');
  }
}

/** Create the platform adapter for the current OS. */
export function createPlatformAdapter(): PlatformAdapter {
  switch (Process.platform) {
    case 'darwin':
      return new DarwinPlatform();
    case 'linux':
      return new LinuxPlatform();
    default:
      throw new Error(`Unsupported platform: ${Process.platform}`);
  }
}
```

**Checkpoint:** New file exists with interface + two implementations. No other files changed yet, so agent still compiles unchanged.

---

### Task 2: Update `cmodule-tracer.ts` to use platform adapter

**Files:**
- Modify: [cmodule-tracer.ts:10](agent/src/cmodule-tracer.ts#L10) (add import)
- Modify: [cmodule-tracer.ts:87-227](agent/src/cmodule-tracer.ts#L87-L227) (CMODULE_SOURCE constant)
- Modify: [cmodule-tracer.ts:309-372](agent/src/cmodule-tracer.ts#L309-L372) (constructor)
- Delete: [cmodule-tracer.ts:607-627](agent/src/cmodule-tracer.ts#L607-L627) (`initTimebaseInfo` method)

**Step 1: Add import**

At the top of the file, add:
```typescript
import { PlatformAdapter } from './platform.js';
```

**Step 2: Remove macOS-specific line from CMODULE_SOURCE**

Remove line 91 (`extern unsigned long long mach_absolute_time(void);`) from the `CMODULE_SOURCE` constant.

Replace line 133 (`e->timestamp = mach_absolute_time();`) with:
```c
  e->timestamp = strobe_timestamp();
```

The platform-specific preamble (which defines `strobe_timestamp()`) will be prepended at CModule construction time.

**Step 3: Update constructor to accept `PlatformAdapter`**

Change constructor signature from:
```typescript
constructor(onEvents: (events: any[]) => void) {
```
to:
```typescript
constructor(onEvents: (events: any[]) => void, platform: PlatformAdapter) {
```

Replace the hardcoded libSystem/mach_absolute_time resolution block (lines 347-368) with:

```typescript
    // --- Compute ticksToNs from platform ---
    this.ticksToNs = platform.getTicksToNs();

    // --- Create CModule with platform-specific timing preamble ---
    const fullSource = platform.getCModuleTimingPreamble() + CMODULE_SOURCE;
    this.cm = new CModule(fullSource, {
      ...platform.getCModuleTimingSymbols(),
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
```

**Step 4: Remove `initTimebaseInfo()` method**

Delete the entire `initTimebaseInfo()` method (lines 607-627). It's fully replaced by `platform.getTicksToNs()`.

**Checkpoint:** `cmodule-tracer.ts` now accepts a `PlatformAdapter` and has no macOS-specific code. Compilation will fail until `agent.ts` is updated (next task).

---

### Task 3: Update `agent.ts` to use platform adapter

**Files:**
- Modify: [agent.ts:1](agent/src/agent.ts#L1) (add import)
- Modify: [agent.ts:119-123](agent/src/agent.ts#L119-L123) (constructor)
- Modify: [agent.ts:532-601](agent/src/agent.ts#L532-L601) (`installOutputCapture`)

**Step 1: Add platform import**

Add at the top of the file:
```typescript
import { createPlatformAdapter, type PlatformAdapter } from './platform.js';
```

**Step 2: Store platform adapter and pass to CModuleTracer**

Add a field to `StrobeAgent`:
```typescript
private platform: PlatformAdapter;
```

Update the constructor:
```typescript
constructor() {
    this.platform = createPlatformAdapter();
    this.tracer = new CModuleTracer((events) => {
      send({ type: 'events', events });
    }, this.platform);
    // ... rest unchanged
```

**Step 3: Update `installOutputCapture()` to use platform adapter**

Replace the hardcoded `libSystem.B.dylib` resolution:
```typescript
private installOutputCapture(): void {
    const self = this;
    const writePtr = this.platform.resolveWritePtr();
    if (!writePtr) return;
    // ... rest of method unchanged (Interceptor.attach(writePtr, { ... }))
```

**Checkpoint:** Agent compiles and runs. All macOS-specific code is now encapsulated in `DarwinPlatform`. Linux uses `LinuxPlatform`.

---

### Task 4: Gate dSYM lookup in `src/dwarf/parser.rs`

**Files:**
- Modify: [parser.rs:108-124](src/dwarf/parser.rs#L108-L124) (dSYM lookup block)

**Step 1: Wrap dSYM lookup with `#[cfg(target_os = "macos")]`**

Replace lines 108-124:
```rust
        // On macOS, check for .dSYM bundle
        let dsym_path = binary_path.with_extension("dSYM");
        if dsym_path.exists() {
            // The actual DWARF is in Contents/Resources/DWARF/<binary_name>
            if let Some(binary_name) = binary_path.file_name() {
                let dwarf_file = dsym_path
                    .join("Contents")
                    .join("Resources")
                    .join("DWARF")
                    .join(binary_name);
                if dwarf_file.exists() {
                    let mut parser = Self::parse_file(&dwarf_file)?;
                    parser.image_base = image_base;
                    return Ok(parser);
                }
            }
        }
```

With:
```rust
        // On macOS, check for .dSYM bundle (Linux debug info is embedded in ELF)
        #[cfg(target_os = "macos")]
        {
            let dsym_path = binary_path.with_extension("dSYM");
            if dsym_path.exists() {
                if let Some(binary_name) = binary_path.file_name() {
                    let dwarf_file = dsym_path
                        .join("Contents")
                        .join("Resources")
                        .join("DWARF")
                        .join(binary_name);
                    if dwarf_file.exists() {
                        let mut parser = Self::parse_file(&dwarf_file)?;
                        parser.image_base = image_base;
                        return Ok(parser);
                    }
                }
            }
        }
```

**Checkpoint:** Rust compiles. dSYM lookup only runs on macOS. ELF binaries on Linux fall through to the embedded debug info path correctly (the `object` crate handles both formats).

---

### Task 5: Build and verify

**Step 1: Build agent**
```bash
cd agent && npm run build && cd ..
```
Expected: Compiles successfully, produces `dist/agent.js`.

**Step 2: Touch spawner for include_str! cache invalidation**
```bash
touch src/frida_collector/spawner.rs
```

**Step 3: Build Rust daemon**
```bash
cargo build
```
Expected: Compiles successfully with no warnings.

**Step 4: Run existing test suite**

Use `debug_test` to run all tests and verify no regressions on macOS.

**Checkpoint:** All existing tests pass. No regressions. Feature is complete.
