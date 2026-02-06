interface FunctionTarget {
  address: string;
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

type EnterCallback = (
  threadId: number,
  func: FunctionTarget,
  args: NativePointer[]
) => string;

type LeaveCallback = (
  threadId: number,
  func: FunctionTarget,
  retval: NativePointer,
  enterEventId: string,
  enterTimestampNs: number
) => void;

export class HookInstaller {
  private hooks: Map<string, InvocationListener> = new Map();
  private onEnter: EnterCallback;
  private onLeave: LeaveCallback;
  private aslrSlide: NativePointer = ptr(0);
  private imageBaseSet: boolean = false;

  constructor(onEnter: EnterCallback, onLeave: LeaveCallback) {
    this.onEnter = onEnter;
    this.onLeave = onLeave;
  }

  setImageBase(imageBase: string): void {
    if (this.imageBaseSet) return;
    const staticBase = ptr(imageBase);
    const runtimeBase = Process.mainModule!.base;
    this.aslrSlide = runtimeBase.sub(staticBase);
    this.imageBaseSet = true;
  }

  installHook(func: FunctionTarget): boolean {
    if (this.hooks.has(func.address)) {
      return true; // Already hooked
    }

    // Adjust address for ASLR: runtime addr = static addr + slide
    const addr = ptr(func.address).add(this.aslrSlide);
    const self = this;

    try {
      const listener = Interceptor.attach(addr, {
        onEnter(args) {
          const threadId = Process.getCurrentThreadId();
          const argsArray: NativePointer[] = [];

          // Capture first 10 arguments (reasonable limit)
          for (let i = 0; i < 10; i++) {
            try {
              argsArray.push(args[i]);
            } catch {
              break;
            }
          }

          const eventId = self.onEnter(threadId, func, argsArray);

          // Store context for onLeave
          (this as any).eventId = eventId;
          (this as any).enterTimestampNs = Date.now() * 1000000;
        },

        onLeave(retval) {
          const threadId = Process.getCurrentThreadId();
          const eventId = (this as any).eventId;
          const enterTimestampNs = (this as any).enterTimestampNs;

          self.onLeave(threadId, func, retval, eventId, enterTimestampNs);
        }
      });

      this.hooks.set(func.address, listener);
      return true;
    } catch (e) {
      // Silently skip functions that can't be hooked (too small, non-executable, etc.)
      return false;
    }
  }

  removeHook(address: string): void {
    const listener = this.hooks.get(address);
    if (listener) {
      listener.detach();
      this.hooks.delete(address);
    }
  }

  activeHookCount(): number {
    return this.hooks.size;
  }

  removeAll(): void {
    for (const listener of this.hooks.values()) {
      listener.detach();
    }
    this.hooks.clear();
  }
}
