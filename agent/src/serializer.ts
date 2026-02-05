const MAX_STRING_LENGTH = 1024;
const MAX_ARRAY_LENGTH = 100;
const MAX_DEPTH = 1;

export class Serializer {
  serialize(value: NativePointer | any, depth: number = 0): any {
    if (value === null || value === undefined) {
      return null;
    }

    // Handle NativePointer
    if (value instanceof NativePointer) {
      if (value.isNull()) {
        return null;
      }
      return `0x${value.toString(16)}`;
    }

    // Primitives
    if (typeof value === 'number' || typeof value === 'boolean') {
      return value;
    }

    if (typeof value === 'string') {
      return this.truncateString(value);
    }

    // Stop at max depth
    if (depth >= MAX_DEPTH) {
      return this.formatTypeRef(value);
    }

    // Arrays
    if (Array.isArray(value)) {
      return value
        .slice(0, MAX_ARRAY_LENGTH)
        .map(item => this.serialize(item, depth + 1));
    }

    // Objects (structs)
    if (typeof value === 'object') {
      const result: Record<string, any> = {};
      let count = 0;

      for (const key of Object.keys(value)) {
        if (count >= MAX_ARRAY_LENGTH) break;
        result[key] = this.serialize(value[key], depth + 1);
        count++;
      }

      return result;
    }

    return String(value);
  }

  private truncateString(s: string): string {
    if (s.length <= MAX_STRING_LENGTH) {
      return s;
    }
    return s.slice(0, MAX_STRING_LENGTH) + '...';
  }

  private formatTypeRef(value: any): string {
    const typeName = value?.constructor?.name || typeof value;
    if (value instanceof NativePointer) {
      return `<${typeName} at ${value}>`;
    }
    return `<${typeName}>`;
  }
}
