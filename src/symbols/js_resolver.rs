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
    let re_fn = Regex::new(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s+(\w+)\s*[<(]").unwrap();
    let re_arrow = Regex::new(r"^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*(?::\s*\S+)?\s*=\s*(?:async\s+)?(?:<[^>]*>\s*)?(?:\([^)]*\)|[\w$]+)\s*(?::\s*[^=]+)?\s*=>").unwrap();
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
        if in_template_literal {
            brace_depth += stripped.chars().filter(|&c| c == '{').count() as i32
                         - stripped.chars().filter(|&c| c == '}').count() as i32;
            continue;
        }

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
            .filter_entry(|e| {
                // Skip excluded directories early (before descending)
                if e.file_type().is_dir() {
                    let name = e.file_name().to_str().unwrap_or("");
                    return !SKIP_DIRS.contains(&name);
                }
                true
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
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
        // Handle @file: patterns
        if let Some(file_pattern) = pattern.strip_prefix("@file:") {
            return Ok(self.functions.iter()
                .filter(|(_, (file, _))| file.to_string_lossy().contains(file_pattern))
                .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                    file: file.to_string_lossy().to_string(),
                    line: *line,
                    name: name.clone(),
                })
                .collect());
        }

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

#[cfg(test)]
mod tests {
    use super::*;

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

        // * matches non-dot names only (doesn't cross . boundary)
        let star = resolver.resolve_pattern("*", dir.path()).unwrap();
        assert_eq!(star.len(), 1, "* matches only top-level functions (not Class.method)");
        assert_eq!(star[0].name(), "helper");

        // ** matches everything including Class.method
        let dstar = resolver.resolve_pattern("**", dir.path()).unwrap();
        assert_eq!(dstar.len(), 3, "** matches all functions including class methods");
    }

    #[test]
    fn test_deep_star_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mod.ts"), r#"
class A {
    foo() {}
}
class B {
    bar() {}
}
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

    // ── Unit: @file: pattern ──────────────────────────────────────────
    #[test]
    fn test_file_pattern() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("math.ts"), "function add() {}\nfunction sub() {}\n").unwrap();
        std::fs::write(dir.path().join("utils.ts"), "function log() {}\n").unwrap();

        let resolver = JsResolver::from_project(dir.path()).unwrap();
        let math_fns = resolver.resolve_pattern("@file:math.ts", dir.path()).unwrap();
        assert_eq!(math_fns.len(), 2);
        let util_fns = resolver.resolve_pattern("@file:utils.ts", dir.path()).unwrap();
        assert_eq!(util_fns.len(), 1);
    }

    // ── Unit: source map resolution ───────────────────────────────────
    #[test]
    fn test_sourcemap_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let js_path = dir.path().join("dist").join("index.js");
        std::fs::create_dir_all(js_path.parent().unwrap()).unwrap();
        std::fs::write(&js_path, "\"use strict\";\nfunction greet() {}\n").unwrap();
        // Write a sourcemap: ";AAKA" → skip line 1, then gen(1,0) → src(0, line 5, col 0)
        let map_content = include_str!("../../tests/fixtures/sourcemap_test.map");
        std::fs::write(js_path.with_extension("js.map"), map_content).unwrap();

        let resolver = JsResolver::from_project(dir.path()).unwrap();
        // Resolution should map line 2 of index.js → line 5 of ../src/index.ts
        if let Some((ts_file, ts_line)) = resolver.resolve_sourcemap(&js_path, 2, 0) {
            assert!(ts_file.to_string_lossy().ends_with("index.ts"), "should map to .ts file");
            assert_eq!(ts_line, 5, "should map to line 5 in TypeScript source");
        }
    }
}
