# Multi-Language Support: Python, TypeScript, Node.js, Bun

**Date:** 2026-02-11
**Status:** Design
**Phase:** Extends Phase 1a-2b to interpreted languages

## Goal

Full Phase 1+2 feature parity for Python (CPython 3.11+), Node.js (V8), and Bun (JSC). Every feature that works for native C/C++/Rust binaries — function tracing, breakpoints, logpoints, stepping, watches, conditional breaks, memory read/write, test instrumentation — works identically for interpreted languages. The LLM uses the same MCP tools regardless of target language.

## Design Principles

1. **Unified interface**: Same MCP tools, same event schema, same query API. The LLM doesn't need language-specific knowledge.
2. **Unified architecture**: Daemon always resolves symbols. Agent always hooks. Language differences are behind the `SymbolResolver` trait (daemon) and `Tracer` interface (agent).
3. **One agent, language modules**: Single compiled `agent.js` with pluggable tracer modules. Runtime detection selects the right tracer at injection time.
4. **Concurrent multi-language sessions**: Daemon handles C++ debug session, Python test run, and Bun app simultaneously. Each session is independent with its own Frida spawner and tracer.
5. **Real-world validation**: Test fixtures mirror the existing C++/Rust fixtures mode-for-mode, plus real web applications (Flask, Express, Bun.serve).

## Language & Runtime Support

| Language | Runtime | Tracer | Symbol Resolution | Test Frameworks |
|----------|---------|--------|-------------------|-----------------|
| C/C++/Rust | Native | CModuleTracer (existing) | DWARF | Cargo, Catch2 |
| Python | CPython 3.11+ | PythonTracer (new) | AST (rustpython-parser) | pytest, unittest |
| TypeScript/JS | Node.js (V8) | V8Tracer (new) | SWC + source maps | vitest, jest |
| TypeScript/JS | Bun (JSC) | JSCTracer (new) | SWC + source maps | bun test |

---

## Architecture

### Agent: One Agent, Pluggable Tracers

The existing `agent.ts` gains a `Tracer` interface. At injection time, the agent probes the target process for runtime symbols and selects the appropriate tracer.

```
agent/src/
├── agent.ts                  # Entry point (shared: output capture, message protocol)
├── cmodule-tracer.ts         # Native CModule tracer (existing, unchanged)
├── tracers/
│   ├── tracer.ts             # Tracer interface
│   ├── native-tracer.ts      # Wraps CModuleTracer
│   ├── python-tracer.ts      # CPython 3.11+ frame evaluation hooks
│   ├── v8-tracer.ts          # V8 Inspector Protocol (internal)
│   └── jsc-tracer.ts         # JavaScriptCore hooks
├── resolvers/
│   └── source-map.ts         # TS→JS source map resolution (agent-side)
├── platform.ts               # (existing, unchanged)
└── rate-tracker.ts           # (existing, shared across all tracers)
```

#### Runtime Detection

```typescript
function detectRuntime(): 'native' | 'cpython' | 'v8' | 'jsc' {
  if (Module.findExportByName(null, '_PyEval_EvalFrameDefault')) return 'cpython';
  if (Module.findExportByName(null, 'Py_Initialize')) return 'cpython';
  if (Module.findExportByName(null, '_ZN2v88internal7Isolate7currentEv')) return 'v8';
  if (Module.findExportByName(null, 'JSGlobalContextCreate')) return 'jsc';
  return 'native';
}
```

The agent constructor calls `detectRuntime()` and instantiates the matching tracer. If detection fails, falls back to `'native'`.

#### Tracer Interface

All tracers implement this contract:

```typescript
interface Tracer {
  // Lifecycle
  initialize(sessionId: string): void;
  dispose(): void;

  // Hook management (resolved targets from daemon)
  installHook(target: ResolvedTarget, mode: HookMode): number | null;
  removeHook(id: number): void;
  activeHookCount(): number;

  // Breakpoints
  installBreakpoint(id: string, file: string, line: number, condition?: string, hitCount?: number): void;
  removeBreakpoint(id: string): void;

  // Stepping
  stepOver(threadId: number): void;
  stepInto(threadId: number): void;
  stepOut(threadId: number): void;

  // Logpoints
  installLogpoint(id: string, file: string, line: number, message: string, condition?: string): void;
  removeLogpoint(id: string): void;

  // Variable access
  readVariable(expr: string): any;
  writeVariable(expr: string, value: any): void;

  // Runtime resolution fallback (for dynamic functions not in static AST)
  resolvePattern(pattern: string): ResolvedTarget[];
}

interface ResolvedTarget {
  // For native: instruction address
  address?: string;
  // For interpreted: source location
  file?: string;
  line?: number;
  // Common
  name: string;
}
```

The existing agent message handlers (`onHooksMessage`, `onSetBreakpointMessage`, etc.) delegate to `this.tracer.installHook()`, `this.tracer.installBreakpoint()`, etc. Shared infrastructure (output capture, output buffering, crash handler, message protocol) stays in `agent.ts`.

### Daemon: Symbol Resolution Abstraction

A new `SymbolResolver` trait abstracts DWARF, Python AST, and JS/TS AST parsing:

```rust
// src/symbols/mod.rs
pub mod dwarf_resolver;
pub mod python_resolver;
pub mod js_resolver;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Language {
    Native,
    Python,
    JavaScript,
}

pub enum ResolvedTarget {
    /// Native: DWARF-resolved instruction address
    Address { address: u64, name: String, file: Option<String>, line: Option<u32> },
    /// Interpreted: source file + line (agent hooks by location)
    SourceLocation { file: String, line: u32, name: String },
}

pub enum VariableResolution {
    /// DWARF-resolved static address (native)
    NativeAddress { address: u64, size: u8, type_kind: TypeKind, deref_depth: u8, deref_offset: u64 },
    /// Runtime expression — agent evaluates in target context (Python/JS)
    RuntimeExpression { expr: String },
}

pub trait SymbolResolver: Send + Sync {
    /// Resolve a glob pattern to concrete function targets
    fn resolve_pattern(&self, pattern: &str, project_root: &Path) -> Result<Vec<ResolvedTarget>>;

    /// Resolve file:line to a hookable target
    fn resolve_line(&self, file: &str, line: u32) -> Result<Option<ResolvedTarget>>;

    /// Resolve a variable name for reading/writing
    fn resolve_variable(&self, name: &str) -> Result<VariableResolution>;

    /// Image base for ASLR (0 for interpreted languages)
    fn image_base(&self) -> u64;

    /// Language identifier
    fn language(&self) -> Language;

    /// Whether this resolver supports agent-side fallback for dynamic symbols
    fn supports_runtime_resolution(&self) -> bool;
}
```

#### DwarfResolver (wraps existing DwarfParser)

```rust
pub struct DwarfResolver {
    dwarf: DwarfHandle,
    image_base: u64,
}

impl SymbolResolver for DwarfResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> Result<Vec<ResolvedTarget>> {
        // Existing DWARF pattern matching logic
        let functions = self.dwarf.get()?.resolve_pattern(pattern)?;
        Ok(functions.iter().map(|f| ResolvedTarget::Address {
            address: f.low_pc, name: f.name.clone(),
            file: f.file.clone(), line: f.line,
        }).collect())
    }
    fn image_base(&self) -> u64 { self.image_base }
    fn language(&self) -> Language { Language::Native }
    fn supports_runtime_resolution(&self) -> bool { false }
}
```

#### PythonResolver (new, uses rustpython-parser)

```rust
pub struct PythonResolver {
    /// Parsed function definitions: (qualified_name → (file, line))
    functions: HashMap<String, (PathBuf, u32)>,
}

impl PythonResolver {
    pub fn parse(project_root: &Path) -> Result<Self> {
        let mut functions = HashMap::new();
        // Walk .py files in project_root (excluding venv, __pycache__)
        for entry in WalkDir::new(project_root)
            .into_iter()
            .filter_entry(|e| !is_python_excluded(e))
        {
            let path = entry?.path().to_owned();
            if path.extension() == Some("py".as_ref()) {
                let source = std::fs::read_to_string(&path)?;
                let ast = rustpython_parser::parse(&source, Mode::Module, "<input>")?;
                extract_functions(&ast, &path, &mut functions);
            }
        }
        Ok(Self { functions })
    }
}

impl SymbolResolver for PythonResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> Result<Vec<ResolvedTarget>> {
        let matcher = PatternMatcher::new(pattern); // Reuse existing pattern matcher
        Ok(self.functions.iter()
            .filter(|(name, _)| matcher.matches(name))
            .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                file: file.to_string_lossy().to_string(),
                line: *line,
                name: name.clone(),
            })
            .collect())
    }

    fn resolve_variable(&self, name: &str) -> Result<VariableResolution> {
        // Python variables are resolved at runtime via eval
        Ok(VariableResolution::RuntimeExpression { expr: name.to_string() })
    }

    fn image_base(&self) -> u64 { 0 } // No ASLR for interpreted
    fn language(&self) -> Language { Language::Python }
    fn supports_runtime_resolution(&self) -> bool { true } // For decorators, metaclasses, etc.
}
```

#### JSResolver (new, uses swc_ecma_parser)

```rust
pub struct JSResolver {
    /// Parsed function/method definitions
    functions: HashMap<String, (PathBuf, u32)>,
    /// Source maps: .js file → SourceMap (for TS→JS mapping)
    source_maps: HashMap<PathBuf, SourceMap>,
}

impl JSResolver {
    pub fn parse(project_root: &Path) -> Result<Self> {
        let mut functions = HashMap::new();
        let mut source_maps = HashMap::new();
        // Walk .ts, .js, .tsx, .jsx files (excluding node_modules, dist)
        for entry in WalkDir::new(project_root)
            .into_iter()
            .filter_entry(|e| !is_js_excluded(e))
        {
            let path = entry?.path().to_owned();
            if is_js_ts_file(&path) {
                let source = std::fs::read_to_string(&path)?;
                let ast = swc_ecma_parser::parse_file_as_module(/* ... */)?;
                extract_js_functions(&ast, &path, &mut functions);
            }
            if path.extension() == Some("map".as_ref()) {
                let map = SourceMap::parse(&std::fs::read_to_string(&path)?)?;
                source_maps.insert(path, map);
            }
        }
        Ok(Self { functions, source_maps })
    }
}

impl SymbolResolver for JSResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> Result<Vec<ResolvedTarget>> {
        let matcher = PatternMatcher::new(pattern);
        Ok(self.functions.iter()
            .filter(|(name, _)| matcher.matches(name))
            .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                file: file.to_string_lossy().to_string(),
                line: *line,
                name: name.clone(),
            })
            .collect())
    }

    fn resolve_variable(&self, name: &str) -> Result<VariableResolution> {
        Ok(VariableResolution::RuntimeExpression { expr: name.to_string() })
    }

    fn image_base(&self) -> u64 { 0 }
    fn language(&self) -> Language { Language::JavaScript }
    fn supports_runtime_resolution(&self) -> bool { true }
}
```

### Session Manager Changes

```rust
// session_manager.rs

pub struct SessionState {
    // ... existing fields ...
    pub language: Language,
    pub resolver: Arc<dyn SymbolResolver>,
}

/// Detect language from command and project root signals.
fn detect_language(command: &str, project_root: &Path) -> Language {
    if command.contains("python") || command.ends_with(".py") {
        return Language::Python;
    }
    if command.contains("node") || command.contains("bun")
       || command.ends_with(".js") || command.ends_with(".ts") {
        return Language::JavaScript;
    }
    if project_root.join("pyproject.toml").exists()
       || project_root.join("requirements.txt").exists() {
        return Language::Python;
    }
    if project_root.join("package.json").exists() {
        return Language::JavaScript;
    }
    Language::Native
}
```

On `spawn_with_frida`:
1. `detect_language()` → determines which `SymbolResolver` to create
2. Native: `DwarfResolver` (existing behavior, uses DwarfParser)
3. Python: `PythonResolver::parse(project_root)`
4. JS/TS: `JSResolver::parse(project_root)`
5. Resolver stored per-session in `SessionState`

On `update_frida_patterns`:
1. Call `resolver.resolve_pattern(pattern)` → `Vec<ResolvedTarget>`
2. If empty and `resolver.supports_runtime_resolution()` → send resolve request to agent
3. Send resolved targets to agent for hooking

On `set_breakpoint`:
1. If `file:line` → `resolver.resolve_line(file, line)`
   - Native: DWARF `.debug_line` → address → send `setBreakpoint` with address
   - Python/JS: Send `setBreakpoint` with `{ file, line }` → agent hooks by location
2. If `function` pattern → `resolver.resolve_pattern(function)` → breakpoint at entry

On `read_variable`:
1. `resolver.resolve_variable(name)` → `VariableResolution`
   - `NativeAddress` → send `read_memory` with address recipe (existing)
   - `RuntimeExpression` → send `eval_variable` with expression → agent evaluates

### Message Protocol Changes

New message types between daemon and agent:

```typescript
// Existing (unchanged for native):
// { type: 'hooks', action: 'add', functions: [{ address, name }] }
// { type: 'setBreakpoint', address, id, condition }

// New (for interpreted languages):
// Hooks by source location (instead of address):
{ type: 'hooks', action: 'add', targets: [{ file, line, name }] }

// Breakpoint by source location:
{ type: 'setBreakpoint', file: 'app.py', line: 42, id: 'bp-1', condition?: '...' }

// Runtime resolution request (fallback for dynamic functions):
{ type: 'resolve', patterns: ['mymodule.*'] }
// Agent responds: { type: 'resolved', targets: [{ file, line, name }] }

// Variable evaluation:
{ type: 'eval_variable', expr: 'mymodule.g_counter' }
// Agent responds: { type: 'eval_response', label: 'g_counter', value: 42 }
```

The agent dispatches based on whether `address` or `file`+`line` is present — no breaking change to the protocol.

---

## Python Tracer: CPython 3.11+ Internals

### Hook Strategy

Hook `_PyEval_EvalFrameDefault(PyThreadState*, _PyInterpreterFrame*, int)` — the single dispatch point for all Python function execution in CPython.

The `_PyInterpreterFrame` struct (CPython 3.11+) contains:
- `f_executable` → `PyCodeObject*` (function's code object: name, filename, line number)
- `f_locals_plus` → local variables array
- `f_lasti` → current bytecode instruction index

### Function Tracing

```typescript
class PythonTracer implements Tracer {
  private trackedCodes: Set<string> = new Set(); // "file:funcname" keys
  private frameEvalHook: InvocationListener | null = null;

  installHook(target: ResolvedTarget): number | null {
    const key = `${target.file}:${target.name}`;
    this.trackedCodes.add(key);

    // Install the eval frame hook once, filtering by tracked codes
    if (!this.frameEvalHook) {
      this.installFrameEvalHook();
    }
    return this.trackedCodes.size;
  }

  private installFrameEvalHook(): void {
    const evalFrame = Module.findExportByName(null, '_PyEval_EvalFrameDefault');
    if (!evalFrame) throw new Error('CPython _PyEval_EvalFrameDefault not found');

    const self = this;
    this.frameEvalHook = Interceptor.attach(evalFrame, {
      onEnter(args) {
        // args[1] = _PyInterpreterFrame*
        const frame = args[1];
        const codeInfo = self.readCodeObject(frame);
        const key = `${codeInfo.filename}:${codeInfo.name}`;

        if (!self.trackedCodes.has(key)) return; // Fast filter

        // Capture arguments from locals array
        const capturedArgs = self.captureLocals(frame, codeInfo);

        send({ type: 'events', events: [{
          id: self.nextId(),
          timestampNs: self.getTimestamp(),
          threadId: Process.getCurrentThreadId(),
          eventType: 'function_enter',
          functionName: codeInfo.qualifiedName,
          sourceFile: codeInfo.filename,
          lineNumber: codeInfo.firstLineno,
          arguments: capturedArgs,
        }]});

        // Store enter time for duration calculation
        (this as any)._strobeEnterNs = self.getTimestamp();
      },
      onLeave(retval) {
        const enterNs = (this as any)._strobeEnterNs;
        if (enterNs === undefined) return;

        // Read Python return value
        const pyRetval = self.readPyObject(retval);

        send({ type: 'events', events: [{
          id: self.nextId(),
          timestampNs: self.getTimestamp(),
          threadId: Process.getCurrentThreadId(),
          eventType: 'function_exit',
          returnValue: pyRetval,
          durationNs: self.getTimestamp() - enterNs,
        }]});
      }
    });
  }
}
```

### Performance: CModule Fast Filter

The `onEnter` callback runs for every Python function call. To minimize overhead, use a CModule hash set for the fast path:

```c
// CModule for Python frame eval filtering
static GHashTable *tracked_codes; // PyCodeObject* → funcId

void onEnter(GumInvocationContext *ic, gpointer user_data) {
    void *frame = gum_invocation_context_get_nth_argument(ic, 1);
    // Read _PyInterpreterFrame.f_executable → PyCodeObject*
    void *code = *(void**)((char*)frame + FRAME_CODE_OFFSET);

    // Fast hash lookup: is this code object tracked?
    gpointer func_id = g_hash_table_lookup(tracked_codes, code);
    if (!func_id) return;

    // Emit enter event (funcId encoded in pointer, same as native CModule)
    // ... ring buffer write ...
}
```

When a new function is tracked, the agent sends its `PyCodeObject*` address to the CModule's hash table. This makes the untracked-function fast path run in nanoseconds.

### Line-Level Breakpoints

For breakpoints at arbitrary source lines (not just function entries), use CPython's `sys.monitoring` API (PEP 669, Python 3.12+) or `sys.settrace` (3.11):

```typescript
installBreakpoint(id: string, file: string, line: number, condition?: string): void {
  // Inject Python code into the target process via Frida
  const setupCode = `
import sys, threading

# Use sys.monitoring (3.12+) or sys.settrace (3.11) for line-level hooks
_strobe_breakpoints = getattr(sys, '_strobe_breakpoints', {})
_strobe_breakpoints[('${file}', ${line})] = {
    'id': '${id}',
    'condition': ${condition ? `'${condition}'` : 'None'},
    'hit_count': 0,
}
sys._strobe_breakpoints = _strobe_breakpoints

if not hasattr(sys, '_strobe_trace_installed'):
    def _strobe_trace(frame, event, arg):
        if event == 'line':
            key = (frame.f_code.co_filename, frame.f_lineno)
            bp = sys._strobe_breakpoints.get(key)
            if bp:
                bp['hit_count'] += 1
                if bp['condition']:
                    try:
                        if not eval(bp['condition'], frame.f_globals, frame.f_locals):
                            return _strobe_trace
                    except:
                        return _strobe_trace
                # Signal pause to Frida agent
                import ctypes
                ctypes.pythonapi.PyGILState_Ensure()
                # The Frida agent will detect the pause signal
        return _strobe_trace

    sys.settrace(_strobe_trace)
    threading.settrace(_strobe_trace)
    sys._strobe_trace_installed = True
  `;
  this.evaluatePython(setupCode);
}
```

### Watch Variables

```typescript
readVariable(expr: string): any {
  // Evaluate Python expression in target process
  return this.evaluatePython(`
import json
try:
    _result = ${expr}
    print(json.dumps(_result, default=str))
except Exception as e:
    print(json.dumps({'error': str(e)}))
  `);
}

writeVariable(expr: string, value: any): void {
  this.evaluatePython(`${expr} = ${JSON.stringify(value)}`);
}
```

### Stepping

- **step-over**: Set line trace on current frame, break on next line in same frame
- **step-into**: Set line trace on all frames, break on first line event
- **step-out**: Set return trace on current frame, break on return event

All implemented via `sys.settrace` events: `'call'`, `'line'`, `'return'`.

---

## V8 Tracer: Node.js

### Hook Strategy

Use V8's built-in Inspector Protocol from inside the process. Node.js always has `require('inspector')` available — no `--inspect` flag needed.

```typescript
class V8Tracer implements Tracer {
  private session: any; // inspector.Session
  private trackedFunctions: Map<string, TrackedFunction> = new Map();
  private scriptSources: Map<string, string> = new Map(); // scriptId → source

  initialize(sessionId: string): void {
    // Access V8 Inspector from within the injected Frida agent
    // The target is a Node.js process — require('inspector') is available
    const inspector = this.requireInTarget('inspector');
    this.session = new inspector.Session();
    this.session.connect();

    // Enable debugging protocols
    this.session.post('Debugger.enable');
    this.session.post('Profiler.enable');
    this.session.post('Runtime.enable');

    // Listen for script compilation (to build function map)
    this.session.on('Debugger.scriptParsed', (event: any) => {
      this.onScriptParsed(event.params);
    });

    // Listen for pause events (breakpoints, stepping)
    this.session.on('Debugger.paused', (event: any) => {
      this.onPaused(event.params);
    });
  }

  installHook(target: ResolvedTarget): number | null {
    // Set a breakpoint at function entry that logs (doesn't pause)
    // V8 breakpoints can be set to "never actually pause" via condition "false"
    // but trigger logpoint-style callbacks
    const result = this.session.post('Debugger.setBreakpointByUrl', {
      urlRegex: this.fileToUrlRegex(target.file),
      lineNumber: target.line - 1, // V8 is 0-indexed
      columnNumber: 0,
    });

    // Alternative: use Profiler for function tracing
    // Profiler.start() captures call tree with timestamps
    return result?.breakpointId ? 1 : null;
  }

  installBreakpoint(id: string, file: string, line: number, condition?: string): void {
    const result = this.session.post('Debugger.setBreakpointByUrl', {
      urlRegex: this.fileToUrlRegex(file),
      lineNumber: line - 1,
      condition: condition || '',
    });
    this.breakpoints.set(id, result.breakpointId);
  }

  stepOver(threadId: number): void {
    this.session.post('Debugger.stepOver');
  }

  stepInto(threadId: number): void {
    this.session.post('Debugger.stepInto');
  }

  stepOut(threadId: number): void {
    this.session.post('Debugger.stepOut');
  }

  readVariable(expr: string): any {
    const result = this.session.post('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
    });
    return result.result.value;
  }

  writeVariable(expr: string, value: any): void {
    this.session.post('Runtime.evaluate', {
      expression: `${expr} = ${JSON.stringify(value)}`,
    });
  }

  private onPaused(params: any): void {
    const frame = params.callFrames[0];
    send({
      type: 'paused',
      threadId: 1, // V8 main isolate is single-threaded
      breakpointId: params.hitBreakpoints?.[0],
      funcName: frame.functionName,
      file: frame.url,
      line: frame.location.lineNumber + 1,
      backtrace: params.callFrames.map((f: any) => ({
        name: f.functionName,
        fileName: f.url,
        lineNumber: f.location.lineNumber + 1,
      })),
    });

    // Block until resume message (same pattern as native)
    const op = recv(`resume-1`, () => {});
    op.wait();
  }
}
```

### Function Tracing via V8 Profiler

For high-volume function tracing (not just breakpoints), use V8's CPU Profiler which captures call trees with timestamps:

```typescript
startTracing(): void {
  this.session.post('Profiler.setSamplingInterval', { interval: 100 }); // 100µs
  this.session.post('Profiler.start');

  // Periodically drain profile data
  setInterval(() => {
    const profile = this.session.post('Profiler.stop');
    this.processProfile(profile);
    this.session.post('Profiler.start'); // Restart
  }, 1000);
}
```

### Worker Threads

Node.js worker_threads run in separate V8 isolates. Each worker needs its own Inspector session. The agent detects new workers via `worker_threads` module hooks and attaches an inspector session per worker.

---

## JSC Tracer: Bun

### Hook Strategy

Bun uses JavaScriptCore (not V8). JSC has a different internal architecture but Bun exposes inspector support.

```typescript
class JSCTracer implements Tracer {
  initialize(sessionId: string): void {
    // Bun supports the inspector protocol via --inspect
    // From inside Frida, we can access JSC's C API directly

    // Check for JSC debug hooks
    const jscSetBreakpoint = Module.findExportByName(null, 'JSGlobalContextSetBreakpointCallback');
    const jscEvalScript = Module.findExportByName(null, 'JSEvaluateScript');

    if (jscEvalScript) {
      // Hook JSEvaluateScript for function tracing
      Interceptor.attach(jscEvalScript, {
        onEnter(args) {
          // args[1] = JSStringRef (script source)
          // Can extract script info for tracing
        }
      });
    }

    // Alternative: Use Bun's built-in inspector
    // Bun has partial WebKit Inspector Protocol support
    this.setupBunInspector();
  }

  private setupBunInspector(): void {
    // Inject JS to access Bun's inspector from within the process
    const setupCode = `
      const inspector = require('bun:inspector');
      // Use inspector API for breakpoints and evaluation
    `;
    this.evaluateJS(setupCode);
  }

  // Breakpoints, stepping, watches follow same pattern as V8Tracer
  // but use JSC/WebKit Inspector Protocol commands instead of CDP
}
```

### Shared JS Utilities

V8Tracer and JSCTracer share significant code:
- Event emission format
- Variable read/write via `eval()`
- Source map resolution
- Pattern matching

These shared utilities live in `agent/src/tracers/js-common.ts`.

---

## Test Adapters

Five new adapters, all implementing the existing `TestAdapter` trait.

### PytestAdapter

```rust
// src/test/pytest_adapter.rs
pub struct PytestAdapter;

impl TestAdapter for PytestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        if has_pytest_config(project_root) { 90 }
        else if project_root.join("conftest.py").exists() { 85 }
        else if has_pytest_in_requirements(project_root) { 80 }
        else if has_python_test_files(project_root) { 60 }
        else { 0 }
    }

    fn name(&self) -> &str { "pytest" }

    fn suite_command(&self, root: &Path, level: Option<TestLevel>, _env: &HashMap<String, String>) -> Result<TestCommand> {
        let mut args = vec!["-m".into(), "pytest".into(), "--tb=short".into(), "-q".into()];
        // pytest-json-report for structured output
        args.extend(["--json-report".into(), "--json-report-file=-".into()]);
        match level {
            Some(TestLevel::Unit) => { args.push("-m".into()); args.push("not integration and not e2e".into()); }
            Some(TestLevel::Integration) => { args.push("-m".into()); args.push("integration".into()); }
            Some(TestLevel::E2e) => { args.push("-m".into()); args.push("e2e".into()); }
            None => {}
        }
        Ok(TestCommand { program: "python3".into(), args, env: HashMap::new() })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "python3".into(),
            args: vec!["-m".into(), "pytest".into(), "-k".into(), test_name.into(),
                       "--json-report".into(), "--json-report-file=-".into(), "--tb=short".into()],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        // Parse pytest-json-report JSON output
        parse_pytest_json_report(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        // "tests/test_parser.py::TestParser::test_empty" → "parser.*"
        extract_python_traces(failure)
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        // Try py-spy first (Python-aware stacks), fall back to native
        capture_python_stacks_via_pyspy(pid)
            .unwrap_or_else(|| super::stacks::capture_native_stacks(pid))
    }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 60_000,
            Some(TestLevel::Integration) => 180_000,
            Some(TestLevel::E2e) => 300_000,
            None => 120_000,
        }
    }
}
```

### UnittestAdapter

```rust
// src/test/unittest_adapter.rs
pub struct UnittestAdapter;

impl TestAdapter for UnittestAdapter {
    fn detect(&self, root: &Path, _command: Option<&str>) -> u8 {
        // Lower priority than pytest (pytest runs unittest tests too)
        if !has_pytest_config(root) && has_unittest_files(root) { 70 }
        else { 0 }
    }
    fn name(&self) -> &str { "unittest" }
    fn suite_command(&self, _root: &Path, _level: Option<TestLevel>, _env: &HashMap<String, String>) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "python3".into(),
            args: vec!["-m".into(), "unittest".into(), "discover".into(), "-v".into()],
            env: HashMap::new(),
        })
    }
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_unittest_verbose_output(stdout, stderr, exit_code)
    }
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> { extract_python_traces(failure) }
}
```

### VitestAdapter

```rust
// src/test/vitest_adapter.rs
pub struct VitestAdapter;

impl TestAdapter for VitestAdapter {
    fn detect(&self, root: &Path, _command: Option<&str>) -> u8 {
        if root.join("vitest.config.ts").exists() || root.join("vitest.config.js").exists() { 90 }
        else if has_dep_in_package_json(root, "vitest") { 85 }
        else { 0 }
    }
    fn name(&self) -> &str { "vitest" }
    fn suite_command(&self, _root: &Path, level: Option<TestLevel>, _env: &HashMap<String, String>) -> Result<TestCommand> {
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
    fn single_test_command(&self, _root: &Path, test_name: &str) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".into(),
            args: vec!["vitest".into(), "run".into(), "-t".into(), test_name.into(), "--reporter=json".into()],
            env: HashMap::new(),
        })
    }
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_vitest_json_output(stdout, stderr, exit_code)
    }
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> { extract_js_traces(failure) }
    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        // Send SIGUSR1 for Node diagnostic report, or native stacks
        super::stacks::capture_native_stacks(pid)
    }
}
```

### JestAdapter

```rust
// src/test/jest_adapter.rs
pub struct JestAdapter;

impl TestAdapter for JestAdapter {
    fn detect(&self, root: &Path, _command: Option<&str>) -> u8 {
        if root.join("jest.config.js").exists() || root.join("jest.config.ts").exists() { 90 }
        else if has_dep_in_package_json(root, "jest") { 85 }
        else { 0 }
    }
    fn name(&self) -> &str { "jest" }
    fn suite_command(&self, _root: &Path, _level: Option<TestLevel>, _env: &HashMap<String, String>) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".into(),
            args: vec!["jest".into(), "--json".into(), "--no-coverage".into()],
            env: HashMap::new(),
        })
    }
    fn single_test_command(&self, _root: &Path, test_name: &str) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".into(),
            args: vec!["jest".into(), "--json".into(), "-t".into(), test_name.into()],
            env: HashMap::new(),
        })
    }
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_jest_json_output(stdout, stderr, exit_code)
    }
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> { extract_js_traces(failure) }
}
```

### BunTestAdapter

```rust
// src/test/bun_adapter.rs
pub struct BunTestAdapter;

impl TestAdapter for BunTestAdapter {
    fn detect(&self, root: &Path, cmd: Option<&str>) -> u8 {
        if let Some(cmd) = cmd { if cmd.contains("bun") { return 95; } }
        if root.join("bun.lockb").exists() { 85 }
        else { 0 }
    }
    fn name(&self) -> &str { "bun" }
    fn suite_command(&self, _root: &Path, _level: Option<TestLevel>, _env: &HashMap<String, String>) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".into(),
            args: vec!["test".into()],
            env: HashMap::new(),
        })
    }
    fn single_test_command(&self, _root: &Path, test_name: &str) -> Result<TestCommand> {
        Ok(TestCommand {
            program: "bun".into(),
            args: vec!["test".into(), "--filter".into(), test_name.into()],
            env: HashMap::new(),
        })
    }
    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_bun_test_output(stdout, stderr, exit_code)
    }
    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> { extract_js_traces(failure) }
}
```

### Registration

```rust
// src/test/mod.rs
impl TestRunner {
    pub fn new() -> Self {
        Self {
            adapters: vec![
                Box::new(CargoTestAdapter),      // Rust
                Box::new(Catch2Adapter),          // C++
                Box::new(PytestAdapter),          // Python (highest priority)
                Box::new(UnittestAdapter),        // Python (fallback)
                Box::new(VitestAdapter),          // TypeScript/Node
                Box::new(JestAdapter),            // TypeScript/Node
                Box::new(BunTestAdapter),         // Bun
                Box::new(GenericAdapter),         // Fallback always last
            ],
        }
    }
}
```

---

## MCP Interface Changes

### No Breaking Changes

The MCP tools remain identical. Language is transparent to the LLM:

```json
// All of these work the same regardless of language:
{ "tool": "debug_launch", "command": "python3", "args": ["app.py"], "projectRoot": "/project" }
{ "tool": "debug_trace", "sessionId": "...", "add": ["modules.audio.*"] }
{ "tool": "debug_breakpoint", "sessionId": "...", "add": [{ "file": "app.py", "line": 42 }] }
{ "tool": "debug_read", "sessionId": "...", "targets": [{ "variable": "g_counter" }] }
{ "tool": "debug_test", "projectRoot": "/my-python-project" }
```

### Updated Tool Descriptions

The MCP tool descriptions (in the daemon's `instructions` field) are updated to mention Python/JS/TS support. No structural changes to tool schemas.

### Pattern Syntax for Interpreted Languages

Same glob syntax, different namespace separators:

| Language | Pattern | Matches |
|----------|---------|---------|
| C++ | `audio::*` | Functions in `audio` namespace |
| Rust | `audio::*` | Functions in `audio` module |
| Python | `modules.audio.*` | Functions in `modules/audio.py` |
| JS/TS | `AudioProcessor.*` | Methods on `AudioProcessor` class |
| All | `@file:parser` | Functions in files containing "parser" |

The `@file:` pattern works identically — it's a source filename substring match regardless of language.

---

## New Dependencies

```toml
# Cargo.toml additions
rustpython-parser = "0.4"    # Python AST parsing
swc_ecma_parser = "5.0"      # TypeScript/JavaScript AST parsing
swc_common = "3.0"           # SWC utilities (source maps, spans)
sourcemap = "9.0"            # Source map parsing (TS→JS line mapping)
walkdir = "2.5"              # Recursive directory traversal for AST parsing
```

---

## Test Fixtures

### Fixture Structure

Each language gets a fixture program that mirrors the C++ `strobe_test_target` mode-for-mode, plus a test suite that mirrors Catch2/cargo test:

```
tests/fixtures/
├── cpp/                     # (existing) C++ fixture + Catch2 suite
├── rust/                    # (existing) Rust fixture + cargo tests
├── python/                  # NEW: Python fixture + pytest suite
├── node/                    # NEW: Node.js/TS fixture + vitest suite
├── node-jest/               # NEW: Node.js/TS fixture + jest suite
├── bun/                     # NEW: Bun/TS fixture + bun test suite
├── python-webapp/           # NEW: Flask/FastAPI real web app
├── node-webapp/             # NEW: Express real web app
└── bun-webapp/              # NEW: Bun.serve real web app
```

### Python Fixture (`tests/fixtures/python/`)

```
tests/fixtures/python/
├── fixture.py              # CLI entry point (argv modes)
├── modules/
│   ├── __init__.py
│   ├── audio.py            # process_buffer(), generate_sine(), apply_effect()
│   ├── midi.py             # note_on(), control_change(), generate_sequence()
│   ├── timing.py           # fast(), medium(), slow(), very_slow()
│   ├── engine.py           # Global state: g_counter, g_tempo, g_sample_rate
│   └── crash.py            # null_deref(), abort_signal(), stack_overflow()
├── tests/
│   ├── conftest.py         # Shared fixtures
│   ├── test_audio.py       # 4 tests: 3 pass, 1 fail (intentional)
│   ├── test_midi.py        # 3 tests: all pass
│   ├── test_engine.py      # 3 tests: 2 pass, 1 skip
│   └── test_stuck.py       # 1 test: infinite loop (for stuck detection)
├── pyproject.toml
└── requirements.txt        # pytest, pytest-json-report
```

**CLI Modes** (`fixture.py`):
- `hello` — `print("Hello from Python fixture")`
- `crash-exception` — `raise RuntimeError("intentional crash")`
- `crash-abort` — `os.abort()`
- `crash-segfault` — `ctypes.string_at(0)` (SIGSEGV via null pointer)
- `slow-functions` — 5 rounds of `timing.fast()`, `timing.medium()`, `timing.slow()`
- `threads` — 3 `threading.Thread` workers (2 audio, 1 MIDI) with named threads
- `globals` — 200 iterations updating `engine.g_counter`, `engine.g_tempo`
- `breakpoint-loop` — 10 iterations of `audio.process_buffer()` (argv[2] overrides count)
- `step-target` — Single pass through all module functions
- `write-target` — Loop until `engine.g_counter >= 999`
- `async-demo` — `asyncio.run()` with async functions (validates async tracing)
- `decorators` — Functions wrapped with `@functools.wraps` (validates dynamic resolution)

**Module globals** (for watch testing):
```python
# modules/engine.py
g_counter: int = 0
g_tempo: float = 120.0
g_sample_rate: int = 44100
g_point: dict = {"x": 1.0, "y": 2.0, "value": 42}
```

### Node.js Fixture (`tests/fixtures/node/`)

```
tests/fixtures/node/
├── fixture.ts              # CLI entry point
├── src/
│   ├── audio.ts            # AudioProcessor class + functions
│   ├── midi.ts             # MidiHandler class + functions
│   ├── timing.ts           # fast(), medium(), slow() with setTimeout
│   ├── engine.ts           # Global state + Engine class
│   └── crash.ts            # Crash scenarios
├── tests/
│   ├── audio.test.ts       # vitest: 4 tests (3 pass, 1 fail)
│   ├── midi.test.ts        # vitest: 3 tests
│   ├── engine.test.ts      # vitest: 3 tests (2 pass, 1 skip)
│   └── stuck.test.ts       # vitest: 1 stuck test
├── package.json            # vitest, typescript, tsx
├── vitest.config.ts
└── tsconfig.json
```

**CLI Modes** (`fixture.ts`):
- `hello` — `console.log("Hello from Node.js fixture")`
- `crash-throw` — `throw new Error("intentional crash")`
- `crash-abort` — `process.abort()`
- `crash-null` — `(null as any).property` (TypeError)
- `slow-functions` — Timing functions with `setTimeout` and `Atomics.wait`
- `threads` — 3 `worker_threads.Worker` instances
- `globals` — 200 iterations updating global state
- `breakpoint-loop` — 10 iterations of `audio.processBuffer()`
- `step-target` — Single pass through all modules
- `write-target` — Loop until `engine.gCounter >= 999`
- `async-demo` — Promise chains + async/await
- `class-methods` — Class with static/instance/private methods

**Module globals** (for watch testing):
```typescript
// src/engine.ts
export let gCounter = 0;
export let gTempo = 120.0;
export const gSampleRate = 44100;
export let gPoint = { x: 1.0, y: 2.0, value: 42 };
```

### Bun Fixture (`tests/fixtures/bun/`)

Same structure as Node fixture but with Bun-specific APIs:

```
tests/fixtures/bun/
├── fixture.ts              # CLI entry point (bun run fixture.ts)
├── src/
│   ├── audio.ts            # Uses Bun.sleep() instead of setTimeout
│   ├── midi.ts
│   ├── timing.ts           # Bun.sleep(), Bun.nanoseconds()
│   ├── engine.ts
│   └── crash.ts
├── tests/
│   ├── audio.test.ts       # bun test: describe/test/expect
│   ├── midi.test.ts
│   ├── engine.test.ts
│   └── stuck.test.ts
├── package.json
├── bunfig.toml
└── tsconfig.json
```

**Bun-specific features to exercise:**
- `Bun.sleep()` — async sleep
- `Bun.nanoseconds()` — high-precision timing
- `Bun.file()` — file I/O
- `bun:ffi` — native FFI call (tests JS↔native boundary)

### Real Web Application Fixtures

#### Python Web App (`tests/fixtures/python-webapp/`)

```
tests/fixtures/python-webapp/
├── app.py                  # FastAPI application
├── models.py               # SQLite models (sqlite3 stdlib)
├── services/
│   ├── auth.py             # Authentication: login(), verify_token()
│   └── data.py             # Data processing: process_items(), aggregate()
├── tests/
│   ├── conftest.py         # Test client fixture
│   ├── test_api.py         # 5 tests: GET/POST/PUT/DELETE + error handling
│   └── test_auth.py        # 3 tests: login, token verify, unauthorized
├── pyproject.toml
└── requirements.txt        # fastapi, uvicorn, httpx (for test client)
```

**Validation scenarios:**
1. `debug_launch("python3", ["-m", "uvicorn", "app:app"])` → launch server
2. `debug_trace(["services.auth.*"])` → trace auth function calls
3. HTTP request → observe traced function enter/exit with args
4. `debug_breakpoint({ file: "services/auth.py", line: 15 })` → pause on login
5. `debug_read({ variable: "services.auth.active_sessions" })` → watch session count
6. `debug_test({ projectRoot: "..." })` → pytest runs API tests with structured output

#### Node.js Web App (`tests/fixtures/node-webapp/`)

```
tests/fixtures/node-webapp/
├── src/
│   ├── index.ts            # Express server
│   ├── routes/
│   │   ├── auth.ts         # POST /login, GET /profile
│   │   └── data.ts         # GET /items, POST /items
│   ├── services/
│   │   ├── auth.ts         # AuthService class
│   │   └── db.ts           # SQLite via better-sqlite3
│   └── middleware/
│       └── validate.ts     # Request validation
├── tests/
│   ├── api.test.ts         # vitest: 5 API endpoint tests
│   └── auth.test.ts        # vitest: 3 auth service tests
├── package.json
├── vitest.config.ts
└── tsconfig.json
```

#### Bun Web App (`tests/fixtures/bun-webapp/`)

```
tests/fixtures/bun-webapp/
├── src/
│   ├── index.ts            # Bun.serve() HTTP server
│   ├── routes/
│   │   ├── auth.ts
│   │   └── data.ts
│   └── services/
│       ├── auth.ts
│       └── db.ts           # Bun's built-in SQLite
├── tests/
│   ├── api.test.ts         # bun test: 5 tests
│   └── auth.test.ts        # bun test: 3 tests
├── package.json
└── tsconfig.json
```

### E2E Test Files (Rust)

```
tests/
├── python_e2e.rs           # 15+ scenarios for Python fixture
├── node_e2e.rs             # 15+ scenarios for Node.js fixture
├── bun_e2e.rs              # 15+ scenarios for Bun fixture
├── python_webapp_e2e.rs    # Web app integration scenarios
├── node_webapp_e2e.rs      # Web app integration scenarios
├── bun_webapp_e2e.rs       # Web app integration scenarios
├── test_runner_python.rs   # pytest + unittest adapter tests
├── test_runner_node.rs     # vitest + jest adapter tests
├── test_runner_bun.rs      # bun test adapter tests
```

Each `*_e2e.rs` follows the same structure as `frida_e2e.rs`:

```rust
// tests/python_e2e.rs

#[test]
fn test_python_output_capture() {
    // Launch: python3 fixture.py hello
    // Assert: stdout contains "Hello from Python fixture"
}

#[test]
fn test_python_function_tracing() {
    // Launch: python3 fixture.py slow-functions
    // Add trace: modules.timing.*
    // Assert: function_enter/exit events with correct names
}

#[test]
fn test_python_crash_exception() {
    // Launch: python3 fixture.py crash-exception
    // Assert: stderr contains traceback
}

#[test]
fn test_python_crash_segfault() {
    // Launch: python3 fixture.py crash-segfault
    // Assert: crash event with SIGSEGV signal
}

#[test]
fn test_python_multi_thread() {
    // Launch: python3 fixture.py threads
    // Add trace: modules.audio.*, modules.midi.*
    // Assert: events from multiple thread IDs
}

#[test]
fn test_python_watch_variables() {
    // Launch: python3 fixture.py globals
    // Add watch: modules.engine.g_counter
    // Assert: watch values change over time
}

#[test]
fn test_python_debug_read() {
    // Launch: python3 fixture.py globals
    // debug_read: modules.engine.g_tempo
    // Assert: returns 120.0 (or close)
}

#[test]
fn test_python_breakpoint_pause_resume() {
    // Launch: python3 fixture.py breakpoint-loop
    // Set breakpoint: modules/audio.py:process_buffer entry
    // Assert: paused event, resume, paused again
}

#[test]
fn test_python_conditional_breakpoint() {
    // Launch: python3 fixture.py breakpoint-loop
    // Set breakpoint: condition "args[0] > 5"
    // Assert: only pauses when condition is true
}

#[test]
fn test_python_stepping() {
    // Launch: python3 fixture.py step-target
    // Set breakpoint at first function
    // step-over, step-into, step-out
    // Assert: paused at correct lines
}

#[test]
fn test_python_logpoint() {
    // Launch: python3 fixture.py breakpoint-loop
    // Set logpoint: "iteration={args[0]}"
    // Assert: logpoint events in timeline, no pause
}

#[test]
fn test_python_debug_write() {
    // Launch: python3 fixture.py write-target
    // Write: modules.engine.g_counter = 999
    // Assert: process exits (loop condition met)
}

#[test]
fn test_python_async_tracing() {
    // Launch: python3 fixture.py async-demo
    // Add trace: modules.*.async_*
    // Assert: async function enter/exit events
}

#[test]
fn test_python_decorator_resolution() {
    // Launch: python3 fixture.py decorators
    // Add trace: modules.*.decorated_func
    // Assert: function resolved despite @decorator wrapper
}
```

Equivalent test files for `node_e2e.rs` and `bun_e2e.rs` with identical scenarios plus language-specific ones (Promise tracing, source map validation, worker_threads, Bun.serve, etc.).

### Test Runner Adapter Tests

```rust
// tests/test_runner_python.rs

#[test]
fn test_pytest_adapter_detection() {
    // Point at fixtures/python/ → confidence >= 85
}

#[test]
fn test_pytest_execution_structured_output() {
    // Run pytest on fixtures/python/tests/
    // Assert: 9 pass, 1 fail, 1 skip
    // Assert: failure has file, line, message, suggested_traces
}

#[test]
fn test_pytest_single_test_filter() {
    // Run single test: "test_audio_process"
    // Assert: only that test runs
}

#[test]
fn test_pytest_stuck_detection() {
    // Run test_stuck.py (infinite loop)
    // Assert: stuck warning within 10s
}
```

```rust
// tests/test_runner_node.rs

#[test]
fn test_vitest_adapter_detection() {
    // Point at fixtures/node/ → confidence >= 85
}

#[test]
fn test_vitest_execution_structured_output() {
    // Run vitest on fixtures/node/tests/
    // Assert: structured JSON with pass/fail/skip counts
}

#[test]
fn test_jest_adapter_detection() {
    // Point at fixtures/node-jest/ → confidence >= 85
}
```

```rust
// tests/test_runner_bun.rs

#[test]
fn test_bun_adapter_detection() {
    // Point at fixtures/bun/ → confidence >= 85
}

#[test]
fn test_bun_test_execution() {
    // Run bun test on fixtures/bun/tests/
    // Assert: structured output with pass/fail counts
}
```

### Web App Integration Tests

```rust
// tests/python_webapp_e2e.rs

#[test]
fn test_python_webapp_launch_and_trace_request() {
    // 1. debug_launch("python3", ["-m", "uvicorn", "app:app", "--port", "0"])
    // 2. debug_trace(["services.auth.*"])
    // 3. HTTP POST /login (via reqwest from test)
    // 4. debug_query → function_enter for auth.login with args
    // 5. debug_stop
}

#[test]
fn test_python_webapp_breakpoint_on_request() {
    // 1. Launch server
    // 2. Set breakpoint: services/auth.py:login
    // 3. Send HTTP request
    // 4. Assert: paused
    // 5. debug_read: request body, session state
    // 6. Continue
}

#[test]
fn test_python_webapp_test_suite() {
    // 1. debug_test({ projectRoot: fixtures/python-webapp })
    // 2. Assert: pytest adapter detected, structured results
}
```

---

## New Source Files Summary

### Rust (daemon)

```
src/
├── symbols/
│   ├── mod.rs              # SymbolResolver trait, Language enum, ResolvedTarget
│   ├── dwarf_resolver.rs   # Wraps existing DwarfParser → SymbolResolver
│   ├── python_resolver.rs  # rustpython-parser AST → SymbolResolver
│   └── js_resolver.rs      # SWC AST + source maps → SymbolResolver
├── test/
│   ├── pytest_adapter.rs   # PytestAdapter
│   ├── unittest_adapter.rs # UnittestAdapter
│   ├── vitest_adapter.rs   # VitestAdapter
│   ├── jest_adapter.rs     # JestAdapter
│   └── bun_adapter.rs      # BunTestAdapter
```

### TypeScript (agent)

```
agent/src/
├── tracers/
│   ├── tracer.ts           # Tracer interface
│   ├── native-tracer.ts    # Wraps CModuleTracer
│   ├── python-tracer.ts    # CPython 3.11+ hooks
│   ├── v8-tracer.ts        # V8 Inspector Protocol
│   ├── jsc-tracer.ts       # JavaScriptCore hooks
│   └── js-common.ts        # Shared V8/JSC utilities
├── resolvers/
│   └── source-map.ts       # TS→JS source map resolution
```

### Test Fixtures

```
tests/fixtures/
├── python/                 # Python CLI fixture + pytest suite
├── node/                   # Node.js/TS fixture + vitest suite
├── node-jest/              # Node.js/TS fixture + jest suite
├── bun/                    # Bun/TS fixture + bun test suite
├── python-webapp/          # FastAPI web application
├── node-webapp/            # Express web application
└── bun-webapp/             # Bun.serve web application
```

### Rust E2E Tests

```
tests/
├── python_e2e.rs
├── node_e2e.rs
├── bun_e2e.rs
├── python_webapp_e2e.rs
├── node_webapp_e2e.rs
├── bun_webapp_e2e.rs
├── test_runner_python.rs
├── test_runner_node.rs
└── test_runner_bun.rs
```

---

## What Doesn't Change

- **Database schema**: Events table, queries — all language-agnostic
- **MCP tool schemas**: Same tools, same parameters
- **Stdout/stderr capture**: Frida Device-level output signal — universal
- **Session lifecycle**: create, stop, retain, delete — unchanged
- **Event storage limits**: 200k FIFO buffer — unchanged
- **Stuck detector**: CPU/stack sampling works for any process
- **Settings system**: Same settings, same resolution
- **VS Code extension**: Consumes same MCP tools — automatic support

---

## Implementation Phases

### Phase A: Foundation (Agent + Daemon Abstractions)

1. Create `Tracer` interface in agent, wrap `CModuleTracer` as `NativeTracer`
2. Add runtime detection in agent constructor
3. Create `SymbolResolver` trait in daemon, wrap `DwarfParser` as `DwarfResolver`
4. Add `Language` enum and `detect_language()` to session manager
5. Modify message protocol to support both `address` and `file:line` targets
6. **Validate**: All existing native tests still pass (no regression)

### Phase B: Python Support

1. Implement `PythonResolver` (rustpython-parser AST)
2. Implement `PythonTracer` (CPython frame eval hook)
3. Implement `PytestAdapter` and `UnittestAdapter`
4. Create Python fixture programs
5. Write Python e2e tests
6. **Validate**: All 15 e2e scenarios pass for Python fixture

### Phase C: Node.js Support

1. Implement `JSResolver` (SWC parser + source maps)
2. Implement `V8Tracer` (V8 Inspector Protocol)
3. Implement `VitestAdapter` and `JestAdapter`
4. Create Node.js fixture programs
5. Write Node.js e2e tests
6. **Validate**: All 15 e2e scenarios pass for Node.js fixture

### Phase D: Bun Support

1. Implement `JSCTracer` (JavaScriptCore hooks)
2. Implement `BunTestAdapter`
3. Create Bun fixture programs
4. Write Bun e2e tests
5. **Validate**: All 15 e2e scenarios pass for Bun fixture

### Phase E: Web App Integration

1. Create Python, Node, Bun web app fixtures
2. Write web app e2e tests (server launch + request tracing + breakpoints)
3. **Validate**: Real-world scenarios work end-to-end

### Phase F: Polish

1. Update MCP tool descriptions for multi-language support
2. Update FEATURES.md
3. Performance benchmarking across languages
4. Edge case testing (mixed-language projects, monorepos)
