mod parser;
mod function;
mod handle;

pub use parser::DwarfParser;
pub use function::{FunctionInfo, VariableInfo, TypeKind, WatchRecipe, LocalVariableInfo, LocalVarLocation, StructFieldRecipe};
pub use handle::DwarfHandle;

// Re-export PatternMatcher for integration tests
pub use parser::PatternMatcher;

/// Resolve local variable values using crash context.
/// `registers`: JSON object mapping register name -> hex address string
/// `frame_memory`: hex-encoded bytes read from [fp-512..fp+128]
/// `frame_base`: hex address of the frame pointer
pub fn resolve_crash_locals(
    locals: &[LocalVariableInfo],
    registers: &serde_json::Value,
    frame_memory: Option<&str>,
    frame_base: Option<&str>,
    arch: &str,
) -> Vec<serde_json::Value> {
    let fp_addr = frame_base
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0);

    let frame_bytes = frame_memory
        .map(|hex| hex_to_bytes(hex))
        .unwrap_or_default();

    // Frame memory starts at fp - 512
    let frame_start = fp_addr.saturating_sub(512);

    locals.iter().filter_map(|local| {
        let value = match &local.location {
            LocalVarLocation::FrameBaseRelative(offset) => {
                let addr = (fp_addr as i64 + offset) as u64;
                read_from_frame(&frame_bytes, frame_start, addr, local.byte_size)
            }
            LocalVarLocation::Register(reg_num) => {
                let reg_name = register_name(*reg_num, arch);
                registers.get(&reg_name)
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                    .map(|v| format_value(v, local.byte_size, &local.type_kind))
            }
            LocalVarLocation::RegisterOffset(reg_num, offset) => {
                let reg_name = register_name(*reg_num, arch);
                let base = registers.get(&reg_name)
                    .and_then(|v| v.as_str())
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())?;
                let addr = (base as i64 + offset) as u64;
                read_from_frame(&frame_bytes, frame_start, addr, local.byte_size)
            }
            LocalVarLocation::Address(_) => {
                // Fixed address — can't read without agent help
                None
            }
            LocalVarLocation::Complex => None,
        };

        value.map(|v| serde_json::json!({
            "name": local.name,
            "value": v,
            "type": local.type_name,
        }))
    }).collect()
}

fn read_from_frame(frame_bytes: &[u8], frame_start: u64, addr: u64, size: u8) -> Option<String> {
    if addr < frame_start || size == 0 {
        return None;
    }
    let offset = (addr - frame_start) as usize;
    if offset + size as usize > frame_bytes.len() {
        return None;
    }
    let bytes = &frame_bytes[offset..offset + size as usize];
    let mut val = 0u64;
    for (i, &b) in bytes.iter().enumerate() {
        val |= (b as u64) << (i * 8);
    }
    Some(format!("0x{:x}", val))
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .filter_map(|i| {
            if i + 2 <= hex.len() {
                u8::from_str_radix(&hex[i..i+2], 16).ok()
            } else {
                None
            }
        })
        .collect()
}

fn register_name(dwarf_reg: u16, arch: &str) -> String {
    match arch {
        "arm64" => match dwarf_reg {
            0..=28 => format!("x{}", dwarf_reg),
            29 => "fp".to_string(),
            30 => "lr".to_string(),
            31 => "sp".to_string(),
            _ => format!("reg{}", dwarf_reg),
        },
        "x64" => match dwarf_reg {
            0 => "rax".to_string(),
            1 => "rdx".to_string(),
            2 => "rcx".to_string(),
            3 => "rbx".to_string(),
            4 => "rsi".to_string(),
            5 => "rdi".to_string(),
            6 => "rbp".to_string(),
            7 => "rsp".to_string(),
            8..=15 => format!("r{}", dwarf_reg),
            16 => "rip".to_string(),
            _ => format!("reg{}", dwarf_reg),
        },
        _ => format!("reg{}", dwarf_reg),
    }
}

fn format_value(raw: u64, size: u8, type_kind: &TypeKind) -> String {
    match type_kind {
        TypeKind::Integer { signed: true } => {
            match size {
                1 => format!("{}", raw as i8),
                2 => format!("{}", raw as i16),
                4 => format!("{}", raw as i32),
                8 => format!("{}", raw as i64),
                _ => format!("0x{:x}", raw),
            }
        }
        TypeKind::Integer { signed: false } => format!("{}", raw),
        TypeKind::Float => {
            match size {
                4 => format!("{}", f32::from_bits(raw as u32)),
                8 => format!("{}", f64::from_bits(raw)),
                _ => format!("0x{:x}", raw),
            }
        }
        TypeKind::Pointer => format!("0x{:x}", raw),
        TypeKind::Unknown => format!("0x{:x}", raw),
    }
}

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
            short_name: Some("gCounter".to_string()),
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

    /// Real integration test: parse the stress_tester binary's dSYM and verify
    /// Rust functions are found, demangled, and matchable.
    #[test]
    fn test_parse_rust_binary_stress_tester() {
        let binary_path = Path::new(
            env!("CARGO_MANIFEST_DIR")
        ).join("tests/stress_test_phase1b/target/debug/stress_tester");

        if !binary_path.exists() {
            eprintln!("Skipping test: stress_tester binary not found at {:?}", binary_path);
            return;
        }

        let dsym_path = binary_path.with_extension("dSYM");
        if !dsym_path.exists() {
            eprintln!("Skipping test: no .dSYM for stress_tester (run dsymutil first)");
            return;
        }

        let parser = DwarfParser::parse(&binary_path)
            .expect("DwarfParser::parse should succeed for stress_tester with dSYM");

        // Should have found functions
        eprintln!("Parsed {} functions, {} variables", parser.functions.len(), parser.variables.len());
        assert!(parser.functions.len() > 0, "Should find at least some functions");

        // Check that we find key Rust functions by demangled name
        let note_on_fns = parser.find_by_pattern("**::process_note_on**");
        eprintln!("process_note_on matches: {:?}", note_on_fns.iter().map(|f| &f.name).collect::<Vec<_>>());
        assert!(!note_on_fns.is_empty(), "Should find process_note_on");

        let audio_fns = parser.find_by_pattern("**::process_audio_buffer**");
        eprintln!("process_audio_buffer matches: {:?}", audio_fns.iter().map(|f| &f.name).collect::<Vec<_>>());
        assert!(!audio_fns.is_empty(), "Should find process_audio_buffer");

        // Check @file: pattern works
        let main_fns = parser.find_by_source_file("main.rs");
        eprintln!("@file:main.rs matches: {} functions", main_fns.len());
        assert!(!main_fns.is_empty(), "Should find functions in main.rs");

        // Print first few function names for debugging
        for func in parser.functions.iter().take(20) {
            eprintln!("  func: {} (raw: {:?}, file: {:?})", func.name, func.name_raw, func.source_file);
        }
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
