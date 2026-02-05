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
