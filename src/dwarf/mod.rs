mod parser;
mod function;

pub use parser::DwarfParser;
pub use function::FunctionInfo;

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
