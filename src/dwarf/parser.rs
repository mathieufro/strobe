use gimli::{self, RunTimeEndian, EndianSlice, SectionId};
use object::{Object, ObjectSection, ObjectSegment};
use memmap2::Mmap;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use crate::{Error, Result};
use crate::symbols::demangle_symbol;
use super::FunctionInfo;

pub struct DwarfParser {
    pub functions: Vec<FunctionInfo>,
    functions_by_name: HashMap<String, Vec<usize>>,
    /// The image base address from the Mach-O/ELF binary (e.g., __TEXT vmaddr).
    /// Used to compute offsets for ASLR adjustment at runtime.
    pub image_base: u64,
}

impl DwarfParser {
    pub fn parse(binary_path: &Path) -> Result<Self> {
        // Extract image base from the original binary (needed for ASLR adjustment)
        let image_base = Self::extract_image_base(binary_path).unwrap_or(0);

        // First try the binary itself
        if let Ok(mut parser) = Self::parse_file(binary_path) {
            parser.image_base = image_base;
            return Ok(parser);
        }

        // On macOS, check for .dSYM bundle
        let dsym_path = binary_path.with_extension("dSYM");
        if dsym_path.exists() {
            // The actual DWARF is in Contents/Resources/DWARF/<binary_name>
            if let Some(binary_name) = binary_path.file_name() {
                let dwarf_file = dsym_path
                    .join("Contents")
                    .join("Resources")
                    .join("DWARF")
                    .join(binary_name);
                if dwarf_file.exists() {
                    let mut parser = Self::parse_file(&dwarf_file)?;
                    parser.image_base = image_base;
                    return Ok(parser);
                }
            }
        }

        Err(Error::NoDebugSymbols)
    }

    /// Extract the image base address from a binary's __TEXT segment (Mach-O) or
    /// first LOAD segment (ELF). This is the expected load address before ASLR.
    fn extract_image_base(binary_path: &Path) -> Result<u64> {
        let file = File::open(binary_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = object::File::parse(&*mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        // Find the first executable segment (typically __TEXT on Mach-O, LOAD on ELF)
        for segment in object.segments() {
            if let Some(name) = segment.name().ok().flatten() {
                if name == "__TEXT" {
                    return Ok(segment.address());
                }
            }
        }

        // Fallback: use the first segment with a non-zero address
        for segment in object.segments() {
            let addr = segment.address();
            if addr > 0 {
                return Ok(addr);
            }
        }

        Ok(0)
    }

    fn parse_file(path: &Path) -> Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = object::File::parse(&*mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        // Check if debug info exists
        if object.section_by_name(".debug_info").is_none()
            && object.section_by_name("__debug_info").is_none() {
            return Err(Error::NoDebugSymbols);
        }

        let endian = if object.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        let load_section = |id: SectionId| -> std::result::Result<Cow<[u8]>, gimli::Error> {
            let name = id.name();
            // Try both ELF and Mach-O section names
            let data = object
                .section_by_name(name)
                .or_else(|| {
                    // Mach-O uses __debug_* instead of .debug_*
                    let macho_name = name.replace(".debug_", "__debug_");
                    object.section_by_name(&macho_name)
                })
                .and_then(|section| section.data().ok())
                .unwrap_or(&[]);
            Ok(Cow::Borrowed(data))
        };

        let dwarf_cow = gimli::Dwarf::load(&load_section)
            .map_err(|e| Error::Frida(format!("Failed to load DWARF: {}", e)))?;

        let dwarf = dwarf_cow.borrow(|section| {
            EndianSlice::new(section.as_ref(), endian)
        });

        let mut functions = Vec::new();

        // Iterate through compilation units
        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let unit = dwarf.unit(header)
                .map_err(|e| Error::Frida(format!("Failed to parse unit: {}", e)))?;

            let mut entries = unit.entries();
            while let Ok(Some((_, entry))) = entries.next_dfs() {
                if entry.tag() == gimli::DW_TAG_subprogram {
                    if let Some(func) = Self::parse_function(&dwarf, &unit, entry)? {
                        functions.push(func);
                    }
                }
            }
        }

        // Build index
        let mut functions_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, func) in functions.iter().enumerate() {
            functions_by_name
                .entry(func.name.clone())
                .or_default()
                .push(idx);
        }

        Ok(Self {
            functions,
            functions_by_name,
            image_base: 0, // Set by parse() from the actual binary
        })
    }

    fn parse_function<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Result<Option<FunctionInfo>> {
        // Get function name: prefer DW_AT_linkage_name (fully qualified mangled name) over
        // DW_AT_name (short name). Handles DWARF v4 and v5 string forms.
        let linkage_name = entry.attr_value(gimli::DW_AT_linkage_name)
            .ok()
            .flatten()
            .and_then(|v| {
                let s = dwarf.attr_string(unit, v).ok()?;
                let cow = s.to_string_lossy().ok()?;
                Some(cow.to_string())
            });

        let short_name = match entry.attr_value(gimli::DW_AT_name)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(attr_val) => {
                match dwarf.attr_string(unit, attr_val) {
                    Ok(s) => Some(
                        s.to_string_lossy()
                            .map_err(|e| Error::Frida(format!("UTF-8 error: {}", e)))?
                            .to_string()
                    ),
                    Err(_) => None,
                }
            }
            _ => None,
        };

        // Use linkage name for demangling (gives qualified names), fall back to short name
        let name = match linkage_name.or(short_name) {
            Some(n) => n,
            None => return Ok(None),
        };

        // Get low_pc (handles DWARF v4 Addr and DWARF v5 DebugAddrIndex)
        let low_pc = match entry.attr_value(gimli::DW_AT_low_pc)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(attr_val) => {
                match dwarf.attr_address(unit, attr_val)
                    .map_err(|e| Error::Frida(format!("DWARF address error: {}", e)))?
                {
                    Some(addr) => addr,
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        // Get high_pc (can be absolute address, indexed address, or offset from low_pc)
        let high_pc = match entry.attr_value(gimli::DW_AT_high_pc)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::Udata(offset)) => low_pc + offset,
            Some(attr_val) => {
                match dwarf.attr_address(unit, attr_val) {
                    Ok(Some(addr)) => addr,
                    _ => low_pc + 1,
                }
            }
            _ => low_pc + 1, // Minimal range if not specified
        };

        // Get source file
        let source_file = match entry.attr_value(gimli::DW_AT_decl_file)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::FileIndex(index)) => {
                if let Some(line_program) = &unit.line_program {
                    let header = line_program.header();
                    if let Some(file) = header.file(index) {
                        let mut path = String::new();
                        if let Some(dir) = file.directory(header) {
                            if let Ok(s) = dwarf.attr_string(unit, dir) {
                                path.push_str(&s.to_string_lossy().unwrap_or_default());
                                path.push('/');
                            }
                        }
                        if let Ok(s) = dwarf.attr_string(unit, file.path_name()) {
                            path.push_str(&s.to_string_lossy().unwrap_or_default());
                        }
                        if !path.is_empty() {
                            Some(path)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        // Get line number
        let line_number = match entry.attr_value(gimli::DW_AT_decl_line)
            .map_err(|e| Error::Frida(format!("DWARF error: {}", e)))?
        {
            Some(gimli::AttributeValue::Udata(n)) => Some(n as u32),
            _ => None,
        };

        // Demangle the name
        let demangled = demangle_symbol(&name);
        let name_raw = if name != demangled { Some(name) } else { None };

        Ok(Some(FunctionInfo {
            name: demangled,
            name_raw,
            low_pc,
            high_pc,
            source_file,
            line_number,
        }))
    }

    pub fn find_by_name(&self, name: &str) -> Vec<&FunctionInfo> {
        self.functions_by_name
            .get(name)
            .map(|indices| indices.iter().map(|&i| &self.functions[i]).collect())
            .unwrap_or_default()
    }

    pub fn find_by_pattern(&self, pattern: &str) -> Vec<&FunctionInfo> {
        let matcher = PatternMatcher::new(pattern);
        self.functions
            .iter()
            .filter(|f| matcher.matches(&f.name))
            .collect()
    }

    pub fn user_code_functions(&self, project_root: &str) -> Vec<&FunctionInfo> {
        self.functions
            .iter()
            .filter(|f| f.is_user_code(project_root))
            .collect()
    }

    /// Find all functions whose source file path contains the given substring.
    /// Used by the `@file:` pattern, e.g. `@file:lv_obj_style.c`.
    pub fn find_by_source_file(&self, file_pattern: &str) -> Vec<&FunctionInfo> {
        self.functions
            .iter()
            .filter(|f| {
                f.source_file
                    .as_ref()
                    .is_some_and(|sf| sf.contains(file_pattern))
            })
            .collect()
    }
}

/// Glob-style pattern matcher for function names
pub struct PatternMatcher {
    pattern: String,
}

impl PatternMatcher {
    pub fn new(pattern: &str) -> Self {
        Self {
            pattern: pattern.to_string(),
        }
    }

    pub fn matches(&self, name: &str) -> bool {
        Self::glob_match(&self.pattern, name)
    }

    fn glob_match(pattern: &str, text: &str) -> bool {
        // Handle **:: (matches zero or more segments including separators)
        if pattern.starts_with("**::") {
            let rest = &pattern[4..]; // skip "**::"

            // Try matching zero segments (skip the :: too)
            if Self::glob_match(rest, text) {
                return true;
            }

            // Try matching at every position in text
            for i in 0..=text.len() {
                if Self::glob_match(&pattern[2..], &text[i..]) { // keep the "::" in pattern
                    return true;
                }
            }
            return false;
        }

        // Handle ** (matches anything including ::)
        if pattern.starts_with("**") {
            let rest = &pattern[2..];
            if rest.is_empty() {
                return true;
            }
            // Try matching rest of pattern at every position in text
            for i in 0..=text.len() {
                if Self::glob_match(rest, &text[i..]) {
                    return true;
                }
            }
            return false;
        }

        // Handle * (matches anything except ::)
        if pattern.starts_with('*') {
            let rest = &pattern[1..];
            if rest.is_empty() {
                // * at end matches if no :: in remaining text
                return !text.contains("::");
            }
            // Find positions in text that don't cross :: boundary
            for i in 0..=text.len() {
                // Check if we crossed a ::
                let consumed = &text[..i];
                if consumed.contains("::") {
                    break;
                }
                if Self::glob_match(rest, &text[i..]) {
                    return true;
                }
            }
            return false;
        }

        // No wildcard at start - must match character by character
        if pattern.is_empty() {
            return text.is_empty();
        }
        if text.is_empty() {
            return false;
        }

        let p_char = pattern.chars().next().unwrap();
        let t_char = text.chars().next().unwrap();

        if p_char == t_char {
            Self::glob_match(&pattern[p_char.len_utf8()..], &text[t_char.len_utf8()..])
        } else {
            false
        }
    }
}

#[cfg(test)]
mod pattern_tests {
    use super::*;

    #[test]
    fn test_exact_match() {
        let m = PatternMatcher::new("foo::bar");
        assert!(m.matches("foo::bar"));
        assert!(!m.matches("foo::baz"));
    }

    #[test]
    fn test_single_star() {
        let m = PatternMatcher::new("foo::*");
        assert!(m.matches("foo::bar"));
        assert!(m.matches("foo::baz"));
        assert!(!m.matches("foo::bar::qux")); // * doesn't match ::
    }

    #[test]
    fn test_double_star() {
        let m = PatternMatcher::new("foo::**");
        assert!(m.matches("foo::bar"));
        assert!(m.matches("foo::bar::baz"));
        assert!(m.matches("foo::bar::baz::qux"));
    }

    #[test]
    fn test_star_middle() {
        let m = PatternMatcher::new("*::process");
        assert!(m.matches("main::process"));
        assert!(m.matches("foo::process"));
        assert!(!m.matches("main::sub::process")); // * doesn't cross ::
    }

    #[test]
    fn test_double_star_middle() {
        let m = PatternMatcher::new("auth::**::validate");
        assert!(m.matches("auth::validate"));
        assert!(m.matches("auth::user::validate"));
        assert!(m.matches("auth::user::session::validate"));
    }
}
