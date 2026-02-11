use gimli::{self, RunTimeEndian, EndianSlice, SectionId};
use object::{Object, ObjectSection, ObjectSegment, FileKind};
use object::read::macho::{FatArch, MachOFatFile32, MachOFatFile64};
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use crate::{Error, Result};
use std::sync::Mutex;
use rayon::prelude::*;
use crate::symbols::demangle_symbol;
use super::{FunctionInfo, VariableInfo, TypeKind, WatchRecipe, LocalVariableInfo, LocalVarLocation};

/// Extract the native architecture slice from a fat (universal) Mach-O binary.
/// Returns `(offset, size)` for the slice matching the current architecture,
/// or `None` if the data is not a fat binary.
fn extract_native_arch_range(data: &[u8]) -> Option<(u64, u64)> {
    let kind = FileKind::parse(data).ok()?;
    match kind {
        FileKind::MachOFat32 => {
            let fat = MachOFatFile32::parse(data).ok()?;
            let target_arch = target_mach_cputype();
            for arch in fat.arches() {
                if arch.cputype() == target_arch {
                    return Some((arch.offset().into(), arch.size().into()));
                }
            }
            // Fallback: return first arch if no exact match
            fat.arches().first().map(|a| (a.offset().into(), a.size().into()))
        }
        FileKind::MachOFat64 => {
            let fat = MachOFatFile64::parse(data).ok()?;
            let target_arch = target_mach_cputype();
            for arch in fat.arches() {
                if arch.cputype() == target_arch {
                    return Some((arch.offset(), arch.size()));
                }
            }
            fat.arches().first().map(|a| (a.offset(), a.size()))
        }
        _ => None,
    }
}

/// Return the Mach-O CPU type constant for the current architecture.
fn target_mach_cputype() -> u32 {
    if cfg!(target_arch = "aarch64") {
        0x0100000C // CPU_TYPE_ARM64
    } else if cfg!(target_arch = "x86_64") {
        0x01000007 // CPU_TYPE_X86_64
    } else {
        0 // Unknown — will fall through to first-arch fallback
    }
}

/// Parse an object file from mmap data, handling fat (universal) binaries
/// by extracting the native architecture slice first.
fn parse_object_file(data: &[u8]) -> std::result::Result<object::File<'_>, object::read::Error> {
    if let Some((offset, size)) = extract_native_arch_range(data) {
        let end = offset.saturating_add(size) as usize;
        let offset = offset as usize;
        if end <= data.len() {
            return object::File::parse(&data[offset..end]);
        }
    }
    object::File::parse(data)
}

/// Parsed DWARF sections with their associated endianness.
/// Owns all section data (copied from mmap) so there are no lifetime constraints.
struct LoadedDwarf {
    sections: gimli::DwarfSections<Vec<u8>>,
    endian: RunTimeEndian,
    has_debug_info: bool,
}

/// Load DWARF sections from a binary file. Section data is copied into owned `Vec<u8>`
/// so the returned value is self-contained with no lifetime dependencies on the mmap.
fn load_dwarf_sections(path: &Path) -> Result<LoadedDwarf> {
    let file = File::open(path)
        .map_err(|e| Error::Frida(format!("Failed to open binary: {}", e)))?;
    let mmap = unsafe { Mmap::map(&file) }
        .map_err(|e| Error::Frida(format!("Failed to mmap binary: {}", e)))?;
    let object = parse_object_file(&mmap)
        .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

    let has_debug_info = object.section_by_name(".debug_info").is_some()
        || object.section_by_name("__debug_info").is_some();

    let endian = if object.is_little_endian() {
        RunTimeEndian::Little
    } else {
        RunTimeEndian::Big
    };

    let load_section = |id: SectionId| -> std::result::Result<Vec<u8>, gimli::Error> {
        let name = id.name();
        let data = object
            .section_by_name(name)
            .or_else(|| {
                let macho_name = name.replace(".debug_", "__debug_");
                object.section_by_name(&macho_name)
            })
            .and_then(|section| section.data().ok())
            .unwrap_or(&[]);
        Ok(data.to_vec())
    };

    let sections = gimli::DwarfSections::load(&load_section)
        .map_err(|e| Error::Frida(format!("Failed to load DWARF: {}", e)))?;

    Ok(LoadedDwarf { sections, endian, has_debug_info })
}

impl LoadedDwarf {
    /// Borrow the owned sections as `EndianSlice` references for DWARF traversal.
    fn borrow(&self) -> gimli::Dwarf<EndianSlice<'_, RunTimeEndian>> {
        self.sections.borrow(|section| {
            EndianSlice::new(section, self.endian)
        })
    }
}

#[derive(Debug, Clone)]
pub struct LineEntry {
    pub address: u64,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub is_statement: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct StructMember {
    pub name: String,
    pub offset: u64,
    pub byte_size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
    pub is_pointer: bool,
    pub pointed_struct_members: Option<Vec<StructMember>>,
}

pub struct DwarfParser {
    pub functions: Vec<FunctionInfo>,
    pub(crate) functions_by_name: HashMap<String, Vec<usize>>,
    /// Sorted by low_pc for binary search in address-to-function lookups.
    pub(crate) functions_by_addr: Vec<(u64, u64)>, // (low_pc, high_pc)
    pub variables: Vec<VariableInfo>,
    pub(crate) variables_by_name: HashMap<String, Vec<usize>>,
    /// Cache of lazily-resolved struct member layouts for pointer variables.
    /// Populated on-demand when resolve_watch_expression encounters `->` syntax.
    pub(crate) struct_members: Mutex<HashMap<String, Vec<StructMember>>>,
    /// Stored DWARF offsets for pointer variables, enabling lazy struct member resolution.
    /// Maps variable name to (CU section offset, type DIE unit offset).
    pub(crate) lazy_struct_info: HashMap<String, (usize, usize)>,
    /// The image base address from the Mach-O/ELF binary (e.g., __TEXT vmaddr).
    /// Used to compute offsets for ASLR adjustment at runtime.
    pub image_base: u64,
    /// Path to the binary (or dSYM) for re-parsing on demand (e.g., crash locals)
    pub(crate) binary_path: Option<std::path::PathBuf>,
    /// Parsed line table entries, sorted by address. Lazily populated on first line query.
    pub(crate) line_table: Mutex<Option<Vec<LineEntry>>>,
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

        // On macOS, check for .dSYM bundle (Linux debug info is embedded in ELF)
        #[cfg(target_os = "macos")]
        {
            let dsym_path = binary_path.with_extension("dSYM");
            if dsym_path.exists() {
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
        }

        Err(Error::NoDebugSymbols)
    }

    /// Extract the image base address from a binary's __TEXT segment (Mach-O) or
    /// first LOAD segment (ELF). This is the expected load address before ASLR.
    pub fn extract_image_base(binary_path: &Path) -> Result<u64> {
        let file = File::open(binary_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = parse_object_file(&mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        // Mach-O: use the __TEXT segment address directly
        for segment in object.segments() {
            if let Some(name) = segment.name().ok().flatten() {
                if name == "__TEXT" {
                    return Ok(segment.address());
                }
            }
        }

        // ELF: find the minimum virtual address among loadable segments.
        // For PIE binaries this is typically 0x0; for non-PIE it's 0x400000.
        // We identify loadable segments by having non-zero size and being
        // page-aligned (distinguishes PT_LOAD from PT_PHDR, PT_INTERP, etc.).
        let mut min_load_addr: Option<u64> = None;
        for segment in object.segments() {
            let addr = segment.address();
            let size = segment.size();
            let align = segment.align();
            // PT_LOAD segments have page-sized alignment (>= 0x1000)
            if size > 0 && align >= 0x1000 {
                min_load_addr = Some(match min_load_addr {
                    Some(prev) => prev.min(addr),
                    None => addr,
                });
            }
        }
        if let Some(addr) = min_load_addr {
            return Ok(addr);
        }

        Ok(0)
    }

    fn parse_file(path: &Path) -> Result<Self> {
        let loaded = load_dwarf_sections(path)?;

        if !loaded.has_debug_info {
            return Err(Error::NoDebugSymbols);
        }

        let dwarf = loaded.borrow();

        // Collect all compilation unit headers for parallel processing
        let mut headers = Vec::new();
        let mut units_iter = dwarf.units();
        while let Ok(Some(header)) = units_iter.next() {
            headers.push(header);
        }

        // Parse each compilation unit in parallel.
        // Collect: functions, variables, lazy struct info, declaration names (for
        // cross-CU reference resolution), and unresolved functions (have code but
        // name is in a different CU via DW_AT_specification).
        //
        // declarations: (absolute_debug_info_offset -> demangled_name)
        // unresolved:   partial FunctionInfo with spec_offset pointing to declaration
        struct UnresolvedFunc {
            low_pc: u64,
            high_pc: u64,
            source_file: Option<String>,
            line_number: Option<u32>,
            spec_offset: usize,
        }

        let results: Vec<_> = headers
            .into_par_iter()
            .filter_map(|header| {
                let unit = dwarf.unit(header).ok()?;
                let cu_offset = match unit.header.offset() {
                    gimli::UnitSectionOffset::DebugInfoOffset(o) => o.0,
                    gimli::UnitSectionOffset::DebugTypesOffset(o) => o.0,
                };
                let mut functions = Vec::new();
                let mut variables = Vec::new();
                let mut lazy_infos: Vec<(String, (usize, usize))> = Vec::new();
                let mut declarations: Vec<(usize, String)> = Vec::new();
                let mut unresolved: Vec<UnresolvedFunc> = Vec::new();

                let mut entries = unit.entries();
                let mut in_subprogram = false;
                let mut subprogram_depth: isize = 0;
                let mut current_depth: isize = 0;

                while let Ok(Some((delta, entry))) = entries.next_dfs() {
                    current_depth += delta;

                    if in_subprogram && current_depth <= subprogram_depth {
                        in_subprogram = false;
                    }

                    match entry.tag() {
                        gimli::DW_TAG_subprogram => {
                            in_subprogram = true;
                            subprogram_depth = current_depth;

                            // Collect declaration names for cross-CU resolution.
                            // These are subprograms with a name but DW_AT_declaration=true
                            // (no code body), typically from class method declarations.
                            let is_declaration = entry.attr_value(gimli::DW_AT_declaration)
                                .ok().flatten()
                                .map(|v| matches!(v, gimli::AttributeValue::Flag(true)))
                                .unwrap_or(false);
                            if is_declaration {
                                let linkage = Self::resolve_string_attr(&dwarf, &unit, entry, gimli::DW_AT_linkage_name);
                                if let Some(name) = linkage {
                                    // Compute absolute .debug_info offset for this entry
                                    let entry_offset = entry.offset();
                                    let abs_offset = cu_offset + entry_offset.0;
                                    let demangled = crate::symbols::demangle_symbol(&name);
                                    declarations.push((abs_offset, demangled));
                                }
                            }

                            match Self::parse_function(&dwarf, &unit, entry) {
                                Ok(Some(func)) => functions.push(func),
                                Ok(None) => {
                                    // Function had no name AND no same-CU reference — check
                                    // for cross-CU DW_AT_specification that we can resolve later
                                    if let Some(spec_off) = Self::cross_cu_spec_offset(&unit, entry) {
                                        // Only collect if it has a code address (low_pc)
                                        if let Some(low_pc) = entry.attr_value(gimli::DW_AT_low_pc).ok().flatten()
                                            .and_then(|v| dwarf.attr_address(&unit, v).ok().flatten())
                                        {
                                            let high_pc = match entry.attr_value(gimli::DW_AT_high_pc).ok().flatten() {
                                                Some(gimli::AttributeValue::Udata(offset)) => low_pc + offset,
                                                Some(attr_val) => dwarf.attr_address(&unit, attr_val).ok().flatten().unwrap_or(low_pc + 1),
                                                _ => low_pc + 1,
                                            };
                                            let source_file = Self::parse_source_file(&dwarf, &unit, entry);
                                            let line_number = match entry.attr_value(gimli::DW_AT_decl_line).ok().flatten() {
                                                Some(gimli::AttributeValue::Udata(n)) => Some(n as u32),
                                                _ => None,
                                            };
                                            unresolved.push(UnresolvedFunc {
                                                low_pc, high_pc, source_file, line_number,
                                                spec_offset: spec_off,
                                            });
                                        }
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                        gimli::DW_TAG_variable if !in_subprogram => {
                            if let Ok(Some(var)) = Self::parse_variable(&dwarf, &unit, entry) {
                                // For pointer variables, store type offset for lazy struct resolution
                                if matches!(var.type_kind, TypeKind::Pointer) {
                                    // Get DW_AT_type — fall back to referenced declaration entry
                                    // for C++ extern vars where definition has DW_AT_specification only
                                    let type_attr = entry.attr_value(gimli::DW_AT_type).ok().flatten()
                                        .or_else(|| {
                                            Self::resolve_reference(&unit, entry)
                                                .and_then(|ref_e| ref_e.attr_value(gimli::DW_AT_type).ok().flatten())
                                        });
                                    let type_unit_off = match type_attr {
                                        Some(gimli::AttributeValue::UnitRef(off)) => Some(off),
                                        Some(gimli::AttributeValue::DebugInfoRef(off)) => off.to_unit_offset(&unit.header),
                                        _ => None,
                                    };
                                    if let Some(type_off) = type_unit_off {
                                        lazy_infos.push((var.name.clone(), (cu_offset, type_off.0)));
                                    }
                                }
                                variables.push(var);
                            }
                        }
                        _ => {}
                    }
                }

                Some((functions, variables, lazy_infos, declarations, unresolved))
            })
            .collect();

        // Merge results from all CUs
        let mut functions = Vec::new();
        let mut variables = Vec::new();
        let mut lazy_struct_info = HashMap::new();
        let mut declaration_map: HashMap<usize, String> = HashMap::new();
        let mut all_unresolved: Vec<UnresolvedFunc> = Vec::new();
        for (funcs, vars, infos, decls, unres) in results {
            functions.extend(funcs);
            variables.extend(vars);
            lazy_struct_info.extend(infos);
            declaration_map.extend(decls);
            all_unresolved.extend(unres);
        }

        // Resolve cross-CU function references: match unresolved functions
        // (code with no name) to declarations (name with no code)
        if !all_unresolved.is_empty() {
            let resolved_count = all_unresolved.len();
            let mut actually_resolved = 0;
            for unres in all_unresolved {
                if let Some(name) = declaration_map.get(&unres.spec_offset) {
                    let name_raw = None; // declarations already have demangled names
                    functions.push(FunctionInfo {
                        name: name.clone(),
                        name_raw,
                        low_pc: unres.low_pc,
                        high_pc: unres.high_pc,
                        source_file: unres.source_file,
                        line_number: unres.line_number,
                    });
                    actually_resolved += 1;
                }
            }
            if actually_resolved > 0 {
                tracing::debug!("Resolved {}/{} cross-CU function references", actually_resolved, resolved_count);
            }
        }

        // Build indexes
        let mut functions_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, func) in functions.iter().enumerate() {
            functions_by_name
                .entry(func.name.clone())
                .or_default()
                .push(idx);
        }

        let mut variables_by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, var) in variables.iter().enumerate() {
            variables_by_name
                .entry(var.name.clone())
                .or_default()
                .push(idx);
            if let Some(ref short) = var.short_name {
                if short != &var.name {
                    variables_by_name
                        .entry(short.clone())
                        .or_default()
                        .push(idx);
                }
            }
        }

        // Build sorted address index for O(log N) address-to-function lookups
        let mut functions_by_addr: Vec<(u64, u64)> = functions
            .iter()
            .filter(|f| f.low_pc > 0 && f.high_pc > f.low_pc)
            .map(|f| (f.low_pc, f.high_pc))
            .collect();
        functions_by_addr.sort_unstable_by_key(|&(low, _)| low);

        Ok(Self {
            functions,
            functions_by_name,
            functions_by_addr,
            variables,
            variables_by_name,
            struct_members: Mutex::new(HashMap::new()),
            lazy_struct_info,
            image_base: 0, // Set by parse() from the actual binary
            binary_path: Some(path.to_path_buf()),
            line_table: Mutex::new(None),
        })
    }

    /// Resolve a string attribute from an entry, handling DWARF v4/v5 string forms.
    fn resolve_string_attr<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
        attr: gimli::DwAt,
    ) -> Option<String> {
        entry.attr_value(attr).ok().flatten()
            .and_then(|v| dwarf.attr_string(unit, v).ok())
            .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()))
    }

    /// Follow DW_AT_specification or DW_AT_abstract_origin to get the referenced entry.
    fn resolve_reference<'a, R: gimli::Reader>(
        unit: &'a gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<gimli::DebuggingInformationEntry<'a, 'a, R>> {
        let ref_offset = entry.attr_value(gimli::DW_AT_specification).ok().flatten()
            .or_else(|| entry.attr_value(gimli::DW_AT_abstract_origin).ok().flatten());

        match ref_offset {
            Some(gimli::AttributeValue::UnitRef(offset)) => {
                unit.entry(offset).ok()
            }
            // Cross-CU reference: try converting DebugInfoRef to same-CU UnitRef
            Some(gimli::AttributeValue::DebugInfoRef(offset)) => {
                offset.to_unit_offset(&unit.header)
                    .and_then(|unit_offset| unit.entry(unit_offset).ok())
            }
            _ => None,
        }
    }

    /// Extract the absolute .debug_info offset for a DW_AT_specification/DW_AT_abstract_origin
    /// that couldn't be resolved within the same CU (cross-CU DebugInfoRef).
    fn cross_cu_spec_offset<R: gimli::Reader<Offset = usize>>(
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<usize> {
        let ref_offset = entry.attr_value(gimli::DW_AT_specification).ok().flatten()
            .or_else(|| entry.attr_value(gimli::DW_AT_abstract_origin).ok().flatten());
        match ref_offset {
            Some(gimli::AttributeValue::DebugInfoRef(offset)) => {
                // Only return if it's truly cross-CU (couldn't be converted to UnitRef)
                if offset.to_unit_offset(&unit.header).is_none() {
                    Some(offset.0)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn parse_function<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Result<Option<FunctionInfo>> {
        // Get function name: prefer DW_AT_linkage_name (fully qualified mangled name) over
        // DW_AT_name (short name). Handles DWARF v4 and v5 string forms.
        let mut linkage_name = Self::resolve_string_attr(dwarf, unit, entry, gimli::DW_AT_linkage_name);
        let mut short_name = Self::resolve_string_attr(dwarf, unit, entry, gimli::DW_AT_name);

        // If no name found, follow DW_AT_specification / DW_AT_abstract_origin to the
        // referenced declaration entry (common in C++ where namespace members are declared
        // inside DW_TAG_namespace and defined separately with DW_AT_specification).
        let ref_entry;
        if linkage_name.is_none() && short_name.is_none() {
            ref_entry = Self::resolve_reference(unit, entry);
            if let Some(ref ref_e) = ref_entry {
                linkage_name = Self::resolve_string_attr(dwarf, unit, ref_e, gimli::DW_AT_linkage_name);
                short_name = Self::resolve_string_attr(dwarf, unit, ref_e, gimli::DW_AT_name);
            }
        }

        // Use linkage name for demangling (gives qualified names), fall back to short name
        let name = match linkage_name.or(short_name) {
            Some(n) => n,
            None => return Ok(None),
        };

        // Get low_pc (handles DWARF v4 Addr and DWARF v5 DebugAddrIndex)
        let low_pc = match entry.attr_value(gimli::DW_AT_low_pc).ok().flatten() {
            Some(attr_val) => {
                match dwarf.attr_address(unit, attr_val).ok().flatten() {
                    Some(addr) => addr,
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        // Get high_pc (can be absolute address, indexed address, or offset from low_pc)
        let high_pc = match entry.attr_value(gimli::DW_AT_high_pc).ok().flatten() {
            Some(gimli::AttributeValue::Udata(offset)) => low_pc + offset,
            Some(attr_val) => {
                match dwarf.attr_address(unit, attr_val) {
                    Ok(Some(addr)) => addr,
                    _ => low_pc + 1,
                }
            }
            _ => low_pc + 1, // Minimal range if not specified
        };

        // Get source file and line number (fall back to referenced entry)
        let source_file = Self::parse_source_file(dwarf, unit, entry)
            .or_else(|| {
                Self::resolve_reference(unit, entry)
                    .and_then(|ref_e| Self::parse_source_file(dwarf, unit, &ref_e))
            });

        let line_number = match entry.attr_value(gimli::DW_AT_decl_line).ok().flatten() {
            Some(gimli::AttributeValue::Udata(n)) => Some(n as u32),
            _ => {
                Self::resolve_reference(unit, entry)
                    .and_then(|ref_e| match ref_e.attr_value(gimli::DW_AT_decl_line).ok().flatten() {
                        Some(gimli::AttributeValue::Udata(n)) => Some(n as u32),
                        _ => None,
                    })
            }
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

    /// Parse a global variable. Struct members for pointer variables are resolved
    /// lazily when resolve_watch_expression encounters `->` syntax.
    fn parse_variable<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Result<Option<VariableInfo>> {
        // Get name: prefer linkage_name over short name for demangling.
        // Follow DW_AT_specification/DW_AT_abstract_origin for C++ extern
        // variable definitions that have no name on the definition entry.
        let linkage_name = Self::resolve_string_attr(dwarf, unit, entry, gimli::DW_AT_linkage_name);
        let short_name = Self::resolve_string_attr(dwarf, unit, entry, gimli::DW_AT_name);

        let (linkage_name, short_name) = if linkage_name.is_none() && short_name.is_none() {
            if let Some(ref_entry) = Self::resolve_reference(unit, entry) {
                let ln = Self::resolve_string_attr(dwarf, unit, &ref_entry, gimli::DW_AT_linkage_name);
                let sn = Self::resolve_string_attr(dwarf, unit, &ref_entry, gimli::DW_AT_name);
                (ln, sn)
            } else {
                (None, None)
            }
        } else {
            (linkage_name, short_name)
        };

        let name = match linkage_name.or(short_name.clone()) {
            Some(n) => n,
            None => return Ok(None),
        };

        // Get location — only accept simple DW_OP_addr (fixed address globals)
        let address = match Self::parse_variable_address(dwarf, unit, entry) {
            Some(addr) => addr,
            None => return Ok(None),
        };

        // Get type info — fall back to referenced entry if not on this entry
        let (byte_size, type_kind, type_name) = Self::resolve_type_info(dwarf, unit, entry)
            .or_else(|| {
                Self::resolve_reference(unit, entry)
                    .and_then(|ref_entry| Self::resolve_type_info(dwarf, unit, &ref_entry))
            })
            .unwrap_or((0, TypeKind::Unknown, None));

        // Skip if size is not 1, 2, 4, or 8
        if !matches!(byte_size, 1 | 2 | 4 | 8) {
            return Ok(None);
        }

        // Get source file
        let source_file = Self::parse_source_file(dwarf, unit, entry);

        // Demangle
        let demangled = demangle_symbol(&name);
        let name_raw = if name != demangled { Some(name) } else { None };

        Ok(Some(VariableInfo {
            name: demangled,
            name_raw,
            short_name,
            address,
            byte_size,
            type_name,
            type_kind,
            source_file,
        }))
    }

    fn parse_variable_address<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<u64> {
        let loc_attr = entry.attr_value(gimli::DW_AT_location).ok()??;
        match loc_attr {
            gimli::AttributeValue::Exprloc(expr) => {
                let mut ops = expr.operations(unit.encoding());
                match ops.next().ok()? {
                    Some(gimli::Operation::Address { address }) => Some(address),
                    // DWARF v5: indexed address via DW_OP_addrx
                    Some(gimli::Operation::AddressIndex { index }) => {
                        dwarf.address(unit, index).ok()
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn resolve_type_info<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<(u8, TypeKind, Option<String>)> {
        let type_attr = entry.attr_value(gimli::DW_AT_type).ok()??;
        Self::follow_type_chain(dwarf, unit, type_attr, 0)
    }

    fn follow_type_chain<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        type_attr: gimli::AttributeValue<R>,
        depth: usize,
    ) -> Option<(u8, TypeKind, Option<String>)> {
        if depth > 10 { return None; } // prevent infinite loops

        match type_attr {
            gimli::AttributeValue::UnitRef(offset) => {
                Self::resolve_type_in_unit(dwarf, unit, offset, depth)
            }
            gimli::AttributeValue::DebugInfoRef(di_offset) => {
                // Fast path: most DebugInfoRef point within the same CU.
                if let Some(unit_offset) = di_offset.to_unit_offset(&unit.header) {
                    return Self::resolve_type_in_unit(dwarf, unit, unit_offset, depth);
                }
                // Slow path: true cross-CU reference. Find the containing unit.
                let mut headers = dwarf.debug_info.units();
                while let Ok(Some(header)) = headers.next() {
                    let cu_range_start = match header.offset() {
                        gimli::UnitSectionOffset::DebugInfoOffset(o) => o.0,
                        gimli::UnitSectionOffset::DebugTypesOffset(o) => o.0,
                    };
                    let cu_range_end = cu_range_start + header.length_including_self();
                    if di_offset.0 < cu_range_start || di_offset.0 >= cu_range_end {
                        continue;
                    }
                    // Found the containing CU
                    let unit_offset = gimli::UnitOffset(di_offset.0 - cu_range_start);
                    if let Ok(target_unit) = dwarf.unit(header) {
                        return Self::resolve_type_in_unit(dwarf, &target_unit, unit_offset, depth);
                    }
                    break;
                }
                None
            }
            _ => None,
        }
    }

    fn resolve_type_in_unit<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        offset: gimli::UnitOffset<R::Offset>,
        depth: usize,
    ) -> Option<(u8, TypeKind, Option<String>)> {
        let mut tree = unit.entries_tree(Some(offset)).ok()?;
        let root = tree.root().ok()?;
        let type_entry = root.entry();

        match type_entry.tag() {
            gimli::DW_TAG_base_type => {
                let byte_size = type_entry.attr_value(gimli::DW_AT_byte_size).ok()?
                    .and_then(|v| match v {
                        gimli::AttributeValue::Udata(n) => Some(n as u8),
                        _ => None,
                    })?;
                let encoding = type_entry.attr_value(gimli::DW_AT_encoding).ok()?
                    .and_then(|v| match v {
                        gimli::AttributeValue::Encoding(e) => Some(e),
                        _ => None,
                    });
                let type_kind = match encoding {
                    Some(gimli::DW_ATE_float) => TypeKind::Float,
                    Some(gimli::DW_ATE_signed) | Some(gimli::DW_ATE_signed_char) =>
                        TypeKind::Integer { signed: true },
                    _ => TypeKind::Integer { signed: false },
                };
                let type_name = type_entry.attr_value(gimli::DW_AT_name).ok()?
                    .and_then(|v| dwarf.attr_string(unit, v).ok())
                    .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));
                Some((byte_size, type_kind, type_name))
            }
            gimli::DW_TAG_pointer_type | gimli::DW_TAG_reference_type => {
                let size = unit.encoding().address_size;
                Some((size, TypeKind::Pointer, Some("pointer".to_string())))
            }
            gimli::DW_TAG_typedef | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => {
                let next = type_entry.attr_value(gimli::DW_AT_type).ok()??;
                Self::follow_type_chain(dwarf, unit, next, depth + 1)
            }
            gimli::DW_TAG_enumeration_type => {
                let byte_size = type_entry.attr_value(gimli::DW_AT_byte_size).ok()?
                    .and_then(|v| match v {
                        gimli::AttributeValue::Udata(n) => Some(n as u8),
                        _ => None,
                    })?;
                Some((byte_size, TypeKind::Integer { signed: false }, Some("enum".to_string())))
            }
            gimli::DW_TAG_structure_type => {
                // For single-field structs at offset 0 (Rust newtypes like AtomicU64,
                // UnsafeCell<T>, etc.), follow through to the inner type.
                // This lets us treat AtomicU64 → UnsafeCell<u64> → u64 as a plain u64.
                let mut children = root.children();
                let mut member_type_attr = None;
                let mut member_count = 0u32;
                while let Ok(Some(child)) = children.next() {
                    if child.entry().tag() != gimli::DW_TAG_member {
                        continue;
                    }
                    member_count += 1;
                    if member_count > 1 { break; } // more than one member, not a newtype
                    // Check offset is 0
                    if Self::parse_member_offset(child.entry()) == 0 {
                        member_type_attr = child.entry()
                            .attr_value(gimli::DW_AT_type).ok().flatten();
                    }
                }
                if member_count == 1 {
                    if let Some(inner) = member_type_attr {
                        return Self::follow_type_chain(dwarf, unit, inner, depth + 1);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Extract the byte offset of a struct member from DW_AT_data_member_location.
    fn parse_member_offset<R: gimli::Reader>(
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> u64 {
        entry.attr_value(gimli::DW_AT_data_member_location)
            .ok()
            .flatten()
            .and_then(|v| match v {
                gimli::AttributeValue::Udata(n) => Some(n),
                gimli::AttributeValue::Sdata(n) if n >= 0 => Some(n as u64),
                _ => None,
            })
            .unwrap_or(0)
    }

    fn parse_source_file<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<String> {
        match entry.attr_value(gimli::DW_AT_decl_file).ok()? {
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
                        if !path.is_empty() { return Some(path); }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Follow type chain (through typedefs, const, volatile) to find a struct/class
    /// and parse its members.
    fn parse_struct_members_from_type<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        type_attr: gimli::AttributeValue<R>,
        depth: usize,
    ) -> Option<Vec<StructMember>> {
        if depth > 10 { return None; }

        let offset = match type_attr {
            gimli::AttributeValue::UnitRef(o) => o,
            gimli::AttributeValue::DebugInfoRef(di_off) => di_off.to_unit_offset(&unit.header)?,
            _ => return None,
        };

        let mut tree = unit.entries_tree(Some(offset)).ok()?;
        let root = tree.root().ok()?;
        let type_entry = root.entry();

        match type_entry.tag() {
            gimli::DW_TAG_structure_type | gimli::DW_TAG_class_type => {
                // Parse member children
                let mut members = Vec::new();
                let mut children = root.children();
                while let Ok(Some(child)) = children.next() {
                    let child_entry = child.entry();
                    if child_entry.tag() != gimli::DW_TAG_member {
                        continue;
                    }

                    let member_name = child_entry.attr_value(gimli::DW_AT_name).ok().flatten()
                        .and_then(|v| dwarf.attr_string(unit, v).ok())
                        .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));

                    let member_name = match member_name {
                        Some(n) => n,
                        None => continue,
                    };

                    let member_offset = Self::parse_member_offset(child_entry);

                    // Get member type info
                    let member_type_attr = child_entry.attr_value(gimli::DW_AT_type).ok().flatten();
                    let (byte_size, type_kind, type_name) = member_type_attr.as_ref()
                        .and_then(|attr| Self::follow_type_chain(dwarf, unit, attr.clone(), 0))
                        .unwrap_or((0, TypeKind::Unknown, None));

                    let is_pointer = matches!(type_kind, TypeKind::Pointer);

                    // For pointer members, try to parse their pointed-to struct (nested)
                    let pointed_struct = if is_pointer && depth < 3 {
                        member_type_attr.and_then(|attr| {
                            let ptr_off = match attr {
                                gimli::AttributeValue::UnitRef(o) => o,
                                gimli::AttributeValue::DebugInfoRef(di_off) => di_off.to_unit_offset(&unit.header)?,
                                _ => return None,
                            };
                            let mut pt = unit.entries_tree(Some(ptr_off)).ok()?;
                            let pr = pt.root().ok()?;
                            let pe = pr.entry();
                            let pointee = pe.attr_value(gimli::DW_AT_type).ok()??;
                            Self::parse_struct_members_from_type(dwarf, unit, pointee, depth + 1)
                        })
                    } else {
                        None
                    };

                    members.push(StructMember {
                        name: member_name,
                        offset: member_offset,
                        byte_size,
                        type_kind,
                        type_name,
                        is_pointer,
                        pointed_struct_members: pointed_struct,
                    });
                }

                if members.is_empty() { None } else { Some(members) }
            }
            gimli::DW_TAG_typedef | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type | gimli::DW_TAG_restrict_type => {
                let next = type_entry.attr_value(gimli::DW_AT_type).ok()??;
                Self::parse_struct_members_from_type(dwarf, unit, next, depth + 1)
            }
            _ => None,
        }
    }

    /// Lazily resolve and cache struct members for a pointer variable.
    /// Uses stored CU/type offsets to jump directly to the right DWARF location.
    fn lazy_resolve_struct_members(&self, var_name: &str) -> Result<()> {
        // Check cache first
        {
            let cache = self.struct_members.lock().unwrap();
            if cache.contains_key(var_name) {
                return Ok(());
            }
        }

        let &(cu_offset, type_die_offset) = self.lazy_struct_info.get(var_name)
            .ok_or_else(|| Error::Frida(format!(
                "No type info stored for pointer variable '{}'", var_name
            )))?;

        let binary_path = self.binary_path.as_ref()
            .ok_or_else(|| Error::Frida("No binary path for lazy struct member resolution".into()))?;

        let loaded = load_dwarf_sections(binary_path)?;
        let dwarf = loaded.borrow();

        // Jump directly to the right CU using stored offset
        let header = dwarf.debug_info.header_from_offset(gimli::DebugInfoOffset(cu_offset))
            .map_err(|e| Error::Frida(format!("Failed to find CU at offset {}: {}", cu_offset, e)))?;
        let unit = dwarf.unit(header)
            .map_err(|e| Error::Frida(format!("Failed to parse CU: {}", e)))?;

        // Navigate to the pointer type DIE using stored offset
        let ptr_offset = gimli::UnitOffset(type_die_offset);
        let mut ptr_tree = unit.entries_tree(Some(ptr_offset))
            .map_err(|e| Error::Frida(format!("Failed to find type DIE: {}", e)))?;
        let ptr_root = ptr_tree.root()
            .map_err(|e| Error::Frida(format!("Failed to read type DIE: {}", e)))?;
        let ptr_entry = ptr_root.entry();

        if ptr_entry.tag() != gimli::DW_TAG_pointer_type {
            return Err(Error::Frida(format!(
                "Type for '{}' is not a pointer type (tag: {:?})", var_name, ptr_entry.tag()
            )));
        }

        let pointee_attr = ptr_entry.attr_value(gimli::DW_AT_type)
            .map_err(|e| Error::Frida(format!("Failed to get pointee type: {}", e)))?
            .ok_or_else(|| Error::Frida("Pointer type has no pointee type".into()))?;

        let members = Self::parse_struct_members_from_type(&dwarf, &unit, pointee_attr, 0)
            .ok_or_else(|| Error::Frida(format!(
                "No struct members found for pointee of '{}'", var_name
            )))?;

        let mut cache = self.struct_members.lock().unwrap();
        cache.insert(var_name.to_string(), members);
        Ok(())
    }

    pub fn resolve_watch_expression(&self, expr: &str) -> Result<WatchRecipe> {
        if !expr.contains("->") {
            // Simple variable — direct read
            let var = self.find_variable_by_name(expr)
                .ok_or_else(|| Error::Frida(format!("Variable '{}' not found", expr)))?;
            return Ok(WatchRecipe {
                label: expr.to_string(),
                base_address: var.address,
                deref_chain: vec![],
                final_size: var.byte_size,
                type_kind: var.type_kind.clone(),
                type_name: var.type_name.clone(),
            });
        }

        // Parse "varName->member1->member2"
        let parts: Vec<&str> = expr.split("->").collect();
        let root_name = parts[0];

        let var = self.find_variable_by_name(root_name)
            .ok_or_else(|| Error::Frida(format!("Variable '{}' not found", root_name)))?;

        // Root must be a pointer
        if !matches!(var.type_kind, TypeKind::Pointer) {
            return Err(Error::Frida(format!(
                "'{}' is not a pointer type (is {:?}), cannot use -> syntax",
                root_name, var.type_kind
            )));
        }

        self.resolve_member_chain(var, &parts[1..], expr)
    }

    fn resolve_member_chain(
        &self,
        root_var: &VariableInfo,
        member_path: &[&str],
        full_expr: &str,
    ) -> Result<WatchRecipe> {
        // Lazily resolve struct members for this variable
        self.lazy_resolve_struct_members(&root_var.name)?;

        let cache = self.struct_members.lock().unwrap();
        let mut deref_chain = Vec::new();
        let mut current_members = cache.get(&root_var.name)
            .ok_or_else(|| Error::Frida(format!(
                "No struct info for pointer '{}'", root_var.name
            )))?;

        let mut final_size = 0u8;
        let mut final_type_kind = TypeKind::Unknown;
        let mut final_type_name = None;

        for (i, &member_name) in member_path.iter().enumerate() {
            let member = current_members.iter()
                .find(|m| m.name == member_name)
                .ok_or_else(|| Error::Frida(format!(
                    "Member '{}' not found in struct", member_name
                )))?;

            deref_chain.push(member.offset);
            final_size = member.byte_size;
            final_type_kind = member.type_kind.clone();
            final_type_name = member.type_name.clone();

            // If this member is itself a pointer and there are more parts, continue
            if member.is_pointer && i + 1 < member_path.len() {
                current_members = member.pointed_struct_members.as_ref()
                    .ok_or_else(|| Error::Frida(format!(
                        "No struct info for pointer member '{}'", member_name
                    )))?;
            }
        }

        Ok(WatchRecipe {
            label: full_expr.to_string(),
            base_address: root_var.address,
            deref_chain,
            final_size,
            type_kind: final_type_kind,
            type_name: final_type_name,
        })
    }

    /// Convert cached StructMembers to flat field recipes for the agent.
    /// This is a pure transformation — no DWARF re-parsing needed.
    ///
    /// NOTE: Currently only depth=1 is effective. Depth > 1 marks nested structs
    /// as truncated rather than recursively expanding them. Recursive expansion
    /// would require nested field JSON in the agent protocol.
    pub(crate) fn struct_members_to_recipes(members: &[StructMember], depth: usize) -> Vec<super::StructFieldRecipe> {
        members.iter().map(|m| {
            let is_struct_field = !matches!(m.type_kind, TypeKind::Integer { .. } | TypeKind::Float | TypeKind::Pointer);
            let is_truncated = is_struct_field && depth <= 1;

            super::StructFieldRecipe {
                name: m.name.clone(),
                offset: m.offset,
                size: m.byte_size,
                type_kind: m.type_kind.clone(),
                type_name: m.type_name.clone(),
                is_truncated_struct: is_truncated,
            }
        }).collect()
    }

    /// Resolve a variable to a read recipe, optionally expanding struct fields.
    /// Returns the WatchRecipe for the variable plus optional struct field recipes.
    pub fn resolve_read_target(
        &self,
        variable: &str,
        depth: u32,
    ) -> Result<(WatchRecipe, Option<Vec<super::StructFieldRecipe>>)> {
        let recipe = self.resolve_watch_expression(variable)?;

        // For pointer variables with no deref chain (bare pointer), expand struct if depth > 0
        if matches!(recipe.type_kind, TypeKind::Pointer) && recipe.deref_chain.is_empty() {
            if self.lazy_resolve_struct_members(variable).is_ok() {
                let cache = self.struct_members.lock().unwrap();
                if let Some(members) = cache.get(variable) {
                    let field_recipes = Self::struct_members_to_recipes(members, depth as usize);
                    return Ok((recipe, Some(field_recipes)));
                }
            }
        }

        Ok((recipe, None))
    }

    pub fn find_variable_by_name(&self, name: &str) -> Option<&VariableInfo> {
        self.variables_by_name
            .get(name)
            .and_then(|indices| indices.first())
            .map(|&i| &self.variables[i])
    }

    pub fn find_variables_by_pattern(&self, pattern: &str) -> Vec<&VariableInfo> {
        let matcher = PatternMatcher::new(pattern);
        self.variables.iter().filter(|v| matcher.matches(&v.name)).collect()
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

    /// Parse local variables for the function containing the given PC address.
    /// Re-opens the DWARF file and does a targeted parse. Only called on crash (rare).
    pub fn parse_locals_at_pc(&self, crash_pc: u64) -> Result<Vec<LocalVariableInfo>> {
        let binary_path = self.binary_path.as_ref()
            .ok_or_else(|| Error::Frida("No binary path for DWARF re-parse".into()))?;

        let loaded = load_dwarf_sections(binary_path)?;
        let dwarf = loaded.borrow();

        let mut locals = Vec::new();

        let mut units = dwarf.units();
        while let Ok(Some(header)) = units.next() {
            let unit = match dwarf.unit(header) {
                Ok(u) => u,
                Err(_) => continue,
            };

            let mut entries = unit.entries();
            let mut in_target_func = false;
            let mut target_depth: isize = 0;
            let mut current_depth: isize = 0;

            while let Ok(Some((delta, entry))) = entries.next_dfs() {
                current_depth += delta;

                // Left the target function
                if in_target_func && current_depth <= target_depth {
                    break;
                }

                match entry.tag() {
                    gimli::DW_TAG_subprogram => {
                        let low_pc = entry.attr_value(gimli::DW_AT_low_pc).ok().flatten()
                            .and_then(|v| dwarf.attr_address(&unit, v).ok().flatten());
                        let high_pc = entry.attr_value(gimli::DW_AT_high_pc).ok().flatten()
                            .map(|v| match v {
                                gimli::AttributeValue::Udata(offset) => low_pc.map(|lp| lp + offset),
                                _ => dwarf.attr_address(&unit, v).ok().flatten(),
                            })
                            .flatten();

                        if let (Some(lp), Some(hp)) = (low_pc, high_pc) {
                            if crash_pc >= lp && crash_pc < hp {
                                in_target_func = true;
                                target_depth = current_depth;
                            }
                        }
                    }
                    gimli::DW_TAG_variable | gimli::DW_TAG_formal_parameter if in_target_func => {
                        if let Some(local) = Self::parse_local_variable(&dwarf, &unit, entry) {
                            locals.push(local);
                        }
                    }
                    _ => {}
                }
            }

            if !locals.is_empty() {
                break;
            }
        }

        Ok(locals)
    }

    fn parse_local_variable<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Option<LocalVariableInfo> {
        let name = entry.attr_value(gimli::DW_AT_name).ok().flatten()
            .and_then(|v| dwarf.attr_string(unit, v).ok())
            .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()))?;

        // Parse location
        let location = match entry.attr_value(gimli::DW_AT_location).ok().flatten() {
            Some(gimli::AttributeValue::Exprloc(expr)) => {
                let mut ops = expr.operations(unit.encoding());
                match ops.next().ok().flatten() {
                    Some(gimli::Operation::FrameOffset { offset }) => {
                        LocalVarLocation::FrameBaseRelative(offset)
                    }
                    Some(gimli::Operation::Register { register }) => {
                        LocalVarLocation::Register(register.0)
                    }
                    Some(gimli::Operation::RegisterOffset { register, offset, .. }) => {
                        LocalVarLocation::RegisterOffset(register.0, offset)
                    }
                    Some(gimli::Operation::Address { address }) => {
                        LocalVarLocation::Address(address)
                    }
                    _ => LocalVarLocation::Complex,
                }
            }
            _ => LocalVarLocation::Complex,
        };

        // Get type info
        let (byte_size, type_kind, type_name) = Self::resolve_type_info(dwarf, unit, entry)
            .unwrap_or((0, TypeKind::Unknown, None));

        Some(LocalVariableInfo {
            name,
            byte_size,
            type_kind,
            type_name,
            location,
        })
    }

    // ========== Phase 2: Line table support ==========

    /// Resolve file:line to instruction address. Snaps to nearest is_statement line.
    /// Returns (address, actual_line) or None if no code at that location.
    pub fn resolve_line(&self, file: &str, line: u32) -> Option<(u64, u32)> {
        self.ensure_line_table();
        let table = self.line_table.lock().unwrap();
        let entries = table.as_ref()?;

        // Find entries matching file.
        // Use path-component-aware matching: require separator before the match
        // to avoid "main.c" matching "not_main.c". Also handle OS separator
        // differences in DWARF paths.
        let sep_file = format!("/{}", file);
        let sep_file_win = format!("\\{}", file);
        let mut matches: Vec<_> = entries
            .iter()
            .filter(|e| {
                e.is_statement && (e.file == file || e.file.ends_with(&sep_file) || e.file.ends_with(&sep_file_win))
            })
            .collect();

        if matches.is_empty() {
            return None;
        }

        // Find closest line >= requested line
        matches.sort_by_key(|e| e.line);
        matches
            .iter()
            .find(|e| e.line >= line)
            .map(|e| (e.address, e.line))
    }

    /// Find nearest valid line numbers for error messages
    pub fn find_nearest_lines(&self, file: &str, target_line: u32, count: usize) -> String {
        self.ensure_line_table();
        let table = self.line_table.lock().unwrap();
        let entries = match table.as_ref() {
            Some(e) => e,
            None => return "unknown".to_string(),
        };

        // Get all unique statement lines for this file
        let sep_file = format!("/{}", file);
        let sep_file_win = format!("\\{}", file);
        let mut lines: Vec<u32> = entries
            .iter()
            .filter(|e| e.is_statement && (e.file == file || e.file.ends_with(&sep_file) || e.file.ends_with(&sep_file_win)))
            .map(|e| e.line)
            .collect();

        lines.sort_unstable();
        lines.dedup();

        if lines.is_empty() {
            return "none".to_string();
        }

        // Find nearest lines (before and after target)
        let mut nearest: Vec<(u32, u32)> = lines
            .iter()
            .map(|&l| ((l as i64 - target_line as i64).unsigned_abs() as u32, l))
            .collect();

        nearest.sort_unstable();
        let result: Vec<String> = nearest
            .iter()
            .take(count)
            .map(|(_, l)| l.to_string())
            .collect();

        result.join(", ")
    }

    /// O(log N) lookup: find the function containing `address` via binary search
    /// on the sorted functions_by_addr index. Returns (low_pc, high_pc) or None.
    fn function_containing(&self, address: u64) -> Option<(u64, u64)> {
        let idx = match self.functions_by_addr.binary_search_by_key(&address, |&(low, _)| low) {
            Ok(idx) => idx,           // Exact match on low_pc
            Err(0) => return None,    // Before all functions
            Err(idx) => idx - 1,      // Check preceding function
        };
        let (low, high) = self.functions_by_addr[idx];
        if address >= low && address < high {
            Some((low, high))
        } else {
            None
        }
    }

    /// Reverse lookup: address → (file, line, column).
    /// For addresses between line entries, returns the closest preceding entry
    /// (the line that "contains" that address). Only returns a result if the
    /// address falls within a known function boundary.
    pub fn resolve_address(&self, address: u64) -> Option<(String, u32, u32)> {
        self.ensure_line_table();
        let table = self.line_table.lock().unwrap();
        let entries = table.as_ref()?;

        let idx = match entries.binary_search_by_key(&address, |e| e.address) {
            Ok(idx) => idx,           // Exact match
            Err(0) => return None,    // Before all entries
            Err(idx) => idx - 1,      // Closest preceding entry
        };

        // Verify the address is within a known function to avoid returning
        // misleading locations for addresses in inter-function dead space.
        if self.function_containing(address).is_none() {
            return None;
        }

        let entry = &entries[idx];
        Some((entry.file.clone(), entry.line, entry.column))
    }

    /// Find next statement line in the same function. Used for step-over.
    /// Respects function boundaries using the DWARF function table (high_pc).
    /// Find the next source line address after `address` within the same function.
    ///
    /// `min_offset` skips line entries within N bytes of `address`. This is needed
    /// because Frida's Interceptor.attach overwrites up to 14 bytes at the hook
    /// address. When the thread resumes from a trampoline, it jumps past the
    /// overwritten region, so any line entries within that region are unreachable.
    /// Use `min_offset = 16` for step hooks, or `0` for initial breakpoints where
    /// the trampoline is at a function entry (typically far from the next line).
    pub fn next_line_in_function(&self, address: u64, min_offset: u64) -> Option<(u64, String, u32)> {
        self.ensure_line_table();
        let table = self.line_table.lock().unwrap();
        let entries = table.as_ref()?;

        // Find current entry (use closest preceding for non-exact addresses)
        let idx = match entries.binary_search_by_key(&address, |e| e.address) {
            Ok(idx) => idx,
            Err(0) => return None,
            Err(idx) => idx - 1,
        };
        let current = &entries[idx];

        // Minimum address the thread can reach after the Interceptor trampoline
        let min_address = address + min_offset;

        // Find the function containing this address to enforce boundary (O(log N))
        let func_high_pc = self.function_containing(address).map(|(_, high)| high);

        // Find next is_statement line with different line number, same file,
        // staying within the function boundary, and past the trampoline region
        entries[idx + 1..]
            .iter()
            .take_while(|e| func_high_pc.map_or(true, |hp| e.address < hp))
            .find(|e| e.is_statement && e.file == current.file && e.line != current.line && e.address >= min_address)
            .map(|e| (e.address, e.file.clone(), e.line))
    }

    /// Get entry addresses of functions callable from the current line.
    /// Currently returns empty — proper callee resolution requires DWARF call site
    /// info (DW_TAG_call_site) or instruction-level analysis, which is not yet
    /// implemented. Step-into therefore behaves like step-over (next line + return
    /// address as one-shot targets).
    pub fn callee_entry_addresses(&self, _address: u64) -> Vec<u64> {
        Vec::new()
    }
    /// Parse line table on first access (lazy initialization)
    fn ensure_line_table(&self) {
        let mut guard = self.line_table.lock().unwrap();
        if guard.is_some() {
            return;
        }

        let binary_path = match &self.binary_path {
            Some(p) => p,
            None => {
                tracing::warn!("No binary path for line table parsing");
                return;
            }
        };

        match self.parse_line_table(binary_path) {
            Ok(entries) => {
                tracing::info!("Parsed {} line table entries", entries.len());
                *guard = Some(entries);
            }
            Err(e) => {
                tracing::error!("Failed to parse line table: {}", e);
            }
        }
    }

    /// Parse DWARF .debug_line section via gimli
    fn parse_line_table(&self, binary_path: &Path) -> Result<Vec<LineEntry>> {
        let loaded = load_dwarf_sections(binary_path)?;
        let dwarf = loaded.borrow();

        let mut entries = Vec::new();

        let mut units_iter = dwarf.units();
        while let Ok(Some(header)) = units_iter.next() {
            let unit = match dwarf.unit(header) {
                Ok(u) => u,
                Err(_) => continue,
            };

            // Get line program for this CU
            let line_program = match unit.line_program {
                Some(ref program) => program.clone(),
                None => continue,
            };

            let mut rows = line_program.rows();
            while let Ok(Some((header, row))) = rows.next_row() {
                if !row.is_stmt() {
                    continue; // Skip non-statement lines
                }

                let address = row.address();
                let line = row.line().map(|l| l.get() as u32).unwrap_or(0);
                let column = match row.column() {
                    gimli::ColumnType::LeftEdge => 0,
                    gimli::ColumnType::Column(c) => c.get() as u32,
                };

                // Resolve file path
                let file = match row.file(header) {
                    Some(file_entry) => {
                        let path_attr = file_entry.path_name();
                        dwarf.attr_string(&unit, path_attr)
                            .map(|s| s.to_string_lossy().to_string())
                            .unwrap_or_else(|_| "<unknown>".to_string())
                    }
                    None => "<unknown>".to_string(),
                };

                entries.push(LineEntry {
                    address,
                    file,
                    line,
                    column,
                    is_statement: true,
                });
            }
        }

        // Sort by address for binary search
        entries.sort_by_key(|e| e.address);

        Ok(entries)
    }
}

/// Glob-style pattern matcher for function names
pub struct PatternMatcher<'a> {
    pattern: &'a str,
}

impl<'a> PatternMatcher<'a> {
    pub fn new(pattern: &'a str) -> Self {
        Self { pattern }
    }

    pub fn matches(&self, name: &str) -> bool {
        // Strip C++ parameter signature before matching.
        // e.g. "timing::fast()" → "timing::fast"
        // e.g. "audio::process_buffer(audio::AudioBuffer*)" → "audio::process_buffer"
        // This ensures patterns like "timing::fast" and "audio::*" work with demangled C++ names.
        let name = match name.find('(') {
            Some(idx) => &name[..idx],
            None => name,
        };
        Self::glob_match(self.pattern, name)
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

    #[test]
    fn test_pattern_matching_real_rust_names() {
        let rust_name = "stress_tester::midi::process_note_on::h7c4d62da364e13f0";

        let m = PatternMatcher::new("stress_tester::*");
        assert!(!m.matches(rust_name), "* should not cross :: boundaries");

        let m = PatternMatcher::new("stress_tester::**");
        assert!(m.matches(rust_name), "** should match through all :: levels");

        let m = PatternMatcher::new("**::process_note_on**");
        assert!(m.matches(rust_name), "**::name** should match anywhere");

        let m = PatternMatcher::new("stress_tester::midi::*");
        assert!(!m.matches(rust_name), "midi::* shouldn't match because of hash suffix");

        let m = PatternMatcher::new("stress_tester::midi::**");
        assert!(m.matches(rust_name), "midi::** should match through hash suffix");
    }

    #[test]
    fn test_cpp_demangled_names() {
        // C++ demangled names include parameter signatures — pattern matching
        // should strip them so users don't need to spell out parameter types.

        // Exact match strips ()
        let m = PatternMatcher::new("timing::fast");
        assert!(m.matches("timing::fast()"), "Should match through ()");

        // Exact match strips full parameter signature
        let m = PatternMatcher::new("audio::process_buffer");
        assert!(
            m.matches("audio::process_buffer(audio::AudioBuffer*)"),
            "Should match through (qualified::params)"
        );

        // Wildcard * should match C++ names after stripping params
        let m = PatternMatcher::new("audio::*");
        assert!(m.matches("audio::process_buffer(audio::AudioBuffer*)"));
        assert!(m.matches("audio::generate_sine(float)"));
        assert!(m.matches("audio::apply_effect(audio::AudioBuffer*, float)"));

        // Wildcard ** should match nested C++ names
        let m = PatternMatcher::new("midi::**");
        assert!(m.matches("midi::note_on(unsigned char, unsigned char)"));

        // Plain names without parens still work
        let m = PatternMatcher::new("timing::fast");
        assert!(m.matches("timing::fast"));
    }

    #[test]
    fn test_watch_pattern_matching_with_real_names() {
        let real_names = vec![
            "stress_tester::audio::process_audio_buffer::h1e1f7984b2d2cfca",
            "stress_tester::audio::generate_sine_buffer::hdeadbeef12345678",
            "stress_tester::audio::apply_effect_chain::habcdef0123456789",
            "stress_tester::midi::process_note_on::h7c4d62da364e13f0",
            "stress_tester::midi::process_control_change::h72b697f824ed75aa",
            "stress_tester::midi::generate_midi_sequence::h77a24745e78bf175",
            "stress_tester::engine::Engine::update_global_state::hfedcba9876543210",
        ];

        let test_cases: Vec<(&str, Vec<usize>)> = vec![
            ("stress_tester::audio::**", vec![0, 1, 2]),
            ("stress_tester::midi::**", vec![3, 4, 5]),
            ("**::process_note_on**", vec![3]),
            ("**::process_audio_buffer**", vec![0]),
            ("stress_tester::*", vec![]),
        ];

        for (pattern, expected_indices) in test_cases {
            let matcher = PatternMatcher::new(pattern);
            let matched: Vec<usize> = real_names.iter().enumerate()
                .filter(|(_, name)| matcher.matches(name))
                .map(|(i, _)| i)
                .collect();
            assert_eq!(matched, expected_indices,
                "Pattern '{}' matched wrong functions", pattern);
        }
    }
}

#[cfg(test)]
mod struct_expansion_tests {
    use super::*;
    use crate::dwarf::TypeKind;

    #[test]
    fn test_expand_struct_from_members() {
        let members = vec![
            StructMember {
                name: "size".to_string(),
                offset: 0,
                byte_size: 4,
                type_kind: TypeKind::Integer { signed: false },
                type_name: Some("uint32_t".to_string()),
                is_pointer: false,
                pointed_struct_members: None,
            },
            StructMember {
                name: "data".to_string(),
                offset: 8,
                byte_size: 8,
                type_kind: TypeKind::Pointer,
                type_name: Some("pointer".to_string()),
                is_pointer: true,
                pointed_struct_members: None,
            },
        ];

        let recipes = DwarfParser::struct_members_to_recipes(&members, 1);
        assert_eq!(recipes.len(), 2);
        assert_eq!(recipes[0].name, "size");
        assert_eq!(recipes[0].offset, 0);
        assert_eq!(recipes[0].size, 4);
        assert_eq!(recipes[1].name, "data");
        assert_eq!(recipes[1].offset, 8);
        assert!(!recipes[0].is_truncated_struct);
        assert!(!recipes[1].is_truncated_struct); // pointer fields are not truncated structs
    }

    #[test]
    fn test_struct_truncation_at_depth_1() {
        let members = vec![
            StructMember {
                name: "id".to_string(),
                offset: 0,
                byte_size: 4,
                type_kind: TypeKind::Integer { signed: false },
                type_name: Some("uint32_t".to_string()),
                is_pointer: false,
                pointed_struct_members: None,
            },
            StructMember {
                name: "owner".to_string(),
                offset: 8,
                byte_size: 16,
                type_kind: TypeKind::Unknown, // struct type — not Integer/Float/Pointer
                type_name: Some("AudioEngine".to_string()),
                is_pointer: false,
                pointed_struct_members: None,
            },
        ];

        // At depth=1, nested struct fields should be truncated
        let recipes = DwarfParser::struct_members_to_recipes(&members, 1);
        assert_eq!(recipes.len(), 2);
        assert!(!recipes[0].is_truncated_struct, "primitive field should not be truncated");
        assert!(recipes[1].is_truncated_struct, "nested struct at depth=1 should be truncated");
        assert_eq!(recipes[1].type_name.as_deref(), Some("AudioEngine"));

        // At depth=2, nested struct fields should NOT be truncated
        // (even though recursive expansion isn't implemented yet)
        let recipes_d2 = DwarfParser::struct_members_to_recipes(&members, 2);
        assert!(!recipes_d2[1].is_truncated_struct, "nested struct at depth=2 should not be truncated");
    }

    #[test]
    fn test_struct_expansion_empty_members() {
        let members: Vec<StructMember> = vec![];
        let recipes = DwarfParser::struct_members_to_recipes(&members, 1);
        assert!(recipes.is_empty());
    }
}
