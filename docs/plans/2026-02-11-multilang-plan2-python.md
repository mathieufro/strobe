# Multi-Language Plan 2: Python Support

**Spec:** `docs/specs/2026-02-11-python-js-ts-support.md`
**Depends on:** `docs/plans/2026-02-11-multilang-plan1-foundation.md` (must be implemented first)
**Goal:** Full Phase 1+2 feature parity for Python (CPython 3.11+): function tracing, breakpoints, logpoints, stepping, watches, test adapters, e2e tests.
**Architecture:** PythonResolver (rustpython-parser AST) in daemon, PythonTracer (CPython frame eval hooks) in agent, PytestAdapter + UnittestAdapter for test runner.
**Tech Stack:** Rust (rustpython-parser, walkdir), TypeScript (Frida agent), Python (fixtures)
**Commit strategy:** Commit at checkpoints (5 commits)

## Workstreams

- **Stream A (daemon resolvers + test adapters):** Tasks 1, 2, 5, 6 — Rust-only, no agent dependency
- **Stream B (agent tracer):** Tasks 3, 4 — TypeScript-only, needs Plan 1 Tracer interface
- **Stream C (fixtures):** Task 7 — Python files, no code dependency
- **Serial:** Tasks 8, 9, 10 (e2e integration — depends on A + B + C)

---

### Task 1: Add Rust Dependencies

**Files:**
- Modify: `Cargo.toml`

Add dependencies for Python AST parsing and directory walking:

```toml
# Python AST parsing
rustpython-parser = "0.4"
# Directory traversal for source file scanning
walkdir = "2.5"
```

**Verify:**
```bash
cargo check
```

**Checkpoint:** Dependencies resolve, project compiles.

---

### Task 2: PythonResolver Implementation

**Files:**
- Create: `src/symbols/python_resolver.rs`
- Modify: `src/symbols/mod.rs` (add module)

**Step 1: Write unit tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_extract_functions_basic() {
        let source = r#"
def hello():
    pass

def greet(name):
    return f"Hello, {name}"
"#;
        let functions = extract_functions_from_source(source, Path::new("app.py")).unwrap();
        assert_eq!(functions.len(), 2);
        assert!(functions.contains_key("hello"));
        assert!(functions.contains_key("greet"));
    }

    #[test]
    fn test_extract_class_methods() {
        let source = r#"
class AudioProcessor:
    def process_buffer(self, buf):
        pass

    def generate_sine(self, freq):
        pass

    @staticmethod
    def default_rate():
        return 44100
"#;
        let functions = extract_functions_from_source(source, Path::new("audio.py")).unwrap();
        assert!(functions.contains_key("AudioProcessor.process_buffer"));
        assert!(functions.contains_key("AudioProcessor.generate_sine"));
        assert!(functions.contains_key("AudioProcessor.default_rate"));
    }

    #[test]
    fn test_extract_nested_functions() {
        let source = r#"
def outer():
    def inner():
        pass
    return inner
"#;
        let functions = extract_functions_from_source(source, Path::new("nested.py")).unwrap();
        assert!(functions.contains_key("outer"));
        assert!(functions.contains_key("outer.inner"));
    }

    #[test]
    fn test_pattern_matching_dot_separator() {
        let resolver = PythonResolver::from_functions(vec![
            ("modules.audio.process_buffer".to_string(), ("audio.py".into(), 10)),
            ("modules.audio.generate_sine".to_string(), ("audio.py".into(), 20)),
            ("modules.midi.note_on".to_string(), ("midi.py".into(), 5)),
        ]);
        let targets = resolver.resolve_pattern("modules.audio.*", Path::new(".")).unwrap();
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn test_file_pattern() {
        let resolver = PythonResolver::from_functions(vec![
            ("handler".to_string(), ("app/handler.py".into(), 10)),
            ("main".to_string(), ("app/main.py".into(), 1)),
        ]);
        let targets = resolver.resolve_pattern("@file:handler", Path::new(".")).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].name(), "handler");
    }

    #[test]
    fn test_excluded_directories() {
        assert!(is_python_excluded("__pycache__"));
        assert!(is_python_excluded("venv"));
        assert!(is_python_excluded(".venv"));
        assert!(is_python_excluded("node_modules"));
        assert!(is_python_excluded(".git"));
        assert!(!is_python_excluded("modules"));
        assert!(!is_python_excluded("tests"));
    }
}
```

**Step 2: Implement PythonResolver**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::resolver::*;

pub struct PythonResolver {
    /// Parsed function definitions: qualified_name → (file_path, line_number)
    functions: HashMap<String, (PathBuf, u32)>,
}

/// Directories to exclude from Python source scanning.
fn is_python_excluded(name: &str) -> bool {
    matches!(name,
        "__pycache__" | "venv" | ".venv" | "env" | ".env" |
        "node_modules" | ".git" | ".tox" | ".mypy_cache" |
        ".pytest_cache" | "dist" | "build" | "*.egg-info"
    )
}

/// Extract function/class method definitions from a Python source string.
pub fn extract_functions_from_source(
    source: &str,
    file_path: &Path,
) -> crate::Result<HashMap<String, (PathBuf, u32)>> {
    use rustpython_parser::{parse, Mode};

    let ast = parse(source, Mode::Module, "<input>")
        .map_err(|e| crate::Error::Internal(format!("Python parse error in {:?}: {}", file_path, e)))?;

    let mut functions = HashMap::new();
    extract_from_module(&ast, file_path, &[], &mut functions);
    Ok(functions)
}

/// Recursively extract function definitions from AST nodes.
/// `prefix` tracks the qualified name (e.g., ["ClassName", "method"]).
fn extract_from_module(
    module: &rustpython_parser::ast::Mod,
    file_path: &Path,
    prefix: &[String],
    functions: &mut HashMap<String, (PathBuf, u32)>,
) {
    // Walk the AST body, extracting:
    // - FunctionDef / AsyncFunctionDef: add qualified_name = prefix.join(".") + "." + name
    // - ClassDef: recurse into body with class name added to prefix
    // Implementation uses rustpython_parser's visitor pattern or direct match on Stmt variants
    // The exact API depends on rustpython-parser version — adapt to 0.4.x
    todo!("Implement AST walking — see spec for logic")
}

impl PythonResolver {
    /// Parse all .py files in project_root.
    pub fn parse(project_root: &Path) -> crate::Result<Self> {
        let mut all_functions = HashMap::new();

        for entry in WalkDir::new(project_root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_str().unwrap_or("");
                !is_python_excluded(name)
            })
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("py") {
                match std::fs::read_to_string(path) {
                    Ok(source) => {
                        if let Ok(fns) = extract_functions_from_source(&source, path) {
                            // Qualify with module path relative to project_root
                            let rel = path.strip_prefix(project_root).unwrap_or(path);
                            let module_path = python_module_path(rel);
                            for (name, (file, line)) in fns {
                                let qualified = if module_path.is_empty() {
                                    name
                                } else {
                                    format!("{}.{}", module_path, name)
                                };
                                all_functions.insert(qualified, (file, line));
                            }
                        }
                    }
                    Err(_) => continue,
                }
            }
        }

        Ok(Self { functions: all_functions })
    }

    /// Create from pre-built function list (for testing).
    #[cfg(test)]
    pub fn from_functions(fns: Vec<(String, (PathBuf, u32))>) -> Self {
        Self {
            functions: fns.into_iter().collect(),
        }
    }
}

/// Convert a file path like "modules/audio.py" to module path "modules.audio".
fn python_module_path(rel_path: &Path) -> String {
    let stem = rel_path.with_extension("");
    let parts: Vec<_> = stem.components()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|s| *s != "__init__")
        .collect();
    parts.join(".")
}

impl SymbolResolver for PythonResolver {
    fn resolve_pattern(&self, pattern: &str, _project_root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
        if pattern.starts_with("@file:") {
            let file_substr = &pattern[6..];
            return Ok(self.functions.iter()
                .filter(|(_, (file, _))| {
                    file.to_string_lossy().contains(file_substr)
                })
                .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                    file: file.to_string_lossy().to_string(),
                    line: *line,
                    name: name.clone(),
                })
                .collect());
        }

        // Use PatternMatcher with `.` as separator for Python
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
        // For Python, file:line is directly usable — find matching function name
        for (name, (func_file, func_line)) in &self.functions {
            if func_file.to_string_lossy().contains(file) && *func_line == line {
                return Ok(Some(ResolvedTarget::SourceLocation {
                    file: func_file.to_string_lossy().to_string(),
                    line: *func_line,
                    name: name.clone(),
                }));
            }
        }
        // Line breakpoints don't need to match a function — they're set directly
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
    fn language(&self) -> Language { Language::Python }
    fn supports_runtime_resolution(&self) -> bool { true }
}
```

**Step 3: Update PatternMatcher for configurable separator**

The existing `PatternMatcher` in `src/dwarf/parser.rs` uses `::` as separator. Add a `new_with_separator` constructor:

```rust
impl PatternMatcher {
    pub fn new_with_separator(pattern: &str, separator: char) -> Self {
        // Same logic as `new()` but uses the given separator instead of `::`
        // For `*`: matches one segment (stops at separator)
        // For `**`: matches across separators
        todo!()
    }
}
```

**Step 4: Update symbols/mod.rs**

```rust
mod demangle;
pub mod resolver;
pub mod dwarf_resolver;
pub mod python_resolver;

pub use demangle::demangle_symbol;
pub use resolver::{Language, ResolvedTarget, VariableResolution, SymbolResolver};
pub use dwarf_resolver::DwarfResolver;
pub use python_resolver::PythonResolver;
```

**Verify:**
```bash
cargo test --lib symbols
```

**Checkpoint:** PythonResolver parses .py files, extracts function definitions, matches patterns. Unit tests pass.

### Commit 1: PythonResolver

```
feat: add PythonResolver for Python AST-based symbol resolution

Uses rustpython-parser to extract function/class method definitions
from .py files. Implements SymbolResolver trait with dot-separated
pattern matching. Excludes __pycache__, venv, etc.
```

---

### Task 3: PythonTracer Agent Implementation

**Files:**
- Create: `agent/src/tracers/python-tracer.ts`
- Modify: `agent/src/agent.ts` (wire PythonTracer into createTracer)

**Step 1: Implement PythonTracer**

Create `agent/src/tracers/python-tracer.ts`:

```typescript
import { Tracer, ResolvedTarget, HookMode, BreakpointMessage, StepHooksMessage,
         LogpointMessage, ReadMemoryMessage, WriteMemoryMessage } from './tracer';

// CPython 3.11+ struct offsets (may vary by version — probe at runtime)
const FRAME_CODE_OFFSET = 0x10;     // _PyInterpreterFrame.f_executable
const CODE_FILENAME_OFFSET = 0x68;  // PyCodeObject.co_filename
const CODE_NAME_OFFSET = 0x70;      // PyCodeObject.co_name
const CODE_FIRSTLINENO_OFFSET = 0x48; // PyCodeObject.co_firstlineno
const CODE_ARGCOUNT_OFFSET = 0x28;  // PyCodeObject.co_argcount

export class PythonTracer implements Tracer {
  private trackedFunctions: Map<string, ResolvedTarget> = new Map(); // "file:name" → target
  private frameEvalHook: InvocationListener | null = null;
  private nextEventId: number = 0;
  private agent: any;

  // Breakpoint state
  private breakpoints: Map<string, { file: string; line: number; condition?: string; hitCount: number; hits: number }> = new Map();
  private pausedThreads: Map<number, string> = new Map();

  // Logpoint state
  private logpoints: Map<string, { file: string; line: number; message: string; condition?: string }> = new Map();

  // sys.settrace installed flag
  private traceInstalled: boolean = false;

  // Python API function pointers (resolved at init)
  private pyRunString: NativeFunction | null = null;
  private pyGILStateEnsure: NativeFunction | null = null;
  private pyGILStateRelease: NativeFunction | null = null;
  private pyUnicodeAsUTF8: NativeFunction | null = null;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    // Resolve CPython API functions
    this.resolveCPythonAPI();
  }

  private resolveCPythonAPI(): void {
    const pyRunString = Module.findExportByName(null, 'PyRun_SimpleString');
    if (pyRunString) {
      this.pyRunString = new NativeFunction(pyRunString, 'int', ['pointer']);
    }

    const pyUnicodeAsUTF8 = Module.findExportByName(null, 'PyUnicode_AsUTF8');
    if (pyUnicodeAsUTF8) {
      this.pyUnicodeAsUTF8 = new NativeFunction(pyUnicodeAsUTF8, 'pointer', ['pointer']);
    }

    const ensure = Module.findExportByName(null, 'PyGILState_Ensure');
    const release = Module.findExportByName(null, 'PyGILState_Release');
    if (ensure && release) {
      this.pyGILStateEnsure = new NativeFunction(ensure, 'int', []);
      this.pyGILStateRelease = new NativeFunction(release, 'void', ['int']);
    }
  }

  dispose(): void {
    if (this.frameEvalHook) {
      this.frameEvalHook.detach();
      this.frameEvalHook = null;
    }
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const key = `${target.file}:${target.name}`;
    this.trackedFunctions.set(key, target);

    // Install the eval frame hook once (filters by tracked functions)
    if (!this.frameEvalHook) {
      this.installFrameEvalHook();
    }

    return this.trackedFunctions.size;
  }

  private installFrameEvalHook(): void {
    const evalFrame = Module.findExportByName(null, '_PyEval_EvalFrameDefault');
    if (!evalFrame) {
      send({ type: 'error', message: 'CPython _PyEval_EvalFrameDefault not found' });
      return;
    }

    const self = this;
    this.frameEvalHook = Interceptor.attach(evalFrame, {
      onEnter(args) {
        // args[1] = _PyInterpreterFrame*
        const frame = args[1];
        const codeInfo = self.readCodeObject(frame);
        if (!codeInfo) return;

        const key = `${codeInfo.filename}:${codeInfo.name}`;
        if (!self.trackedFunctions.has(key)) {
          // Also try just the function name (without file qualification)
          const nameOnly = self.trackedFunctions.has(codeInfo.name);
          if (!nameOnly) return;
        }

        send({ type: 'events', events: [{
          id: self.nextEventId++,
          timestampNs: Date.now() * 1_000_000, // ms → ns approximation
          threadId: Process.getCurrentThreadId(),
          eventType: 'function_enter',
          functionName: codeInfo.qualifiedName || codeInfo.name,
          sourceFile: codeInfo.filename,
          lineNumber: codeInfo.firstLineno,
          arguments: JSON.stringify({}), // TODO: capture args from locals array
        }]});

        (this as any)._strobeEnterTime = Date.now();
      },
      onLeave(retval) {
        const enterTime = (this as any)._strobeEnterTime;
        if (enterTime === undefined) return;
        delete (this as any)._strobeEnterTime;

        const durationMs = Date.now() - enterTime;
        send({ type: 'events', events: [{
          id: self.nextEventId++,
          timestampNs: Date.now() * 1_000_000,
          threadId: Process.getCurrentThreadId(),
          eventType: 'function_exit',
          durationNs: durationMs * 1_000_000,
        }]});
      }
    });
  }

  private readCodeObject(frame: NativePointer): { filename: string; name: string; qualifiedName: string; firstLineno: number } | null {
    try {
      // Read _PyInterpreterFrame.f_executable → PyCodeObject*
      const codeObj = frame.add(FRAME_CODE_OFFSET).readPointer();
      if (codeObj.isNull()) return null;

      // Read co_filename (PyUnicodeObject*)
      const filenameObj = codeObj.add(CODE_FILENAME_OFFSET).readPointer();
      const filename = this.readPyUnicode(filenameObj);

      // Read co_name (PyUnicodeObject*)
      const nameObj = codeObj.add(CODE_NAME_OFFSET).readPointer();
      const name = this.readPyUnicode(nameObj);

      // Read co_firstlineno
      const firstLineno = codeObj.add(CODE_FIRSTLINENO_OFFSET).readS32();

      return { filename, name, qualifiedName: name, firstLineno };
    } catch (e) {
      return null;
    }
  }

  private readPyUnicode(obj: NativePointer): string {
    if (this.pyUnicodeAsUTF8) {
      const utf8 = this.pyUnicodeAsUTF8(obj) as NativePointer;
      if (!utf8.isNull()) {
        return utf8.readUtf8String() || '<unknown>';
      }
    }
    return '<unknown>';
  }

  removeHook(id: number): void {
    // Remove by index — not directly applicable for Python
    // Would need to rebuild the tracked functions set
  }

  removeAllHooks(): void {
    this.trackedFunctions.clear();
    if (this.frameEvalHook) {
      this.frameEvalHook.detach();
      this.frameEvalHook = null;
    }
  }

  activeHookCount(): number {
    return this.trackedFunctions.size;
  }

  installBreakpoint(msg: BreakpointMessage): void {
    if (!msg.file || !msg.line) {
      send({ type: 'error', message: 'Python breakpoints require file and line' });
      return;
    }

    this.breakpoints.set(msg.id, {
      file: msg.file,
      line: msg.line,
      condition: msg.condition,
      hitCount: msg.hitCount || 0,
      hits: 0,
    });

    // Install sys.settrace if not already done
    if (!this.traceInstalled) {
      this.installSysTrace();
    }

    send({ type: 'breakpoint_set', id: msg.id, file: msg.file, line: msg.line });
  }

  private installSysTrace(): void {
    // Inject Python trace function via PyRun_SimpleString
    if (!this.pyRunString || !this.pyGILStateEnsure || !this.pyGILStateRelease) {
      send({ type: 'warning', message: 'Cannot install sys.settrace — CPython API not available' });
      return;
    }

    // This injects a trace function that sends Frida messages on line events
    // The actual implementation will use sys.settrace or sys.monitoring (3.12+)
    const traceCode = Memory.allocUtf8String(`
import sys, threading

def _strobe_trace(frame, event, arg):
    if event == 'line':
        # Check filename:line against registered breakpoints
        # Signal via native hook if matched
        pass
    return _strobe_trace

sys.settrace(_strobe_trace)
threading.settrace(_strobe_trace)
`);

    const gilState = this.pyGILStateEnsure();
    this.pyRunString(traceCode);
    this.pyGILStateRelease(gilState);
    this.traceInstalled = true;
  }

  removeBreakpoint(id: string): void {
    this.breakpoints.delete(id);
  }

  installStepHooks(msg: StepHooksMessage): void {
    // Python stepping uses sys.settrace events
    // step-over: set line trace on current frame
    // step-into: set trace on all frames
    // step-out: set return trace on current frame
    send({ type: 'warning', message: 'Python stepping: basic implementation' });
  }

  installLogpoint(msg: LogpointMessage): void {
    if (!msg.file || !msg.line) {
      send({ type: 'error', message: 'Python logpoints require file and line' });
      return;
    }
    this.logpoints.set(msg.id, {
      file: msg.file,
      line: msg.line,
      message: msg.message,
      condition: msg.condition,
    });
    send({ type: 'logpoint_set', id: msg.id });
  }

  removeLogpoint(id: string): void {
    this.logpoints.delete(id);
  }

  readVariable(expr: string): any {
    if (!this.pyRunString || !this.pyGILStateEnsure || !this.pyGILStateRelease) {
      return { error: 'CPython API not available' };
    }

    // Evaluate Python expression and capture result
    const code = Memory.allocUtf8String(`
import json, sys
try:
    _strobe_result = ${expr}
    sys.stdout.write('__STROBE_EVAL__' + json.dumps(_strobe_result, default=str) + '__STROBE_END__')
    sys.stdout.flush()
except Exception as e:
    sys.stderr.write('__STROBE_EVAL_ERR__' + str(e) + '__STROBE_END__')
    sys.stderr.flush()
`);

    const gilState = this.pyGILStateEnsure();
    this.pyRunString(code);
    this.pyGILStateRelease(gilState);

    // Result will arrive via stdout capture — agent.ts will parse it
    return null; // Async response
  }

  writeVariable(expr: string, value: any): void {
    if (!this.pyRunString || !this.pyGILStateEnsure || !this.pyGILStateRelease) return;

    const code = Memory.allocUtf8String(`${expr} = ${JSON.stringify(value)}`);
    const gilState = this.pyGILStateEnsure();
    this.pyRunString(code);
    this.pyGILStateRelease(gilState);
  }

  setImageBase(imageBase: string): void {
    // No-op for Python
  }

  getSlide(): NativePointer {
    return ptr(0);
  }

  resolvePattern(pattern: string): ResolvedTarget[] {
    // Agent-side fallback: enumerate loaded modules via sys.modules
    // This catches dynamically created functions (decorators, metaclasses)
    return [];
  }
}
```

**Step 2: Wire PythonTracer into agent.ts createTracer**

```typescript
import { PythonTracer } from './tracers/python-tracer';

function createTracer(runtime: string, agent: StrobeAgent): Tracer {
  switch (runtime) {
    case 'cpython':
      return new PythonTracer(agent);
    // ... existing cases ...
  }
}
```

**Step 3: Build agent**

```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
```

**Checkpoint:** Agent builds with PythonTracer. Not yet tested e2e.

### Commit 2: PythonTracer agent implementation

```
feat: add PythonTracer for CPython 3.11+ frame evaluation hooks

Hooks _PyEval_EvalFrameDefault for function tracing, uses
PyRun_SimpleString for breakpoints/watches, sys.settrace for
line-level hooks. Implements full Tracer interface.
```

---

### Task 4: Python Test Fixture Programs

**Files:**
- Create: `tests/fixtures/python/fixture.py`
- Create: `tests/fixtures/python/modules/__init__.py`
- Create: `tests/fixtures/python/modules/audio.py`
- Create: `tests/fixtures/python/modules/midi.py`
- Create: `tests/fixtures/python/modules/timing.py`
- Create: `tests/fixtures/python/modules/engine.py`
- Create: `tests/fixtures/python/modules/crash.py`
- Create: `tests/fixtures/python/pyproject.toml`
- Create: `tests/fixtures/python/requirements.txt`

**Step 1: Create the Python CLI fixture**

`tests/fixtures/python/fixture.py` — mirrors C++ `main.cpp` CLI modes:

```python
#!/usr/bin/env python3
"""Strobe test fixture — Python equivalent of C++ strobe_test_target."""

import sys
import os

def main():
    mode = sys.argv[1] if len(sys.argv) > 1 else "hello"

    if mode == "hello":
        print("Hello from Python fixture")
        print("Debug output on stderr", file=sys.stderr)

    elif mode == "crash-exception":
        print(f"[TARGET] PID={os.getpid()} mode=crash-exception")
        sys.stdout.flush()
        raise RuntimeError("intentional crash for testing")

    elif mode == "crash-abort":
        print(f"[TARGET] PID={os.getpid()} mode=crash-abort")
        sys.stdout.flush()
        os.abort()

    elif mode == "crash-segfault":
        import ctypes
        print(f"[TARGET] PID={os.getpid()} mode=crash-segfault")
        sys.stdout.flush()
        ctypes.string_at(0)  # SIGSEGV

    elif mode == "slow-functions":
        from modules import timing
        print("[TIMING] Running functions with varied durations...")
        for round_num in range(5):
            timing.fast()
            timing.fast()
            timing.fast()
            timing.medium()
            timing.slow()
            if round_num == 2:
                timing.very_slow()
        print("[TIMING] Done")

    elif mode == "threads":
        import threading
        from modules import audio, midi
        print("[THREADS] Starting multi-threaded mode")

        def audio_worker(worker_id):
            for i in range(50):
                buf = audio.generate_sine(440.0)
                audio.process_buffer(buf)
                import time; time.sleep(0.01)

        def midi_worker():
            for i in range(50):
                midi.note_on(60 + (i % 12), 100)
                import time; time.sleep(0.02)

        threads = [
            threading.Thread(target=audio_worker, args=(0,), name="audio-0"),
            threading.Thread(target=audio_worker, args=(1,), name="audio-1"),
            threading.Thread(target=midi_worker, name="midi-processor"),
        ]
        for t in threads: t.start()
        for t in threads: t.join()
        print("[THREADS] Done")

    elif mode == "globals":
        from modules import engine, audio
        print("[GLOBALS] Starting global variable updates")
        for i in range(200):
            engine.g_counter = i
            engine.g_tempo = 120.0 + (i % 10)
            engine.g_point["x"] = float(i)
            engine.g_point["y"] = float(i * 2)
            buf = audio.generate_sine(440.0)
            audio.process_buffer(buf)
            import time; time.sleep(0.1)
        print("[GLOBALS] Done")

    elif mode == "breakpoint-loop":
        from modules import audio, engine
        iterations = int(sys.argv[2]) if len(sys.argv) > 2 else 10
        print(f"[BP-LOOP] Running {iterations} iterations")
        for i in range(iterations):
            engine.g_counter = i
            engine.g_tempo = 120.0 + i
            buf = audio.generate_sine(440.0)
            rms = audio.process_buffer(buf)
            audio.apply_effect(buf, 0.5)
            print(f"[BP-LOOP] iter={i} counter={engine.g_counter} rms={rms:.3f} tempo={engine.g_tempo:.1f}")
        print(f"[BP-LOOP] Done, counter={engine.g_counter}")

    elif mode == "step-target":
        from modules import audio, midi, engine
        print("[STEP] Start")
        engine.g_counter = 0
        buf = audio.generate_sine(440.0)
        rms = audio.process_buffer(buf)
        audio.apply_effect(buf, 0.5)
        midi.note_on(60, 100)
        midi.control_change(1, 64)
        engine.g_counter = 42
        print(f"[STEP] Done counter={engine.g_counter} rms={rms:.3f}")

    elif mode == "write-target":
        from modules import audio, engine
        import time
        print("[WRITE] Waiting for g_counter to reach 999")
        engine.g_counter = 0
        for i in range(100):
            buf = audio.generate_sine(440.0)
            audio.process_buffer(buf)
            if engine.g_counter >= 999:
                print(f"[WRITE] g_counter reached 999 (actual={engine.g_counter}) at iteration {i}")
                return
            time.sleep(0.05)
        print(f"[WRITE] Timed out, g_counter={engine.g_counter}")

    elif mode == "async-demo":
        import asyncio
        from modules import audio

        async def async_process():
            buf = audio.generate_sine(440.0)
            await asyncio.sleep(0.01)
            return audio.process_buffer(buf)

        async def async_main():
            results = await asyncio.gather(
                async_process(),
                async_process(),
                async_process(),
            )
            print(f"[ASYNC] Results: {results}")

        asyncio.run(async_main())

    elif mode == "decorators":
        from modules.audio import decorated_process
        result = decorated_process(440.0)
        print(f"[DECORATORS] Result: {result}")

    else:
        print(f"Unknown mode: {mode}", file=sys.stderr)
        sys.exit(1)

if __name__ == "__main__":
    main()
```

**Step 2: Create module files**

`tests/fixtures/python/modules/__init__.py`: empty

`tests/fixtures/python/modules/audio.py`:
```python
"""Audio processing module."""
import math
import functools

def generate_sine(frequency: float, size: int = 512, sample_rate: int = 44100) -> list:
    """Generate a sine wave buffer."""
    return [math.sin(2 * math.pi * frequency * i / sample_rate) for i in range(size)]

def process_buffer(buf: list) -> float:
    """Calculate RMS of buffer."""
    if not buf:
        return 0.0
    sum_sq = sum(x * x for x in buf)
    return math.sqrt(sum_sq / len(buf))

def apply_effect(buf: list, gain: float) -> None:
    """Apply gain effect in-place."""
    for i in range(len(buf)):
        buf[i] *= gain

@functools.wraps
def decorated_process(freq: float) -> float:
    """Decorated function for testing dynamic resolution."""
    buf = generate_sine(freq)
    return process_buffer(buf)
```

`tests/fixtures/python/modules/midi.py`:
```python
"""MIDI processing module."""

def note_on(note: int, velocity: int) -> bool:
    """Process a MIDI note-on event."""
    return 0 <= note <= 127 and 0 <= velocity <= 127

def control_change(cc: int, value: int) -> bool:
    """Process a MIDI control change."""
    return 0 <= cc <= 127 and 0 <= value <= 127

def generate_sequence(length: int) -> list:
    """Generate a sequence of MIDI events."""
    return [{"note": 60 + (i % 12), "velocity": 100} for i in range(length)]
```

`tests/fixtures/python/modules/timing.py`:
```python
"""Timing functions with varied durations."""
import time

def fast():
    time.sleep(0.001)  # 1ms

def medium():
    time.sleep(0.05)   # 50ms

def slow():
    time.sleep(0.1)    # 100ms

def very_slow():
    time.sleep(0.3)    # 300ms
```

`tests/fixtures/python/modules/engine.py`:
```python
"""Global state for watch variable testing."""

g_counter: int = 0
g_tempo: float = 120.0
g_sample_rate: int = 44100
g_point: dict = {"x": 1.0, "y": 2.0, "value": 42}
```

`tests/fixtures/python/modules/crash.py`:
```python
"""Crash scenarios."""
import os
import ctypes

def raise_exception():
    raise RuntimeError("intentional crash for testing")

def abort_signal():
    os.abort()

def null_deref():
    ctypes.string_at(0)

def stack_overflow(depth=0):
    return stack_overflow(depth + 1)
```

**Step 3: Create pytest test suite**

`tests/fixtures/python/tests/conftest.py`:
```python
import pytest

@pytest.fixture
def audio_buffer():
    from modules.audio import generate_sine
    return generate_sine(440.0)
```

`tests/fixtures/python/tests/test_audio.py`:
```python
from modules import audio

def test_audio_generate_sine():
    buf = audio.generate_sine(440.0)
    assert len(buf) == 512

def test_audio_process_buffer(audio_buffer):
    rms = audio.process_buffer(audio_buffer)
    assert rms > 0.0

def test_audio_apply_effect(audio_buffer):
    audio.apply_effect(audio_buffer, 2.0)
    rms = audio.process_buffer(audio_buffer)
    assert rms > 0.0

def test_audio_intentional_failure():
    """Intentional failure for adapter validation."""
    assert audio.process_buffer([]) == 1.0  # Will fail: returns 0.0
```

`tests/fixtures/python/tests/test_midi.py`:
```python
from modules import midi

def test_midi_note_on():
    assert midi.note_on(60, 100) is True

def test_midi_control_change():
    assert midi.control_change(1, 64) is True

def test_midi_generate_sequence():
    seq = midi.generate_sequence(8)
    assert len(seq) == 8
```

`tests/fixtures/python/tests/test_engine.py`:
```python
import pytest
from modules import engine

def test_engine_counter():
    engine.g_counter = 42
    assert engine.g_counter == 42

def test_engine_tempo():
    engine.g_tempo = 140.0
    assert engine.g_tempo == 140.0

@pytest.mark.skip(reason="Skipped for adapter validation")
def test_engine_skipped():
    pass
```

`tests/fixtures/python/tests/test_stuck.py`:
```python
def test_infinite_loop():
    """Intentionally stuck test for stuck detection validation."""
    while True:
        pass
```

**Step 4: Create project config**

`tests/fixtures/python/pyproject.toml`:
```toml
[project]
name = "strobe-python-fixture"
version = "0.1.0"
requires-python = ">=3.11"

[tool.pytest.ini_options]
testpaths = ["tests"]
markers = [
    "integration: integration tests",
    "e2e: end-to-end tests",
]
```

`tests/fixtures/python/requirements.txt`:
```
pytest>=7.0
pytest-json-report>=1.5
```

**Checkpoint:** Python fixture runs: `python3 tests/fixtures/python/fixture.py hello` prints "Hello from Python fixture". Pytest runs: `cd tests/fixtures/python && python3 -m pytest` shows 3 pass, 1 fail, 1 skip.

### Commit 3: Python test fixtures

```
feat: add Python test fixture programs

CLI fixture with 12 modes mirroring C++ strobe_test_target.
Pytest suite with intentional fail/skip/stuck for adapter validation.
Modules: audio, midi, timing, engine, crash.
```

---

### Task 5: PytestAdapter Implementation

**Files:**
- Create: `src/test/pytest_adapter.rs`
- Modify: `src/test/mod.rs` (register adapter)

**Step 1: Write unit tests for output parsing**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_pytest_config() {
        let adapter = PytestAdapter;
        let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python");
        if fixture_dir.exists() {
            let confidence = adapter.detect(&fixture_dir, None);
            assert!(confidence >= 80, "Should detect pytest in fixture dir: got {}", confidence);
        }
    }

    #[test]
    fn test_parse_pytest_json_report() {
        let json_output = r#"{"summary":{"passed":3,"failed":1,"total":4,"collected":4},"tests":[{"nodeid":"tests/test_audio.py::test_audio_generate_sine","outcome":"passed","duration":0.001},{"nodeid":"tests/test_audio.py::test_audio_intentional_failure","outcome":"failed","duration":0.002,"call":{"longrepr":"AssertionError: assert 0.0 == 1.0"},"lineno":15}]}"#;
        let result = parse_pytest_json_report(json_output, "", 1);
        assert_eq!(result.summary.passed, 3);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].name.contains("intentional_failure"));
    }

    #[test]
    fn test_suggest_traces_python() {
        let failure = TestFailure {
            name: "tests/test_audio.py::TestAudio::test_process".to_string(),
            file: Some("tests/test_audio.py".to_string()),
            line: Some(15),
            message: "AssertionError".to_string(),
            rerun: None,
            suggested_traces: vec![],
        };
        let adapter = PytestAdapter;
        let traces = adapter.suggest_traces(&failure);
        assert!(!traces.is_empty());
    }
}
```

**Step 2: Implement PytestAdapter**

```rust
use std::collections::HashMap;
use std::path::Path;
use super::adapter::*;

pub struct PytestAdapter;

impl TestAdapter for PytestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // pyproject.toml with [tool.pytest] section
        if project_root.join("pyproject.toml").exists() {
            if let Ok(content) = std::fs::read_to_string(project_root.join("pyproject.toml")) {
                if content.contains("[tool.pytest") { return 90; }
            }
        }
        // pytest.ini or setup.cfg with [tool:pytest]
        if project_root.join("pytest.ini").exists() { return 90; }
        if project_root.join("conftest.py").exists() { return 85; }
        // requirements.txt with pytest
        if let Ok(content) = std::fs::read_to_string(project_root.join("requirements.txt")) {
            if content.contains("pytest") { return 80; }
        }
        // Any test_*.py files
        if has_python_test_files(project_root) { return 60; }
        0
    }

    fn name(&self) -> &str { "pytest" }

    fn suite_command(
        &self,
        _project_root: &Path,
        level: Option<TestLevel>,
        _env: &HashMap<String, String>,
    ) -> crate::Result<TestCommand> {
        let mut args = vec![
            "-m".into(), "pytest".into(),
            "--tb=short".into(), "-q".into(),
            "--json-report".into(), "--json-report-file=-".into(),
        ];
        match level {
            Some(TestLevel::Unit) => { args.extend(["-m".into(), "not integration and not e2e".into()]); }
            Some(TestLevel::Integration) => { args.extend(["-m".into(), "integration".into()]); }
            Some(TestLevel::E2e) => { args.extend(["-m".into(), "e2e".into()]); }
            None => {}
        }
        Ok(TestCommand { program: "python3".into(), args, env: HashMap::new() })
    }

    fn single_test_command(&self, _root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "python3".into(),
            args: vec![
                "-m".into(), "pytest".into(),
                "-k".into(), test_name.into(),
                "--json-report".into(), "--json-report-file=-".into(),
                "--tb=short".into(),
            ],
            env: HashMap::new(),
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        parse_pytest_json_report(stdout, stderr, exit_code)
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        extract_python_traces(failure)
    }

    fn capture_stacks(&self, pid: u32) -> Vec<ThreadStack> {
        // Fall back to native stacks (py-spy integration is future work)
        super::stacks::capture_native_stacks(pid)
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

fn has_python_test_files(root: &Path) -> bool {
    // Check for test_*.py files in common locations
    for dir in ["tests", "test", "."] {
        let test_dir = root.join(dir);
        if test_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&test_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("test_") && name.ends_with(".py") {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Parse pytest-json-report output.
fn parse_pytest_json_report(stdout: &str, _stderr: &str, _exit_code: i32) -> TestResult {
    // Find JSON in stdout (may be mixed with other output)
    // pytest-json-report writes JSON to stdout when --json-report-file=-
    // Parse the JSON and extract test results
    // Implementation: serde_json::from_str on the JSON portion
    todo!("Implement pytest JSON report parsing")
}

/// Extract suggested trace patterns from a Python test failure.
fn extract_python_traces(failure: &TestFailure) -> Vec<String> {
    let mut traces = Vec::new();

    // Extract module name from test path: "tests/test_audio.py::TestAudio::test_process"
    if let Some(ref file) = failure.file {
        if let Some(filename) = Path::new(file).file_stem().and_then(|s| s.to_str()) {
            // "test_audio" → trace "audio.*"
            let module = filename.strip_prefix("test_").unwrap_or(filename);
            traces.push(format!("{}.*", module));
        }
        traces.push(format!("@file:{}", Path::new(file).file_name().unwrap_or_default().to_string_lossy()));
    }

    traces
}

/// Update progress from pytest output (line-by-line incremental parsing).
pub fn update_progress(line: &str, progress: &std::sync::Arc<std::sync::Mutex<super::TestProgress>>) {
    let trimmed = line.trim();

    // Detect test collection phase
    if trimmed.starts_with("collecting") || trimmed.starts_with("collected") {
        let mut p = progress.lock().unwrap();
        if p.phase == super::TestPhase::Compiling {
            p.phase = super::TestPhase::Running;
        }
    }

    // Detect individual test results from verbose output
    // "tests/test_audio.py::test_generate PASSED"
    if trimmed.contains(" PASSED") {
        let mut p = progress.lock().unwrap();
        p.passed += 1;
    } else if trimmed.contains(" FAILED") {
        let mut p = progress.lock().unwrap();
        p.failed += 1;
    } else if trimmed.contains(" SKIPPED") || trimmed.contains(" XFAIL") {
        let mut p = progress.lock().unwrap();
        p.skipped += 1;
    }
}
```

**Step 3: Register in TestRunner**

In `src/test/mod.rs`, add:
```rust
pub mod pytest_adapter;
use pytest_adapter::PytestAdapter;

// In TestRunner::new():
adapters: vec![
    Box::new(CargoTestAdapter),
    Box::new(Catch2Adapter),
    Box::new(PytestAdapter),      // NEW
    // ... more adapters later ...
],
```

**Verify:**
```bash
cargo test --lib test::pytest_adapter
```

**Checkpoint:** PytestAdapter detects Python projects, generates correct commands, parses output.

---

### Task 6: UnittestAdapter Implementation

**Files:**
- Create: `src/test/unittest_adapter.rs`
- Modify: `src/test/mod.rs` (register adapter)

Similar structure to PytestAdapter but lower priority detection (70 when no pytest config found) and parses verbose unittest output format.

**Checkpoint:** Both Python test adapters registered and unit-tested.

### Commit 4: Python test adapters

```
feat: add PytestAdapter and UnittestAdapter for Python test execution

Pytest: auto-detects via pyproject.toml/conftest.py, uses --json-report
for structured output. Unittest: fallback when pytest not configured.
Both registered in TestRunner.
```

---

### Task 7: Python E2E Tests

**Files:**
- Create: `tests/python_e2e.rs`
- Modify: `tests/common/mod.rs` (add python fixture helpers)

**Step 1: Add Python fixture helper to common/mod.rs**

```rust
/// Return the Python fixture directory.
pub fn python_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/python")
}

/// Return the path to fixture.py
pub fn python_fixture() -> PathBuf {
    python_fixture_dir().join("fixture.py")
}
```

**Step 2: Write Python e2e tests**

```rust
// tests/python_e2e.rs
mod common;
use common::*;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_python_e2e_scenarios() {
    let fixture = python_fixture();
    let fixture_str = fixture.to_str().unwrap();
    let project_root = python_fixture_dir().to_str().unwrap().to_string();
    let (sm, _dir) = create_session_manager();

    eprintln!("=== Python 1/6: Output capture ===");
    python_output_capture(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== Python 2/6: Function tracing ===");
    python_function_tracing(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== Python 3/6: Crash capture (exception) ===");
    python_crash_exception(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== Python 4/6: Multi-threaded tracing ===");
    python_multithreaded(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== Python 5/6: Watch variables ===");
    python_watch_variables(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== Python 6/6: Breakpoint pause/resume ===");
    python_breakpoint(&sm, fixture_str, &project_root).await;

    eprintln!("\n=== All Python E2E scenarios passed ===");
}

async fn python_output_capture(
    sm: &strobe::daemon::SessionManager,
    fixture: &str,
    project_root: &str,
) {
    let session_id = "py-e2e-output";
    let pid = sm
        .spawn_with_frida(session_id, "python3", &[fixture.to_string(), "hello".to_string()],
                          None, project_root, None, false)
        .await
        .unwrap();
    sm.create_session(session_id, "python3", project_root, pid).unwrap();

    let stdout_events = poll_events_typed(
        sm, session_id, Duration::from_secs(10),
        strobe::db::EventType::Stdout,
        |events| collect_stdout(events).contains("Hello from Python fixture"),
    ).await;

    assert!(collect_stdout(&stdout_events).contains("Hello from Python fixture"));
    let _ = sm.stop_frida(session_id).await;
    let _ = sm.stop_session(session_id);
}

// ... additional scenario implementations following the same pattern as frida_e2e.rs ...
```

**Verify:**
```bash
cargo test --test python_e2e -- --nocapture
```

**Checkpoint:** Python e2e tests pass, validating output capture, tracing, crashes, threads, watches, breakpoints.

### Commit 5: Python e2e tests

```
feat: add Python e2e integration tests

6 scenarios testing output capture, function tracing, crash handling,
multi-threading, watch variables, and breakpoints against the Python
fixture. Follows same pattern as frida_e2e.rs.
```

---

### Task 8: Python Web App Fixture (Stretch)

**Files:**
- Create: `tests/fixtures/python-webapp/` directory structure

FastAPI web application with auth + data services, pytest test suite. This validates real-world server debugging scenarios (launch server, trace requests, set breakpoints on handlers).

**Deferred:** Can be implemented after core Python support is validated.

---

### Task 9: Test Runner Integration for Python

**Files:**
- Create: `tests/test_runner_python.rs`

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_runner_python_scenarios() {
    let (sm, _dir) = create_session_manager();

    eprintln!("=== 1/4: Pytest adapter detection ===");
    test_pytest_detection();

    eprintln!("\n=== 2/4: Pytest execution ===");
    test_pytest_execution(&sm).await;

    eprintln!("\n=== 3/4: Pytest single test filter ===");
    test_pytest_single_test(&sm).await;

    eprintln!("\n=== 4/4: Pytest stuck detection ===");
    test_pytest_stuck_detection(&sm).await;

    eprintln!("\n=== All Python test runner scenarios passed ===");
}
```

**Checkpoint:** Test runner correctly spawns pytest via Frida, captures structured output, detects stuck tests.
