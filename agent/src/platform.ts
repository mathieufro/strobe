/**
 * Platform abstraction for OS-specific Frida agent behavior.
 * Selected at startup via Process.platform.
 */

import { findGlobalExport } from './utils.js';

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
  private ticksToNs_: number = 1.0;

  constructor() {
    this.libSystem = Process.getModuleByName('libSystem.B.dylib');
    this.ticksToNs_ = this.computeTimebaseRatio();
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
    return this.ticksToNs_;
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
        // struct mach_timebase_info { uint32_t numer; uint32_t denom; }
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
      clockGettime = findGlobalExport('clock_gettime');
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
    return findGlobalExport('write');
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
