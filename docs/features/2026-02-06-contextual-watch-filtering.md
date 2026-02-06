# Contextual Watch Filtering (`on` Field) Implementation

**Date:** 2026-02-06
**Status:** ✅ Complete
**Parent Plan:** [docs/plans/2026-02-06-contextual-watches.md](../plans/2026-02-06-contextual-watches.md)

## Overview

Implemented runtime pattern-based filtering for watch values using the `on` field in watch specifications. This allows watches to be scoped to specific functions, reducing noise and improving clarity in trace output.

## Feature Description

Watches can now be restricted to specific functions using pattern matching:

```typescript
debug_trace({
  sessionId: "...",
  watches: {
    add: [
      { variable: "gCounter" },                           // Global - captures on ALL functions
      { variable: "gTempo", on: ["audio::process"] },     // Only during audio::process
      { variable: "gClock", on: ["midi::*"] },            // Only during midi:: namespace functions
      { address: "0x1234", type: "u32", on: ["NoteOn", "NoteOff"] }  // Multiple patterns
    ]
  }
})
```

## Architecture

### Data Flow

1. **MCP Request** → Rust daemon receives `on: ["pattern1", "pattern2"]` in `WatchTarget`
2. **Pattern Storage** → Patterns stored in `WatchTarget.on_patterns: Option<Vec<String>>`
3. **Agent Transmission** → Patterns sent to TypeScript agent via `SetWatches` message (`onPatterns` field)
4. **Runtime Resolution** → Agent matches patterns against installed hooks' function names
5. **Filtering** → During drain, watches check if current `funcId` matches resolved patterns

### Pattern Matching Logic

Patterns support wildcards with namespace-aware semantics:

- `*` — matches any sequence of characters **except** `::`
  - `audio::*` matches `audio::process` but NOT `audio::process::internal`
- `**` — matches any sequence including `::`
  - `audio::**` matches `audio::process::internal`
  - `**::callback` matches `foo::bar::callback`
- Exact match — `audio::process` matches only `audio::process`

**Implementation:**
- Replace `**` with temporary marker `\x00DEEP\x00`
- Escape regex special chars
- Replace `*` with `[^:]+` (one or more non-colon chars)
- Restore `**` marker as `.*` (anything including colons)

### Why Runtime Resolution?

- **funcId assignment happens in agent** — The TypeScript agent assigns funcIds dynamically as hooks are installed, not deterministically in Rust
- **Pattern matching needs function names** — Only the agent knows the mapping from address → funcName → funcId
- **Cleaner architecture** — Avoids complex state synchronization between Rust and agent

## Implementation Details

### Files Modified

1. **src/frida_collector/spawner.rs**
   - Added `on_patterns: Option<Vec<String>>` to `WatchTarget` struct
   - Updated `SetWatches` message to include `onPatterns` field

2. **src/daemon/server.rs**
   - Changed watch resolution to pass `watch_target.on.clone()` as `on_patterns`
   - No longer attempts to resolve patterns to funcIds in Rust

3. **agent/src/cmodule-tracer.ts**
   - Updated `hooks` map to store `funcName: string` alongside `listener` and `funcId`
   - Added `matchPatternsToFuncIds(patterns: string[]): Set<number>` method
   - Added `matchPattern(name: string, pattern: string): boolean` method with wildcard support
   - Updated `updateWatches()` to accept `onPatterns?: string[]` parameter
   - Resolve patterns to funcIds at runtime when watches are installed
   - Filter watch values during drain based on resolved funcIds

4. **src/mcp/types.rs**
   - `WatchTarget.on` field already existed but was documented as NOT IMPLEMENTED
   - Removed warning comment indicating feature is now complete

### Testing

**Unit Tests (tests/integration.rs):**

1. **test_watch_on_field_patterns** — Validates MCP type structures
   - Watches with `on` field serialize/deserialize correctly
   - Global watches (no `on` field) work as expected

2. **test_watch_pattern_matching_logic** — Tests pattern matching algorithm
   - 12 test cases covering exact match, `*`, `**`, namespace boundaries
   - Verifies `audio::*` matches `audio::process` but not `audio::process::internal`
   - Verifies `**::baz` matches `foo::bar::baz`
   - Verifies patterns work without namespaces (`parse*` matches `parseValue`)

**Build Verification:**
- TypeScript agent builds successfully
- Cargo builds successfully in release mode
- All 78 tests pass (58 unit + 20 integration)

## Usage Examples

### Basic Scoping

```typescript
// Only capture gCounter during audio processing
debug_trace({
  sessionId: "audio-session",
  watches: {
    add: [
      { variable: "gCounter", on: ["audio::processBlock"] }
    ]
  }
})
```

### Multiple Patterns

```typescript
// Capture during any MIDI event
debug_trace({
  sessionId: "midi-session",
  watches: {
    add: [
      { variable: "gLastNote", on: ["midi::NoteOn", "midi::NoteOff", "midi::CC"] }
    ]
  }
})
```

### Wildcard Patterns

```typescript
// Capture during ANY function in juce namespace
debug_trace({
  sessionId: "juce-session",
  watches: {
    add: [
      { variable: "gAudioBuffer", on: ["juce::**"] }  // Deep wildcard crosses namespaces
    ]
  }
})
```

### Global + Contextual Mix

```typescript
// Mix global and scoped watches
debug_trace({
  sessionId: "mix-session",
  watches: {
    add: [
      { variable: "gGlobalCounter" },                    // Always captured
      { variable: "gTempo", on: ["audio::*"] },          // Only audio:: functions
      { variable: "gMidiState", on: ["midi::NoteOn"] }   // Very specific
    ]
  }
})
```

## Performance Characteristics

- **Hook Installation:** No overhead (patterns stored but not immediately resolved)
- **Pattern Resolution:** One-time cost when `updateWatches()` is called (~1μs per pattern)
- **Runtime Filtering:** O(1) set lookup per watch per event (uses resolved `Set<funcId>`)
- **Memory:** ~8 bytes per funcId per scoped watch (typically 1-10 funcIds per watch)

## Limitations

- **@file: patterns not supported at runtime** — Source file info not available in agent
  - Workaround: Use function name patterns instead
- **Patterns resolved at watch installation time** — If new hooks are added after watches, patterns won't match new functions
  - Workaround: Re-send watches after adding new hook patterns
- **Maximum 4 CModule watches total** — Hard limit due to ring buffer size
  - No limit on expression watches

## Future Enhancements

Potential improvements (not currently planned):

1. **Auto-reresolution** — Re-resolve patterns when new hooks are installed
2. **Per-watch pattern reporting** — Show which funcIds each pattern matched
3. **Negation patterns** — `on: ["!audio::*"]` to exclude specific functions
4. **Regex patterns** — Full regex support instead of just wildcards

## Commit

This feature was implemented and tested as part of the architectural review and issue-fixing work following the initial contextual watches implementation.

**Branch:** feature/contextual-watches
**Tests:** All 78 tests passing
**Build:** Clean release build with no errors
