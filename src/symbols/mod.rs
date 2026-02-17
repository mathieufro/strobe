mod demangle;
pub mod resolver;
pub mod dwarf_resolver;
pub mod python_resolver;
pub mod js_resolver;

pub use demangle::demangle_symbol;
pub use resolver::{Language, ResolvedTarget, VariableResolution, SymbolResolver};
pub use dwarf_resolver::DwarfResolver;
pub use python_resolver::PythonResolver;
pub use js_resolver::JsResolver;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_demangle_rust_symbol() {
        let mangled = "_ZN4test7example17h1234567890abcdefE";
        let demangled = demangle_symbol(mangled);
        assert!(demangled.contains("test::example"));
    }

    #[test]
    fn test_demangle_cpp_symbol() {
        let mangled = "_ZN4test7exampleEv";
        let demangled = demangle_symbol(mangled);
        assert!(demangled.contains("test::example"));
    }

    #[test]
    fn test_demangle_c_symbol() {
        // C symbols have no mangling
        let symbol = "main";
        let demangled = demangle_symbol(symbol);
        assert_eq!(demangled, "main");
    }

    #[test]
    fn test_demangle_unknown() {
        // Unknown format returns as-is
        let symbol = "some_random_symbol";
        let demangled = demangle_symbol(symbol);
        assert_eq!(demangled, "some_random_symbol");
    }

    #[test]
    fn test_demangle_real_rust_symbols() {
        let cases: Vec<(&str, &str)> = vec![
            (
                "_ZN13stress_tester4midi15process_note_on17h7c4d62da364e13f0E",
                "stress_tester::midi::process_note_on",
            ),
            (
                "_ZN13stress_tester5audio20process_audio_buffer17h1e1f7984b2d2cfcaE",
                "stress_tester::audio::process_audio_buffer",
            ),
        ];

        for (mangled, expected_prefix) in cases {
            let demangled = demangle_symbol(mangled);
            assert!(demangled.contains(expected_prefix),
                "Demangling '{}' should contain '{}', got '{}'", mangled, expected_prefix, demangled);
            assert!(!demangled.starts_with("_ZN"),
                "Demangled should not start with _ZN");
        }
    }
}
