use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionInfo {
    pub name: String,
    pub name_raw: Option<String>,
    pub low_pc: u64,
    pub high_pc: u64,
    pub source_file: Option<String>,
    pub line_number: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeKind {
    Integer { signed: bool },
    Float,
    Pointer,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableInfo {
    pub name: String,
    pub name_raw: Option<String>,
    /// The short DW_AT_name (e.g. "G_TEMPO") — useful for lookup by user-facing name.
    pub short_name: Option<String>,
    pub address: u64,
    pub byte_size: u8,
    pub type_name: Option<String>,
    pub type_kind: TypeKind,
    pub source_file: Option<String>,
}

/// Recipe for reading a watched value at runtime.
/// Simple global: deref_chain is empty, read directly at base_address.
/// ptr->member: deref_chain = [member_offset], read pointer at base, add offset, read final value.
/// ptr->ptr->member: deref_chain = [first_offset, second_offset], etc.
#[derive(Debug, Clone)]
pub struct WatchRecipe {
    pub label: String,
    pub base_address: u64,
    pub deref_chain: Vec<u64>,
    pub final_size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
}

/// A local variable or parameter in a function, with its DWARF location.
#[derive(Debug, Clone)]
pub struct LocalVariableInfo {
    pub name: String,
    pub byte_size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
    /// Location: either a simple expression or a location list
    pub location: LocalVarLocation,
}

#[derive(Debug, Clone)]
pub enum LocalVarLocation {
    /// Frame-base relative: value is at [frame_base + offset]
    FrameBaseRelative(i64),
    /// In a register
    Register(u16),
    /// Register + offset: value is at [register_value + offset]
    RegisterOffset(u16, i64),
    /// Fixed address
    Address(u64),
    /// Complex expression we can't evaluate
    Complex,
}

impl FunctionInfo {
    pub fn contains_address(&self, addr: u64) -> bool {
        addr >= self.low_pc && addr < self.high_pc
    }

    pub fn is_user_code(&self, project_root: &str) -> bool {
        self.source_file
            .as_ref()
            .map(|f| f.starts_with(project_root))
            .unwrap_or(false)
    }
}
