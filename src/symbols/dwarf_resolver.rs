use std::path::Path;
use super::resolver::*;
use crate::dwarf::DwarfHandle;

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
        // Access the DwarfHandle synchronously via borrow() — assumes parse is complete.
        // In practice, the session manager won't create a DwarfResolver until parse finishes.
        let parser_result = self.dwarf.try_borrow_parser()
            .ok_or_else(|| crate::Error::Internal("DWARF parse not yet complete".to_string()))?
            .map_err(|e| crate::Error::Internal(format!("DWARF parse failed: {}", e)))?;

        let functions = if pattern.starts_with("@file:") {
            let file_pattern = &pattern[6..];
            parser_result.find_by_source_file(file_pattern)
        } else {
            parser_result.find_by_pattern(pattern)
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
        let parser_result = self.dwarf.try_borrow_parser()
            .ok_or_else(|| crate::Error::Internal("DWARF parse not yet complete".to_string()))?
            .map_err(|e| crate::Error::Internal(format!("DWARF parse failed: {}", e)))?;

        match parser_result.resolve_line(file, line) {
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
        // Existing DWARF variable resolution logic
        // This delegates to the existing WatchRecipe / ReadRecipe building
        // For now, return an error — the existing flow bypasses this trait
        let _ = name;
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
