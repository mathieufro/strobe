# MCP Tool Description Improvements

## Problem Summary

The current `debug_trace` tool description **contradicts** the "Observation Loop" instructions:

**Tool description (line 335):**
> "Call BEFORE debug_launch (without sessionId) to set which functions to trace"

**Observation Loop (line 244):**
> "Launch with no tracing — Call debug_launch with no prior debug_trace"

This causes LLMs to:
1. Pre-set patterns before launch (against best practice)
2. Be confused when `hookedFunctions: 0` appears
3. Not understand the two distinct usage modes

---

## Root Causes

### 1. **Unclear Two-Mode Operation**
`debug_trace` has two distinct modes but this isn't explicit:

**Mode A: Pre-Launch Pattern Staging (DISCOURAGED)**
- Called WITHOUT sessionId
- Sets "pending patterns" for next launch
- No hooks installed yet → `hookedFunctions: 0` is expected
- Against recommended workflow

**Mode B: Runtime Pattern Addition (RECOMMENDED)**
- Called WITH sessionId on running session
- Installs hooks immediately
- `hookedFunctions: N` shows actual matches
- Follows observation loop pattern

### 2. **Ambiguous Response Format**
When `hookedFunctions: 0`, LLMs can't tell if:
- Patterns are pending (pre-launch mode)
- Patterns didn't match anything (runtime mode)
- Process hasn't started yet
- Patterns are malformed

### 3. **Missing Workflow Context**
Tool description doesn't mention:
- stdout/stderr are ALWAYS captured (no tracing needed)
- Most bugs are found via output alone
- Tracing is for understanding behavior, not crashes

---

## Proposed Fixes

### Fix 1: Rewrite `debug_trace` Tool Description

**Current (line 335):**
```rust
description: "Configure trace patterns and event limits. Call BEFORE debug_launch (without sessionId) to set which functions to trace — patterns are applied when the process spawns. Can also be called WITH sessionId to add/remove patterns or adjust event limits on a running session."
```

**Proposed:**
```rust
description: r#"Add or remove function trace patterns on a RUNNING debug session. IMPORTANT: Follow the observation loop workflow:

1. Launch with debug_launch (no prior debug_trace) - stdout/stderr captured automatically
2. Query stderr/stdout first - most issues are visible in output
3. If output insufficient, add targeted patterns: debug_trace({ sessionId, add: ["pattern"] })
4. Query traces, iterate patterns as needed

ANTI-PATTERN: Setting patterns before launch (without sessionId). While supported for advanced use cases, this bypasses the recommended workflow where you observe output first, then add instrumentation only if needed.

When called WITH sessionId (recommended):
- Immediately installs hooks on running process
- Returns hookedFunctions count showing actual matches
- Start with 1-3 specific patterns (under 50 hooks ideal)

When called WITHOUT sessionId (advanced only):
- Stages "pending patterns" for next debug_launch by THIS connection
- hookedFunctions will be 0 (not installed yet)
- Use only when you know exactly what to trace upfront"#
```

### Fix 2: Improve Response Messages

**Current response:**
```json
{
  "activePatterns": ["foo::*"],
  "eventLimit": 200000,
  "hookedFunctions": 0
}
```

**Proposed - Pre-Launch Mode:**
```json
{
  "mode": "pending",
  "pendingPatterns": ["foo::*"],
  "eventLimit": 200000,
  "status": "Patterns will be applied when debug_launch is called. Note: Consider launching without patterns first (recommended workflow)."
}
```

**Proposed - Runtime Mode (No Matches):**
```json
{
  "mode": "runtime",
  "sessionId": "myapp-2026-02-06-16h11",
  "activePatterns": ["foo::*"],
  "hookedFunctions": 0,
  "eventLimit": 200000,
  "status": "Warning: 0 functions matched patterns. Patterns may not exist in binary, are inline/constexpr, or names are mangled. Try: 1) Check debug symbols exist, 2) Use @file: patterns, 3) List all functions with broader pattern first."
}
```

**Proposed - Runtime Mode (Success):**
```json
{
  "mode": "runtime",
  "sessionId": "myapp-2026-02-06-16h11",
  "activePatterns": ["foo::*", "bar::baz"],
  "hookedFunctions": 47,
  "eventLimit": 200000,
  "status": "Successfully hooked 47 functions. Under 50 hooks - excellent stability."
}
```

### Fix 3: Add Explicit Workflow Guidance

Add this to **"Common Mistakes"** section (after line 305):

```markdown
## Common Mistakes

- Do NOT set trace patterns before launch unless you already know exactly what to trace. Launch clean, read output first.
- Do NOT use @usercode. It hooks all project functions and will overwhelm the target.
- Do NOT use broad `@file:` patterns that match many source files. Be specific: `@file:parser.cpp` not `@file:src`.
- Do NOT restart the session to add traces. Use debug_trace with sessionId on the running session.
- Always check stderr before instrumenting — the answer is often already there.
- If debug_trace returns warnings about hook limits, narrow your patterns. Do NOT retry the same broad pattern.

**NEW:**
- If hookedFunctions is 0 on a running session, DO NOT blindly try more patterns. Instead:
  1. Verify debug symbols exist (check for .dSYM on macOS, separate debug info on Linux)
  2. Check if functions are inline/constexpr (won't appear in binary)
  3. Verify name mangling with `nm` or similar tools
  4. Try @file: patterns to match by source file instead
```

### Fix 4: Clarify `debug_launch` Response

When `debug_launch` returns, include hint about next steps:

**Current:**
```json
{
  "pid": 21057,
  "sessionId": "erae_mk2_simulator-2026-02-06-16h10"
}
```

**Proposed:**
```json
{
  "pid": 21057,
  "sessionId": "erae_mk2_simulator-2026-02-06-16h10",
  "pendingPatternsApplied": 0,
  "nextSteps": "Query stderr/stdout first with debug_query. Add trace patterns only if output insufficient."
}
```

Or if patterns were pending:
```json
{
  "pid": 21057,
  "sessionId": "erae_mk2_simulator-2026-02-06-16h10",
  "pendingPatternsApplied": 3,
  "hookedFunctions": 42,
  "warning": "Pre-launch patterns were applied. Consider observation loop workflow: launch clean, check output first, then add targeted traces."
}
```

---

## Implementation Checklist

- [ ] Update `debug_trace` description in [server.rs:335](server.rs#L335)
- [ ] Modify response schema to include `mode` field
- [ ] Add contextual `status` messages based on:
  - Pre-launch vs runtime
  - Hook count (0, <50, 50-100, >100)
  - Pattern match success/failure
- [ ] Update `debug_launch` response to show pending pattern state
- [ ] Add "Common Mistakes" entry about `hookedFunctions: 0`
- [ ] Consider renaming `hookedFunctions` to `matchedFunctions` (more accurate)
- [ ] Add examples to tool description showing both modes

---

## Example: Ideal LLM Interaction

**Current (Confusing):**
```
LLM: Let me set trace patterns first
> debug_trace({ add: ["foo::*"] })
Response: { hookedFunctions: 0 }
LLM: Hmm, 0 hooks. Let me try different patterns...
> debug_trace({ add: ["@file:foo"] })
Response: { hookedFunctions: 0 }
LLM: Still 0, maybe try broader pattern?
[continues guessing]
```

**Proposed (Clear):**
```
LLM: Let me launch and check output first
> debug_launch({ command: "./app" })
Response: { sessionId: "app-123", nextSteps: "Query stderr/stdout first..." }
LLM: Checking stderr...
> debug_query({ sessionId: "app-123", eventType: "stderr" })
Response: { events: [{ text: "Error in parse()" }] }
LLM: Found crash in parse(). Let me trace that function.
> debug_trace({ sessionId: "app-123", add: ["parse"] })
Response: { mode: "runtime", hookedFunctions: 1, status: "Successfully hooked 1 function..." }
[proceeds with confidence]
```

---

## Key Principle

**Make the tool description match the recommended workflow, not just describe technical capabilities.**

The tool CAN be called pre-launch, but this shouldn't be emphasized because it's an anti-pattern. The description should guide LLMs toward success, not just document all possible uses.
