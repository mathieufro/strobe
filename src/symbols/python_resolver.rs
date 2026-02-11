use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use rustpython_parser::{parse, ast, Mode};

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

/// Build a lookup table of byte-offset → 1-indexed line number.
/// Pre-scans once so all offset lookups are O(log n) via binary search.
fn build_line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0u32]; // line 1 starts at offset 0
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push((i + 1) as u32);
        }
    }
    starts
}

fn offset_to_line(line_starts: &[u32], offset: u32) -> u32 {
    match line_starts.binary_search(&offset) {
        Ok(idx) => (idx + 1) as u32,
        Err(idx) => idx as u32, // idx is the next line, so current line = idx
    }
}

/// Extract function/class method definitions from a Python source string.
pub fn extract_functions_from_source(
    source: &str,
    file_path: &Path,
) -> crate::Result<HashMap<String, (PathBuf, u32)>> {
    let ast = parse(source, Mode::Module, "<input>")
        .map_err(|e| crate::Error::Internal(format!("Python parse error in {:?}: {}", file_path, e)))?;

    let line_starts = build_line_starts(source);
    let mut functions = HashMap::new();
    extract_from_module(&ast, file_path, &[], &line_starts, &mut functions);
    Ok(functions)
}

/// Recursively extract function definitions from AST nodes.
/// `prefix` tracks the qualified name (e.g., ["ClassName", "method"]).
fn extract_from_module(
    module: &ast::Mod,
    file_path: &Path,
    prefix: &[String],
    line_starts: &[u32],
    functions: &mut HashMap<String, (PathBuf, u32)>,
) {
    match module {
        ast::Mod::Module(m) => {
            for stmt in &m.body {
                extract_from_stmt(stmt, file_path, prefix, line_starts, functions);
            }
        }
        _ => {}
    }
}

fn extract_from_stmt(
    stmt: &ast::Stmt,
    file_path: &Path,
    prefix: &[String],
    line_starts: &[u32],
    functions: &mut HashMap<String, (PathBuf, u32)>,
) {
    match stmt {
        ast::Stmt::FunctionDef(f) => {
            let qualified_name = if prefix.is_empty() {
                f.name.to_string()
            } else {
                format!("{}.{}", prefix.join("."), f.name)
            };
            let line = offset_to_line(line_starts, f.range.start().to_u32());
            functions.insert(qualified_name.clone(), (file_path.to_path_buf(), line));

            let mut new_prefix = prefix.to_vec();
            new_prefix.push(qualified_name);
            for nested_stmt in &f.body {
                extract_from_stmt(nested_stmt, file_path, &new_prefix, line_starts, functions);
            }
        }
        ast::Stmt::AsyncFunctionDef(f) => {
            let qualified_name = if prefix.is_empty() {
                f.name.to_string()
            } else {
                format!("{}.{}", prefix.join("."), f.name)
            };
            let line = offset_to_line(line_starts, f.range.start().to_u32());
            functions.insert(qualified_name.clone(), (file_path.to_path_buf(), line));

            let mut new_prefix = prefix.to_vec();
            new_prefix.push(qualified_name);
            for nested_stmt in &f.body {
                extract_from_stmt(nested_stmt, file_path, &new_prefix, line_starts, functions);
            }
        }
        ast::Stmt::ClassDef(c) => {
            let mut class_prefix = prefix.to_vec();
            class_prefix.push(c.name.to_string());
            for method_stmt in &c.body {
                extract_from_stmt(method_stmt, file_path, &class_prefix, line_starts, functions);
            }
        }
        _ => {}
    }
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

    pub fn function_count(&self) -> usize {
        self.functions.len()
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
        // Python variables are runtime expressions
        Ok(VariableResolution::RuntimeExpression { expr: name.to_string() })
    }

    fn image_base(&self) -> u64 {
        0 // Interpreted language — no ASLR
    }

    fn language(&self) -> Language {
        Language::Python
    }

    fn supports_runtime_resolution(&self) -> bool {
        true // Python can resolve dynamic symbols at runtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            ("modules.audio.process_buffer".to_string(), (PathBuf::from("audio.py"), 10)),
            ("modules.audio.generate_sine".to_string(), (PathBuf::from("audio.py"), 20)),
            ("modules.midi.note_on".to_string(), (PathBuf::from("midi.py"), 5)),
        ]);
        let targets = resolver.resolve_pattern("modules.audio.*", Path::new(".")).unwrap();
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn test_file_pattern() {
        let resolver = PythonResolver::from_functions(vec![
            ("handler".to_string(), (PathBuf::from("app/handler.py"), 10)),
            ("main".to_string(), (PathBuf::from("app/main.py"), 1)),
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
