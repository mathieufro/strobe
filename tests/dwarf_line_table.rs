use std::path::PathBuf;
use strobe::dwarf::DwarfParser;

#[test]
fn test_line_table_resolution() {
    // Use the C++ test fixture (has DWARF debug info)
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cpp/build/strobe_test_target");

    if !binary.exists() {
        // Skip test if fixture not built
        eprintln!("Skipping: fixture not built. Run: cd tests/fixtures/cpp && make");
        return;
    }

    let parser = DwarfParser::parse(&binary).unwrap();

    // Test 1: Resolve file:line → address
    let result = parser.resolve_line("main.cpp", 10);
    assert!(result.is_some(), "Should find code at main.cpp:10");
    let (address, actual_line) = result.unwrap();
    assert!(address > 0, "Address should be non-zero");
    assert!(actual_line >= 10, "Actual line should be >= requested line");

    // Test 2: Reverse lookup address → file:line
    let result = parser.resolve_address(address);
    assert!(result.is_some(), "Should find line for address");
    let (file, line, _col) = result.unwrap();
    assert!(file.contains("main.cpp"));
    assert_eq!(line, actual_line);

    // Test 3: Find next line in same function
    let result = parser.next_line_in_function(address, 0);
    assert!(result.is_some(), "Should find next line");
    let (next_addr, _next_file, next_line) = result.unwrap();
    assert!(next_addr > address, "Next address should be after current");
    assert!(next_line > actual_line, "Next line should be after current");
}

#[test]
fn test_line_table_errors() {
    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/cpp/build/strobe_test_target");

    if !binary.exists() {
        return;
    }

    let parser = DwarfParser::parse(&binary).unwrap();

    // Line 1 has no code, but resolve_line snaps forward to nearest executable line
    let result = parser.resolve_line("main.cpp", 1);
    if let Some((_addr, actual_line)) = result {
        assert!(actual_line > 1, "Should snap forward past #include directives");
    }

    // Non-existent file
    let result = parser.resolve_line("does_not_exist.cpp", 10);
    assert!(result.is_none());
}
