/**
 * Recursive object serializer with circular reference detection and depth limiting.
 *
 * Reads memory at runtime via Frida APIs, guided by DWARF type metadata.
 * Replaces raw hex pointer display with structured object inspection.
 */

import { reinterpretAsFloat, signExtend } from './utils.js';

export type TypeInfo = {
  typeKind: 'int' | 'uint' | 'float' | 'pointer' | 'struct' | 'array';
  byteSize: number;
  typeName: string;
  signed?: boolean;
  members?: Array<{ name: string; offset: number; byteSize: number; typeKind: string; typeName?: string }>;
  pointedType?: TypeInfo;
  arrayLength?: number;
  elementType?: TypeInfo;
};

export type SerializedValue = string | number | Record<string, any> | any[];

export class ObjectSerializer {
  private visited: Set<string> = new Set();
  private currentDepth: number = 0;

  constructor(private maxDepth: number) {}

  serialize(address: NativePointer, typeInfo: TypeInfo): SerializedValue {
    const addrStr = address.toString();

    // Circular reference detection
    if (this.visited.has(addrStr)) {
      return `<circular ref to ${addrStr}>`;
    }

    // Depth limit
    if (this.currentDepth >= this.maxDepth) {
      return `<max depth ${this.maxDepth} reached>`;
    }

    this.visited.add(addrStr);
    this.currentDepth++;

    try {
      return this.serializeValue(address, typeInfo);
    } finally {
      this.currentDepth--;
      this.visited.delete(addrStr);
    }
  }

  private serializeValue(address: NativePointer, typeInfo: TypeInfo): SerializedValue {
    switch (typeInfo.typeKind) {
      case 'int':
        return this.readInteger(address, typeInfo.byteSize, typeInfo.signed !== false);

      case 'uint':
        return this.readInteger(address, typeInfo.byteSize, false);

      case 'float':
        return this.readFloat(address, typeInfo.byteSize);

      case 'pointer':
        return this.serializePointer(address, typeInfo);

      case 'struct':
        return this.serializeStruct(address, typeInfo);

      case 'array':
        return this.serializeArray(address, typeInfo);

      default:
        return address.toString();
    }
  }

  private readInteger(addr: NativePointer, size: number, signed: boolean): number {
    switch (size) {
      case 1: {
        const val = addr.readU8();
        return signed ? signExtend(val, 1) : val;
      }
      case 2: {
        const val = addr.readU16();
        return signed ? signExtend(val, 2) : val;
      }
      case 4: {
        const val = addr.readU32();
        return signed ? signExtend(val, 4) : val >>> 0;
      }
      case 8: {
        const val = addr.readU64();
        return Number(val);
      }
      default:
        return 0;
    }
  }

  private readFloat(addr: NativePointer, size: number): number {
    if (size === 4) {
      return reinterpretAsFloat(addr.readU32(), 0, 4);
    }
    // Frida UInt64 -> reinterpret as f64 via two 32-bit halves
    const raw = addr.readU64();
    const lo = raw.and(0xFFFFFFFF).toNumber();
    const hi = raw.shr(32).and(0xFFFFFFFF).toNumber();
    return reinterpretAsFloat(lo, hi, 8);
  }

  private serializePointer(addr: NativePointer, typeInfo: TypeInfo): SerializedValue {
    try {
      // Check 8-byte alignment to prevent SIGBUS on ARM64
      if (addr.and(ptr(7)).toInt32() !== 0) {
        return `<unaligned ptr at ${addr}>`;
      }
      const ptrValue = addr.readU64();
      if (ptrValue.toNumber() === 0) {
        return 'nullptr';
      }

      const targetAddr = ptr(ptrValue.toString());

      // Check if readable
      const range = Process.findRangeByAddress(targetAddr);
      if (!range || !range.protection.includes('r')) {
        return `<invalid ptr ${targetAddr}>`;
      }

      if (typeInfo.pointedType) {
        return this.serialize(targetAddr, typeInfo.pointedType);
      }

      return targetAddr.toString();
    } catch (e) {
      return `<read error: ${e}>`;
    }
  }

  private serializeStruct(addr: NativePointer, typeInfo: TypeInfo): Record<string, any> {
    const result: Record<string, any> = {};

    if (!typeInfo.members || typeInfo.members.length === 0) {
      return { _address: addr.toString() };
    }

    for (const member of typeInfo.members) {
      const memberAddr = addr.add(member.offset);
      const memberTypeInfo: TypeInfo = {
        typeKind: member.typeKind as any,
        byteSize: member.byteSize,
        typeName: member.typeName || 'unknown',
      };

      try {
        result[member.name] = this.serialize(memberAddr, memberTypeInfo);
      } catch (e) {
        result[member.name] = `<error: ${e}>`;
      }
    }

    return result;
  }

  private serializeArray(addr: NativePointer, typeInfo: TypeInfo): any[] {
    const result: any[] = [];
    const length = Math.min(typeInfo.arrayLength || 0, 100); // Cap at 100 elements

    if (!typeInfo.elementType) {
      return [addr.toString()];
    }

    for (let i = 0; i < length; i++) {
      const elementAddr = addr.add(i * typeInfo.elementType.byteSize);
      try {
        result.push(this.serialize(elementAddr, typeInfo.elementType));
      } catch (e) {
        result.push(`<error at index ${i}>`);
        break;
      }
    }

    return result;
  }

  reset(): void {
    this.visited.clear();
    this.currentDepth = 0;
  }
}
