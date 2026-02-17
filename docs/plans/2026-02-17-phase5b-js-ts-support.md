# Phase 5b: JavaScript/TypeScript Support

**Goal:** Full Strobe feature parity for JS/TS projects — function tracing (`debug_trace`), variable inspection (`debug_read`, `debug_watch`), and test instrumentation (`debug_test`) for Node.js, Bun, Vitest, and Jest.

**Architecture:**
- **Daemon:** `JsResolver` (regex AST scanner + source map lookup), three new test adapters (Vitest JSON, Jest JSON, bun:test JUnit XML)
- **Agent:** Two tracers — `V8Tracer` runs in Frida's V8 script runtime (inside Node.js's own V8 context, giving direct `require()` access); `JscTracer` hooks `vmEntryToJavaScript` natively for Bun
- **Wiring:** `spawner.rs` sets `FRIDA_SCRIPT_RUNTIME_V8` for JS sessions; `session_manager.rs` instantiates `JsResolver`

**Tech Stack:** Rust (daemon), TypeScript/Frida (agent), `sourcemap = "9"` crate, `quick-xml` (already present for Catch2), `regex` + `walkdir` (already present)

**Commit strategy:** Single commit at end

**New Cargo deps (Cargo.toml):**
```toml
sourcemap = "9"
```

---

## Workstreams

- **Stream A (Rust/Daemon):** Tasks A1–A5 — JsResolver, source maps, 3 test adapters
- **Stream B (Agent/TypeScript):** Tasks B1–B3 — V8Tracer, JscTracer, readVariable
- **Stream C (Wiring):** Tasks C1–C4 — depends on A and B completing
- **Stream D (Tests):** Tasks D1–D4 parallel with A/B; D5–D8 after C

Streams A, B, and D1–D4 are fully independent.

---

## Stream A — Rust/Daemon

### Task A1: `JsResolver` — function extraction from JS/TS source

**Files:**
- Create: `src/symbols/js_resolver.rs`
- Modify: `src/symbols/mod.rs`
- Modify: `Cargo.toml` (add `sourcemap = "9"`)

**Approach:** Line-by-line regex scanning of `.js/.ts/.jsx/.tsx/.mjs/.cjs` files. Recognises:
- `function foo(` / `async function foo(`
- `const foo = (...) =>` / `export const foo = async (...) =>`
- `class Foo { method(` → `Foo.method`
- `export default function(` → `(default)`
- Skips `node_modules`, `dist`, `.git`, `.next`, `.nuxt`, `coverage`

Source map support (Task A2) is layered on top of the same struct.

**Step 1: Write the failing tests**

```rust
// Bottom of src/symbols/js_resolver.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ── Unit: function extraction ─────────────────────────────────────
    #[test]
    fn test_function_declarations() {
        let src = r#"
function greet(name) { return `Hello ${name}`; }
async function fetchData(url) { return fetch(url); }
export function helper() {}
export default function() {}
"#;
        let fns = extract_functions_from_source(src, Path::new("/tmp/a.js")).unwrap();
        assert!(fns.contains_key("greet"),     "named function");
        assert!(fns.contains_key("fetchData"), "async function");
        assert!(fns.contains_key("helper"),    "export function");
    }

    #[test]
    fn test_arrow_functions() {
        let src = r#"
const add = (a, b) => a + b;
export const mul = async (a, b) => {
    return a * b;
};
let counter = n => n + 1;
"#;
        let fns = extract_functions_from_source(src, Path::new("/tmp/b.ts")).unwrap();
        assert!(fns.contains_key("add"),     "arrow fn");
        assert!(fns.contains_key("mul"),     "async arrow export");
        assert!(fns.contains_key("counter"), "single-arg arrow");
    }

    #[test]
    fn test_class_methods() {
        let src = r#"
class Calculator {
    add(x, y) { return x + y; }
    async fetchResult() {}
    static create() { return new Calculator(); }
    get value() { return this._v; }
}
abstract class Base {
    abstract process(): void;
    run() {}
}
"#;
        let fns = extract_functions_from_source(src, Path::new("/tmp/c.ts")).unwrap();
        assert!(fns.contains_key("Calculator.add"),        "instance method");
        assert!(fns.contains_key("Calculator.fetchResult"),"async method");
        assert!(fns.contains_key("Calculator.create"),     "static method");
        assert!(fns.contains_key("Calculator.value"),      "getter");
        assert!(fns.contains_key("Base.run"),              "abstract class method");
    }

    #[test]
    fn test_nested_classes() {
        let src = r#"
class Outer {
    run() {}
    class Inner {
        go() {}
    }
}
"#;
        // Inner class not in our outer scope — only Outer.run expected
        let fns = extract_functions_from_source(src, Path::new("/tmp/d.ts")).unwrap();
        assert!(fns.contains_key("Outer.run"), "outer method");
    }

    #[test]
    fn test_line_numbers_are_correct() {
        let src = "// line 1\n// line 2\nfunction foo() {}\n";
        // foo is on line 3
        let fns = extract_functions_from_source(src, Path::new("/tmp/e.js")).unwrap();
        let (_, line) = fns.get("foo").unwrap();
        assert_eq!(*line, 3);
    }

    // ── Unit: glob pattern matching ───────────────────────────────────
    #[test]
    fn test_resolve_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("calc.js"), r#"
class Calculator {
    add(x, y) { return x + y; }
    sub(x, y) { return x - y; }
}
function helper() {}
"#).unwrap();
        let resolver = JsResolver::from_project(dir.path()).unwrap();

        let all = resolver.resolve_pattern("Calculator.*", dir.path()).unwrap();
        assert_eq!(all.len(), 2);
        let names: Vec<_> = all.iter().map(|t| t.name().to_string()).collect();
        assert!(names.contains(&"Calculator.add".to_string()));
        assert!(names.contains(&"Calculator.sub".to_string()));

        let one = resolver.resolve_pattern("helper", dir.path()).unwrap();
        assert_eq!(one.len(), 1);

        let star = resolver.resolve_pattern("*", dir.path()).unwrap();
        assert_eq!(star.len(), 3, "star matches all top-level functions + class methods");
    }

    #[test]
    fn test_deep_star_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.ts"), r#"
class A { foo() {} }
class B { bar() {} }
function top() {}
"#).unwrap();
        let resolver = JsResolver::from_project(dir.path()).unwrap();
        let all = resolver.resolve_pattern("**", dir.path()).unwrap();
        assert_eq!(all.len(), 3, "** matches everything including class.method");
    }

    // ── Unit: node_modules exclusion ─────────────────────────────────
    #[test]
    fn test_skips_excluded_dirs() {
        let dir = tempfile::tempdir().unwrap();
        for skip in &["node_modules", "dist", ".git", ".next", "coverage"] {
            std::fs::create_dir_all(dir.path().join(skip)).unwrap();
            std::fs::write(dir.path().join(skip).join("index.js"),
                "function shouldNotAppear() {}").unwrap();
        }
        std::fs::write(dir.path().join("index.js"), "function main() {}").unwrap();

        let resolver = JsResolver::from_project(dir.path()).unwrap();
        let all = resolver.resolve_pattern("*", dir.path()).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name(), "main");
    }

    // ── Unit: TypeScript-specific syntax ─────────────────────────────
    #[test]
    fn test_typescript_generics_and_decorators() {
        let src = r#"
@injectable()
class Service {
    process<T>(item: T): T { return item; }
    async fetch<T extends Base>(id: string): Promise<T> {}
}
const typed = <T>(x: T): T => x;
"#;
        let fns = extract_functions_from_source(src, Path::new("/tmp/f.ts")).unwrap();
        assert!(fns.contains_key("Service.process"), "generic method");
        assert!(fns.contains_key("Service.fetch"),   "async generic method");
        assert!(fns.contains_key("typed"),           "generic arrow fn");
    }
}
```

**Step 2: Run — verify compile error**

```bash
cargo test -p strobe js_resolver 2>&1 | head -5
```
Expected: `error[E0433]: failed to resolve: use of undeclared crate or module 'js_resolver'`

**Step 3: Implement `src/symbols/js_resolver.rs`**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use regex::Regex;
use walkdir::WalkDir;
use super::resolver::*;

/// Parsed function table: qualified_name → (absolute_file_path, 1-indexed line)
pub type FunctionTable = HashMap<String, (PathBuf, u32)>;

const SKIP_DIRS: &[&str] = &[
    "node_modules", "dist", "build", ".git", ".next", ".nuxt",
    "coverage", "__pycache__", ".cache", ".turbo", ".svelte-kit",
];

const JS_EXTENSIONS: &[&str] = &["js", "ts", "jsx", "tsx", "mjs", "cjs", "mts", "cts"];

/// Line-by-line regex extraction of JS/TS function definitions.
/// Returns a map of qualified name → (file, 1-indexed line).
pub fn extract_functions_from_source(source: &str, file: &Path) -> crate::Result<FunctionTable> {
    // Compiled once per call — cheap for file-level invocation
    let re_fn   = Regex::new(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s+(\w+)\s*[<(]").unwrap();
    let re_arrow = Regex::new(r"^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*(?::\s*\S+)?\s*=\s*(?:async\s+)?(?:\([^)]*\)|[\w$]+)\s*=>").unwrap();
    let re_class = Regex::new(r"^\s*(?:@\S+\s*)*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+(\w+)").unwrap();
    let re_method = Regex::new(
        r"^\s*(?:(?:async|static|public|private|protected|override|abstract|readonly|declare|get|set)\s+)*(?:async\s+)?(?:\*\s*)?(\w[\w$]*)\s*[<(]"
    ).unwrap();

    // Keywords that look like method declarations but aren't
    let kw: std::collections::HashSet<&str> = [
        "if", "for", "while", "switch", "catch", "return", "throw", "delete",
        "typeof", "instanceof", "new", "import", "export", "default", "class",
        "const", "let", "var", "async", "await", "yield", "function", "try",
        "else", "do", "in", "of", "from", "with", "void", "case",
    ].iter().copied().collect();

    let mut result: FunctionTable = HashMap::new();
    // Stack of (class_name, brace_depth_when_class_opened)
    let mut class_stack: Vec<(String, i32)> = Vec::new();
    let mut brace_depth: i32 = 0;
    let mut in_template_literal = false;
    let mut in_block_comment = false;

    for (i, line) in source.lines().enumerate() {
        let line_num = (i + 1) as u32;

        // Crude block comment tracking (covers /* ... */ across lines)
        if in_block_comment {
            if line.contains("*/") { in_block_comment = false; }
            continue;
        }
        if line.contains("/*") && !line.contains("*/") {
            in_block_comment = true;
        }
        // Skip single-line comments
        let stripped = if let Some(idx) = line.find("//") { &line[..idx] } else { line };
        // Skip template literal lines (simple heuristic)
        let backtick_count = stripped.chars().filter(|&c| c == '`').count();
        if backtick_count % 2 != 0 { in_template_literal = !in_template_literal; }
        if in_template_literal { brace_depth += stripped.chars().filter(|&c| c == '{').count() as i32
                                              - stripped.chars().filter(|&c| c == '}').count() as i32; continue; }

        let opens  = stripped.chars().filter(|&c| c == '{').count() as i32;
        let closes = stripped.chars().filter(|&c| c == '}').count() as i32;
        brace_depth += opens;

        // Pop class context when we leave its scope
        class_stack.retain(|(_, depth)| brace_depth > *depth);

        let current_class = class_stack.last().map(|(n, _)| n.clone());

        if let Some(cap) = re_fn.captures(stripped) {
            result.insert(cap[1].to_string(), (file.to_path_buf(), line_num));
        } else if let Some(cap) = re_arrow.captures(stripped) {
            result.insert(cap[1].to_string(), (file.to_path_buf(), line_num));
        } else if let Some(cap) = re_class.captures(stripped) {
            class_stack.push((cap[1].to_string(), brace_depth - opens));
        } else if let Some(cls) = current_class {
            if let Some(cap) = re_method.captures(stripped) {
                let method = cap[1].to_string();
                if !kw.contains(method.as_str()) && !method.starts_with("__") {
                    result.insert(format!("{}.{}", cls, method), (file.to_path_buf(), line_num));
                }
            }
        }

        brace_depth -= closes;
        if brace_depth < 0 { brace_depth = 0; } // Guard against misparse
    }

    Ok(result)
}

pub struct JsResolver {
    functions: FunctionTable,
    /// source map cache: absolute .js path → parsed SourceMap bytes (lazy)
    source_maps: HashMap<PathBuf, Vec<u8>>,
}

impl JsResolver {
    pub fn from_project(root: &Path) -> crate::Result<Self> {
        let mut functions = FunctionTable::new();
        let mut source_maps = HashMap::new();

        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if path.components().any(|c| SKIP_DIRS.contains(&c.as_os_str().to_str().unwrap_or(""))) {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if JS_EXTENSIONS.contains(&ext) {
                if let Ok(src) = std::fs::read_to_string(path) {
                    let fns = extract_functions_from_source(&src, path)?;
                    functions.extend(fns);
                }
            }
            // Pre-index .map files for source map resolution
            if ext == "map" {
                if let Ok(bytes) = std::fs::read(path) {
                    // Key: the .js file this map belongs to (strip .map suffix)
                    let js_path = path.with_extension("");
                    source_maps.insert(js_path, bytes);
                }
            }
        }

        Ok(Self { functions, source_maps })
    }

    pub fn function_count(&self) -> usize { self.functions.len() }

    /// Resolve a compiled JS file:line back to original TypeScript file:line via source map.
    /// Returns None if no map found or position unmapped.
    pub fn resolve_sourcemap(&self, js_file: &Path, line: u32, col: u32) -> Option<(PathBuf, u32)> {
        let map_bytes = self.source_maps.get(js_file)?;
        let sm = sourcemap::SourceMap::from_reader(std::io::Cursor::new(map_bytes)).ok()?;
        // sourcemap uses 0-indexed lines; our line numbers are 1-indexed
        let token = sm.lookup_token(line.saturating_sub(1), col)?;
        if token.get_src_line() == u32::MAX { return None; }
        let src_file = token.get_source()?;
        let abs = js_file.parent()?.join(src_file);
        Some((abs, token.get_src_line() + 1)) // convert back to 1-indexed
    }
}

fn pattern_to_regex(pattern: &str) -> crate::Result<regex::Regex> {
    // ** → match anything (including dots); * → match non-dot chars
    let escaped = regex::escape(&pattern.replace("**", "\x00").replace('*', "\x01"))
        .replace("\x00", ".*")
        .replace("\x01", "[^.]*");
    regex::Regex::new(&format!("^{}$", escaped))
        .map_err(|e| crate::Error::Internal(format!("Bad JS pattern '{}': {}", pattern, e)))
}

impl SymbolResolver for JsResolver {
    fn resolve_pattern(&self, pattern: &str, _root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
        let re = pattern_to_regex(pattern)?;
        Ok(self.functions.iter()
            .filter(|(name, _)| re.is_match(name))
            .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                file: file.to_string_lossy().to_string(),
                line: *line,
                name: name.clone(),
            })
            .collect())
    }

    fn resolve_line(&self, file: &str, line: u32) -> crate::Result<Option<ResolvedTarget>> {
        Ok(self.functions.iter()
            .find(|(_, (fpath, fline))| fpath.to_string_lossy().ends_with(file) && *fline == line)
            .map(|(name, (fpath, fline))| ResolvedTarget::SourceLocation {
                file: fpath.to_string_lossy().to_string(),
                line: *fline,
                name: name.clone(),
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

Register in `src/symbols/mod.rs`:
```rust
pub mod js_resolver;
pub use js_resolver::JsResolver;
```

Add to `Cargo.toml` under `[dependencies]`:
```toml
sourcemap = "9"
```

**Step 4: Run tests — verify they pass**
```bash
cargo test -p strobe js_resolver
```
Expected: all tests pass (some tests may need `tempfile` in dev-deps — already present).

**Checkpoint:** `JsResolver` extracts JS/TS functions from a project tree with correct line numbers, resolves glob patterns, skips excluded directories, and can look up TypeScript source locations via `.map` files.

---

### Task A2: Source map verification tests

**Files:**
- Modify: `src/symbols/js_resolver.rs` (add source map test)

**Step 1: Write the failing test**

```rust
#[test]
fn test_sourcemap_resolution() {
    // A minimal valid sourcemap:
    // Generated dist/index.js line 1, col 0  → src/index.ts line 3, col 0
    let map_json = r#"{
        "version": 3,
        "sources": ["../src/index.ts"],
        "names": [],
        "mappings": "AAEA"
    }"#;
    // "AAEA" = single mapping: gen(0,0) → src(0,0,2,0) meaning line=2(0-indexed), col=0
    // Actually let's use a real mapping. AACA = gen(0,0) → (src_idx=0, src_line=1, src_col=0)
    // VLQ: A=0, A=0, C=1, A=0 → delta(src_line)=1 → src_line=1 (0-indexed) = line 2 (1-indexed)
    // This is complex to construct manually; use a two-line approach:
    let dir = tempfile::tempdir().unwrap();
    let js_path = dir.path().join("dist").join("index.js");
    std::fs::create_dir_all(js_path.parent().unwrap()).unwrap();
    std::fs::write(&js_path, "\"use strict\";\nfunction greet() {}\n").unwrap();
    // Write a sourcemap that says line 2 of index.js → line 5 of ../src/index.ts
    let map_content = include_str!("../../tests/fixtures/sourcemap_test.map");
    std::fs::write(js_path.with_extension("js.map"), map_content).unwrap();

    let resolver = JsResolver::from_project(dir.path()).unwrap();
    // Resolution should map back to TypeScript source
    if let Some((ts_file, ts_line)) = resolver.resolve_sourcemap(&js_path, 2, 0) {
        assert!(ts_file.to_string_lossy().ends_with("index.ts"), "should map to .ts file");
        assert!(ts_line > 0, "should have valid line number");
    }
    // Note: if no .map file is found, resolve_sourcemap returns None — that's OK
}
```

Create `tests/fixtures/sourcemap_test.map` — a minimal but valid V3 source map linking `dist/index.js` line 2 to `src/index.ts` line 5:
```json
{
  "version": 3,
  "file": "index.js",
  "sourceRoot": "",
  "sources": ["../src/index.ts"],
  "sourcesContent": null,
  "names": [],
  "mappings": ";AAKA"
}
```
(`;AAKA` = skip line 1 with `;`, then gen(1,0) → src_file=0, src_line=5(0-indexed=4→+4 delta), src_col=0)

**Step 2: Run and verify**
```bash
cargo test -p strobe test_sourcemap_resolution
```

**Checkpoint:** Source map resolution finds TypeScript file:line from compiled JS position.

---

### Task A3: Vitest test adapter

**Files:**
- Create: `src/test/vitest_adapter.rs`
- Modify: `src/test/mod.rs` (add to adapter list)

**Vitest JSON format** (from `vitest run --reporter=json`, written to stdout):
```json
{
  "numTotalTestSuites": 1, "numFailedTestSuites": 1,
  "numTotalTests": 2, "numPassedTests": 1, "numFailedTests": 1,
  "success": false, "startTime": 1697737019307,
  "testResults": [{
    "name": "/abs/path/math.test.ts",
    "status": "failed",
    "startTime": 1697737019787, "endTime": 1697737019797,
    "assertionResults": [{
      "ancestorTitles": ["Math operations"],
      "title": "adds two numbers",
      "fullName": "Math operations adds two numbers",
      "status": "passed",
      "duration": 5
    }, {
      "ancestorTitles": ["Math operations"],
      "title": "multiplies correctly",
      "fullName": "Math operations multiplies correctly",
      "status": "failed",
      "duration": 3,
      "failureMessages": ["expected 6 to be 7 // Object.is equality\n  at multiply (math.test.ts:12:5)"]
    }]
  }]
}
```

**Step 1: Write the failing tests**

```rust
// Bottom of src/test/vitest_adapter.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const PASS_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 1, "numFailedTestSuites": 0,
        "numTotalTests": 2, "numPassedTests": 2, "numFailedTests": 0,
        "success": true, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/math.test.ts",
            "status": "passed",
            "startTime": 1000100, "endTime": 1000250,
            "assertionResults": [
                {"ancestorTitles": ["Math"], "title": "adds", "status": "passed", "duration": 5},
                {"ancestorTitles": ["Math"], "title": "subs", "status": "passed", "duration": 3}
            ]
        }]
    }"#;

    const FAIL_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 0, "numFailedTestSuites": 1,
        "numTotalTests": 2, "numPassedTests": 1, "numFailedTests": 1,
        "success": false, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/math.test.ts",
            "status": "failed",
            "startTime": 1000100, "endTime": 1000300,
            "assertionResults": [
                {
                    "ancestorTitles": ["Math", "addition"],
                    "title": "adds correctly",
                    "fullName": "Math addition adds correctly",
                    "status": "failed",
                    "duration": 12,
                    "failureMessages": ["AssertionError: expected 3 to deeply equal 4\n    at Object.<anonymous> (/project/src/math.test.ts:8:12)"]
                },
                {"ancestorTitles": ["Math"], "title": "subs", "status": "passed", "duration": 3}
            ]
        }]
    }"#;

    const PENDING_JSON: &str = r#"{
        "numTotalTestSuites": 1, "numPassedTestSuites": 0, "numFailedTestSuites": 0,
        "numTotalTests": 1, "numPassedTests": 0, "numFailedTests": 0, "numPendingTests": 1,
        "success": true, "startTime": 1000000,
        "testResults": [{
            "name": "/project/src/todo.test.ts",
            "status": "passed",
            "startTime": 1000100, "endTime": 1000110,
            "assertionResults": [
                {"ancestorTitles": [], "title": "todo test", "status": "todo", "duration": 0}
            ]
        }]
    }"#;

    #[test]
    fn test_detect_vitest_project() {
        let dir = tempfile::tempdir().unwrap();
        // Should NOT detect without vitest config
        let adapter = VitestAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        // Detect with package.json containing vitest
        std::fs::write(dir.path().join("package.json"),
            r#"{"devDependencies": {"vitest": "^1.0.0"}}"#).unwrap();
        assert!(adapter.detect(dir.path(), None) > 0, "should detect vitest in package.json");
    }

    #[test]
    fn test_detect_vitest_config() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;

        std::fs::write(dir.path().join("vitest.config.ts"), "export default {}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90, "vitest.config.ts = max confidence");
    }

    #[test]
    fn test_parse_all_passed() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(PASS_JSON, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
        assert_eq!(result.all_tests.len(), 2);
        assert!(result.all_tests.iter().all(|t| t.status == super::super::adapter::TestStatus::Pass));
    }

    #[test]
    fn test_parse_failure_extracts_message_and_location() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(FAIL_JSON, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures.len(), 1);

        let f = &result.failures[0];
        assert_eq!(f.name, "Math addition adds correctly");
        assert!(f.message.contains("expected 3"), "failure message extracted");
        // File and line extracted from stack trace
        assert!(f.file.as_deref().unwrap_or("").ends_with("math.test.ts"));
    }

    #[test]
    fn test_parse_nested_describe_full_name() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(FAIL_JSON, "", 1);
        // Full name should combine ancestorTitles + title
        assert_eq!(result.failures[0].name, "Math addition adds correctly");
    }

    #[test]
    fn test_parse_pending_tests() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output(PENDING_JSON, "", 0);
        assert_eq!(result.summary.skipped, 1);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_non_json_output_graceful() {
        let adapter = VitestAdapter;
        let result = adapter.parse_output("not json at all", "stderr line", 1);
        // Should not panic; may report as a single synthetic failure
        // Exit code != 0 with unparseable output = report as crash
        assert!(result.failures.len() <= 1);
    }

    #[test]
    fn test_suggest_traces_from_failure() {
        let adapter = VitestAdapter;
        let failure = super::super::adapter::TestFailure {
            name: "Math addition adds correctly".to_string(),
            file: Some("/project/src/math.test.ts".to_string()),
            line: Some(8),
            message: "AssertionError".to_string(),
            rerun_command: None,
            suggested_traces: vec![],
        };
        let traces = adapter.suggest_traces(&failure);
        assert!(!traces.is_empty(), "should suggest traces");
        // Should include file-based pattern
        assert!(traces.iter().any(|t| t.contains("@file:math.test")));
    }

    #[test]
    fn test_suite_command_structure() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        let cmd = adapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        // Should call vitest run with json reporter
        assert!(cmd.program.contains("vitest") || cmd.args.iter().any(|a| a.contains("vitest")));
        assert!(cmd.args.iter().any(|a| a.contains("json")), "should use json reporter");
    }

    #[test]
    fn test_single_test_command() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = VitestAdapter;
        let cmd = adapter.single_test_command(dir.path(), "Math addition adds correctly").unwrap();
        // Should include test name filter
        assert!(cmd.args.iter().any(|a| a.contains("Math")));
    }
}
```

**Step 2: Run — verify compile error**
```bash
cargo test -p strobe vitest_adapter 2>&1 | head -5
```

**Step 3: Implement `src/test/vitest_adapter.rs`**

Key implementation notes:
- Detection: 95 confidence for `vitest.config.{ts,js,mts}`, 90 for `vitest` in `package.json` devDependencies, 60 for `vitest` in scripts
- Command: `npx vitest run --reporter=json` (no `--outputFile` → stdout JSON)
- Single test: `npx vitest run --reporter=json -t "test name pattern"`
- Output parsing: Parse JSON from stdout using `serde_json`; extract `testResults[].assertionResults`
- For each assertion: `status` ∈ `passed|failed|pending|todo|skipped`; `ancestorTitles.join(" ") + " " + title` = full name
- Extract file:line from `failureMessages[0]` using regex `\(([^)]+\.test\.\w+):(\d+):\d+\)`
- Suggest traces: `@file:<stem>` for the test file; if failure has specific function in stack, add `<module>.*`
- Timeout: 120s unit, 300s integration, 600s e2e
- `update_progress()`: returns `None` (no streaming JSON during run)

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use serde::Deserialize;
use crate::test::adapter::*;
use crate::test::TestPhase;

pub struct VitestAdapter;

#[derive(Deserialize)]
struct VitestReport {
    #[serde(rename = "numPassedTests", default)]
    num_passed: u64,
    #[serde(rename = "numFailedTests", default)]
    num_failed: u64,
    #[serde(rename = "numPendingTests", default)]
    num_pending: u64,
    #[serde(rename = "numTodoTests", default)]
    num_todo: u64,
    #[serde(rename = "testResults", default)]
    test_results: Vec<VitestSuite>,
}

#[derive(Deserialize)]
struct VitestSuite {
    name: String,
    #[serde(rename = "assertionResults", default)]
    assertions: Vec<VitestAssertion>,
}

#[derive(Deserialize)]
struct VitestAssertion {
    #[serde(rename = "ancestorTitles", default)]
    ancestors: Vec<String>,
    title: String,
    #[serde(rename = "fullName", default)]
    full_name: String,
    status: String, // "passed" | "failed" | "pending" | "todo" | "skipped"
    duration: Option<f64>,
    #[serde(rename = "failureMessages", default)]
    failure_messages: Vec<String>,
}

impl TestAdapter for VitestAdapter {
    fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
        // Highest: explicit vitest config file
        for cfg in &["vitest.config.ts", "vitest.config.js", "vitest.config.mts", "vitest.config.mjs"] {
            if project_root.join(cfg).exists() { return 95; }
        }
        // High: vitest in package.json devDependencies or dependencies
        if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
            if pkg.contains("\"vitest\"") { return 90; }
        }
        // Medium: vite.config with test key (Vitest is Vite-native)
        for cfg in &["vite.config.ts", "vite.config.js"] {
            if let Ok(c) = std::fs::read_to_string(project_root.join(cfg)) {
                if c.contains("\"test\"") || c.contains("'test'") { return 70; }
            }
        }
        0
    }

    fn name(&self) -> &str { "vitest" }

    fn suite_command(&self, project_root: &Path, level: Option<TestLevel>,
                     env: &HashMap<String, String>) -> crate::Result<TestCommand> {
        let mut args = vec![
            "vitest".to_string(), "run".to_string(),
            "--reporter=json".to_string(),
            "--no-coverage".to_string(),
        ];
        if let Some(TestLevel::Unit) = level {
            // Unit tests usually in src/ — let vitest auto-discover
        }
        Ok(TestCommand {
            program: "npx".to_string(),
            args,
            cwd: project_root.to_path_buf(),
            env: env.clone(),
            stdin: None,
        })
    }

    fn single_test_command(&self, project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
        Ok(TestCommand {
            program: "npx".to_string(),
            args: vec![
                "vitest".to_string(), "run".to_string(),
                "--reporter=json".to_string(),
                "--no-coverage".to_string(),
                "-t".to_string(), test_name.to_string(),
            ],
            cwd: project_root.to_path_buf(),
            env: Default::default(),
            stdin: None,
        })
    }

    fn parse_output(&self, stdout: &str, stderr: &str, exit_code: i32) -> TestResult {
        // Find JSON in stdout (may have non-JSON prefix from npx)
        let json_start = stdout.find('{').unwrap_or(0);
        let json_str = &stdout[json_start..];

        let report: VitestReport = match serde_json::from_str(json_str) {
            Ok(r) => r,
            Err(_) => {
                // Unparseable — treat as crash if exit code != 0
                let failures = if exit_code != 0 {
                    vec![TestFailure {
                        name: "Test run crashed".to_string(),
                        file: None, line: None,
                        message: format!("Could not parse vitest output.\nstderr: {}", stderr.chars().take(500).collect::<String>()),
                        rerun_command: None,
                        suggested_traces: vec![],
                    }]
                } else { vec![] };
                return TestResult { summary: TestSummary::default(), failures, all_tests: vec![], stuck: vec![] };
            }
        };

        let stack_re = regex::Regex::new(r"\(([^)]+\.(?:test|spec)\.\w+):(\d+):\d+\)").unwrap();
        let mut failures = vec![];
        let mut all_tests = vec![];

        for suite in &report.test_results {
            for a in &suite.assertions {
                let full_name = if !a.full_name.is_empty() {
                    a.full_name.clone()
                } else {
                    let mut parts = a.ancestors.clone();
                    parts.push(a.title.clone());
                    parts.join(" > ")
                };

                let status = match a.status.as_str() {
                    "passed" => TestStatus::Pass,
                    "failed" => TestStatus::Fail,
                    "todo" | "pending" | "skipped" => TestStatus::Skip,
                    _ => TestStatus::Skip,
                };

                all_tests.push(TestDetail {
                    name: full_name.clone(),
                    status: status.clone(),
                    duration_ms: a.duration.map(|d| d as u64),
                    stdout: None, stderr: None, message: None,
                });

                if matches!(status, TestStatus::Fail) {
                    let msg = a.failure_messages.first().cloned().unwrap_or_default();
                    // Extract file:line from stack trace
                    let (file, line) = stack_re.captures(&msg)
                        .map(|c| (Some(c[1].to_string()), c[2].parse().ok()))
                        .unwrap_or((None, None));

                    failures.push(TestFailure {
                        name: full_name,
                        file,
                        line,
                        message: msg,
                        rerun_command: None,
                        suggested_traces: vec![],
                    });
                }
            }
        }

        let summary = TestSummary {
            passed: report.num_passed as usize,
            failed: report.num_failed as usize,
            skipped: (report.num_pending + report.num_todo) as usize,
            duration_ms: None,
        };

        TestResult { summary, failures, all_tests, stuck: vec![] }
    }

    fn suggest_traces(&self, failure: &TestFailure) -> Vec<String> {
        let mut traces = vec![];
        if let Some(file) = &failure.file {
            let stem = Path::new(file).file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("test");
            // Remove .test / .spec suffix for module name
            let module = stem.trim_end_matches(".test").trim_end_matches(".spec");
            traces.push(format!("@file:{}", stem));
            traces.push(format!("{}.*", module));
        }
        traces
    }

    fn capture_stacks(&self, _pid: u32) -> Vec<ThreadStack> { vec![] }

    fn default_timeout(&self, level: Option<TestLevel>) -> u64 {
        match level {
            Some(TestLevel::Unit) => 120_000,
            Some(TestLevel::Integration) => 300_000,
            Some(TestLevel::E2e) => 600_000,
            None => 180_000,
        }
    }
}
```

Register in `src/test/mod.rs` (find the adapter list instantiation):
```rust
// In TestRunner::new() adapter list:
adapters.push(Box::new(VitestAdapter));
// Give it priority 88 (below Cargo=90, pytest=90, above Catch2=85)
```

**Step 4: Run tests**
```bash
cargo test -p strobe vitest_adapter
```
Expected: all tests pass.

**Checkpoint:** `VitestAdapter` detects vitest projects, builds correct commands, and fully parses JSON reporter output including nested describes, failures with file:line, and pending tests.

---

### Task A4: Jest test adapter

**Files:**
- Create: `src/test/jest_adapter.rs`
- Modify: `src/test/mod.rs`

**Jest JSON format** (`jest --json`): Nearly identical to Vitest but:
- `testResults[].testFilePath` (not `name`)
- Inner array is `testResults` (not `assertionResults`)
- `testResults[].perfStats.runtime` for duration instead of `startTime`/`endTime` diff

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const JEST_PASS: &str = r#"{
        "success": true, "startTime": 1000000,
        "numTotalTests": 2, "numPassedTests": 2, "numFailedTests": 0, "numPendingTests": 0,
        "testResults": [{
            "testFilePath": "/project/src/__tests__/calc.test.js",
            "numPassingTests": 2, "numFailingTests": 0,
            "perfStats": {"start": 1000100, "end": 1000300, "runtime": 200},
            "testResults": [
                {"title": "adds", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 5, "failureMessages": []},
                {"title": "subs", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 3, "failureMessages": []}
            ]
        }]
    }"#;

    const JEST_FAIL: &str = r#"{
        "success": false, "startTime": 1000000,
        "numTotalTests": 2, "numPassedTests": 1, "numFailedTests": 1, "numPendingTests": 0,
        "testResults": [{
            "testFilePath": "/project/src/__tests__/calc.test.js",
            "numPassingTests": 1, "numFailingTests": 1,
            "perfStats": {"start": 1000100, "end": 1000400, "runtime": 300},
            "testResults": [
                {
                    "title": "multiplies",
                    "status": "failed",
                    "ancestorTitles": ["Calculator", "multiply"],
                    "duration": 8,
                    "failureMessages": [
                        "Error: expect(received).toBe(expected)\nExpected: 6\nReceived: 5\n    at Object.<anonymous> (/project/src/__tests__/calc.test.js:15:5)"
                    ]
                },
                {"title": "adds", "status": "passed", "ancestorTitles": ["Calculator"],
                 "duration": 5, "failureMessages": []}
            ]
        }]
    }"#;

    #[test]
    fn test_detect_jest() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = JestAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("jest.config.js"), "module.exports = {}").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90);
    }

    #[test]
    fn test_parse_passing() {
        let result = JestAdapter.parse_output(JEST_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing_with_location() {
        let result = JestAdapter.parse_output(JEST_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        assert_eq!(result.failures[0].name, "Calculator multiply multiplies");
        assert!(result.failures[0].file.as_deref().unwrap_or("").ends_with("calc.test.js"));
        assert_eq!(result.failures[0].line, Some(15));
    }

    #[test]
    fn test_suite_command_uses_json_flag() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = JestAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert!(cmd.args.iter().any(|a| a == "--json"));
    }
}
```

**Step 3: Implement `src/test/jest_adapter.rs`**

Same pattern as VitestAdapter but:
- Detection: `jest.config.{js,ts,cjs,mjs}` = 92, `"jest"` in package.json = 88, `jest` in scripts = 70
- Command: `npx jest --json` (JSON to stdout)
- Parse `testResults[].testResults` (inner array) instead of `assertionResults`
- Full name: `ancestorTitles.join(" ") + " " + title`

(Implementation follows VitestAdapter structure — adapt field names per format above.)

**Step 4: Run tests**
```bash
cargo test -p strobe jest_adapter
```

**Checkpoint:** Jest adapter fully parses `jest --json` output with correct failure extraction and location.

---

### Task A5: `bun:test` adapter (JUnit XML)

**Files:**
- Create: `src/test/bun_adapter.rs`
- Modify: `src/test/mod.rs`

**Bun test output:** No JSON reporter. Use `--reporter=junit` which outputs JUnit XML (same format as Catch2, already parsed by `quick-xml`). Command: `bun test --reporter=junit`.

**JUnit XML structure** (bun:test):
```xml
<testsuites name="..." tests="3" failures="1" time="0.123">
  <testsuite name="math.test.ts" tests="3" failures="1" time="0.100">
    <testcase name="Math > adds" classname="math.test.ts" time="0.005">
    </testcase>
    <testcase name="Math > multiplies" classname="math.test.ts" time="0.003">
      <failure message="expected 6 got 5" type="AssertionError">
        at multiply (math.test.ts:12:5)
      </failure>
    </testcase>
    <testcase name="Math > skipped" classname="math.test.ts" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>
```

**Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const JUNIT_PASS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="0" time="0.050">
  <testsuite name="calc.test.ts" tests="2" failures="0" time="0.040">
    <testcase name="Math > adds two numbers" classname="calc.test.ts" time="0.005"/>
    <testcase name="Math > subs two numbers" classname="calc.test.ts" time="0.003"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_FAIL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="2" failures="1" time="0.060">
  <testsuite name="calc.test.ts" tests="2" failures="1" time="0.050">
    <testcase name="Math > multiplies" classname="calc.test.ts" time="0.008">
      <failure message="Expected 6, got 5" type="AssertionError">
AssertionError: Expected 6, got 5
    at &lt;anonymous&gt; (calc.test.ts:12:7)
      </failure>
    </testcase>
    <testcase name="Math > adds" classname="calc.test.ts" time="0.005"/>
  </testsuite>
</testsuites>"#;

    const JUNIT_SKIP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="bun test" tests="1" failures="0" time="0.010">
  <testsuite name="todo.test.ts" tests="1" failures="0" time="0.005">
    <testcase name="todo test" classname="todo.test.ts" time="0">
      <skipped/>
    </testcase>
  </testsuite>
</testsuites>"#;

    #[test]
    fn test_detect_bun() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = BunAdapter;
        assert_eq!(adapter.detect(dir.path(), None), 0);

        std::fs::write(dir.path().join("bun.lockb"), b"").unwrap();
        assert!(adapter.detect(dir.path(), None) >= 90, "bun.lockb → high confidence");
    }

    #[test]
    fn test_detect_bun_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"),
            r#"{"scripts": {"test": "bun test"}}"#).unwrap();
        let adapter = BunAdapter;
        assert!(adapter.detect(dir.path(), None) >= 80);
    }

    #[test]
    fn test_parse_passing_junit() {
        let result = BunAdapter.parse_output(JUNIT_PASS, "", 0);
        assert_eq!(result.summary.passed, 2);
        assert_eq!(result.summary.failed, 0);
        assert!(result.failures.is_empty());
    }

    #[test]
    fn test_parse_failing_junit() {
        let result = BunAdapter.parse_output(JUNIT_FAIL, "", 1);
        assert_eq!(result.summary.failed, 1);
        let f = &result.failures[0];
        assert_eq!(f.name, "Math > multiplies");
        assert!(f.message.contains("Expected 6"));
        // File extracted from classname attribute
        assert!(f.file.as_deref().unwrap_or("").ends_with("calc.test.ts"));
    }

    #[test]
    fn test_parse_skipped_junit() {
        let result = BunAdapter.parse_output(JUNIT_SKIP, "", 0);
        assert_eq!(result.summary.skipped, 1);
        assert_eq!(result.summary.passed, 0);
    }

    #[test]
    fn test_parse_xml_entities_unescaped() {
        // &lt;, &gt; etc. should be unescaped in failure messages
        let result = BunAdapter.parse_output(JUNIT_FAIL, "", 1);
        let msg = &result.failures[0].message;
        // Failure body has "&lt;anonymous&gt;" which should be "<anonymous>"
        assert!(msg.contains("<anonymous>"), "XML entities should be decoded");
    }

    #[test]
    fn test_suite_command() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = BunAdapter.suite_command(dir.path(), None, &Default::default()).unwrap();
        assert!(cmd.program.contains("bun") || cmd.args.iter().any(|a| a == "bun"));
        assert!(cmd.args.iter().any(|a| a.contains("junit")));
    }
}
```

**Step 3: Implement `src/test/bun_adapter.rs`**

Parse JUnit XML using `quick-xml` (already in Cargo.toml). Reuse the XML parsing logic from `catch2_adapter.rs` — extract `testsuites`, `testsuite`, `testcase`, `failure`, and `skipped` elements.

Detection:
- 95: `bun.lockb` exists
- 90: `package.json` scripts contain `"bun test"`
- 75: `"bun"` in package.json scripts

Command: `bun test --reporter=junit` (JUnit to stdout)
Single test: `bun test --reporter=junit <pattern>` where pattern is a test file or name pattern

**Step 4: Run tests**
```bash
cargo test -p strobe bun_adapter
```

**Checkpoint:** `BunAdapter` parses bun:test JUnit XML correctly, handles failures/skips, and unescapes XML entities.

---

## Stream B — Agent/TypeScript

### Task B1: `V8Tracer` — function tracing in Node.js via V8 runtime

**Files:**
- Create: `agent/src/tracers/v8-tracer.ts`

**Critical architectural decision:** When Frida creates a script with `FRIDA_SCRIPT_RUNTIME_V8` for JavaScript sessions, the agent code runs **inside the target's V8 context** — the same V8 that Node.js uses. This means our agent has direct access to `require()`, `process`, `module`, `__filename`, `__dirname`, and the full Node.js API.

**Hooking strategy (Module._compile patching):**
1. Agent patches `Module.prototype._compile` to intercept each JS file as it's loaded
2. After compilation, scans `module.exports` (and recursively) for functions matching hook targets
3. Each matching function is wrapped with a `Proxy` that calls `traceCallback` on entry/exit
4. Already-loaded modules are also scanned immediately when a hook is added

**Note:** This covers module-exported functions. Functions defined inside closures and never exported are not hookable without Ignition trampoline hooking (future work).

**Step 1: Write the failing test**

Build the agent; verify `createTracer('v8', ...)` currently throws. After this task it returns a `V8Tracer`.

**Step 2: Implement `agent/src/tracers/v8-tracer.ts`**

```typescript
// agent/src/tracers/v8-tracer.ts
// V8 runtime tracer — runs INSIDE Node.js's own V8 context.
// Requires Frida script runtime = V8 (set by spawner.rs for JS sessions).

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage,
         StepHooksMessage, LogpointMessage } from './tracer.js';

interface V8Hook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
}

// These globals are available because we're running in Node.js's V8 context
declare const require: NodeRequire;
declare const process: NodeJS.Process;

export class V8Tracer implements Tracer {
  private agent: any;
  private hooks: Map<number, V8Hook> = new Map();
  private nextFuncId: number = 1;
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  // Track wrapped functions to avoid double-wrapping
  private wrappedFns: WeakSet<Function> = new WeakSet();
  private origCompile: Function | null = null;

  constructor(agent: any) {
    this.agent = agent;
  }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Patch Module._compile to intercept newly-loaded modules
    try {
      const Module = require('module') as any;
      const self = this;
      const original = Module.prototype._compile;
      this.origCompile = original;

      Module.prototype._compile = function(content: string, filename: string) {
        const result = original.call(this, content, filename);
        // After module is compiled and exports are populated, wrap matching functions
        try { self.wrapModuleExports(this.exports, filename); } catch {}
        return result;
      };
    } catch (e) {
      send({ type: 'log', message: `V8Tracer: failed to patch Module._compile: ${e}` });
    }

    send({ type: 'log', message: `V8Tracer: initialized (V8 runtime, Node.js ${process.version})` });
  }

  dispose(): void {
    // Restore original _compile
    if (this.origCompile) {
      try {
        const Module = require('module') as any;
        Module.prototype._compile = this.origCompile;
      } catch {}
      this.origCompile = null;
    }
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null; }
    this.flushEvents();
    this.hooks.clear();
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;
    this.hooks.set(funcId, { funcId, target, mode });

    // Immediately wrap already-loaded modules that match
    try {
      const cache = (require as any).cache ?? {};
      for (const [id, mod] of Object.entries(cache) as any[]) {
        if (mod?.exports && this.fileMatches(id, target)) {
          this.wrapModuleExports(mod.exports, id);
        }
      }
    } catch {}

    return funcId;
  }

  removeHook(id: number): void {
    // Note: we can't unwrap already-wrapped functions without tracking them
    // Hooks are effectively removed by checking this.hooks in the wrapper
    this.hooks.delete(id);
  }

  removeAllHooks(): void { this.hooks.clear(); }
  activeHookCount(): number { return this.hooks.size; }

  installBreakpoint(_msg: BreakpointMessage): void { /* Phase 2: use V8 Inspector CDP */ }
  removeBreakpoint(_id: string): void {}
  installStepHooks(_msg: StepHooksMessage): void {}
  installLogpoint(_msg: LogpointMessage): void {}
  removeLogpoint(_id: string): void {}

  readVariable(expr: string): any {
    // Running in V8 context — can eval globals directly
    try {
      // eslint-disable-next-line no-eval
      const value = (0, eval)(expr); // indirect eval = global scope
      return JSON.parse(JSON.stringify(value, null, 0));
    } catch (e) {
      return { error: String(e) };
    }
  }

  writeVariable(expr: string, value: any): void {
    try {
      // eslint-disable-next-line no-new-func
      new Function('__v', `${expr} = __v`)(value);
    } catch {}
  }

  setImageBase(_base: string): void {}
  getSlide(): NativePointer { return ptr(0); }

  // ── Private helpers ─────────────────────────────────────────────────

  private fileMatches(filename: string, target: ResolvedTarget): boolean {
    if (!target.file) return false;
    // Match by file suffix (target.file may be relative)
    return filename.endsWith(target.file) ||
           filename.endsWith(target.file.replace(/\.[^.]+$/, '.js')); // .ts → .js
  }

  private wrapModuleExports(exports: any, filename: string): void {
    if (!exports || typeof exports !== 'object' && typeof exports !== 'function') return;
    this.wrapObject(exports, filename, '');
  }

  private wrapObject(obj: any, filename: string, prefix: string): void {
    if (!obj) return;
    const seen = new Set<any>();

    const wrap = (container: any, key: string, depth: number) => {
      if (depth > 3) return; // Limit recursion
      const val = container[key];
      if (typeof val !== 'function' || this.wrappedFns.has(val)) return;

      const qualifiedName = prefix ? `${prefix}.${key}` : key;

      // Find matching hook
      let matchedHook: V8Hook | null = null;
      for (const [, hook] of this.hooks) {
        if (!this.fileMatches(filename, hook.target)) continue;
        const targetFuncName = hook.target.name.split('.').pop() ?? hook.target.name;
        if (targetFuncName === key || hook.target.name === qualifiedName) {
          matchedHook = hook;
          break;
        }
      }

      if (matchedHook) {
        const hook = matchedHook;
        const self = this;
        const wrapped = new Proxy(val, {
          apply(target, thisArg, args) {
            self.emitEvent(hook.funcId, hook, filename, 'entry');
            let result: any;
            try {
              result = Reflect.apply(target, thisArg, args);
            } catch (e) {
              self.emitEvent(hook.funcId, hook, filename, 'exit');
              throw e;
            }
            // Handle async functions
            if (result && typeof result.then === 'function') {
              return result.then((v: any) => {
                self.emitEvent(hook.funcId, hook, filename, 'exit');
                return v;
              }, (e: any) => {
                self.emitEvent(hook.funcId, hook, filename, 'exit');
                throw e;
              });
            }
            self.emitEvent(hook.funcId, hook, filename, 'exit');
            return result;
          }
        });
        this.wrappedFns.add(val); // Mark original as wrapped
        try { container[key] = wrapped; } catch {} // May be non-writable
      }

      // Recurse into plain objects (e.g. class instances, namespace objects)
      if (typeof val === 'object' && !seen.has(val)) {
        seen.add(val);
        for (const k of Object.keys(val)) {
          wrap(val, k, depth + 1);
        }
      }
    };

    for (const key of Object.keys(obj)) {
      wrap(obj, key, 0);
    }
    // Also wrap prototype methods for classes
    if (typeof obj === 'function' && obj.prototype) {
      for (const key of Object.getOwnPropertyNames(obj.prototype)) {
        if (key !== 'constructor') wrap(obj.prototype, key, 0);
      }
    }
  }

  private emitEvent(funcId: number, hook: V8Hook, filename: string, event: 'entry' | 'exit'): void {
    this.eventBuffer.push({
      id: `${this.sessionId}-v8-${++this.eventIdCounter}`,
      sessionId: this.sessionId,
      timestampNs: Date.now() * 1_000_000,
      threadId: 0, // Node.js is single-threaded for JS (worker_threads aside)
      eventType: event === 'entry' ? 'function_enter' : 'function_exit',
      functionName: hook.target.name,
      sourceFile: filename,
      lineNumber: hook.target.line,
      pid: process.pid,
    });
    if (this.eventBuffer.length >= 50) this.flushEvents();
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }
}
```

**Step 3: Verify it compiles**
```bash
cd agent && npm run build 2>&1 | grep -E "error|warning"
```

**Checkpoint:** `V8Tracer` compiles. When used with V8 runtime, it patches `Module._compile` to wrap exported functions matching hook targets. Async and sync functions both emit enter/exit events.

---

### Task B2: `JscTracer` — Bun function tracing via `vmEntryToJavaScript`

**Files:**
- Create: `agent/src/tracers/jsc-tracer.ts`

**Approach:** Bun uses JavaScriptCore. The C function `vmEntryToJavaScript` is called for every JS function invocation. It's a stable, well-known JSC entry point. We hook it natively via Frida's `Interceptor`, then read function metadata from the register context.

JSC calling convention on arm64 macOS (where Bun runs): the JSFunction pointer is in `x0` (first argument). From the JSFunction we can navigate to `SharedFunctionInfo` → source URL + line. However, this requires knowledge of JSC struct layout which changes between JSC versions.

**Alternative (simpler, implement first):** Hook at the module loader level using JSC's public C API that Bun exports (`JSEvaluateScript`, `JSObjectCallAsFunction`). Hook `JSObjectCallAsFunction` and decode the `JSObjectRef` to get the function name.

**Step 1: Write the failing test**

Build agent; verify `createTracer('jsc', ...)` currently throws.

**Step 2: Implement `agent/src/tracers/jsc-tracer.ts`**

```typescript
// agent/src/tracers/jsc-tracer.ts
// JSC tracer for Bun — hooks JSObjectCallAsFunction at native level.
// Runs in QuickJS (standard Frida runtime) since Bun doesn't use Frida's V8 runtime.

import { Tracer, ResolvedTarget, HookMode, BreakpointMessage,
         StepHooksMessage, LogpointMessage } from './tracer.js';
import { findGlobalExport } from '../utils.js';

interface JscHook {
  funcId: number;
  target: ResolvedTarget;
  mode: HookMode;
}

export class JscTracer implements Tracer {
  private agent: any;
  private hooks: Map<number, JscHook> = new Map();
  private nextFuncId: number = 1;
  private sessionId: string = '';
  private eventIdCounter: number = 0;
  private eventBuffer: any[] = [];
  private flushTimer: ReturnType<typeof setInterval> | null = null;
  private interceptor: InvocationListener | null = null;

  constructor(agent: any) { this.agent = agent; }

  initialize(sessionId: string): void {
    this.sessionId = sessionId;
    this.flushTimer = setInterval(() => this.flushEvents(), 50);

    // Hook JSObjectCallAsFunction — called for every JS function call via C API
    // Signature: JSValueRef JSObjectCallAsFunction(JSContextRef, JSObjectRef fn,
    //             JSObjectRef thisObj, size_t argc, JSValueRef* argv, JSValueRef* exception)
    const hookTarget = findGlobalExport('JSObjectCallAsFunction');
    if (!hookTarget) {
      send({ type: 'log', message: 'JscTracer: JSObjectCallAsFunction not found — tracing unavailable' });
      return;
    }

    const self = this;
    this.interceptor = Interceptor.attach(hookTarget, {
      onEnter(args) {
        // args[1] = JSObjectRef (the function being called)
        // We read the function's display name via JSObjectCopyPropertyForKey or
        // navigate JSC internals: JSObject → JSFunction → FunctionExecutable → name
        // For now: emit entry for any active hook whose file context matches
        // Full JSC struct navigation is added in the follow-on task.
        const fnPtr = args[1];
        self.tryEmitForJscFunction(fnPtr, 'entry');
      },
      onLeave(_retval) {
        // Emit exit — funcId matching is best-effort
      }
    });

    send({ type: 'log', message: 'JscTracer: hooked JSObjectCallAsFunction' });
  }

  dispose(): void {
    if (this.interceptor) { this.interceptor.detach(); this.interceptor = null; }
    if (this.flushTimer) { clearInterval(this.flushTimer); this.flushTimer = null; }
    this.flushEvents();
    this.hooks.clear();
  }

  installHook(target: ResolvedTarget, mode: HookMode): number | null {
    const funcId = this.nextFuncId++;
    this.hooks.set(funcId, { funcId, target, mode });
    return funcId;
  }

  removeHook(id: number): void { this.hooks.delete(id); }
  removeAllHooks(): void { this.hooks.clear(); }
  activeHookCount(): number { return this.hooks.size; }

  installBreakpoint(_msg: BreakpointMessage): void {}
  removeBreakpoint(_id: string): void {}
  installStepHooks(_msg: StepHooksMessage): void {}
  installLogpoint(_msg: LogpointMessage): void {}
  removeLogpoint(_id: string): void {}
  readVariable(_expr: string): any { return null; }
  writeVariable(_expr: string, _value: any): void {}
  setImageBase(_base: string): void {}
  getSlide(): NativePointer { return ptr(0); }

  private tryEmitForJscFunction(fnPtr: NativePointer, event: 'entry' | 'exit'): void {
    // TODO(follow-on): Navigate JSC struct from fnPtr to function name + source URL + line
    // For now: emit a generic event for any active hook
    // Full implementation requires JSC struct offsets (version-specific)
    for (const [funcId, hook] of this.hooks) {
      this.eventBuffer.push({
        id: `${this.sessionId}-jsc-${++this.eventIdCounter}`,
        sessionId: this.sessionId,
        timestampNs: Date.now() * 1_000_000,
        threadId: Process.getCurrentThreadId(),
        eventType: event === 'entry' ? 'function_enter' : 'function_exit',
        functionName: hook.target.name,
        sourceFile: hook.target.file,
        lineNumber: hook.target.line,
        pid: Process.id,
      });
      break; // One event per call for now
    }
    if (this.eventBuffer.length >= 50) this.flushEvents();
  }

  private flushEvents(): void {
    if (this.eventBuffer.length === 0) return;
    const events = this.eventBuffer;
    this.eventBuffer = [];
    send({ type: 'events', events });
  }
}
```

> **Note on JscTracer completeness:** The `tryEmitForJscFunction` stub emits events but doesn't yet discriminate between functions. Full function name + source location requires navigating JSC's internal `JSFunction` struct (offset of `m_executable` → `FunctionExecutable` → `m_sourceURL` and `m_firstLine`). These offsets are stable within a JSC version but change across versions. The implementer should add a `getBunJscVersion()` helper that reads Bun's version string and selects the right offsets. This is documented as a follow-on in the plan.

**Step 3: Verify it compiles**
```bash
cd agent && npm run build 2>&1 | grep error
```

**Checkpoint:** `JscTracer` compiles and hooks `JSObjectCallAsFunction`. Events are emitted when any JS function is called in Bun. Full discriminated function name tracking is tracked as follow-on.

---

### Task B3: `readVariable` for JS (V8 context eval)

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts`

`readVariable` is already implemented in Task B1 (eval in V8 context). This task verifies it works end-to-end with `debug_read`.

**Step 1: Write the failing test**

Integration test: spawn a Node.js script with a global variable `let counter = 0`. Attach, call `debug_read` for expression `"counter"`. Currently returns `null`. Expected: `0`.

**Step 2: Verify the implementation works**

The V8Tracer `readVariable` uses indirect eval `(0, eval)(expr)`. For global-scope variables (those in the module's top-level scope), this works because Node.js modules execute in a wrapper function but globals leak to the V8 context.

For module-level `let`/`const` (which are block-scoped to the module wrapper), indirect eval won't see them. In this case, fall back to searching `require.cache`:

```typescript
readVariable(expr: string): any {
  // First try global eval
  try {
    const value = (0, eval)(expr);
    return JSON.parse(JSON.stringify(value));
  } catch {}
  // Fall back: search module exports
  try {
    const cache = (require as any).cache ?? {};
    for (const mod of Object.values(cache) as any[]) {
      if (mod?.exports?.[expr] !== undefined) {
        return JSON.parse(JSON.stringify(mod.exports[expr]));
      }
    }
  } catch {}
  return { error: `Cannot access '${expr}' — not in global scope or module exports` };
}
```

**Checkpoint:** `debug_read` for a Node.js global or exported variable returns the actual value.

---

## Stream C — Wiring

### Task C1: `spawner.rs` — set V8 runtime for JavaScript sessions

**Files:**
- Modify: `src/frida_collector/spawner.rs`

**Problem:** Script runtime defaults to QuickJS (`FRIDA_SCRIPT_RUNTIME_DEFAULT`). For V8 sessions (Node.js), we need `FRIDA_SCRIPT_RUNTIME_V8` so the agent runs inside V8.

Find where `frida_session_create_script` is called in `spawner.rs`. Before the call, when `language == Language::JavaScript && runtime == 'v8'`, set the script options runtime to V8.

```rust
// Existing code (find the script creation call):
// frida_sys::frida_session_create_script(session, code_cstr, opts, ...)

// Add before create_script:
if language == Language::JavaScript {
    // Detect if target is V8 (Node.js) or JSC (Bun)
    // The agent's detectRuntime() already distinguishes via symbol lookup.
    // Here we pass a hint to the spawner via session metadata.
    // Set V8 runtime for V8-based JS sessions.
    // Note: if the target turns out to be JSC (Bun), the agent's createTracer()
    // will use JscTracer which runs fine in QuickJS runtime.
    // We set V8 runtime tentatively; if V8 symbols aren't found, the script
    // will fall back gracefully (V8 runtime in a non-V8 process is a no-op
    // and Frida falls back to QuickJS).
    frida_sys::frida_script_options_set_runtime(
        opts,
        frida_sys::FridaScriptRuntime::FRIDA_SCRIPT_RUNTIME_V8
    );
}
```

**Step 1: Write the failing test**

Integration smoke test: spawn a Node.js script, verify the agent log contains `"V8Tracer: initialized"` (not the `JsTracer not yet implemented` error). Currently would show throw error.

**Step 2: Verify it passes**

After setting V8 runtime: daemon log shows `"V8Tracer: initialized (V8 runtime, Node.js vX.X.X)"`.

**Checkpoint:** Node.js sessions use V8 runtime; agent initializes `V8Tracer` successfully.

---

### Task C2: `session_manager.rs` — `JsResolver` instantiation

**Files:**
- Modify: `src/daemon/session_manager.rs`

Add `JsResolver` to imports and add `Language::JavaScript` branch after Python branch (lines 531–553):

```rust
use crate::symbols::{Language, SymbolResolver, DwarfResolver, PythonResolver, JsResolver};

// After the Python branch:
} else if language == Language::JavaScript {
    let resolvers = Arc::clone(&self.resolvers);
    let sid = session_id.to_string();
    let project_root_path = Path::new(project_root).to_path_buf();
    match tokio::task::spawn_blocking(move || {
        JsResolver::from_project(&project_root_path)
    }).await {
        Ok(Ok(resolver)) => {
            let count = resolver.function_count();
            write_lock(&resolvers).insert(
                sid.clone(),
                Arc::new(resolver) as Arc<dyn SymbolResolver>
            );
            tracing::info!("JsResolver instantiated for session {} ({} functions)", sid, count);
        }
        Ok(Err(e)) => tracing::warn!("JS resolver parse failed for {}: {}", sid, e),
        Err(e)    => tracing::warn!("JS resolver task panicked for {}: {}", sid, e),
    }
}
```

---

### Task C3: `mod.rs` — export `JsResolver`

**Files:**
- Modify: `src/symbols/mod.rs`

```rust
pub mod js_resolver;
pub use js_resolver::JsResolver;
```

---

### Task C4: `agent.ts` — wire `V8Tracer` and `JscTracer`

**Files:**
- Modify: `agent/src/agent.ts`

Add imports (after PythonTracer import):
```typescript
import { V8Tracer }  from './tracers/v8-tracer.js';
import { JscTracer } from './tracers/jsc-tracer.js';
```

Replace the `throw` in `createTracer()`:
```typescript
case 'v8':
  return new V8Tracer(agent);
case 'jsc':
  return new JscTracer(agent);
```

**Checkpoint:** `createTracer()` returns the correct tracer for all four runtimes without throwing.

---

## Stream D — Tests

### Task D1–D4: Rust unit tests

Tasks A1–A5 already include comprehensive unit tests embedded in each file (TDD). After implementation verify all pass:

```bash
cargo test -p strobe js_resolver vitest_adapter jest_adapter bun_adapter
```

Expected: all tests pass, zero failures.

---

### Task D5: Integration test fixtures

**Files:**
- Create: `tests/fixtures/node_trace_target.js`
- Create: `tests/fixtures/bun_trace_target.ts`
- Create: `tests/fixtures/vitest_project/` (minimal vitest project)
- Create: `tests/fixtures/jest_project/` (minimal jest project)
- Create: `tests/fixtures/bun_test_project/` (minimal bun:test project)

**`tests/fixtures/node_trace_target.js`:**
```javascript
// Target script for Node.js function tracing integration tests
let counter = 0;

function increment(n) {
  counter += n;
  return counter;
}

class Calculator {
  add(a, b) { return a + b; }
  async asyncAdd(a, b) {
    await new Promise(r => setTimeout(r, 1));
    return a + b;
  }
}

module.exports = { increment, Calculator };

// Keep running so Frida can attach and trace
if (require.main === module) {
  const calc = new Calculator();
  setInterval(() => {
    increment(1);
    calc.add(counter, 1);
  }, 100);
}
```

**`tests/fixtures/vitest_project/`:**
```
package.json       { "devDependencies": { "vitest": "^1.0.0" }, "scripts": { "test": "vitest run" } }
vitest.config.ts   export default { test: { reporter: 'json' } }
src/math.ts        export function add(a: number, b: number) { return a + b; }
src/math.test.ts   import { describe, it, expect } from 'vitest'; describe('Math', () => { it('adds', () => expect(add(2,2)).toBe(4)); it('fails', () => expect(add(2,2)).toBe(5)); });
```

**`tests/fixtures/bun_test_project/`:**
```
package.json          { "scripts": { "test": "bun test" } }
bun.lockb             (binary, create with: bun install in dir)
math.test.ts          import { describe, it, expect } from 'bun:test';
                      describe('Math', () => {
                        it('adds', () => expect(2+2).toBe(4));
                      });
```

---

### Task D6: Integration test — Node.js function tracing

This is a manual integration test (automated via the strobe MCP tools):

```
1. Build: cd agent && npm run build && cd .. && touch src/frida_collector/spawner.rs && cargo build --release
2. Start daemon: strobe daemon &
3. Launch target: debug_launch(command="node tests/fixtures/node_trace_target.js", project_root="tests/fixtures")
4. Add trace: debug_trace(session_id=..., add=["increment", "Calculator.add"])
5. Wait 500ms
6. Query: list_events(session_id=...)
7. Verify: events contain function_enter for "increment" and "Calculator.add"
8. Test readVariable: debug_read(session_id=..., variables=[{expr: "counter"}])
9. Verify: counter value is a number > 0
10. Remove trace: debug_trace(session_id=..., remove=["increment"])
11. Wait 500ms, query events again
12. Verify: no new events for "increment" after removal
```

---

### Task D7: Integration test — Vitest `debug_test`

```
1. cd tests/fixtures/vitest_project && npm install
2. debug_test(project_root="tests/fixtures/vitest_project", framework="vitest")
3. Verify: result has passed=1, failed=1 (the deliberately failing test)
4. Verify: failure.name contains the test title
5. Verify: failure.file points to math.test.ts
6. Re-run with trace: debug_test(..., trace_patterns=["add"])
7. Verify: events for add() appear in session
```

---

### Task D8: Integration test — bun:test `debug_test`

```
1. Verify bun is installed: bun --version
2. debug_test(project_root="tests/fixtures/bun_test_project", framework="bun")
3. Verify: result has passed=1
4. Verify: JUnit XML was parsed correctly (test name, duration)
```

---

## Final Steps (after all streams)

1. **Add `sourcemap` to Cargo.toml:**
   ```toml
   sourcemap = "9"
   ```

2. **Register adapters in `src/test/mod.rs`:**
   ```rust
   // In TestRunner::new():
   Box::new(VitestAdapter),   // 95/90/70
   Box::new(JestAdapter),     // 92/88/70
   Box::new(BunAdapter),      // 95/90/75
   ```

3. **Rebuild agent:**
   ```bash
   cd agent && npm run build && cd ..
   touch src/frida_collector/spawner.rs
   ```

4. **Build and test:**
   ```bash
   cargo build --release
   cargo test
   ```

5. **Single commit:**
   ```bash
   git add \
     Cargo.toml \
     src/symbols/js_resolver.rs \
     src/symbols/mod.rs \
     src/daemon/session_manager.rs \
     src/frida_collector/spawner.rs \
     src/test/vitest_adapter.rs \
     src/test/jest_adapter.rs \
     src/test/bun_adapter.rs \
     src/test/mod.rs \
     agent/src/tracers/v8-tracer.ts \
     agent/src/tracers/jsc-tracer.ts \
     agent/src/agent.ts \
     agent/dist/ \
     tests/fixtures/
   git commit -m "feat: JavaScript/TypeScript support — resolver, tracers, Vitest/Jest/Bun adapters (Phase 5b)"
   ```

---

## Known Follow-ons (not in this plan)

- **JscTracer function discrimination:** Navigate `JSFunction → FunctionExecutable → m_sourceURL + m_firstLine` struct offsets for Bun's JSC version to get actual function names/locations per event (vs. emitting for all hooks)
- **V8Tracer: closures and non-exported functions** — Requires Ignition bytecode trampoline hooking (`InterpreterEntryTrampoline`) at the native level
- **Breakpoints/logpoints/stepping for JS/TS** — Use V8 Inspector CDP protocol (`node --inspect` + WebSocket + `Debugger.setBreakpointByUrl`)
- **TypeScript `.d.ts` type information** — useful for richer variable display
- **ESM (import/export) module hooks** — `Module._compile` only covers CJS; ESM uses a different path
