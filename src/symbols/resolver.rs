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
    /// For tracing hooks: returns the function definition line (matches co_firstlineno).
    fn resolve_pattern(&self, pattern: &str, project_root: &Path) -> crate::Result<Vec<ResolvedTarget>>;

    /// Resolve a function pattern for breakpoints.
    /// For Python: returns the first executable line in the function body (not the `def` line).
    /// Default: falls back to resolve_pattern (correct for native/DWARF).
    fn resolve_breakpoint_pattern(&self, pattern: &str, project_root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
        self.resolve_pattern(pattern, project_root)
    }

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
