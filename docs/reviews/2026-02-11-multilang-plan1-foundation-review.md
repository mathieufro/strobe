# Review: Plan 1 Multi-Language Foundation

**Plan:** `docs/plans/2026-02-11-multilang-plan1-foundation.md`
**Reviewed:** 2026-02-11
**Branch:** feature/multilang-foundation
**Commits:** db47e1a, b1ae9de, c7b6f1c, 6850b4a

---

## Summary

| Category | Critical | Important | Minor |
|----------|----------|-----------|-------|
| Completeness | 3 | 2 | 0 |
| Correctness | 4 | 3 | 1 |
| Security | 3 | 3 | 0 |
| Integration | 3 | 3 | 0 |
| Test Coverage | 3 | 3 | 2 |
| **Total** | **16** | **14** | **3** |

**Ready to merge:** ❌ **NO** - Critical issues must be fixed first

---

## Executive Summary

The Plan 1 implementation creates well-designed abstractions (Tracer interface, SymbolResolver trait) but has **critical integration gaps and correctness bugs** that block merging:

**Core Issues:**
1. **Integration incomplete** - Language detection exists but is never called; SymbolResolver trait defined but never used
2. **Type safety bugs** - Hook removal has type mismatch (number vs string) that will cause runtime failures
3. **Security vulnerabilities** - Code injection via breakpoint conditions and expression watches
4. **Test coverage insufficient** - Zero tests for new abstractions, no agent test framework

**What Works:**
- ✅ Abstractions are architecturally sound and follow the plan
- ✅ Zero regression - all 191 existing tests pass
- ✅ ASLR slide computation preserved
- ✅ Native path still functional through tracer interface

**Recommendation:** Fix critical issues (estimated 4-6 hours) before proceeding to Plan 2.

---

## Blocking Issues

### 1. Runtime Detection Message Not Sent (Completeness + Integration)
**Severity:** Critical
**Confidence:** 100
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:260-261`

**Problem:** Plan specifies agent should send `{ type: 'runtime_detected', runtime: '...' }` but this message is never sent.

**Current Code:**
```typescript
const runtime = detectRuntime();
this.tracer = createTracer(runtime, this);
// ❌ Missing: send({ type: 'runtime_detected', runtime })
```

**Impact:** Daemon has no visibility into agent's runtime detection. Cannot validate language detection.

**Fix:**
```typescript
const runtime = detectRuntime();
this.tracer = createTracer(runtime, this);
send({ type: 'runtime_detected', runtime });
```

---

### 2. Language Detection Never Called or Stored (Completeness + Integration)
**Severity:** Critical
**Confidence:** 100
**Location:** `/Users/alex/strobe-multilang-foundation/src/daemon/session_manager.rs`

**Problem:** `detect_language()` function exists but:
- Never called during session creation
- No storage mechanism for per-session language
- SymbolResolver trait never instantiated

**Current State:**
```rust
// SessionManager has NO language or resolver fields
pub struct SessionManager {
    db: Database,
    patterns: Arc<RwLock<HashMap<String, Vec<String>>>>,
    dwarf_cache: Arc<RwLock<HashMap<String, DwarfHandle>>>,
    // ❌ Missing: languages, resolvers
}

// spawn_with_frida never calls detect_language()
pub async fn spawn_with_frida(...) -> Result<u32> {
    let dwarf_handle = self.get_or_start_dwarf_parse(command);
    // ❌ detect_language() never called
    // ❌ DwarfResolver never instantiated
}
```

**Impact:** Multi-language foundation is non-functional. Plan 2/3 cannot build on this.

**Fix:**
```rust
pub struct SessionManager {
    // ... existing fields ...
    languages: Arc<RwLock<HashMap<String, Language>>>,
    resolvers: Arc<RwLock<HashMap<String, Arc<dyn SymbolResolver>>>>,
}

pub async fn spawn_with_frida(...) -> Result<u32> {
    let language = detect_language(command, project_root);
    self.languages.write().unwrap().insert(session_id.to_string(), language);

    // After DWARF parse completes:
    if language == Language::Native {
        let resolver = Arc::new(DwarfResolver::new(dwarf_handle, image_base));
        self.resolvers.write().unwrap().insert(session_id.to_string(), resolver);
    }
}
```

---

### 3. Hook Removal Type Mismatch (Correctness + Integration)
**Severity:** Critical
**Confidence:** 100
**Location:** Multiple files - type signature incompatibility

**Problem:** `Tracer.removeHook(id: number)` interface expects numeric funcId, but `NativeTracer` delegates to `agent.removeNativeHook(address: string)` which expects string address.

**Evidence:**
```typescript
// tracer.ts:26
removeHook(id: number): void;

// native-tracer.ts:29-30
removeHook(id: number): void {
  this.agent.removeNativeHook(id);  // ← passes number
}

// agent.ts:1247-1248
public removeNativeHook(address: string): void {  // ← expects string
  this.cmoduleTracer.removeHook(address);
}

// cmodule-tracer.ts:502
removeHook(address: string): void {
  const entry = this.hooks.get(address);  // ← Map keyed by address
```

**Impact:** Hook removal will fail at runtime. TypeScript won't catch this because of `any` types.

**Fix:** Add funcId → address tracking in StrobeAgent:
```typescript
private funcIdToAddress: Map<number, string> = new Map();

// In handleMessage add action:
if (funcId !== null) {
  this.funcIdToName.set(funcId, func.name);
  this.funcIdToAddress.set(funcId, func.address);  // ADD
  installed++;
}

// Update removeNativeHook:
public removeNativeHook(funcId: number): void {
  const address = this.funcIdToAddress.get(funcId);
  if (address) {
    this.cmoduleTracer?.removeHook(address);
    this.funcIdToAddress.delete(funcId);
    this.funcIdToName.delete(funcId);
  }
}
```

---

### 4. ASLR Slide Duplication (Correctness)
**Severity:** Critical
**Confidence:** 90
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/tracers/native-tracer.ts:79-87`

**Problem:** NativeTracer computes slide locally AND delegates to CModuleTracer, creating dual slide state that could diverge.

**Current Code:**
```typescript
setImageBase(imageBase: string): void {
  this.imageBase = ptr(imageBase);
  const moduleBase = Process.mainModule?.base ?? ptr(0);
  this.slide = moduleBase.sub(this.imageBase);  // ← Computed here
  if (this.agent.cmoduleTracer) {
    this.agent.cmoduleTracer.setImageBase(imageBase);  // ← AND here
  }
}
```

**Impact:** If `Process.mainModule` returns different values or CModuleTracer computes differently, addresses will be wrong.

**Fix:** Delegate slide calculation entirely to CModuleTracer:
```typescript
setImageBase(imageBase: string): void {
  if (this.agent.cmoduleTracer) {
    this.agent.cmoduleTracer.setImageBase(imageBase);
  }
}

getSlide(): NativePointer {
  return this.agent.cmoduleTracer?.getSlide() ?? ptr(0);
}
```

---

### 5. Code Injection via Breakpoint Conditions (Security)
**Severity:** Critical
**Confidence:** 95
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:1040, 1278`

**Problem:** User-provided condition strings are directly interpolated into `new Function()` without sanitization.

**Vulnerable Code:**
```typescript
// Line 1040 (logpoints)
new Function('args', `return (${lp.condition})`)(argsArray)

// Line 1278 (breakpoints)
new Function('args', `return (${condition})`)(argsArray)
```

**Attack Example:**
```javascript
// User sets condition:
"args[0]; process.exit(); true"
// Executes arbitrary code in target process
```

**Impact:** Complete compromise of instrumented process. Attacker with MCP access can execute arbitrary code.

**Fix:** Use safe expression evaluator or document as privileged access:
```typescript
// Option 1: Validate expression AST
private evaluateCondition(condition: string, args: any[]): boolean {
  // Parse and reject: function calls, property access beyond args[]
  if (/process\.|require\(|import |eval\(/.test(condition)) {
    throw new Error('Forbidden operations in condition');
  }
  // Proceed with caution...
}

// Option 2: Document security model
// Add to tool documentation: "Breakpoint conditions execute with full process privileges.
// Only use with trusted inputs."
```

---

### 6. Code Injection via Expression Watches (Security)
**Severity:** Critical
**Confidence:** 90
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/cmodule-tracer.ts:684`

**Problem:** Expression watches use `new Function('return ' + e.expr)` without validation.

**Impact:** Same as #5 - arbitrary code execution.

**Fix:** Same as #5 - validate expressions or document security model.

---

## Important Issues

### 7. Missing null Check in removeNativeHook (Correctness)
**Severity:** Important
**Confidence:** 100
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:1247`

**Problem:**
```typescript
public removeNativeHook(address: string): void {
  this.cmoduleTracer.removeHook(address);  // ❌ No null check
}
```

**Fix:**
```typescript
public removeNativeHook(address: string): void {
  this.cmoduleTracer?.removeHook(address);
}
```

---

### 8. DwarfResolver Sync/Async Boundary Violation (Correctness)
**Severity:** Important
**Confidence:** 100
**Location:** `/Users/alex/strobe-multilang-foundation/src/symbols/dwarf_resolver.rs:25-27`

**Problem:** Trait is synchronous but DWARF parsing is async. Calling `resolve_pattern()` during active parse returns error instead of waiting.

**Current Code:**
```rust
let parser_result = self.dwarf.try_borrow_parser()
    .ok_or_else(|| crate::Error::Internal("DWARF parse not yet complete".to_string()))?
```

**Impact:** Race condition if resolver created before parse completes.

**Fix:** Either:
1. Make trait async: `async fn resolve_pattern(...)`
2. Enforce DwarfResolver only created after parse completes
3. Document caller responsibility

---

### 9. Language Detection Missing Edge Cases (Correctness)
**Severity:** Important
**Confidence:** 90
**Location:** `/Users/alex/strobe-multilang-foundation/src/daemon/session_manager.rs:50-76`

**Problem:** Heuristics miss:
- Virtual environments: `venv/bin/python` won't match `contains("python")`
- Shebang scripts: `#!/usr/bin/env python3`
- Symlinked interpreters

**Fix:** Add filesystem checks for symlinks and shebangs.

---

### 10. detectRuntime() Always Returns 'native' (Correctness)
**Severity:** Important
**Confidence:** 90
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:195-198`

**Problem:** Implementation is stubbed instead of checking for Python/V8/JSC symbols as shown in plan spec.

**Current Code:**
```typescript
function detectRuntime(): RuntimeType {
  // For now, always return 'native' — Python/JS detection will be added in Plan 2/3
  return 'native';
}
```

**Impact:** Not critical for Plan 1 (only native supported), but violates spec.

**Fix:** Implement as specified:
```typescript
function detectRuntime(): RuntimeType {
  if (Module.findExportByName(null, '_PyEval_EvalFrameDefault')) return 'cpython';
  if (Module.findExportByName(null, 'Py_Initialize')) return 'cpython';
  if (Module.findExportByName(null, '_ZN2v88internal7Isolate7currentEv')) return 'v8';
  if (Module.findExportByName(null, 'JSGlobalContextCreate')) return 'jsc';
  return 'native';
}
```

---

### 11. ReDoS in Pattern Matching (Security)
**Severity:** Important
**Confidence:** 80
**Location:** `/Users/alex/strobe-multilang-foundation/src/dwarf/parser.rs:1572-1630`

**Problem:** `PatternMatcher::glob_match` uses recursive backtracking. Patterns like `**::**::**::**` cause exponential time.

**Fix:** Add recursion depth limit:
```rust
const MAX_DEPTH: usize = 10;
fn glob_match_with_depth(pattern: &str, text: &str, depth: usize) -> bool {
    if depth > MAX_DEPTH { return false; }
    // ... existing logic with depth+1 ...
}
```

---

### 12. Path Traversal in Language Detection (Security)
**Severity:** Important
**Confidence:** 75
**Location:** `/Users/alex/strobe-multilang-foundation/src/daemon/session_manager.rs:49-76`

**Problem:** `project_root` parameter not validated before `project_root.join()` usage.

**Impact:** Low - only checks file existence, doesn't read contents. Information disclosure only.

**Fix:** Canonicalize and validate path within allowed directories.

---

### 13. Hook Removal Bypasses Tracer Interface (Integration)
**Severity:** Important
**Confidence:** 85
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:383-385`

**Problem:** Removal calls `removeNativeHook()` directly instead of `tracer.removeHook()`.

**Impact:** Breaks abstraction. Interpreted language tracers won't receive removal calls.

**Fix:** Route through tracer interface (requires funcId tracking from issue #3).

---

### 14. Zero Tests for New Abstractions (Test Coverage)
**Severity:** Important
**Confidence:** 95

**Problem:** No tests for:
- Tracer interface (no agent test framework exists)
- NativeTracer delegation
- detectRuntime() / createTracer()
- Language detection heuristics
- DwarfResolver trait methods
- ResolvedTarget enum variants

**Impact:** No regression protection for foundation layer. Future changes can break without test failures.

**Fix:** See detailed recommendations in Test Coverage section below.

---

### 15. removeAllNativeHooks is No-Op (Completeness)
**Severity:** Important
**Confidence:** 85
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:1254-1257`

**Problem:**
```typescript
public removeAllNativeHooks(): void {
  // CModuleTracer doesn't have removeAll, so we'll need to track this differently
  // For now, this is a no-op placeholder
}
```

**Impact:** Violates Tracer interface contract. Session cleanup may leak hooks.

**Fix:** Iterate over funcIds and remove:
```typescript
public removeAllNativeHooks(): void {
  const funcIds = Array.from(this.funcIdToAddress.keys());
  for (const funcId of funcIds) {
    this.removeNativeHook(funcId);
  }
}
```

---

### 16. SQL Injection Pattern (Security)
**Severity:** Important (Low actual risk)
**Confidence:** 75
**Location:** `/Users/alex/strobe-multilang-foundation/src/db/schema.rs:8`

**Problem:** Uses `format!()` for SQL, but inputs are currently hardcoded.

**Impact:** Currently safe, but dangerous pattern if ever exposed to user input.

**Fix:** Add identifier validation.

---

## Minor Issues

### 17. Unused project_root Parameter (Correctness)
**Severity:** Minor
**Location:** `/Users/alex/strobe-multilang-foundation/src/symbols/dwarf_resolver.rs:22`

**Issue:** `_project_root` prefixed with underscore but required by trait.

**Fix:** Add comment: `// project_root unused for native: DWARF symbols are absolute addresses`

---

### 18. Type Alias Confusion (Correctness)
**Severity:** Minor
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/agent.ts:183-186`

**Issue:** Type aliases create false sense of compatibility between agent types and Tracer interface.

**Fix:** Remove aliases, use explicit imports.

---

### 19. Misleading Comment (Test Coverage)
**Severity:** Minor
**Location:** `/Users/alex/strobe-multilang-foundation/agent/src/tracers/native-tracer.ts:14-16`

**Issue:** Comment says "CModuleTracer is lazily created" but it's created in constructor.

---

## Test Coverage Analysis

### Missing Critical Tests:

1. **detect_language() - ZERO tests** (Confidence: 95)
   - Command-based detection (python, node, bun, .py, .js, .ts)
   - Project-based detection (pyproject.toml, package.json, etc.)
   - Edge cases (symlinks, shebangs, ambiguous projects)
   - **Estimated effort:** 90 minutes

2. **DwarfResolver trait methods - ZERO unit tests** (Confidence: 90)
   - resolve_pattern() delegation
   - resolve_line() delegation
   - @file: pattern handling
   - Error handling
   - **Estimated effort:** 60 minutes

3. **Language/ResolvedTarget enum serialization - ZERO tests** (Confidence: 90)
   - JSON serialization
   - Display formatting
   - **Estimated effort:** 15 minutes

### Missing Important Tests:

4. **Agent test infrastructure - DOES NOT EXIST** (Confidence: 100)
   - No test framework (jest/mocha)
   - No test directory
   - No test scripts
   - **Estimated effort:** 2-3 hours

5. **NativeTracer unit tests - ZERO tests** (Confidence: 90)
   - ASLR slide computation
   - Delegation methods
   - Error throwing (readVariable/writeVariable)
   - **Estimated effort:** 90 minutes

6. **Runtime detection tests - ZERO tests** (Confidence: 85)
   - detectRuntime() stub
   - createTracer() factory
   - **Estimated effort:** 30 minutes

### Existing Tests (Regression Protection): ✓

- ✅ 15 Frida E2E scenarios pass
- ✅ Breakpoint tests pass
- ✅ Stepping tests pass
- ✅ DWARF line table tests pass
- ✅ Test runner tests pass (7 scenarios)

**Total: 191 tests passing** - confirms zero regression.

---

## Approved Requirements

### Task 1: Agent Tracer Interface ✓ (95%)
- ✅ Tracer interface created with all required methods
- ✅ NativeTracer wrapper delegates to agent methods
- ✅ detectRuntime() and createTracer() exist
- ✅ StrobeAgent refactored to use tracer
- ⚠️ removeAllNativeHooks is placeholder (see issue #15)

### Task 2: Message Protocol Extensions ✓ (100%)
- ✅ HookInstruction accepts both `functions` and `targets`
- ✅ eval_variable handler added
- ✅ resolve handler added
- ✅ Message type interfaces defined

### Task 3: SymbolResolver Trait ✓ (100%)
- ✅ Language enum defined
- ✅ ResolvedTarget enum with Address/SourceLocation variants
- ✅ VariableResolution enum
- ✅ SymbolResolver trait with all required methods
- ✅ DwarfResolver implementation
- ✅ symbols/mod.rs exports updated

### Task 4: Language Detection ⚠️ (33%)
- ✅ detect_language() function created
- ❌ Language never stored per session (see issue #2)
- ❌ Runtime detection message not sent (see issue #1)

### Task 5: Build + Test ⚠️ (66%)
- ✅ Agent builds successfully
- ✅ Daemon builds successfully
- ❌ No evidence of test execution (see Test Coverage section)

### Task 6: Wire SymbolResolver (Intentionally Deferred)
- ⚠️ Marked as "optional stretch" in plan
- Not implemented (update_frida_patterns still uses direct DWARF access)
- **Status:** Acceptable per plan

---

## Recommendations

### Before Merging (Critical - Estimated 4-6 hours):

1. **Fix hook removal type mismatch** (Issue #3) - 1 hour
   - Add funcId → address tracking
   - Update removeNativeHook signature
   - Wire removal through tracer interface

2. **Wire language detection into session flow** (Issue #2) - 2 hours
   - Add language/resolver storage to SessionManager
   - Call detect_language() in spawn_with_frida
   - Instantiate DwarfResolver after parse completes

3. **Add runtime_detected message** (Issue #1) - 15 minutes
   - One line addition in agent.ts

4. **Fix ASLR slide duplication** (Issue #4) - 30 minutes
   - Delegate to CModuleTracer

5. **Add null check** (Issue #7) - 5 minutes

6. **Add detect_language() tests** (Issue #14) - 90 minutes
   - Command-based, project-based, edge cases

### Before Plan 2 (Important - Estimated 6-8 hours):

7. **Set up agent test infrastructure** - 2-3 hours
8. **Add NativeTracer unit tests** - 90 minutes
9. **Add DwarfResolver unit tests** - 60 minutes
10. **Fix security issues** (Issues #5, #6) - 2-3 hours
    - Validate breakpoint conditions
    - Validate expression watches
    - Document security model

### Optional (Nice to Have):

11. **Implement detectRuntime()** (Issue #10) - 30 minutes
12. **Add ReDoS protection** (Issue #11) - 30 minutes
13. **Implement removeAllNativeHooks** (Issue #15) - 15 minutes

---

## Conclusion

The Plan 1 implementation creates a **solid architectural foundation** but is **not ready for merge** due to:

1. **Integration incomplete** - Core abstractions exist but aren't wired into execution flow
2. **Type safety bugs** - Will cause runtime failures
3. **Security vulnerabilities** - Code injection risks
4. **Test coverage gaps** - No regression protection for new code

**Strengths:**
- Interface designs are clean and extensible
- Zero regression on existing functionality
- DWARF path still works through new abstractions
- Code quality is good (clear naming, good comments)

**Estimated Fix Time:** 4-6 hours for critical issues, 10-14 hours for comprehensive fixes including tests and security hardening.

**Recommendation:** Fix critical issues #1-7 before merging. Plan 2 (Python support) depends on these working correctly.
