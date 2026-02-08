use gimli::{self, RunTimeEndian, EndianSlice, SectionId};
use object::{Object, ObjectSection, ObjectSegment};
use memmap2::Mmap;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs::File;
use std::path::Path;
use crate::{Error, Result};
use std::sync::Mutex;
use rayon::prelude::*;
use crate::symbols::demangle_symbol;
use super::{FunctionInfo, VariableInfo, TypeKind, WatchRecipe, LocalVariableInfo, LocalVarLocation};

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
    pub fn extract_image_base(binary_path: &Path) -> Result<u64> {
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
            let data = object
                .section_by_name(name)
                .or_else(|| {
                    let macho_name = name.replace(".debug_", "__debug_");
                    object.section_by_name(&macho_name)
                })
                .and_then(|section| section.data().ok())
                .unwrap_or(&[]);
            Ok(Cow::Borrowed(data))
        };

        let dwarf_cow = gimli::DwarfSections::load(&load_section)
            .map_err(|e| Error::Frida(format!("Failed to load DWARF: {}", e)))?;

        let dwarf = dwarf_cow.borrow(|section| {
            EndianSlice::new(section.as_ref(), endian)
        });

        // Collect all compilation unit headers for parallel processing
        let mut headers = Vec::new();
        let mut units_iter = dwarf.units();
        while let Ok(Some(header)) = units_iter.next() {
            headers.push(header);
        }

        // Parse each compilation unit in parallel
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
                let mut lazy_infos: Vec<(String, usize, usize)> = Vec::new();

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
                            if let Ok(Some(func)) = Self::parse_function(&dwarf, &unit, entry) {
                                functions.push(func);
                            }
                        }
                        gimli::DW_TAG_variable if !in_subprogram => {
                            if let Ok(Some(var)) = Self::parse_variable(&dwarf, &unit, entry) {
                                // For pointer variables, store type offset for lazy struct resolution
                                if matches!(var.type_kind, TypeKind::Pointer) {
                                    if let Some(gimli::AttributeValue::UnitRef(type_off)) =
                                        entry.attr_value(gimli::DW_AT_type).ok().flatten()
                                    {
                                        lazy_infos.push((var.name.clone(), cu_offset, type_off.0));
                                    }
                                }
                                variables.push(var);
                            }
                        }
                        _ => {}
                    }
                }

                Some((functions, variables, lazy_infos))
            })
            .collect();

        // Merge results from all CUs
        let mut functions = Vec::new();
        let mut variables = Vec::new();
        let mut lazy_struct_info = HashMap::new();
        for (funcs, vars, infos) in results {
            functions.extend(funcs);
            variables.extend(vars);
            for (name, cu_off, type_off) in infos {
                lazy_struct_info.insert(name, (cu_off, type_off));
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

        Ok(Self {
            functions,
            functions_by_name,
            variables,
            variables_by_name,
            struct_members: Mutex::new(HashMap::new()),
            lazy_struct_info,
            image_base: 0, // Set by parse() from the actual binary
            binary_path: Some(path.to_path_buf()),
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

    /// Parse a global variable. Struct members for pointer variables are resolved
    /// lazily when resolve_watch_expression encounters `->` syntax.
    fn parse_variable<R: gimli::Reader>(
        dwarf: &gimli::Dwarf<R>,
        unit: &gimli::Unit<R>,
        entry: &gimli::DebuggingInformationEntry<R>,
    ) -> Result<Option<VariableInfo>> {
        // Get name: prefer linkage_name over short name for demangling
        let linkage_name = entry.attr_value(gimli::DW_AT_linkage_name).ok().flatten()
            .and_then(|v| dwarf.attr_string(unit, v).ok())
            .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));

        let short_name = entry.attr_value(gimli::DW_AT_name).ok().flatten()
            .and_then(|v| dwarf.attr_string(unit, v).ok())
            .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));

        let name = match linkage_name.or(short_name.clone()) {
            Some(n) => n,
            None => return Ok(None),
        };

        // Get location — only accept simple DW_OP_addr (fixed address globals)
        let address = match Self::parse_variable_address(dwarf, unit, entry) {
            Some(addr) => addr,
            None => return Ok(None),
        };

        // Get type info
        let (byte_size, type_kind, type_name) = Self::resolve_type_info(dwarf, unit, entry)
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

        let offset = match type_attr {
            gimli::AttributeValue::UnitRef(o) => o,
            _ => return None,
        };

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
                    let offset_val = child.entry()
                        .attr_value(gimli::DW_AT_data_member_location).ok().flatten()
                        .and_then(|v| match v {
                            gimli::AttributeValue::Udata(n) => Some(n),
                            gimli::AttributeValue::Sdata(n) if n >= 0 => Some(n as u64),
                            _ => None,
                        })
                        .unwrap_or(0);
                    if offset_val == 0 {
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

                    // Get member offset (DW_AT_data_member_location)
                    let member_offset = child_entry.attr_value(gimli::DW_AT_data_member_location)
                        .ok().flatten()
                        .and_then(|v| match v {
                            gimli::AttributeValue::Udata(n) => Some(n),
                            gimli::AttributeValue::Sdata(n) if n >= 0 => Some(n as u64),
                            _ => None,
                        })
                        .unwrap_or(0);

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

        let file = File::open(binary_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = object::File::parse(&*mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        let endian = if object.is_little_endian() { RunTimeEndian::Little } else { RunTimeEndian::Big };

        let load_section = |id: SectionId| -> std::result::Result<Cow<[u8]>, gimli::Error> {
            let name = id.name();
            let data = object.section_by_name(name)
                .or_else(|| object.section_by_name(&name.replace(".debug_", "__debug_")))
                .and_then(|section| section.data().ok())
                .unwrap_or(&[]);
            Ok(Cow::Borrowed(data))
        };

        let dwarf_cow = gimli::DwarfSections::load(&load_section)
            .map_err(|e| Error::Frida(format!("DWARF load: {}", e)))?;
        let dwarf = dwarf_cow.borrow(|s| EndianSlice::new(s.as_ref(), endian));

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

        let file = File::open(binary_path)?;
        let mmap = unsafe { Mmap::map(&file)? };
        let object = object::File::parse(&*mmap)
            .map_err(|e| Error::Frida(format!("Failed to parse binary: {}", e)))?;

        let endian = if object.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        let load_section = |id: SectionId| -> std::result::Result<Cow<[u8]>, gimli::Error> {
            let name = id.name();
            let data = object.section_by_name(name)
                .or_else(|| object.section_by_name(&name.replace(".debug_", "__debug_")))
                .and_then(|section| section.data().ok())
                .unwrap_or(&[]);
            Ok(Cow::Borrowed(data))
        };

        let dwarf_cow = gimli::Dwarf::load(&load_section)
            .map_err(|e| Error::Frida(format!("DWARF load: {}", e)))?;
        #[allow(deprecated)]
        let dwarf = dwarf_cow.borrow(|s| EndianSlice::new(s.as_ref(), endian));

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
        // Strip C++ parameter signature before matching.
        // e.g. "timing::fast()" → "timing::fast"
        // e.g. "audio::process_buffer(audio::AudioBuffer*)" → "audio::process_buffer"
        // This ensures patterns like "timing::fast" and "audio::*" work with demangled C++ names.
        let name = match name.find('(') {
            Some(idx) => &name[..idx],
            None => name,
        };
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
