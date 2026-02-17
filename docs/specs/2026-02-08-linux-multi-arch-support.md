# Linux Support & Multi-Arch Platform Abstraction

> Cross-platform agent architecture with clean per-OS adapters. Linux production-ready, Windows-extensible.

## Motivation

Strobe's Rust daemon is already cross-platform (Unix sockets, `object` crate handles ELF/Mach-O, test monitoring has `#[cfg(target_os)]` branches). But the Frida agent — TypeScript injected into the target process — has 3 hardcoded macOS dependencies that crash on Linux:

1. `mach_absolute_time()` in CModule C source (hot path, every trace entry)
2. `libSystem.B.dylib` lookup to resolve `mach_absolute_time` for CModule construction
3. `libSystem.B.dylib` lookup for `write(2)` in output capture

Additionally, `mach_timebase_info()` for timestamp conversion is macOS-only (has try/catch fallback today).

## Design: Platform Adapter Pattern

### New file: `agent/src/platform.ts`

```typescript
export interface PlatformAdapter {
  /** C source preamble implementing `static uint64_t strobe_timestamp(void)` */
  getCModuleTimingPreamble(): string;

  /** Native symbols the CModule timing preamble needs (e.g., mach_absolute_time) */
  getCModuleTimingSymbols(): Record<string, NativePointer>;

  /** Conversion factor: timestamp ticks -> nanoseconds */
  getTicksToNs(): number;

  /** Resolve write(2) for output capture, or null if unavailable */
  resolveWritePtr(): NativePointer | null;
}
```

Selected at agent startup via `Process.platform` (`'darwin'` | `'linux'` | `'windows'`).

### Why a C preamble approach (not swappable function pointers)

`mach_absolute_time()` returns `uint64_t` directly. Linux `clock_gettime()` fills a `struct timespec` and returns `int`. The calling conventions differ, so we can't just swap CModule extern pointers.

Instead, each platform provides a small C preamble that implements `static uint64_t strobe_timestamp(void)`. The shared CModule body calls `strobe_timestamp()` — platform-agnostic. Because `strobe_timestamp()` is `static`, TinyCC can inline it: zero overhead vs the current direct call.

## Platform Implementations

### macOS (Darwin)

**C preamble:**
```c
extern unsigned long long mach_absolute_time(void);
static unsigned long long strobe_timestamp(void) {
  return mach_absolute_time();
}
```

**Symbols:** `{ mach_absolute_time: <resolved from libSystem.B.dylib> }`

**ticksToNs:** Computed via `mach_timebase_info()` — 1.0 on Apple Silicon, varies on Intel.

**write(2):** Resolved from `libSystem.B.dylib`.

### Linux

**C preamble:**
```c
struct timespec { long tv_sec; long tv_nsec; };
extern int clock_gettime(int, struct timespec*);
static unsigned long long strobe_timestamp(void) {
  struct timespec ts;
  clock_gettime(1, &ts);  /* CLOCK_MONOTONIC = 1 */
  return (unsigned long long)ts.tv_sec * 1000000000ULL
       + (unsigned long long)ts.tv_nsec;
}
```

**Symbols:** `{ clock_gettime: <resolved from libc.so.6, fallback librt.so> }`

On older glibc (<2.17), `clock_gettime` lives in `librt.so`. Resolution strategy:
1. Try `Process.getModuleByName('libc.so.6').getExportByName('clock_gettime')`
2. Fallback: `Module.findExportByName(null, 'clock_gettime')` (searches all loaded modules)

**ticksToNs:** Always `1.0` — `CLOCK_MONOTONIC` returns nanoseconds directly.

**write(2):** Resolved from `libc.so.6`, with fallback `Module.findExportByName(null, 'write')`.

### Windows (future, not implemented now)

Would implement `QueryPerformanceCounter` / `QueryPerformanceFrequency` in the preamble, resolve from `kernel32.dll`. The adapter pattern makes this a single new class with no changes to CModule body or agent.ts.

## CModule Changes

The shared `CMODULE_SOURCE` constant changes minimally:

**Remove:** Line 91 `extern unsigned long long mach_absolute_time(void);`
**Replace:** Line 133 `e->timestamp = mach_absolute_time();` → `e->timestamp = strobe_timestamp();`

The platform-specific preamble is prepended at CModule construction time:

```typescript
const fullSource = platform.getCModuleTimingPreamble() + '\n' + CMODULE_SOURCE;
this.cm = new CModule(fullSource, {
  ...platform.getCModuleTimingSymbols(),
  write_idx: this.writeIdxPtr,
  // ... rest of shared symbols unchanged
});
```

## Agent Changes

### `cmodule-tracer.ts`

- Constructor accepts `PlatformAdapter` parameter
- Remove hardcoded `libSystem.B.dylib` / `mach_absolute_time` resolution from constructor
- Remove `initTimebaseInfo()` method — replaced by `platform.getTicksToNs()`
- CModule source construction uses platform preamble

### `agent.ts`

- Import and instantiate platform adapter at module level
- Pass platform to `CModuleTracer` constructor
- `installOutputCapture()`: use `platform.resolveWritePtr()` instead of hardcoded `libSystem.B.dylib`

## Rust-Side Changes

### `src/dwarf/parser.rs` — dSYM lookup gate

Lines 108-124 unconditionally check for `.dSYM` bundles (macOS concept). Gate with `#[cfg(target_os = "macos")]`:

```rust
// On macOS, check for .dSYM bundle
#[cfg(target_os = "macos")]
{
    let dsym_path = binary_path.with_extension("dSYM");
    if dsym_path.exists() {
        if let Some(binary_name) = binary_path.file_name() {
            let dwarf_file = dsym_path
                .join("Contents").join("Resources").join("DWARF").join(binary_name);
            if dwarf_file.exists() {
                let mut parser = Self::parse_file(&dwarf_file)?;
                parser.image_base = image_base;
                return Ok(parser);
            }
        }
    }
}
```

No Linux equivalent needed — Linux debug info is typically embedded in the ELF. The `object` crate handles this transparently.

### `extract_image_base` — already cross-platform

The existing logic checks for `__TEXT` (Mach-O) first, then falls back to first non-zero segment address (works for ELF LOAD segments). No change needed.

## File Summary

| File | Change | Effort |
|------|--------|--------|
| `agent/src/platform.ts` (new) | `PlatformAdapter` interface + `DarwinPlatform` + `LinuxPlatform` | Medium |
| `agent/src/cmodule-tracer.ts` | Accept platform adapter, use preamble/symbols/ticksToNs | Medium |
| `agent/src/agent.ts` | Instantiate platform, pass to tracer, use for write(2) | Small |
| `src/dwarf/parser.rs` | Gate dSYM lookup with `#[cfg(target_os = "macos")]` | Trivial |

No new Rust dependencies. No new npm dependencies.

## Testing

- Agent compiles: `cd agent && npm run build`
- Rust compiles: `cargo build`
- Existing `debug_test` suite passes (macOS — verifies no regressions)
- Manual Linux validation: launch a simple binary, verify output capture + function tracing
