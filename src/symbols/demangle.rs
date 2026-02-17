use rustc_demangle::demangle as rust_demangle;
use cpp_demangle::Symbol as CppSymbol;

/// Demangle a symbol name from any supported format (Rust, C++, or plain C).
/// Returns the demangled name, or the original if demangling fails.
pub fn demangle_symbol(mangled: &str) -> String {
    // Try Rust demangling first
    let rust_demangled = rust_demangle(mangled).to_string();
    if rust_demangled != mangled {
        return rust_demangled;
    }

    // Try C++ (Itanium ABI) demangling
    if let Ok(symbol) = CppSymbol::new(mangled) {
        if let Ok(demangled) = symbol.demangle(&cpp_demangle::DemangleOptions::default()) {
            return demangled;
        }
    }

    // Return original if no demangling worked (plain C or unknown)
    mangled.to_string()
}
