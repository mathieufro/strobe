# Multi-Language Plan 3: JavaScript/TypeScript Support (Node.js + Bun)

**Spec:** `docs/specs/2026-02-11-python-js-ts-support.md`
**Depends on:** Plan 1 (Foundation) must be complete. Plan 2 (Python) is independent.
**Goal:** Full Phase 1+2 feature parity for Node.js (V8) and Bun (JSC): function tracing, breakpoints, logpoints, stepping, watches, source maps, test adapters, e2e tests.
**Architecture:** JSResolver (SWC parser + source maps) in daemon, V8Tracer (Inspector Protocol) + JSCTracer (JSC hooks) in agent, VitestAdapter + JestAdapter + BunTestAdapter for test runner.
**Tech Stack:** Rust (swc_ecma_parser, swc_common, sourcemap, walkdir), TypeScript (Frida agent)
**Commit strategy:** Commit at checkpoints (7 commits)

## Workstreams

- **Stream A (daemon resolver):** Tasks 1, 2 — JSResolver, Rust-only
- **Stream B (Node.js agent tracer):** Tasks 3, 4 — V8Tracer, TypeScript
- **Stream C (Bun agent tracer):** Task 5 — JSCTracer, TypeScript (depends on Stream B for shared js-common.ts)
- **Stream D (test adapters):** Tasks 6, 7, 8 — Rust, independent of agent
- **Stream E (fixtures):** Tasks 9, 10, 11 — TS/JS files, no code dependency
- **Serial:** Tasks 12, 13, 14 (e2e + web apps + polish — depends on all streams)

---

### Task 1: Add Rust Dependencies

**Files:**
- Modify: `Cargo.toml`

```toml
# JavaScript/TypeScript AST parsing
swc_ecma_parser = "5.0"
swc_common = "3.0"
swc_ecma_ast = "3.0"
# Source map parsing (TS→JS line mapping)
sourcemap = "9.0"
# walkdir already added in Plan 2
```

**Verify:**
```bash
cargo check
```

**Note:** SWC crate versions should be checked against crates.io — the version numbers in the spec are estimates. Use the latest compatible versions.

**Checkpoint:** Dependencies resolve.

---

### Task 2: JSResolver Implementation

**Files:**
- Create: `src/symbols/js_resolver.rs`
- Modify: `src/symbols/mod.rs` (add module)

**Step 1: Write unit tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_functions_basic() {
        let source = r#"
function hello() { console.log("hi"); }
const greet = (name: string) => `Hello, ${name}`;
export function processBuffer(buf: number[]): number { return 0; }
"#;
        let functions = extract_js_functions_from_source(source, Path::new("app.ts"), true).unwrap();
        assert!(functions.contains_key("hello"));
        assert!(functions.contains_key("greet"));
        assert!(functions.contains_key("processBuffer"));
    }

    #[test]
    fn test_extract_class_methods() {
        let source = r#"
export class AudioProcessor {
    processBuffer(buf: number[]): number { return 0; }
    static defaultRate(): number { return 44100; }
    private init(): void {}
}
"#;
        let functions = extract_js_functions_from_source(source, Path::new("audio.ts"), true).unwrap();
        assert!(functions.contains_key("AudioProcessor.processBuffer"));
        assert!(functions.contains_key("AudioProcessor.defaultRate"));
        assert!(functions.contains_key("AudioProcessor.init"));
    }

    #[test]
    fn test_extract_arrow_functions() {
        let source = r#"
export const process = (data: any) => { return data; };
const handler = async (req: Request) => { return new Response("ok"); };
"#;
        let functions = extract_js_functions_from_source(source, Path::new("handler.ts"), true).unwrap();
        assert!(functions.contains_key("process"));
        assert!(functions.contains_key("handler"));
    }

    #[test]
    fn test_pattern_matching() {
        let resolver = JSResolver::from_functions(vec![
            ("AudioProcessor.processBuffer".to_string(), ("src/audio.ts".into(), 10)),
            ("AudioProcessor.generateSine".to_string(), ("src/audio.ts".into(), 20)),
            ("MidiHandler.noteOn".to_string(), ("src/midi.ts".into(), 5)),
        ]);
        let targets = resolver.resolve_pattern("AudioProcessor.*", Path::new(".")).unwrap();
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn test_excluded_directories() {
        assert!(is_js_excluded("node_modules"));
        assert!(is_js_excluded("dist"));
        assert!(is_js_excluded(".next"));
        assert!(is_js_excluded("coverage"));
        assert!(!is_js_excluded("src"));
    }
}
```

**Step 2: Implement JSResolver**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use super::resolver::*;

pub struct JSResolver {
    /// Parsed function/method definitions: qualified_name → (file_path, line_number)
    functions: HashMap<String, (PathBuf, u32)>,
    /// Source maps: .js file → source map data (for TS→JS mapping)
    source_maps: HashMap<PathBuf, sourcemap::SourceMap>,
}

fn is_js_excluded(name: &str) -> bool {
    matches!(name,
        "node_modules" | "dist" | "build" | ".next" | ".nuxt" |
        "coverage" | ".git" | "__tests__" | "__mocks__" |
        ".cache" | ".turbo"
    )
}

fn is_js_ts_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ts" | "tsx" | "js" | "jsx" | "mts" | "mjs")
    )
}

/// Extract function/class method definitions from JS/TS source.
pub fn extract_js_functions_from_source(
    source: &str,
    file_path: &Path,
    is_typescript: bool,
) -> crate::Result<HashMap<String, (PathBuf, u32)>> {
    use swc_common::{FileName, SourceMap as SwcSourceMap};
    use swc_ecma_parser::{Syntax, TsSyntax, EsSyntax};

    let cm = SwcSourceMap::default();
    let fm = cm.new_source_file(
        FileName::Custom(file_path.to_string_lossy().to_string()).into(),
        source.to_string(),
    );

    let syntax = if is_typescript {
        Syntax::Typescript(TsSyntax { tsx: true, ..Default::default() })
    } else {
        Syntax::Es(EsSyntax { jsx: true, ..Default::default() })
    };

    let module = swc_ecma_parser::parse_file_as_module(&fm, syntax, Default::default(), None, &mut vec![])
        .map_err(|e| crate::Error::Internal(format!("JS/TS parse error in {:?}: {:?}", file_path, e)))?;

    let mut functions = HashMap::new();
    extract_from_module_items(&module.body, file_path, &[], &mut functions, &cm);
    Ok(functions)
}

/// Walk AST module items extracting function declarations, arrow functions,
/// class methods, and exported functions.
fn extract_from_module_items(
    items: &[swc_ecma_ast::ModuleItem],
    file_path: &Path,
    prefix: &[String],
    functions: &mut HashMap<String, (PathBuf, u32)>,
    cm: &swc_common::SourceMap,
) {
    // Implementation walks:
    // - FnDecl: add name to functions
    // - VarDecl with ArrowExpr or FnExpr initializer: add binding name
    // - ClassDecl: recurse into class body with class name as prefix
    // - ExportDecl: unwrap and recurse
    // - ExportDefaultDecl: add as "default"
    // Line numbers extracted from span via cm.lookup_char_pos()
    todo!("Implement SWC AST walking — see spec for logic")
}

impl JSResolver {
    /// Parse all JS/TS files in project_root.
    pub fn parse(project_root: &Path) -> crate::Result<Self> {
        let mut all_functions = HashMap::new();
        let mut source_maps = HashMap::new();

        for entry in WalkDir::new(project_root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_str().unwrap_or("");
                !is_js_excluded(name)
            })
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            if is_js_ts_file(path) {
                let is_ts = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("ts" | "tsx" | "mts")
                );
                match std::fs::read_to_string(path) {
                    Ok(source) => {
                        if let Ok(fns) = extract_js_functions_from_source(&source, path, is_ts) {
                            // Qualify with module path relative to project root
                            for (name, (file, line)) in fns {
                                all_functions.insert(name, (file, line));
                            }
                        }
                    }
                    Err(_) => continue,
                }
            }

            // Parse .map source map files
            if path.extension().and_then(|e| e.to_str()) == Some("map") {
                if let Ok(content) = std::fs::read_to_string(path) {
                    if let Ok(sm) = sourcemap::SourceMap::from_reader(content.as_bytes()) {
                        source_maps.insert(path.to_path_buf(), sm);
                    }
                }
            }
        }

        Ok(Self { functions: all_functions, source_maps })
    }

    /// Create from pre-built function list (for testing).
    #[cfg(test)]
    pub fn from_functions(fns: Vec<(String, (PathBuf, u32))>) -> Self {
        Self {
            functions: fns.into_iter().collect(),
            source_maps: HashMap::new(),
        }
    }
}

impl SymbolResolver for JSResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
        if pattern.starts_with("@file:") {
            let file_substr = &pattern[6..];
            return Ok(self.functions.iter()
                .filter(|(_, (file, _))| file.to_string_lossy().contains(file_substr))
                .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                    file: file.to_string_lossy().to_string(),
                    line: *line,
                    name: name.clone(),
                })
                .collect());
        }

        // JS uses `.` as namespace separator
        let matcher = crate::dwarf::PatternMatcher::new_with_separator(pattern, '.');
        Ok(self.functions.iter()
            .filter(|(name, _)| matcher.matches(name))
            .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                file: file.to_string_lossy().to_string(),
                line: *line,
                name: name.clone(),
            })
            .collect())
    }

    fn resolve_line(&self, file: &str, line: u32) -> crate::Result<Option<ResolvedTarget>> {
        // For TS files, check if we have a source map to resolve to JS line
        // For JS files, use directly
        Ok(Some(ResolvedTarget::SourceLocation {
            file: file.to_string(),
            line,
            name: format!("{}:{}", file, line),
        }))
    }

    fn resolve_variable(&self, name: &str) -> crate::Result<VariableResolution> {
        Ok(VariableResolution::RuntimeExpression { expr: name.to_string() })
    }

    fn image_base(&self) -> u64 { 0 }
    fn language(&self) -> Language { Language::JavaScript }
    fn supports_runtime_resolution(&self) -> bool { true }
}
```

**Step 3: Update symbols/mod.rs**

```rust
pub mod js_resolver;
pub use js_resolver::JSResolver;
```

**Verify:**
```bash
cargo test --lib symbols::js_resolver
```

**Checkpoint:** JSResolver parses TS/JS files, extracts functions/classes/methods, matches patterns.

### Commit 1: JSResolver

```
feat: add JSResolver for JavaScript/TypeScript AST-based symbol resolution

Uses SWC parser to extract function/class method definitions from
.ts/.js/.tsx/.jsx files. Source map support for TS→JS line mapping.
Implements SymbolResolver trait with dot-separated pattern matching.
```

---

### Task 3: Shared JS Utilities for Agent

**Files:**
- Create: `agent/src/tracers/js-common.ts`

Shared utilities between V8Tracer and JSCTracer:

```typescript
// agent/src/tracers/js-common.ts

/**
 * Evaluate a JavaScript expression in the target process.
 * Uses Frida's Script.evaluate or the runtime's eval.
 */
export function evaluateInTarget(code: string): any {
  // Implementation depends on runtime — V8 uses Inspector, JSC uses direct eval
  // This provides the common interface
}

/**
 * Convert a file path to a V8/JSC script URL regex.
 * Handles both absolute paths and module-relative paths.
 */
export function fileToUrlRegex(file: string): string {
  // Escape regex chars, support both file:// URLs and bare paths
  const escaped = file.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return `.*${escaped}$`;
}

/**
 * Format a JavaScript value for event serialization.
 * Handles circular references, large objects, functions, etc.
 */
export function serializeValue(value: any, maxDepth: number = 3): string {
  try {
    return JSON.stringify(value, (key, val) => {
      if (typeof val === 'function') return `[Function: ${val.name || 'anonymous'}]`;
      if (typeof val === 'bigint') return val.toString();
      if (val instanceof Error) return { name: val.name, message: val.message, stack: val.stack };
      return val;
    }, undefined);
  } catch {
    return String(value);
  }
}

/**
 * Common breakpoint state management shared between V8 and JSC tracers.
 */
export interface JSBreakpointState {
  id: string;
  file: string;
  line: number;
  condition?: string;
  hitCount: number;
  hits: number;
  runtimeId?: string; // V8 breakpointId or JSC equivalent
}
```

**Checkpoint:** Shared utilities ready for both tracers.

---

### Task 4: V8Tracer Implementation (Node.js)

**Files:**
- Create: `agent/src/tracers/v8-tracer.ts`
- Modify: `agent/src/agent.ts` (wire V8Tracer)

**Step 1: Implement V8Tracer**

```typescript
// agent/src/tracers/v8-tracer.ts
import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage } from './tracer';
import { fileToUrlRegex, serializeValue, JSBreakpointState } from './js-common';

export class V8Tracer implements Tracer {
  private session: any = null; // inspector.Session
  private trackedFunctions: Map<string, { breakpointId: string; target: ResolvedTarget }> = new Map();
  private breakpoints: Map<string, JSBreakpointState> = new Map();
  private logpoints: Map<string, { runtimeId: string; message: string }> = new Map();
  private nextEventId: number = 0;
  private agent: any;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    // Access V8 Inspector Protocol from inside the Frida-injected agent.
    // Node.js has require('inspector') available — no --inspect flag needed.
    //
    // Key insight: We're running inside Frida's JS runtime (QuickJS/V8),
    // but we need to call into the TARGET process's Node.js V8 Inspector.
    // Use Frida's Module.getExportByName to call into Node's inspector API.
    this.setupInspectorSession();
  }

  private setupInspectorSession(): void {
    // Strategy: Use Frida's NativeFunction to call into Node.js's C++ inspector API
    // OR: Inject JS code into the target's V8 isolate to create an inspector.Session
    //
    // Option 1: Call require('inspector') in target via Frida's eval
    // Option 2: Hook V8's internal debugging API directly
    //
    // For initial implementation, use Option 1: inject code to set up inspector
    const setupCode = `
      const inspector = require('inspector');
      const session = new inspector.Session();
      session.connect();
      session.post('Debugger.enable');
      session.post('Runtime.enable');
      globalThis.__strobe_inspector = session;
      globalThis.__strobe_breakpoints = new Map();
    `;
    // This code needs to run in the TARGET's V8 context, not Frida's
    // Use Frida's Script.evaluate or a hook on a suitable target function
    this.evaluateInTarget(setupCode);
  }

  private evaluateInTarget(code: string): any {
    // Use Frida's ObjC.schedule (on macOS) or Interceptor-based injection
    // to run code in the target's event loop context.
    // For Node.js: hook process._tickCallback or setImmediate to inject
    // This is the key challenge — executing arbitrary JS in the target
    // while the Frida agent runs in a separate V8/QuickJS context.
    //
    // Implementation: Send the code as a message, have a bootstrap hook
    // in the target that evaluates it.
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    if (!target.file || !target.line) return null;

    // Set a V8 breakpoint that logs but doesn't pause (for tracing)
    const breakpointCmd = `
      globalThis.__strobe_inspector.post('Debugger.setBreakpointByUrl', {
        urlRegex: '${fileToUrlRegex(target.file)}',
        lineNumber: ${target.line - 1},
        condition: '(function(){
          // Emit trace event and return false (don't pause)
          process.send && process.send({
            type: "strobe_trace",
            name: "${target.name}",
            file: "${target.file}",
            line: ${target.line}
          });
          return false;
        })()'
      }, (err, result) => {
        if (!err) {
          globalThis.__strobe_breakpoints.set("${target.name}", result.breakpointId);
        }
      });
    `;
    this.evaluateInTarget(breakpointCmd);
    this.trackedFunctions.set(target.name, { breakpointId: '', target });
    return this.trackedFunctions.size;
  }

  removeHook(id: number): void {}

  removeAllHooks(): void {
    // Remove all V8 breakpoints
    const removeCmd = `
      for (const [name, bpId] of globalThis.__strobe_breakpoints) {
        globalThis.__strobe_inspector.post('Debugger.removeBreakpoint', { breakpointId: bpId });
      }
      globalThis.__strobe_breakpoints.clear();
    `;
    this.evaluateInTarget(removeCmd);
    this.trackedFunctions.clear();
  }

  activeHookCount(): number {
    return this.trackedFunctions.size;
  }

  installBreakpoint(msg: BreakpointMessage): void {
    if (!msg.file || !msg.line) return;

    const conditionCode = msg.condition
      ? `if (!(${msg.condition})) return false;`
      : '';

    const bpCmd = `
      globalThis.__strobe_inspector.post('Debugger.setBreakpointByUrl', {
        urlRegex: '${fileToUrlRegex(msg.file)}',
        lineNumber: ${msg.line - 1},
        ${msg.condition ? `condition: '${msg.condition}',` : ''}
      }, (err, result) => {
        if (!err) {
          globalThis.__strobe_breakpoints.set("bp-${msg.id}", result.breakpointId);
        }
      });
    `;
    this.evaluateInTarget(bpCmd);

    this.breakpoints.set(msg.id, {
      id: msg.id,
      file: msg.file,
      line: msg.line,
      condition: msg.condition,
      hitCount: msg.hitCount || 0,
      hits: 0,
    });

    send({ type: 'breakpoint_set', id: msg.id, file: msg.file, line: msg.line });
  }

  removeBreakpoint(id: string): void {
    const removeCmd = `
      const bpId = globalThis.__strobe_breakpoints.get("bp-${id}");
      if (bpId) {
        globalThis.__strobe_inspector.post('Debugger.removeBreakpoint', { breakpointId: bpId });
        globalThis.__strobe_breakpoints.delete("bp-${id}");
      }
    `;
    this.evaluateInTarget(removeCmd);
    this.breakpoints.delete(id);
  }

  installStepHooks(msg: StepHooksMessage): void {
    // V8 Inspector has native step support
    const action = 'stepOver'; // Determine from msg context
    const stepCmd = `globalThis.__strobe_inspector.post('Debugger.${action}');`;
    this.evaluateInTarget(stepCmd);
  }

  installLogpoint(msg: LogpointMessage): void {
    if (!msg.file || !msg.line) return;

    // V8 supports logpoints as breakpoints with logMessage
    const lpCmd = `
      globalThis.__strobe_inspector.post('Debugger.setBreakpointByUrl', {
        urlRegex: '${fileToUrlRegex(msg.file)}',
        lineNumber: ${msg.line - 1},
        condition: '(function(){
          console.log("${msg.message.replace(/"/g, '\\"')}");
          return false;
        })()'
      }, (err, result) => {
        if (!err) {
          globalThis.__strobe_breakpoints.set("lp-${msg.id}", result.breakpointId);
        }
      });
    `;
    this.evaluateInTarget(lpCmd);
    send({ type: 'logpoint_set', id: msg.id });
  }

  removeLogpoint(id: string): void {
    const removeCmd = `
      const bpId = globalThis.__strobe_breakpoints.get("lp-${id}");
      if (bpId) {
        globalThis.__strobe_inspector.post('Debugger.removeBreakpoint', { breakpointId: bpId });
        globalThis.__strobe_breakpoints.delete("lp-${id}");
      }
    `;
    this.evaluateInTarget(removeCmd);
  }

  readVariable(expr: string): any {
    // Use Runtime.evaluate to read a variable in the target's context
    const evalCmd = `
      globalThis.__strobe_inspector.post('Runtime.evaluate', {
        expression: '${expr.replace(/'/g, "\\'")}',
        returnByValue: true,
      }, (err, result) => {
        if (!err && result.result) {
          process.send && process.send({
            type: 'strobe_eval_response',
            value: result.result.value,
          });
        }
      });
    `;
    this.evaluateInTarget(evalCmd);
    return null; // Async
  }

  writeVariable(expr: string, value: any): void {
    const evalCmd = `
      globalThis.__strobe_inspector.post('Runtime.evaluate', {
        expression: '${expr} = ${JSON.stringify(value)}',
      });
    `;
    this.evaluateInTarget(evalCmd);
  }

  dispose(): void {
    this.removeAllHooks();
    const disconnectCmd = `
      if (globalThis.__strobe_inspector) {
        globalThis.__strobe_inspector.disconnect();
        delete globalThis.__strobe_inspector;
      }
    `;
    this.evaluateInTarget(disconnectCmd);
  }

  setImageBase(imageBase: string): void {
    // No-op for V8
  }

  getSlide(): NativePointer {
    return ptr(0);
  }
}
```

**Step 2: Wire V8Tracer into createTracer**

```typescript
import { V8Tracer } from './tracers/v8-tracer';

// In createTracer():
case 'v8':
  return new V8Tracer(agent);
```

**Checkpoint:** Agent builds with V8Tracer. Not yet e2e tested.

### Commit 2: V8Tracer

```
feat: add V8Tracer for Node.js debugging via V8 Inspector Protocol

Uses require('inspector').Session for breakpoints, stepping,
variable evaluation. Internal inspector (no --inspect flag needed).
Shared JS utilities in js-common.ts.
```

---

### Task 5: JSCTracer Implementation (Bun)

**Files:**
- Create: `agent/src/tracers/jsc-tracer.ts`
- Modify: `agent/src/agent.ts` (wire JSCTracer)

JSCTracer follows the same pattern as V8Tracer but uses Bun's inspector API (`bun:inspector` or WebKit Inspector Protocol commands).

```typescript
// agent/src/tracers/jsc-tracer.ts
import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage } from './tracer';
import { fileToUrlRegex, serializeValue, JSBreakpointState } from './js-common';

export class JSCTracer implements Tracer {
  // Similar structure to V8Tracer but using JSC/WebKit Inspector Protocol
  // Bun's inspector uses WebKit Inspector Protocol commands:
  // - Debugger.setBreakpointByUrl (same as V8)
  // - Debugger.stepOver/stepInto/stepOut (same)
  // - Runtime.evaluate (same)
  //
  // Key differences from V8:
  // - Access via require('bun:inspector') instead of require('inspector')
  // - Some command parameters may differ
  // - Worker model is different (Bun uses OS threads, not isolates)

  constructor(agent: any) { /* ... */ }
  initialize(sessionId: string): void { /* setup bun:inspector */ }
  installHook(target: ResolvedTarget, mode: HookMode): number | null { /* ... */ }
  installBreakpoint(msg: BreakpointMessage): void { /* ... */ }
  // ... all other Tracer methods, adapted for JSC ...

  dispose(): void { /* ... */ }
  setImageBase(imageBase: string): void { /* no-op */ }
  getSlide(): NativePointer { return ptr(0); }
}
```

**Step 2: Wire JSCTracer**

```typescript
import { JSCTracer } from './tracers/jsc-tracer';

// In createTracer():
case 'jsc':
  return new JSCTracer(agent);
```

**Checkpoint:** Agent builds with JSCTracer.

### Commit 3: JSCTracer

```
feat: add JSCTracer for Bun/JavaScriptCore debugging

Uses bun:inspector or WebKit Inspector Protocol for breakpoints,
stepping, variable evaluation. Shares JS utilities with V8Tracer.
```

---

### Task 6: VitestAdapter Implementation

**Files:**
- Create: `src/test/vitest_adapter.rs`
- Modify: `src/test/mod.rs` (register)

```rust
pub struct VitestAdapter;

impl TestAdapter for VitestAdapter {
    fn detect(&self, root: &Path, _command: Option<&str>) -> u8 {
        if root.join("vitest.config.ts").exists() || root.join("vitest.config.js").exists() { 90 }
        else if has_dep_in_package_json(root, "vitest") { 85 }
        else { 0 }
    }

    fn name(&self) -> &str { "vitest" }

    fn suite_command(&self, _root: &Path, level: Option<TestLevel>, _env: &HashMap<String, String>) -> crate::Result<TestCommand> {
        let mut args = vec!["vitest".into(), "run".into(), "--reporter=json".into()];
        if let Some(lvl) = level {
            let pattern = match lvl {
                TestLevel::Unit => "**/*.unit.test.*",
                TestLevel::Integration => "**/*.integration.test.*",
                TestLevel::E2e => "**/*.e2e.test.*",
            };
            args.push(format!("--include={}", pattern));
        }
        Ok(TestCommand { program: "npx".into(), args, env: HashMap::new() })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".into(),
            args: vec!["vitest".into(), "run".into(), "-t".into(), test_name.into(), "--reporter=json".into()],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_vitest_json_output(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        extract_js_traces(failure)
    }
}

/// Parse vitest JSON reporter output.
fn parse_vitest_json_output(stdout: &str, _stderr: &str, _exit_code: i32) -> TestResult {
    // Vitest JSON output format:
    // { "testResults": [{ "name": "...", "status": "passed"|"failed", "duration": N, ... }] }
    todo!("Implement vitest JSON parsing")
}

/// Shared JS test failure → trace pattern extraction.
fn extract_js_traces(failure: &TestFailure) -> Vec<String> {
    let mut traces = Vec::new();
    if let Some(ref file) = failure.file {
        if let Some(filename) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
            let module = filename.strip_suffix(".test").unwrap_or(filename);
            traces.push(format!("{}.*", module));
        }
        traces.push(format!("@file:{}", Path::new(file).file_name().unwrap_or_default().to_string_lossy()));
    }
    traces
}

/// Read package.json and check for a dependency.
fn has_dep_in_package_json(root: &Path, dep: &str) -> bool {
    let pkg_path = root.join("package.json");
    if let Ok(content) = std::fs::read_to_string(&pkg_path) {
        content.contains(&format!("\"{}\"", dep))
    } else {
        false
    }
}

/// Update progress from vitest output.
pub fn update_progress(line: &str, progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>) {
    let trimmed = line.trim();
    if trimmed.contains("✓") || trimmed.contains("PASS") {
        let mut p = progress.lock().unwrap();
        p.passed += 1;
    } else if trimmed.contains("✗") || trimmed.contains("FAIL") || trimmed.contains("×") {
        let mut p = progress.lock().unwrap();
        p.failed += 1;
    } else if trimmed.contains("↓") || trimmed.contains("SKIP") {
        let mut p = progress.lock().unwrap();
        p.skipped += 1;
    }
}
```

**Checkpoint:** VitestAdapter detects vitest projects, generates correct commands.

---

### Task 7: JestAdapter Implementation

**Files:**
- Create: `src/test/jest_adapter.rs`
- Modify: `src/test/mod.rs` (register)

Similar to VitestAdapter but detects jest.config.js and uses `--json` flag.

**Checkpoint:** JestAdapter registered and unit-tested.

---

### Task 8: BunTestAdapter Implementation

**Files:**
- Create: `src/test/bun_adapter.rs`
- Modify: `src/test/mod.rs` (register)

```rust
pub struct BunTestAdapter;

impl TestAdapter for BunTestAdapter {
    fn detect(&self, root: &Path, cmd: Option<&str>) -> u8 {
        if let Some(cmd) = cmd { if cmd.contains("bun") { return 95; } }
        if root.join("bun.lockb").exists() { 85 }
        else if root.join("bunfig.toml").exists() { 80 }
        else { 0 }
    }

    fn name(&self) -> &str { "bun" }

    fn suite_command(&self, _root: &Path, _level: Option<TestLevel>, _env: &HashMap<String, String>) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".into(),
            args: vec!["test".into()],
            env: HashMap::new(),
        })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".into(),
            args: vec!["test".into(), "--filter".into(), test_name.into()],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_bun_test_output(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        extract_js_traces(failure)
    }
}
```

**Step: Register all JS/TS adapters in TestRunner::new()**

```rust
adapters: vec![
    Box::new(CargoTestAdapter),
    Box::new(Catch2Adapter),
    Box::new(PytestAdapter),       // From Plan 2
    Box::new(UnittestAdapter),     // From Plan 2
    Box::new(VitestAdapter),       // NEW
    Box::new(JestAdapter),         // NEW
    Box::new(BunTestAdapter),      // NEW
],
```

**Verify:**
```bash
cargo test --lib test
```

### Commit 4: JS/TS test adapters

```
feat: add VitestAdapter, JestAdapter, and BunTestAdapter

Vitest: detects via vitest.config.ts, uses --reporter=json.
Jest: detects via jest.config.js, uses --json.
Bun: detects via bun.lockb, uses bun test.
All registered in TestRunner alongside existing adapters.
```

---

### Task 9: Node.js Test Fixture

**Files:**
- Create: `tests/fixtures/node/` directory structure

```
tests/fixtures/node/
├── fixture.ts          # CLI entry point
├── src/
│   ├── audio.ts        # AudioProcessor + functions
│   ├── midi.ts         # MidiHandler + functions
│   ├── timing.ts       # fast(), medium(), slow()
│   ├── engine.ts       # Global state
│   └── crash.ts        # Crash scenarios
├── tests/
│   ├── audio.test.ts   # vitest: 4 tests (3 pass, 1 fail)
│   ├── midi.test.ts    # vitest: 3 tests
│   ├── engine.test.ts  # vitest: 3 tests (2 pass, 1 skip)
│   └── stuck.test.ts   # vitest: 1 stuck test
├── package.json
├── vitest.config.ts
└── tsconfig.json
```

CLI modes mirror the C++ and Python fixtures: hello, crash-throw, crash-abort, crash-null, slow-functions, threads, globals, breakpoint-loop, step-target, write-target, async-demo, class-methods.

Global state:
```typescript
// src/engine.ts
export let gCounter = 0;
export let gTempo = 120.0;
export const gSampleRate = 44100;
export let gPoint = { x: 1.0, y: 2.0, value: 42 };
```

**Checkpoint:** `npx tsx fixture.ts hello` prints "Hello from Node.js fixture". `npx vitest run` shows 3 pass, 1 fail, 1 skip.

---

### Task 10: Bun Test Fixture

**Files:**
- Create: `tests/fixtures/bun/` directory structure

Same structure as Node fixture but uses Bun-specific APIs:
- `Bun.sleep()` instead of setTimeout
- `Bun.nanoseconds()` for timing
- `bun test` with describe/test/expect

**Checkpoint:** `bun run fixture.ts hello` prints "Hello from Bun fixture". `bun test` shows expected results.

### Commit 5: Node.js and Bun test fixtures

```
feat: add Node.js and Bun test fixture programs

Node: 12 CLI modes, vitest suite with intentional fail/skip/stuck.
Bun: same modes with Bun-specific APIs, bun test suite.
TypeScript modules: audio, midi, timing, engine, crash.
```

---

### Task 11: Node.js E2E Tests

**Files:**
- Create: `tests/node_e2e.rs`
- Modify: `tests/common/mod.rs` (add node fixture helpers)

```rust
// tests/node_e2e.rs
mod common;
use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_node_e2e_scenarios() {
    let fixture_dir = node_fixture_dir();
    let (sm, _dir) = create_session_manager();

    // Run tsx (TypeScript executor) with the fixture
    let tsx = "npx";
    let fixture_args = vec!["tsx".to_string(), fixture_dir.join("fixture.ts").to_str().unwrap().to_string()];

    eprintln!("=== Node 1/6: Output capture ===");
    node_output_capture(&sm, tsx, &fixture_args, fixture_dir.to_str().unwrap()).await;

    // ... same 6 scenarios as Python ...

    eprintln!("\n=== All Node E2E scenarios passed ===");
}
```

---

### Task 12: Bun E2E Tests

**Files:**
- Create: `tests/bun_e2e.rs`

Same pattern as Node e2e but using `bun run fixture.ts`.

### Commit 6: JS/TS e2e tests

```
feat: add Node.js and Bun e2e integration tests

6 scenarios each testing output capture, function tracing, crash handling,
multi-threading, watch variables, and breakpoints.
```

---

### Task 13: Web App Fixtures (Stretch)

**Files:**
- Create: `tests/fixtures/python-webapp/` — FastAPI app
- Create: `tests/fixtures/node-webapp/` — Express app
- Create: `tests/fixtures/bun-webapp/` — Bun.serve app

Each web app has:
- Auth routes (login, profile)
- Data routes (CRUD)
- Service layer (for tracing)
- Test suite (pytest / vitest / bun test)

E2e tests validate: launch server → trace handlers → HTTP request → observe events → breakpoint on handler → debug_read variables.

---

### Task 14: Polish — MCP Descriptions + FEATURES.md

**Files:**
- Modify: `src/daemon/server.rs` (update MCP tool descriptions to mention Python/JS/TS)
- Modify: `docs/FEATURES.md` (move Python/JS from Phase 9 to completed)

**MCP instruction updates:**
- `debug_launch`: "Launch any program — C/C++/Rust, Python, Node.js, Bun"
- `debug_trace`: "Works with C++ namespaces (::), Python modules (.), JS/TS classes (.)"
- `debug_test`: "Auto-detects: Cargo, Catch2, pytest, vitest, jest, bun test"

### Commit 7: Polish

```
feat: update MCP descriptions and FEATURES.md for multi-language support

Tool descriptions now mention Python/JS/TS support.
FEATURES.md updated: Python/JS/TS moved from Phase 9 to completed.
```
