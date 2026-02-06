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
