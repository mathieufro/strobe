# Contextual Watches Implementation Plan

**Status:** ✅ Complete (including `on` field contextual filtering - see [../features/2026-02-06-contextual-watch-filtering.md](../features/2026-02-06-contextual-watch-filtering.md))
**Spec:** Inline (see MCP API section below)
**Goal:** Read global/static variable values at the exact moment traced functions fire, integrated into `debug_trace`.
**Architecture:** Extend CModule ring buffer (48→80 bytes/entry) with 4 watch slots read in native `onEnter`. DWARF parser gains variable+type+struct parsing to resolve names like `gClock->counter`. Per-pattern watch filtering at JS drain time. JS expressions as escape hatch.
**Tech Stack:** gimli (DWARF), Frida CModule (TinyCC), TypeScript agent, SQLite
**Commit strategy:** Single at end

## MCP API

```
debug_trace({
  sessionId: "...",
  add: ["NoteOn", "ClockTick"],
  watches: {
    add: [
      { variable: "gClock->counter" },                         // DWARF ptr->member
      { variable: "gSequencer.stepIdx", on: ["NoteOn"] },      // per-pattern only
      { address: "0x1234", type: "f64", label: "tempo" },      // raw address
      { expr: "ptr(0x5678).readPointer().add(0x10).readU32()", label: "custom" }
    ],
    remove: ["tempo"]
  }
})
```

Events gain `watchValues`:
```
{ function: "NoteOn", timestampNs: 12345, watchValues: { "gClock->counter": 48291 } }
```

## Workstreams

- **Stream A (DWARF parsing):** Tasks 1, 2, 3 — Rust, independent of agent
- **Stream B (Agent/CModule):** Tasks 4, 5, 6 — TypeScript, independent of DWARF
- **Serial (integration):** Tasks 7, 8, 9, 10 — depend on both A and B

---

### Task 1: VariableInfo and TypeKind structs

**Files:**
- Modify: `src/dwarf/function.rs`
- Modify: `src/dwarf/mod.rs`
- Test: `src/dwarf/mod.rs` (inline tests)

**Step 1: Write the failing test**

In `src/dwarf/mod.rs`, add to the `tests` module:

```rust
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
    assert!(recipe.deref_chain.is_empty()); // direct read
    assert_eq!(recipe.final_size, 4);
}

#[test]
fn test_watch_recipe_ptr_member() {
    let recipe = WatchRecipe {
        label: "gClock->counter".to_string(),
        base_address: 0x2000,
        deref_chain: vec![0x10], // deref pointer at 0x2000, add offset 0x10
        final_size: 8,
        type_kind: TypeKind::Integer { signed: true },
        type_name: Some("int64_t".to_string()),
    };
    assert_eq!(recipe.deref_chain.len(), 1);
    assert_eq!(recipe.deref_chain[0], 0x10);
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test test_variable_info_basics test_watch_recipe -- --nocapture`
Expected: FAIL — `VariableInfo`, `TypeKind`, `WatchRecipe` don't exist

**Step 3: Write minimal implementation**

In `src/dwarf/function.rs`, add after `FunctionInfo`:

```rust
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeKind {
    Integer { signed: bool },
    Float,
    Pointer,
    Unknown,
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
```

In `src/dwarf/mod.rs`, add exports:

```rust
pub use function::{VariableInfo, TypeKind, WatchRecipe};
```

And add `use super::*;` to tests if not already present, plus the new import for the test.

**Step 4: Run test — verify it passes**
Run: `cargo test test_variable_info_basics test_watch_recipe -- --nocapture`
Expected: PASS

**Checkpoint:** `VariableInfo`, `TypeKind`, `WatchRecipe` types exist and compile.

---

### Task 2: DWARF variable parsing — globals and type chains

**Files:**
- Modify: `src/dwarf/parser.rs` (main parser loop + new helper functions)
- Test: `tests/integration.rs` (new test with compiled C binary)

**Step 1: Write the failing test**

In `tests/integration.rs`, add:

```rust
#[cfg(target_os = "macos")]
fn create_c_test_binary_with_globals(dir: &std::path::Path) -> PathBuf {
    let src = r#"
#include <stdint.h>

uint32_t gCounter = 42;
int64_t gSignedVal = -100;
double gTempo = 120.5;
static float sLocalFloat = 3.14f;

typedef struct {
    int32_t x;
    int32_t y;
    double value;
} Point;

Point gPoint = { 10, 20, 99.9 };
Point *gPointPtr = &gPoint;

int main(void) {
    gCounter++;
    return 0;
}
"#;
    let src_path = dir.join("test_globals.c");
    std::fs::write(&src_path, src).unwrap();
    let out_path = dir.join("test_globals");

    let status = std::process::Command::new("cc")
        .args(["-g", "-O0", "-o"])
        .arg(&out_path)
        .arg(&src_path)
        .status()
        .expect("Failed to compile C test binary");
    assert!(status.success(), "C test binary compilation failed");
    out_path
}

#[test]
#[cfg(target_os = "macos")]
fn test_dwarf_global_variable_parsing() {
    let dir = tempdir().unwrap();
    let binary = create_c_test_binary_with_globals(dir.path());
    let parser = strobe::dwarf::DwarfParser::parse(&binary).unwrap();

    // Should find global variables
    assert!(!parser.variables.is_empty(), "Should find global variables");

    // Find specific globals by name
    let counter = parser.find_variable_by_name("gCounter");
    assert!(counter.is_some(), "Should find gCounter");
    let counter = counter.unwrap();
    assert_eq!(counter.byte_size, 4);
    assert!(matches!(counter.type_kind, strobe::dwarf::TypeKind::Integer { signed: false }));

    let signed_val = parser.find_variable_by_name("gSignedVal");
    assert!(signed_val.is_some(), "Should find gSignedVal");
    assert_eq!(signed_val.unwrap().byte_size, 8);

    let tempo = parser.find_variable_by_name("gTempo");
    assert!(tempo.is_some(), "Should find gTempo");
    let tempo = tempo.unwrap();
    assert_eq!(tempo.byte_size, 8);
    assert!(matches!(tempo.type_kind, strobe::dwarf::TypeKind::Float));

    // Verify address is non-zero (will be a static address)
    assert!(counter.address > 0, "Variable should have a valid static address");
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test test_dwarf_global_variable_parsing -- --nocapture`
Expected: FAIL — `parser.variables` doesn't exist, `find_variable_by_name` doesn't exist

**Step 3: Write implementation**

In `src/dwarf/parser.rs`:

1. Add to `DwarfParser` struct:
```rust
pub struct DwarfParser {
    pub functions: Vec<FunctionInfo>,
    pub(crate) functions_by_name: HashMap<String, Vec<usize>>,
    pub variables: Vec<VariableInfo>,
    pub(crate) variables_by_name: HashMap<String, Vec<usize>>,
    pub image_base: u64,
}
```

2. In `parse_file`, extend the iteration loop to track scope and parse variables:

```rust
fn parse_file(path: &Path) -> Result<Self> {
    // ... existing setup through line 119 ...

    let mut functions = Vec::new();
    let mut variables = Vec::new();

    let mut units = dwarf.units();
    while let Ok(Some(header)) = units.next() {
        let unit = dwarf.unit(header)
            .map_err(|e| Error::Frida(format!("Failed to parse unit: {}", e)))?;

        let mut entries = unit.entries();
        let mut in_subprogram = false;
        let mut subprogram_depth: isize = 0;
        let mut current_depth: isize = 0;

        while let Ok(Some((delta, entry))) = entries.next_dfs() {
            current_depth += delta;

            // Track whether we're inside a function (to skip local variables)
            if in_subprogram && current_depth <= subprogram_depth {
                in_subprogram = false;
            }

            match entry.tag() {
                gimli::DW_TAG_subprogram => {
                    in_subprogram = true;
                    subprogram_depth = current_depth;
                    if let Some(func) = Self::parse_function(&dwarf, &unit, entry)? {
                        functions.push(func);
                    }
                }
                gimli::DW_TAG_variable if !in_subprogram => {
                    if let Some(var) = Self::parse_variable(&dwarf, &unit, entry)? {
                        variables.push(var);
                    }
                }
                _ => {}
            }
        }
    }

    // Build indexes
    let mut functions_by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, func) in functions.iter().enumerate() {
        functions_by_name.entry(func.name.clone()).or_default().push(idx);
    }

    let mut variables_by_name: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, var) in variables.iter().enumerate() {
        variables_by_name.entry(var.name.clone()).or_default().push(idx);
    }

    Ok(Self {
        functions,
        functions_by_name,
        variables,
        variables_by_name,
        image_base: 0,
    })
}
```

3. Add `parse_variable` method:

```rust
fn parse_variable<R: gimli::Reader>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    entry: &gimli::DebuggingInformationEntry<R>,
) -> Result<Option<VariableInfo>> {
    // Get name (same logic as functions)
    let linkage_name = entry.attr_value(gimli::DW_AT_linkage_name).ok().flatten()
        .and_then(|v| dwarf.attr_string(unit, v).ok())
        .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));

    let short_name = entry.attr_value(gimli::DW_AT_name).ok().flatten()
        .and_then(|v| dwarf.attr_string(unit, v).ok())
        .and_then(|s| s.to_string_lossy().ok().map(|c| c.to_string()));

    let name = match linkage_name.or(short_name) {
        Some(n) => n,
        None => return Ok(None),
    };

    // Get location — only accept simple DW_OP_addr (fixed address globals)
    let address = match Self::parse_variable_address(unit, entry) {
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

    // Get source file (same logic as functions)
    let source_file = Self::parse_source_file(dwarf, unit, entry);

    // Demangle
    let demangled = demangle_symbol(&name);
    let name_raw = if name != demangled { Some(name) } else { None };

    Ok(Some(VariableInfo {
        name: demangled,
        name_raw,
        address,
        byte_size,
        type_name,
        type_kind,
        source_file,
    }))
}

fn parse_variable_address<R: gimli::Reader>(
    unit: &gimli::Unit<R>,
    entry: &gimli::DebuggingInformationEntry<R>,
) -> Option<u64> {
    let loc_attr = entry.attr_value(gimli::DW_AT_location).ok()??;
    match loc_attr {
        gimli::AttributeValue::Exprloc(expr) => {
            let mut ops = expr.operations(unit.encoding());
            match ops.next().ok()? {
                Some(gimli::Operation::Address { address }) => Some(address),
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
        _ => None,
    }
}

fn parse_source_file<R: gimli::Reader>(
    dwarf: &gimli::Dwarf<R>,
    unit: &gimli::Unit<R>,
    entry: &gimli::DebuggingInformationEntry<R>,
) -> Option<String> {
    // Extract into shared helper — same logic as in parse_function
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
```

4. Add query methods:

```rust
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
```

5. Update `DwarfHandle` test helper `make_parser()` in `src/dwarf/handle.rs`:

```rust
fn make_parser() -> Arc<DwarfParser> {
    Arc::new(DwarfParser {
        functions: vec![],
        functions_by_name: std::collections::HashMap::new(),
        variables: vec![],
        variables_by_name: std::collections::HashMap::new(),
        image_base: 0x100000,
    })
}
```

**Step 4: Run test — verify it passes**
Run: `cargo test test_dwarf_global_variable_parsing -- --nocapture`
Expected: PASS — finds gCounter (u32), gSignedVal (i64), gTempo (f64)

**Checkpoint:** DWARF parser extracts global variables with correct addresses, sizes, and types.

---

### Task 3: Struct layout parsing + watch expression resolution (`ptr->member`)

**Files:**
- Modify: `src/dwarf/parser.rs`
- Test: `tests/integration.rs`

**Step 1: Write the failing test**

```rust
#[test]
#[cfg(target_os = "macos")]
fn test_dwarf_watch_expression_ptr_member() {
    let dir = tempdir().unwrap();
    let binary = create_c_test_binary_with_globals(dir.path());
    let parser = strobe::dwarf::DwarfParser::parse(&binary).unwrap();

    // "gPointPtr->x" should resolve to: deref gPointPtr, add offset of x, read i32
    let recipe = parser.resolve_watch_expression("gPointPtr->x");
    assert!(recipe.is_ok(), "Should resolve gPointPtr->x: {:?}", recipe);
    let recipe = recipe.unwrap();
    assert_eq!(recipe.label, "gPointPtr->x");
    assert_eq!(recipe.deref_chain.len(), 1); // one dereference
    assert_eq!(recipe.deref_chain[0], 0);    // x is at offset 0 in Point
    assert_eq!(recipe.final_size, 4);        // int32_t = 4 bytes

    // "gPointPtr->value" — double at offset in struct
    let recipe2 = parser.resolve_watch_expression("gPointPtr->value");
    assert!(recipe2.is_ok(), "Should resolve gPointPtr->value");
    let recipe2 = recipe2.unwrap();
    assert_eq!(recipe2.final_size, 8);       // double
    assert!(matches!(recipe2.type_kind, strobe::dwarf::TypeKind::Float));

    // Simple global (no ->) should also work
    let recipe3 = parser.resolve_watch_expression("gCounter");
    assert!(recipe3.is_ok());
    let recipe3 = recipe3.unwrap();
    assert!(recipe3.deref_chain.is_empty()); // direct read, no deref
}
```

**Step 2: Run test — verify it fails**
Run: `cargo test test_dwarf_watch_expression_ptr_member -- --nocapture`
Expected: FAIL — `resolve_watch_expression` doesn't exist

**Step 3: Write implementation**

In `src/dwarf/parser.rs`, add struct layout tracking and resolution:

```rust
use super::{VariableInfo, TypeKind, WatchRecipe};

// Add to DwarfParser (internal, not pub):
// Store struct layouts indexed by type DIE offset within their compilation unit.
// Key: (unit index, offset) — but since we process one unit at a time,
// we store per-CU during parsing and merge into a flat map keyed by (unit_header_offset, type_offset).

// Actually simpler: store full member info inline per pointer variable.
// During parse_variable, if the type is a pointer to a struct, record the struct layout.
```

The core addition is `resolve_watch_expression`:

```rust
pub fn resolve_watch_expression(&self, expr: &str) -> crate::Result<WatchRecipe> {
    if !expr.contains("->") {
        // Simple variable — direct read
        let var = self.find_variable_by_name(expr)
            .ok_or_else(|| crate::Error::Frida(format!("Variable '{}' not found", expr)))?;
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
        .ok_or_else(|| crate::Error::Frida(format!("Variable '{}' not found", root_name)))?;

    // Root must be a pointer
    if !matches!(var.type_kind, TypeKind::Pointer) {
        return Err(crate::Error::Frida(format!(
            "'{}' is not a pointer type (is {:?}), cannot use -> syntax",
            root_name, var.type_kind
        )));
    }

    // Resolve member chain through struct_members map
    // This requires the DWARF parser to have stored struct member info
    // during parsing. See struct_members field added to DwarfParser.
    self.resolve_member_chain(var, &parts[1..], expr)
}
```

This requires storing struct layout info during DWARF parsing. Add to `DwarfParser`:

```rust
/// Struct member layouts: maps (pointer variable name) → pointed-to struct members
/// Populated during parse for pointer-type global variables
pub(crate) struct_members: HashMap<String, Vec<StructMember>>,
```

```rust
#[derive(Debug, Clone)]
pub(crate) struct StructMember {
    pub name: String,
    pub offset: u64,
    pub byte_size: u8,
    pub type_kind: TypeKind,
    pub type_name: Option<String>,
    pub is_pointer: bool,
    pub pointed_struct_members: Option<Vec<StructMember>>, // for nested ptr->ptr->member
}
```

During `parse_variable`, when a variable is a pointer type, follow the pointed-to type to find struct members:

```rust
// In parse_variable, after determining type_kind is Pointer:
if matches!(type_kind, TypeKind::Pointer) {
    // Try to parse struct members of the pointed-to type
    if let Some(members) = Self::parse_pointed_struct_members(dwarf, unit, entry) {
        struct_members_map.insert(demangled.clone(), members);
    }
}
```

`parse_pointed_struct_members` follows the type chain: pointer → struct/class → iterate DW_TAG_member children.

Then `resolve_member_chain` walks the deref chain:

```rust
fn resolve_member_chain(
    &self,
    root_var: &VariableInfo,
    member_path: &[&str],
    full_expr: &str,
) -> crate::Result<WatchRecipe> {
    let mut deref_chain = Vec::new();
    let mut current_members = self.struct_members.get(&root_var.name)
        .ok_or_else(|| crate::Error::Frida(format!(
            "No struct info for pointer '{}'", root_var.name
        )))?;

    let mut final_size = 0u8;
    let mut final_type_kind = TypeKind::Unknown;
    let mut final_type_name = None;

    for (i, &member_name) in member_path.iter().enumerate() {
        let member = current_members.iter()
            .find(|m| m.name == member_name)
            .ok_or_else(|| crate::Error::Frida(format!(
                "Member '{}' not found in struct", member_name
            )))?;

        deref_chain.push(member.offset);
        final_size = member.byte_size;
        final_type_kind = member.type_kind.clone();
        final_type_name = member.type_name.clone();

        // If this member is itself a pointer and there are more parts, continue
        if member.is_pointer && i + 1 < member_path.len() {
            current_members = member.pointed_struct_members.as_ref()
                .ok_or_else(|| crate::Error::Frida(format!(
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
```

**Step 4: Run test — verify it passes**
Run: `cargo test test_dwarf_watch_expression_ptr_member -- --nocapture`
Expected: PASS

**Checkpoint:** `resolve_watch_expression("gPointPtr->x")` returns a WatchRecipe with correct deref chain and size.

---

### Task 4: CModule ring buffer extension + watch reads

**Files:**
- Modify: `agent/src/cmodule-tracer.ts`

**No automated test** — CModule code runs inside Frida target process. Verified via end-to-end test in Task 10.

**Step 1: Update constants**

```typescript
const ENTRY_SIZE = 80;      // was 48
const HEADER_SIZE = 128;    // was 32
```

**Step 2: Extend header layout**

Add after `globalCounterPtr` initialization:

```typescript
// Watch table in header (offsets 24-103)
this.watchCountPtr      = this.ringBuffer.add(24);
this.watchAddrsPtr      = this.ringBuffer.add(32);   // 4 × 8 bytes
this.watchSizesPtr      = this.ringBuffer.add(64);   // 4 × 1 byte
this.watchDerefDepthsPtr = this.ringBuffer.add(68);  // 4 × 1 byte
this.watchDerefOffsetsPtr = this.ringBuffer.add(72); // 4 × 8 bytes = 32 bytes

// Initialize watch_count to 0
this.watchCountPtr.writeU32(0);
```

**Step 3: Extend CModule C source**

Add extern symbols and extend `write_entry`:

```c
extern volatile gint watch_count;
extern guint64 watch_addrs[4];
extern guint8 watch_sizes[4];
extern guint8 watch_deref_depths[4];
extern guint64 watch_deref_offsets[4];

#define ENTRY_SIZE 80

// In TraceEntry, add after _pad:
//   guint64 watch0, watch1, watch2, watch3; (offsets 48-79)

// In write_entry, after existing fields:
guint32 wc = (guint32)g_atomic_int_add(&watch_count, 0);
e->watch_count = (guint8)(wc > 4 ? 4 : wc);
guint32 w;
for (w = 0; w < e->watch_count; w++) {
  guint64 addr = watch_addrs[w];
  guint8 dd = watch_deref_depths[w];
  guint8 sz = watch_sizes[w];
  guint64 val = 0;

  if (addr != 0) {
    if (dd > 0) {
      guint64 ptr_val = *(volatile guint64*)(gpointer)addr;
      if (ptr_val != 0) {
        addr = ptr_val + watch_deref_offsets[w];
      } else {
        addr = 0;
      }
    }
    if (addr != 0) {
      if (sz == 1) val = *(volatile guint8*)(gpointer)addr;
      else if (sz == 2) val = *(volatile guint16*)(gpointer)addr;
      else if (sz == 4) val = *(volatile guint32*)(gpointer)addr;
      else val = *(volatile guint64*)(gpointer)addr;
    }
  }
  *((guint64*)(((guint8*)e) + 48 + w * 8)) = val;
}
for (; w < 4; w++) {
  *((guint64*)(((guint8*)e) + 48 + w * 8)) = 0;
}
```

**Step 4: Pass new symbols to CModule constructor**

```typescript
this.cm = new CModule(CMODULE_SOURCE, {
  mach_absolute_time: machAbsTimePtr,
  write_idx:            this.writeIdxPtr,
  overflow_count:       this.overflowCountPtr,
  sample_interval:      this.sampleIntervalPtr,
  global_counter:       this.globalCounterPtr,
  ring_data:            this.ringDataPtrHolder,
  watch_count:          this.watchCountPtr,
  watch_addrs:          this.watchAddrsPtr,
  watch_sizes:          this.watchSizesPtr,
  watch_deref_depths:   this.watchDerefDepthsPtr,
  watch_deref_offsets:  this.watchDerefOffsetsPtr,
});
```

**Checkpoint:** Agent builds (`npm run build`), ring buffer entries now 80 bytes with watch slots.

---

### Task 5: updateWatches API + drain extension + value formatting

**Files:**
- Modify: `agent/src/cmodule-tracer.ts`
- Modify: `agent/src/hooks.ts`

**Step 1: Add WatchConfig type and state to CModuleTracer**

```typescript
interface WatchConfig {
  label: string;
  size: number;
  typeKind: 'int' | 'uint' | 'float' | 'pointer';
  isGlobal: boolean;
  onFuncIds: Set<number>;
}

// Add to class:
private watchConfigs: (WatchConfig | null)[] = [null, null, null, null];
private exprWatches: Array<{
  label: string;
  expr: string;
  compiledFn: () => any;
  isGlobal: boolean;
  onFuncIds: Set<number>;
}> = [];
```

**Step 2: Implement updateWatches()**

```typescript
updateWatches(watches: Array<{
  address: string; size: number; label: string;
  derefDepth: number; derefOffset: number;
  typeKind: string; isGlobal: boolean; onFuncIds: number[];
}>): void {
  if (watches.length > 4) throw new Error('Max 4 CModule watches');

  // Atomic disable
  this.watchCountPtr.writeU32(0);

  for (let i = 0; i < 4; i++) {
    if (i < watches.length) {
      const w = watches[i];
      const runtimeAddr = ptr(w.address).add(this.aslrSlide);

      // Validate address is readable
      const range = Process.findRangeByAddress(runtimeAddr);
      if (!range || !range.protection.includes('r')) {
        throw new Error(`Watch address ${runtimeAddr} not readable`);
      }

      this.watchAddrsPtr.add(i * 8).writeU64(uint64(runtimeAddr.toString()));
      this.watchSizesPtr.add(i).writeU8(w.size);
      this.watchDerefDepthsPtr.add(i).writeU8(w.derefDepth);
      this.watchDerefOffsetsPtr.add(i * 8).writeU64(uint64(w.derefOffset.toString()));

      this.watchConfigs[i] = {
        label: w.label,
        size: w.size,
        typeKind: w.typeKind as WatchConfig['typeKind'],
        isGlobal: w.isGlobal,
        onFuncIds: new Set(w.onFuncIds),
      };
    } else {
      this.watchAddrsPtr.add(i * 8).writeU64(uint64(0));
      this.watchSizesPtr.add(i).writeU8(0);
      this.watchDerefDepthsPtr.add(i).writeU8(0);
      this.watchDerefOffsetsPtr.add(i * 8).writeU64(uint64(0));
      this.watchConfigs[i] = null;
    }
  }

  // Atomic enable
  this.watchCountPtr.writeU32(watches.length);
}

updateExprWatches(exprs: Array<{
  expr: string; label: string; isGlobal: boolean; onFuncIds: number[];
}>): void {
  this.exprWatches = exprs.map(e => ({
    label: e.label,
    expr: e.expr,
    compiledFn: new Function('return ' + e.expr) as () => any,
    isGlobal: e.isGlobal,
    onFuncIds: new Set(e.onFuncIds),
  }));
}
```

**Step 3: Extend drain to read watch values and format**

In `drain()`, after building the function_enter event (around line 441), add:

```typescript
// Read watch values from ring buffer entry
const entryWatchCount = entryPtr.add(46).readU8();
let watchValues: Record<string, number | string> | undefined;

if (entryWatchCount > 0 || this.exprWatches.length > 0) {
  watchValues = {};

  // CModule watches
  for (let w = 0; w < entryWatchCount && w < 4; w++) {
    const cfg = this.watchConfigs[w];
    if (!cfg) continue;
    if (!cfg.isGlobal && !cfg.onFuncIds.has(funcId)) continue;

    const raw = entryPtr.add(48 + w * 8).readU64();
    watchValues[cfg.label] = this.formatWatchValue(raw, cfg);
  }

  // JS expression watches
  for (const ew of this.exprWatches) {
    if (!ew.isGlobal && !ew.onFuncIds.has(funcId)) continue;
    try { watchValues[ew.label] = ew.compiledFn(); }
    catch { watchValues[ew.label] = '<error>'; }
  }

  if (Object.keys(watchValues).length === 0) watchValues = undefined;
}

// Add to event
if (watchValues) (event as any).watchValues = watchValues;
```

**Step 4: Add formatWatchValue**

```typescript
private formatWatchValue(raw: UInt64, cfg: WatchConfig): number | string {
  if (cfg.typeKind === 'float') {
    const buf = new ArrayBuffer(8);
    const view = new DataView(buf);
    if (cfg.size === 4) {
      view.setUint32(0, raw.toNumber(), true);
      return view.getFloat32(0, true);
    } else {
      view.setBigUint64(0, BigInt(raw.toString()), true);
      return view.getFloat64(0, true);
    }
  }
  if (cfg.typeKind === 'int') {
    const n = raw.toNumber();
    if (cfg.size === 1) return (n << 24) >> 24;
    if (cfg.size === 2) return (n << 16) >> 16;
    if (cfg.size === 4) return n | 0;
    return n;
  }
  return raw.toNumber();
}
```

**Step 5: Extend TraceEvent interface**

```typescript
interface TraceEvent {
  // ... existing fields ...
  watchValues?: Record<string, number | string>;
}
```

**Step 6: Update hooks.ts pass-through**

```typescript
// In HookInstaller class:
updateWatches(watches: Parameters<CModuleTracer['updateWatches']>[0]): void {
  this.tracer.updateWatches(watches);
}

updateExprWatches(exprs: Parameters<CModuleTracer['updateExprWatches']>[0]): void {
  this.tracer.updateExprWatches(exprs);
}
```

**Checkpoint:** Agent compiles with watch support. CModule reads 4 watch addresses per entry, drain formats values with per-pattern filtering.

---

### Task 6: Agent watches message handler

**Files:**
- Modify: `agent/src/agent.ts`

**Step 1: Add WatchInstruction interface and handler**

```typescript
interface WatchInstruction {
  watches: Array<{
    address: string;
    size: number;
    label: string;
    derefDepth: number;
    derefOffset: number;
    typeKind: string;
    isGlobal: boolean;
    onFuncIds: number[];
  }>;
  exprWatches?: Array<{
    expr: string;
    label: string;
    isGlobal: boolean;
    onFuncIds: number[];
  }>;
}
```

**Step 2: Add to StrobeAgent class**

```typescript
handleWatches(message: WatchInstruction): void {
  try {
    this.hookInstaller.updateWatches(message.watches);
    if (message.exprWatches) {
      this.hookInstaller.updateExprWatches(message.exprWatches);
    }
    send({ type: 'watches_updated', activeCount: message.watches.length });
  } catch (e: any) {
    send({ type: 'log', message: `handleWatches error: ${e.message}` });
    send({ type: 'watches_updated', activeCount: 0 });
  }
}
```

**Step 3: Register message handler at bottom of file**

```typescript
function onWatchesMessage(message: WatchInstruction): void {
  recv('watches', onWatchesMessage);
  agent.handleWatches(message);
}
recv('watches', onWatchesMessage);
```

**Step 4: Build agent**

Run: `cd agent && npm run build`

Then: `touch src/frida_collector/spawner.rs`

**Checkpoint:** Agent handles `watches` messages, updates CModule watch table, responds with `watches_updated`.

---

### Task 7: Extend MCP types for watches in debug_trace

**Files:**
- Modify: `src/mcp/types.rs`
- Modify: `src/error.rs`
- Test: `tests/integration.rs`

**Step 1: Write the failing test**

```rust
#[test]
fn test_watch_types_serialization() {
    let target = strobe::mcp::WatchTarget {
        variable: Some("gClock->counter".to_string()),
        address: None,
        type_hint: None,
        label: None,
        expr: None,
        on: Some(vec!["NoteOn".to_string()]),
    };
    let json = serde_json::to_string(&target).unwrap();
    assert!(json.contains("gClock->counter"));
    assert!(json.contains("NoteOn"));

    let update = strobe::mcp::WatchUpdate {
        add: Some(vec![target]),
        remove: Some(vec!["old_watch".to_string()]),
    };
    let json = serde_json::to_string(&update).unwrap();
    assert!(json.contains("gClock->counter"));
    assert!(json.contains("old_watch"));
}
```

**Step 2: Run test — verify fails**

**Step 3: Add types to `src/mcp/types.rs`**

Add `watches` field to `DebugTraceRequest`:
```rust
pub struct DebugTraceRequest {
    pub session_id: Option<String>,
    pub add: Option<Vec<String>>,
    pub remove: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watches: Option<WatchUpdate>,
}
```

Add new types:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchUpdate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<Vec<WatchTarget>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveWatch {
    pub label: String,
    pub address: String,
    pub size: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<Vec<String>>,
}
```

Add `active_watches` to `DebugTraceResponse`:
```rust
pub struct DebugTraceResponse {
    pub active_patterns: Vec<String>,
    pub hooked_functions: u32,
    pub matched_functions: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub active_watches: Vec<ActiveWatch>,
    pub warnings: Vec<String>,
}
```

Add error variant in `src/error.rs`:
```rust
#[error("WATCH_FAILED: {0}")]
WatchFailed(String),
```

And in `src/mcp/types.rs` ErrorCode:
```rust
pub enum ErrorCode {
    // ... existing ...
    WatchFailed,
}
```

Update the `From<crate::Error>` impl to handle `WatchFailed`.

**Step 4: Run test — verify passes**

**Checkpoint:** MCP types compile and serialize correctly with watch fields.

---

### Task 8: Event storage — watch_values column

**Files:**
- Modify: `src/db/schema.rs`
- Modify: `src/db/event.rs`
- Test: `tests/integration.rs`

**Step 1: Write failing test**

```rust
#[test]
fn test_event_with_watch_values() {
    let dir = tempdir().unwrap();
    let db = strobe::db::Database::open(&dir.path().join("test.db")).unwrap();
    db.create_session("s1", "/bin/test", "/home", 1).unwrap();

    let event = strobe::db::Event {
        id: "evt-w1".to_string(),
        session_id: "s1".to_string(),
        timestamp_ns: 5000,
        thread_id: 1,
        parent_event_id: None,
        event_type: strobe::db::EventType::FunctionEnter,
        function_name: "NoteOn".to_string(),
        function_name_raw: None,
        source_file: None,
        line_number: None,
        arguments: None,
        return_value: None,
        duration_ns: None,
        text: None,
        sampled: None,
        watch_values: Some(serde_json::json!({"gClock": 48291, "tempo": 120.5})),
    };
    db.insert_event(event).unwrap();

    let events = db.query_events("s1", |q| q).unwrap();
    assert_eq!(events.len(), 1);
    let wv = events[0].watch_values.as_ref().unwrap();
    assert_eq!(wv["gClock"], 48291);
}
```

**Step 2: Run — fails (watch_values field doesn't exist)**

**Step 3: Implement**

In `src/db/schema.rs`, add migration after creating events table:
```rust
// Add watch_values column (idempotent for existing DBs)
match conn.execute("ALTER TABLE events ADD COLUMN watch_values JSON", []) {
    Ok(_) => {}
    Err(e) if e.to_string().contains("duplicate column") => {}
    Err(e) => return Err(e.into()),
}
```

In `src/db/event.rs`, add field to `Event`:
```rust
pub watch_values: Option<serde_json::Value>,
```

Update `insert_event` and `insert_events_batch` SQL to include `watch_values` as 16th column.

Update `query_events` SELECT to include `watch_values`, read at column index 15.

Update `TraceEventVerbose` to include `watch_values`.

**Step 4: Run — passes**

**Checkpoint:** Events with watch_values round-trip through SQLite.

---

### Task 9: Daemon integration — watch resolution + Frida command

**Files:**
- Modify: `src/daemon/server.rs` (tool_debug_trace, tool schema, query output)
- Modify: `src/daemon/session_manager.rs` (watch state, DWARF access)
- Modify: `src/frida_collector/spawner.rs` (SetWatches command, parse_event)
- Modify: `src/mcp/protocol.rs` (if tool schema is defined there)

**Step 1: Add watch state to SessionManager**

```rust
// In SessionManager struct:
watches: Arc<RwLock<HashMap<String, Vec<ActiveWatchState>>>>,

// New internal type:
#[derive(Clone)]
struct ActiveWatchState {
    label: String,
    address: u64,
    size: u8,
    type_kind_str: String,
    deref_depth: u8,
    deref_offset: u64,
    type_name: Option<String>,
    on_patterns: Option<Vec<String>>,
    is_expr: bool,
    expr: Option<String>,
}
```

**Step 2: Add set_watches + get_dwarf methods to SessionManager**

**Step 3: Add SetWatches command to FridaCommand enum in spawner.rs**

**Step 4: Extend tool_debug_trace in server.rs**

After existing pattern handling, add watch processing:
```rust
// After hook update, handle watches if present
if let Some(ref watch_update) = req.watches {
    // Resolve watch targets...
    // Send to agent...
}
// Include active_watches in response
```

**Step 5: Update debug_trace tool schema to include watches property**

**Step 6: Update parse_event to extract watchValues**

```rust
watch_values: json.get("watchValues").cloned(),
```

**Step 7: Update query output to include watchValues**

In both verbose and non-verbose JSON output in `tool_debug_query`:
```rust
// Add to the json! macro:
"watchValues": e.watch_values,
```

**Checkpoint:** Full pipeline works: debug_trace with watches → DWARF resolution → agent message → CModule reads → events with watchValues → query returns them.

---

### Task 10: Agent rebuild + end-to-end verification

**Files:**
- Build: `agent/` (npm run build)
- Build: root (cargo build)

**Step 1: Rebuild agent**
```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
```

**Step 2: Build daemon**
```bash
cargo build
```

**Step 3: Run all tests**
```bash
cargo test
```

**Step 4: End-to-end test with Strobe MCP**

1. Launch a test binary with a known global variable
2. `debug_trace({ add: ["main"], watches: { add: [{ variable: "gCounter" }] } })`
3. `debug_query({ sessionId: "...", verbose: true })`
4. Verify `watchValues` appears on function_enter events

**Checkpoint:** Feature complete. All tests pass. MCP interface works end-to-end.

---

## Key Design Decisions

- **CModule reads ALL 4 watch slots on every entry** (~20ns). Per-pattern `on` filtering at JS drain time.
- **Pointer dereference in CModule**: `deref_depth` + `deref_offset` per slot with null-pointer guard.
- **JS expressions**: Evaluated at drain time (0-10ms delay), not in CModule. Unlimited count.
- **Float/double formatting**: Reinterpret raw bits as IEEE 754 based on DWARF type encoding.
- **Max 4 CModule watches**: Ring buffer 768KB→1.28MB. JS expr watches don't count.
- **DWARF struct parsing**: Only parse struct layouts for global pointer variables (not all structs in the binary).
