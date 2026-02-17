# Multi-Language Foundation Implementation Plan

**Spec:** `docs/specs/2026-02-11-python-js-ts-support.md`
**Goal:** Create the abstraction layers (Tracer interface in agent, SymbolResolver trait in daemon) that enable pluggable language support without breaking any existing native functionality.
**Architecture:** Wrap existing CModuleTracer behind a `Tracer` interface, wrap DwarfParser behind a `SymbolResolver` trait, add runtime detection in agent and language detection in daemon. Extend message protocol to support `file:line` targets alongside `address` targets.
**Tech Stack:** TypeScript (agent), Rust (daemon), no new dependencies
**Commit strategy:** Commit at checkpoints (3 commits)

## Workstreams

Serial execution required — agent changes depend on interface design, daemon changes depend on trait design, and integration depends on both.

---

### Task 1: Agent Tracer Interface + NativeTracer Wrapper

**Files:**
- Create: `agent/src/tracers/tracer.ts`
- Create: `agent/src/tracers/native-tracer.ts`
- Modify: `agent/src/agent.ts` (refactor to delegate to Tracer)

**Step 1: Create the Tracer interface**

Create `agent/src/tracers/tracer.ts`:

```typescript
// agent/src/tracers/tracer.ts
// Core tracer interface — all language tracers implement this contract.

export type HookMode = 'full' | 'light';

export interface ResolvedTarget {
  // For native: instruction address (hex)
  address?: string;
  // For interpreted: source location
  file?: string;
  line?: number;
  // Common
  name: string;
  nameRaw?: string;
  sourceFile?: string;
  lineNumber?: number;
}

export interface Tracer {
  // Lifecycle
  initialize(sessionId: string): void;
  dispose(): void;

  // Hook management (resolved targets from daemon)
  installHook(target: ResolvedTarget, mode: HookMode): number | null;
  removeHook(id: number): void;
  removeAllHooks(): void;
  activeHookCount(): number;

  // Breakpoints
  installBreakpoint(msg: BreakpointMessage): void;
  removeBreakpoint(id: string): void;

  // Stepping
  installStepHooks(msg: StepHooksMessage): void;

  // Logpoints
  installLogpoint(msg: LogpointMessage): void;
  removeLogpoint(id: string): void;

  // Variable access
  readVariable(expr: string): any;
  writeVariable(expr: string, value: any): void;

  // Memory access (native only — interpreted languages use readVariable/writeVariable)
  handleReadMemory?(msg: ReadMemoryMessage): void;
  handleWriteMemory?(msg: WriteMemoryMessage): void;

  // ASLR slide (native only, no-op for interpreted)
  setImageBase(imageBase: string): void;
  getSlide(): NativePointer;

  // Runtime resolution fallback (for dynamic functions not in static AST)
  resolvePattern?(pattern: string): ResolvedTarget[];
}

// Message types used by the tracer interface
export interface BreakpointMessage {
  address?: string;
  file?: string;
  line?: number;
  id: string;
  condition?: string;
  hitCount?: number;
  funcName?: string;
  imageBase?: string;
}

export interface StepHooksMessage {
  threadId: number;
  oneShot: Array<{ address: string; noSlide?: boolean }>;
  imageBase?: string;
  returnAddress?: string | null;
}

export interface LogpointMessage {
  address?: string;
  file?: string;
  line?: number;
  id: string;
  message: string;
  condition?: string;
  funcName?: string;
  imageBase?: string;
}

export interface ReadMemoryMessage {
  recipes: any[];
  imageBase?: string;
  poll?: { intervalMs: number; durationMs: number };
}

export interface WriteMemoryMessage {
  targets: any[];
  imageBase?: string;
}
```

**Step 2: Create NativeTracer wrapper**

Create `agent/src/tracers/native-tracer.ts` — this wraps the existing CModuleTracer and breakpoint/stepping/memory logic currently inline in `agent.ts`:

```typescript
// agent/src/tracers/native-tracer.ts
import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage, ReadMemoryMessage, WriteMemoryMessage } from './tracer';
import { CModuleTracer } from '../cmodule-tracer';

export class NativeTracer implements Tracer {
  private tracer: CModuleTracer | null = null;
  private imageBase: NativePointer = ptr(0);
  private slide: NativePointer = ptr(0);
  private agent: any; // Reference to StrobeAgent for delegation

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    // CModuleTracer is lazily created on first hook install (existing behavior)
  }

  dispose(): void {
    if (this.tracer) {
      this.tracer.dispose();
      this.tracer = null;
    }
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    // Delegate to existing CModuleTracer hook installation
    // This is the existing logic from agent.ts handleMessage()
    return this.agent.installNativeHook(target, mode);
  }

  removeHook(id: number): void {
    this.agent.removeNativeHook(id);
  }

  removeAllHooks(): void {
    this.agent.removeAllNativeHooks();
  }

  activeHookCount(): number {
    return this.tracer?.activeHookCount() ?? 0;
  }

  installBreakpoint(msg: BreakpointMessage): void {
    // Delegate to existing setBreakpoint logic
    this.agent.setNativeBreakpoint(msg);
  }

  removeBreakpoint(id: string): void {
    this.agent.removeNativeBreakpoint(id);
  }

  installStepHooks(msg: StepHooksMessage): void {
    this.agent.installNativeStepHooks(msg);
  }

  installLogpoint(msg: LogpointMessage): void {
    this.agent.setNativeLogpoint(msg);
  }

  removeLogpoint(id: string): void {
    this.agent.removeNativeLogpoint(id);
  }

  readVariable(expr: string): any {
    // Native variables go through DWARF → read_memory path, not eval
    throw new Error('Native variables use handleReadMemory, not readVariable');
  }

  writeVariable(expr: string, value: any): void {
    throw new Error('Native variables use handleWriteMemory, not writeVariable');
  }

  handleReadMemory(msg: ReadMemoryMessage): void {
    this.agent.handleNativeReadMemory(msg);
  }

  handleWriteMemory(msg: WriteMemoryMessage): void {
    this.agent.handleNativeWriteMemory(msg);
  }

  setImageBase(imageBase: string): void {
    this.imageBase = ptr(imageBase);
    const moduleBase = Process.mainModule?.base ?? ptr(0);
    this.slide = moduleBase.sub(this.imageBase);
  }

  getSlide(): NativePointer {
    return this.slide;
  }
}
```

**Step 3: Add runtime detection in agent.ts**

Add at top of `agent.ts`:

```typescript
import { Tracer } from './tracers/tracer';
import { NativeTracer } from './tracers/native-tracer';

function detectRuntime(): 'native' | 'cpython' | 'v8' | 'jsc' {
  if (Module.findExportByName(null, '_PyEval_EvalFrameDefault')) return 'cpython';
  if (Module.findExportByName(null, 'Py_Initialize')) return 'cpython';
  // V8: look for v8::Isolate::Current
  if (Module.findExportByName(null, '_ZN2v88internal7Isolate7currentEv')) return 'v8';
  // JSC: look for JSGlobalContextCreate
  if (Module.findExportByName(null, 'JSGlobalContextCreate')) return 'jsc';
  return 'native';
}

function createTracer(runtime: string, agent: StrobeAgent): Tracer {
  switch (runtime) {
    case 'cpython':
      // Will be implemented in Plan 2
      throw new Error(`Python tracer not yet implemented`);
    case 'v8':
      // Will be implemented in Plan 3
      throw new Error(`V8 tracer not yet implemented`);
    case 'jsc':
      // Will be implemented in Plan 3
      throw new Error(`JSC tracer not yet implemented`);
    case 'native':
    default:
      return new NativeTracer(agent);
  }
}
```

**Step 4: Refactor StrobeAgent to use Tracer**

In `agent.ts`, the StrobeAgent constructor creates the tracer:

```typescript
class StrobeAgent {
  public tracer: Tracer;
  private runtime: string;

  constructor() {
    // ... existing initialization ...
    this.runtime = detectRuntime();
    this.tracer = createTracer(this.runtime, this);
    send({ type: 'runtime_detected', runtime: this.runtime });
  }
}
```

The key refactoring is to rename existing methods to `*Native*` variants (e.g., `setBreakpoint` → `setNativeBreakpoint`) and have the public message handlers delegate through `this.tracer`:

```typescript
// Message handler delegates to tracer:
private onSetBreakpoint(msg: any): void {
  this.tracer.installBreakpoint(msg);
}

// Existing breakpoint logic lives in setNativeBreakpoint (unchanged code, just renamed)
public setNativeBreakpoint(msg: BreakpointMessage): void {
  // ... all existing breakpoint code from current setBreakpoint() ...
}
```

This is a pure refactoring — all existing logic moves to `*Native*` methods, the `Tracer` interface provides the dispatch layer.

**Checkpoint:** Agent builds successfully with `npm run build`. All existing native behavior is identical — NativeTracer delegates to the same code paths.

---

### Task 2: Extend Agent Message Protocol for file:line Targets

**Files:**
- Modify: `agent/src/agent.ts` (message handlers accept both address and file:line)

**Step 1: Update hook message handler**

The `onHooksMessage` handler currently expects `functions: [{ address, name }]`. Update to also accept `targets: [{ file, line, name }]`:

```typescript
private handleMessage(message: HookInstruction): void {
  if (message.action === 'add') {
    // Existing: address-based targets (native)
    if (message.functions) {
      for (const func of message.functions) {
        this.tracer.installHook({
          address: func.address,
          name: func.name,
          nameRaw: func.nameRaw,
          sourceFile: func.sourceFile,
          lineNumber: func.lineNumber,
        }, message.mode || 'full');
      }
    }
    // New: file:line targets (interpreted)
    if (message.targets) {
      for (const target of message.targets) {
        this.tracer.installHook({
          file: target.file,
          line: target.line,
          name: target.name,
        }, message.mode || 'full');
      }
    }
  }
  // ... remove logic ...
}
```

**Step 2: Update breakpoint message handler**

Already supports both: `address` (existing) and `file`+`line` (new). The BreakpointMessage interface already has both fields. The NativeTracer ignores file:line, interpreted tracers ignore address.

**Step 3: Add eval_variable message handler**

```typescript
// New message type for interpreted language variable reads
recv('eval_variable', function onEvalVariable(msg: any) {
  recv('eval_variable', onEvalVariable); // Re-register
  try {
    const value = agent.tracer.readVariable(msg.expr);
    send({ type: 'eval_response', label: msg.label || msg.expr, value });
  } catch (e: any) {
    send({ type: 'eval_response', label: msg.label || msg.expr, error: e.message });
  }
});
```

**Step 4: Add runtime resolve message handler**

```typescript
// New message type for agent-side resolution fallback
recv('resolve', function onResolve(msg: any) {
  recv('resolve', onResolve);
  if (agent.tracer.resolvePattern) {
    const targets = [];
    for (const pattern of msg.patterns) {
      targets.push(...agent.tracer.resolvePattern(pattern));
    }
    send({ type: 'resolved', targets });
  } else {
    send({ type: 'resolved', targets: [] });
  }
});
```

**Checkpoint:** Agent builds. Message protocol accepts both formats. Existing native flows unchanged.

---

### Task 3: Daemon SymbolResolver Trait + DwarfResolver Wrapper

**Files:**
- Create: `src/symbols/resolver.rs`
- Create: `src/symbols/dwarf_resolver.rs`
- Modify: `src/symbols/mod.rs` (add modules)
- Modify: `src/lib.rs` (already has `pub mod symbols`)

**Step 1: Define the SymbolResolver trait**

Create `src/symbols/resolver.rs`:

```rust
use std::path::Path;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Native,
    Python,
    JavaScript,
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Native => write!(f, "native"),
            Language::Python => write!(f, "python"),
            Language::JavaScript => write!(f, "javascript"),
        }
    }
}

/// A function/method resolved to a hookable target.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// Native: DWARF-resolved instruction address
    Address {
        address: u64,
        name: String,
        name_raw: Option<String>,
        file: Option<String>,
        line: Option<u32>,
    },
    /// Interpreted: source file + line (agent hooks by location)
    SourceLocation {
        file: String,
        line: u32,
        name: String,
    },
}

impl ResolvedTarget {
    pub fn name(&self) -> &str {
        match self {
            ResolvedTarget::Address { name, .. } => name,
            ResolvedTarget::SourceLocation { name, .. } => name,
        }
    }
}

/// How a variable should be read/written.
#[derive(Debug, Clone)]
pub enum VariableResolution {
    /// DWARF-resolved static address (native) — existing WatchRecipe/ReadRecipe flow
    NativeAddress {
        address: u64,
        size: u8,
        type_kind: crate::dwarf::TypeKind,
        deref_depth: u8,
        deref_offset: u64,
    },
    /// Runtime expression — agent evaluates in target context (Python/JS)
    RuntimeExpression {
        expr: String,
    },
}

/// Trait for language-specific symbol resolution.
/// Implementations: DwarfResolver (native), PythonResolver, JSResolver
pub trait SymbolResolver: Send + Sync {
    /// Resolve a glob pattern to concrete function targets.
    fn resolve_pattern(&self, pattern: &str, project_root: &Path) -> crate::Result<Vec<ResolvedTarget>>;

    /// Resolve file:line to a hookable target.
    fn resolve_line(&self, file: &str, line: u32) -> crate::Result<Option<ResolvedTarget>>;

    /// Resolve a variable name for reading/writing.
    fn resolve_variable(&self, name: &str) -> crate::Result<VariableResolution>;

    /// Image base for ASLR (0 for interpreted languages).
    fn image_base(&self) -> u64;

    /// Language identifier.
    fn language(&self) -> Language;

    /// Whether this resolver supports agent-side fallback for dynamic symbols.
    fn supports_runtime_resolution(&self) -> bool;
}
```

**Step 2: Create DwarfResolver wrapper**

Create `src/symbols/dwarf_resolver.rs`:

```rust
use std::path::Path;
use super::resolver::*;
use crate::dwarf::{DwarfHandle, DwarfParser, TypeKind};

/// Wraps the existing DwarfParser behind the SymbolResolver trait.
pub struct DwarfResolver {
    dwarf: DwarfHandle,
    image_base: u64,
}

impl DwarfResolver {
    pub fn new(dwarf: DwarfHandle, image_base: u64) -> Self {
        Self { dwarf, image_base }
    }

    pub fn dwarf_handle(&self) -> &DwarfHandle {
        &self.dwarf
    }
}

impl SymbolResolver for DwarfResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
        let parser = self.dwarf.get()?;
        let functions = if pattern.starts_with("@file:") {
            let file_pattern = &pattern[6..];
            parser.find_by_source_file(file_pattern)
        } else {
            parser.find_by_pattern(pattern)
        };

        Ok(functions.iter().map(|f| ResolvedTarget::Address {
            address: f.low_pc,
            name: f.name.clone(),
            name_raw: f.name_raw.clone(),
            file: f.source_file.clone(),
            line: f.line_number,
        }).collect())
    }

    fn resolve_line(&self, file: &str, line: u32) -> crate::Result<Option<ResolvedTarget>> {
        let parser = self.dwarf.get()?;
        match parser.resolve_line(file, line) {
            Some((address, actual_line)) => Ok(Some(ResolvedTarget::Address {
                address,
                name: format!("{}:{}", file, actual_line),
                name_raw: None,
                file: Some(file.to_string()),
                line: Some(actual_line),
            })),
            None => Ok(None),
        }
    }

    fn resolve_variable(&self, name: &str) -> crate::Result<VariableResolution> {
        let parser = self.dwarf.get()?;
        // Existing DWARF variable resolution logic
        // This delegates to the existing WatchRecipe / ReadRecipe building
        // For now, return an error — the existing flow bypasses this trait
        Err(crate::Error::Internal(
            "DwarfResolver::resolve_variable not yet wired — use existing DWARF flow".to_string()
        ))
    }

    fn image_base(&self) -> u64 {
        self.image_base
    }

    fn language(&self) -> Language {
        Language::Native
    }

    fn supports_runtime_resolution(&self) -> bool {
        false
    }
}
```

**Step 3: Update symbols/mod.rs**

```rust
mod demangle;
pub mod resolver;
pub mod dwarf_resolver;

pub use demangle::demangle_symbol;
pub use resolver::{Language, ResolvedTarget, VariableResolution, SymbolResolver};
pub use dwarf_resolver::DwarfResolver;
```

**Checkpoint:** `cargo check` passes. No functional changes yet — the trait and wrapper exist but aren't wired into session manager.

---

### Task 4: Language Detection + SessionState Integration

**Files:**
- Modify: `src/daemon/session_manager.rs` (add language field, detect_language, store resolver)

**Step 1: Add language detection function**

```rust
use crate::symbols::{Language, SymbolResolver, DwarfResolver};

/// Detect language from command and project root signals.
pub fn detect_language(command: &str, project_root: &Path) -> Language {
    let cmd_lower = command.to_lowercase();

    // Check command name
    if cmd_lower.contains("python") || command.ends_with(".py") {
        return Language::Python;
    }
    if cmd_lower.contains("node") || cmd_lower.contains("bun")
       || command.ends_with(".js") || command.ends_with(".ts")
       || cmd_lower.contains("npx") || cmd_lower.contains("tsx") {
        return Language::JavaScript;
    }

    // Check project root signals
    if project_root.join("pyproject.toml").exists()
       || project_root.join("requirements.txt").exists()
       || project_root.join("setup.py").exists() {
        return Language::Python;
    }
    if project_root.join("package.json").exists()
       || project_root.join("bun.lockb").exists()
       || project_root.join("deno.json").exists() {
        return Language::JavaScript;
    }

    Language::Native
}
```

**Step 2: Store language per session**

Add `language` field to the session state (wherever sessions are tracked — in the `sessions` HashMap):

```rust
// In the session creation / spawn_with_frida flow:
let language = detect_language(command, &std::path::Path::new(project_root));

// Store with session (exact location depends on existing SessionState struct)
// This is informational for now — used by update_frida_patterns to choose resolution path
```

**Step 3: Report language in runtime_detected message**

When the agent sends `{ type: 'runtime_detected', runtime: '...' }`, the daemon logs it and can validate against its own detection.

**Checkpoint:** `cargo check` passes. Language detection works. No behavioral changes to existing native path.

---

### Task 5: Build Agent + Verify No Regression

**Step 1: Build agent**

```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
```

**Step 2: Build daemon**

```bash
cargo build
```

**Step 3: Run existing tests**

```bash
cargo test --lib  # Unit tests
```

The full e2e tests (frida_e2e, breakpoint_e2e, etc.) should be run to verify no regression.

**Checkpoint:** All existing tests pass. Agent builds with new tracer interface. Foundation is ready for language-specific tracers.

---

### Commit 1: Foundation abstractions

After Task 5, commit:
```
feat: add Tracer interface and SymbolResolver trait for multi-language support

Foundation for Python/JS/TS support:
- Agent: Tracer interface with NativeTracer wrapping existing CModuleTracer
- Agent: Runtime detection (cpython/v8/jsc/native)
- Agent: Message protocol extended for file:line targets
- Daemon: SymbolResolver trait with DwarfResolver wrapping existing DwarfParser
- Daemon: Language enum and detect_language()
- No behavioral changes to existing native path
```

---

### Task 6: Wire SymbolResolver into Pattern Resolution (Optional Stretch)

This task integrates the SymbolResolver into the actual `update_frida_patterns` flow for native sessions. For interpreted languages, the resolver would be used instead of direct DWARF access.

**Note:** This can be deferred to Plan 2 if it risks destabilizing the native path. The key deliverable of Plan 1 is the interface definitions, not the full integration.

**Files:**
- Modify: `src/daemon/session_manager.rs` (update_frida_patterns to use resolver)

The current `update_frida_patterns` calls `dwarf.get()?.find_by_pattern(pattern)` directly. To make it language-aware:

```rust
// In update_frida_patterns:
match session_language {
    Language::Native => {
        // Existing DWARF path (unchanged)
        let functions = dwarf.get()?.find_by_pattern(pattern)?;
        // ... send address-based hooks to agent ...
    }
    Language::Python | Language::JavaScript => {
        // Use resolver to get SourceLocation targets
        let targets = resolver.resolve_pattern(pattern, project_root)?;
        // Send file:line targets to agent
        let target_msg: Vec<_> = targets.iter().map(|t| match t {
            ResolvedTarget::SourceLocation { file, line, name } => {
                serde_json::json!({ "file": file, "line": line, "name": name })
            }
            ResolvedTarget::Address { .. } => unreachable!(),
        }).collect();
        // ... send targets to agent ...
    }
}
```

**Checkpoint:** Native path still works identically. Infrastructure ready for interpreted language resolvers.

### Commit 2: Wire resolver into pattern flow

```
feat: integrate SymbolResolver into pattern resolution flow

update_frida_patterns now dispatches through SymbolResolver based on
session language. Native path unchanged (DwarfResolver delegates to
existing DwarfParser).
```
