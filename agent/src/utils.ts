/**
 * Shared utilities for float reinterpretation and sign extension.
 *
 * Used by both CModuleTracer (watch value formatting) and
 * ObjectSerializer (memory reads) to avoid duplicated logic.
 */

export function reinterpretAsFloat(lo: number, hi: number, size: 4 | 8): number {
  const buf = new ArrayBuffer(8);
  const view = new DataView(buf);
  view.setUint32(0, lo, true);
  if (size === 8) view.setUint32(4, hi, true);
  return size === 4 ? view.getFloat32(0, true) : view.getFloat64(0, true);
}

export function signExtend(value: number, byteSize: number): number {
  if (byteSize === 1) return (value << 24) >> 24;
  if (byteSize === 2) return (value << 16) >> 16;
  if (byteSize === 4) return value | 0;
  return value;
}

/**
 * Find an export across all loaded modules.
 * Replaces Module.findExportByName(null, name) which was removed as a
 * static method in Frida 17.x.
 */
export function findGlobalExport(exportName: string): NativePointer | null {
  for (const m of Process.enumerateModules()) {
    const addr = m.findExportByName(exportName);
    if (addr !== null) return addr;
  }
  return null;
}
