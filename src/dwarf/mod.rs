mod parser;
mod function;
mod handle;

pub use parser::DwarfParser;
pub use function::{FunctionInfo, VariableInfo, TypeKind, WatchRecipe};
pub use handle::DwarfHandle;

// Re-export PatternMatcher for integration tests
pub use parser::PatternMatcher;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_parser_no_debug_info() {
        // A binary without debug info should return an error
        let result = DwarfParser::parse(Path::new("/bin/ls"));
        // Note: /bin/ls typically has no debug info
        // This may need adjustment based on system
        assert!(result.is_err() || result.unwrap().functions.is_empty());
    }

    #[test]
    fn test_function_info() {
        let func = FunctionInfo {
            name: "main::process".to_string(),
            name_raw: Some("_ZN4main7processEv".to_string()),
            low_pc: 0x1000,
            high_pc: 0x1100,
            source_file: Some("/home/user/src/main.rs".to_string()),
            line_number: Some(42),
        };

        assert!(func.contains_address(0x1050));
        assert!(!func.contains_address(0x2000));
    }

    #[test]
    fn test_variable_info_basics() {
        let var = VariableInfo {
            name: "gCounter".to_string(),
            name_raw: Some("_ZN7gCounter".to_string()),
            address: 0x1000,
            byte_size: 4,
            type_name: Some("uint32_t".to_string()),
            type_kind: TypeKind::Integer { signed: false },
            source_file: Some("/src/main.cpp".to_string()),
        };
        assert_eq!(var.byte_size, 4);
        assert!(matches!(var.type_kind, TypeKind::Integer { signed: false }));
    }

    #[test]
    fn test_watch_recipe_simple_global() {
        let recipe = WatchRecipe {
            label: "gCounter".to_string(),
            base_address: 0x1000,
            deref_chain: vec![],
            final_size: 4,
            type_kind: TypeKind::Integer { signed: false },
            type_name: Some("uint32_t".to_string()),
        };
        assert!(recipe.deref_chain.is_empty());
        assert_eq!(recipe.final_size, 4);
    }

    #[test]
    fn test_watch_recipe_ptr_member() {
        let recipe = WatchRecipe {
            label: "gClock->counter".to_string(),
            base_address: 0x2000,
            deref_chain: vec![0x10],
            final_size: 8,
            type_kind: TypeKind::Integer { signed: true },
            type_name: Some("int64_t".to_string()),
        };
        assert_eq!(recipe.deref_chain.len(), 1);
        assert_eq!(recipe.deref_chain[0], 0x10);
    }

    #[test]
    fn test_user_code_detection() {
        let func = FunctionInfo {
            name: "myapp::handler".to_string(),
            name_raw: None,
            low_pc: 0x1000,
            high_pc: 0x1100,
            source_file: Some("/home/user/myproject/src/handler.rs".to_string()),
            line_number: Some(10),
        };

        assert!(func.is_user_code("/home/user/myproject"));
        assert!(!func.is_user_code("/home/user/otherproject"));
    }
}
