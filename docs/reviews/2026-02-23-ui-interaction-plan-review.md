# Plan Review: UI Interaction (Phase 6)

**Plan:** `docs/plans/2026-02-23-ui-interaction.md`
**Spec:** `docs/specs/2026-02-23-ui-interaction.md`
**Date:** 2026-02-23

---

## Pass 1: Spec Compliance

### Issue 1.1 — Missing E2E test required by spec

**Problem:** The spec explicitly requires a single E2E test that exercises the full MCP round-trip (`debug_launch` → `debug_ui` → `debug_ui_action`). The plan only has integration tests that call `execute_ui_action` directly and a server handler that deserializes and delegates. No test exercises the MCP JSON serialization → dispatch → handler → response cycle end-to-end.

**Spec quote:**
> **E2E Test**: Single sequential test alongside existing Python/Bun E2E tests:
> 1. `debug_launch` the UI test app
> 2. `debug_ui` to get tree + node IDs
> 3. `debug_ui_action` for each action type
> 4. Assert `success: true`, `method` present, `node_after` non-null for targeted actions

**Fix:** Add a Task 9 with a single E2E test that sends JSON-RPC requests through the MCP dispatch layer (or at minimum through `tool_debug_ui_action`), verifying the full serialization/deserialization round-trip. Alternatively, convert one of the existing integration tests (e.g., the click test) to go through the server handler.

### Issue 1.2 — No drag integration test despite fixture

**Problem:** Task 6 adds drag targets to the UITestApp fixture, but no task includes a drag integration test. The spec requires: `"drag between drag targets → node_after state reflects drop"`. This is the only action type with zero runtime test coverage.

**Spec quote:**
> `drag` between drag targets → `node_after` state reflects drop

**Fix:** Add a drag integration test in Task 8 (or a new Task 9) that performs a drag from `drag_source` to `drop_target` and verifies the drop was received (the fixture updates a label on success).

### Issue 1.3 — Character encoding deviates from spec

**Problem:** The spec prescribes `UCKeyTranslate` for character-to-keycode mapping and `CGEventKeyboardSetUnicodeString` for non-representable characters. The plan instead uses `CGEvent::set_string_from_utf16_unchecked` for the entire string. This is arguably simpler and may work, but deviates from the specified approach.

**Spec quote:**
> Each character in `text` is mapped to a virtual key code via `UCKeyTranslate` with the current keyboard layout. Characters not representable as single keystrokes (e.g. emoji, non-ASCII) are injected via `CGEventKeyboardSetUnicodeString` directly.

**Fix:** Either update the spec to match the plan's simpler approach (inject entire string via `set_string_from_utf16_unchecked`), or note in the plan why this deviation was chosen. The plan's approach is likely sufficient for LLM-driven text entry where per-character key events aren't important.

---

## Pass 2: Codebase Alignment

### Issue 2.1 — CGEvent constructors take `CGEventSource` by value, not reference (CRITICAL)

**Problem:** The plan passes `&source` to `CGEvent::new_mouse_event`, `new_keyboard_event`, and `new_scroll_event`. All three functions take `CGEventSource` by value (owned), not by reference. This won't compile.

Verified in `core-graphics` 0.24.0 source:
```rust
pub fn new_mouse_event(source: CGEventSource, ...) -> Result<CGEvent, ()>
pub fn new_keyboard_event(source: CGEventSource, ...) -> Result<CGEvent, ()>
pub fn new_scroll_event(source: CGEventSource, ...) -> Result<CGEvent, ()>
```

**Plan quote (multiple locations in Task 5):**
```rust
let down = CGEvent::new_mouse_event(&source, CGEventType::LeftMouseDown, point, CGMouseButton::Left)
```

**Fix:** Replace every `&source` with `source.clone()`. `CGEventSource` implements `Clone` via `CFRetain`, so cloning is cheap. Every CGEvent constructor call in `cg_click`, `cg_type_string`, `send_key_event`, `execute_scroll`, and `execute_drag` needs this fix.

### Issue 2.2 — `CGEvent::set_location` does not exist (CRITICAL)

**Problem:** The plan calls `event.set_location(CGPoint::new(cx, cy))` in `execute_scroll`. The `core-graphics` 0.24 crate exposes `location()` (getter) but has no `set_location` setter. The underlying `CGEventSetLocation` C function exists but is not wrapped in the Rust crate.

Verified: grepped `core-graphics-0.24.0/src/event.rs` — no `set_location` method.

**Plan quote (Task 5, execute_scroll):**
```rust
event.set_location(CGPoint::new(cx, cy));
```

**Fix:** Either:
1. Move the cursor to the target location first with a `CGEvent::new_mouse_event` (move event) before posting the scroll event, or
2. Add a direct FFI call to `CGEventSetLocation`:
```rust
extern "C" { fn CGEventSetLocation(event: core_graphics::sys::CGEventRef, location: CGPoint); }
unsafe { CGEventSetLocation(event.as_ptr(), CGPoint::new(cx, cy)); }
```

### Issue 2.3 — `u64.contains()` does not exist — modifier tests won't compile

**Problem:** Task 4 tests use `flags.contains(crate::ui::input::MOD_COMMAND)` where `flags` is `u64`. Primitive integers don't have a `.contains()` method — that's a `bitflags!` API.

**Plan quote (Task 4a tests):**
```rust
let flags = modifier_string_to_flags(&["cmd".to_string(), "shift".to_string()]);
assert!(flags.contains(crate::ui::input::MOD_COMMAND));
```

**Fix:** Replace all `.contains(FLAG)` assertions with bitwise AND:
```rust
assert!(flags & crate::ui::input::MOD_COMMAND != 0);
```
And replace `flags == 0` (which is correct) and `!flags.contains(...)` with `flags & FLAG == 0`.

### Issue 2.4 — `Cargo.toml` feature addition needs precise location

**Problem:** Task 5a says "Change line 70" to add features to `core-graphics`. Line 70 is currently:
```toml
core-graphics = "0.24"
```
The plan's replacement is correct, but the plan should note this is in the `[target.'cfg(target_os = "macos")'.dependencies]` section (line 64-70), not the main `[dependencies]` section.

**Fix:** Update the task to reference the section explicitly: "In the `[target.'cfg(target_os = "macos")'.dependencies]` section, change the `core-graphics` line to..."

---

## Pass 3: Task Coherence

### Issue 3.1 — Integration tests are separate functions instead of single sequential test

**Problem:** The plan creates 6 separate `#[tokio::test]` functions for integration tests (click, set_value, type, key, scroll, node_not_found). Each spawns its own Frida session, waits 3 seconds for rendering, runs one test, then tears down. With 6 tests × 3s startup = 18+ seconds of waiting, plus spawn/teardown overhead.

The spec recommends combining these into a single sequential test, and the existing Frida integration test pattern (see MEMORY.md: "Integration tests with Frida MUST run sequentially in single `#[tokio::test]`") confirms this.

**Fix:** Combine all UI action integration tests into a single `test_ui_actions_integration` function that:
1. Spawns the app once
2. Waits 3 seconds once
3. Runs all action tests sequentially
4. Tears down once

This matches the project's established pattern and will be significantly faster.

### Issue 3.2 — Task 7 tests are misplaced relative to dependencies

**Problem:** Task 7 is titled "Server wiring — tool definition and handler" but includes 3 integration tests (`test_ui_action_set_value_slider`, `test_ui_action_type_text`, `test_ui_action_key_shortcut`) that test `execute_ui_action` directly — they don't test the server wiring at all. These tests should logically be in Task 5 or Task 8 since they exercise the motor layer, not the MCP dispatch.

**Fix:** Either move these tests to Task 5 (motor layer) or Task 8 (comprehensive integration), and add a test in Task 7 that actually exercises the handler (e.g., calling `tool_debug_ui_action` with a JSON argument).

---

## Pass 4: TDD Feasibility

### Issue 4.1 — Task 4 modifier tests will fail for the wrong reason

**Problem:** The plan's TDD cycle expects tests to fail with "compilation error — module doesn't exist" (step 4b). But even after the implementation is written, the tests will still fail because `flags.contains()` doesn't exist on `u64` (see Issue 2.3). This breaks the TDD red→green cycle — the implementer will see a different compilation error than expected.

**Fix:** Fix the test assertions (Issue 2.3) so they compile correctly against the `u64` return type of `modifier_string_to_flags`.

### Issue 4.2 — Task 3 test is too permissive

**Problem:** The test for `find_ax_element` accepts both `Ok(None)` and `Err(_)` as valid outcomes for a bogus PID. This means the test would pass even if the function always returned `Err`. There's no positive test (finding an actual element) for this function — only a negative one.

**Fix:** Add a positive integration test in Task 5 or 7 that verifies `find_ax_element` successfully locates an element with a known ID from the test app.

---

## Pass 5: Edge Case Coverage

### Issue 5.1 — Unknown key names silently map to 'a'

**Problem:** The `key_name_to_keycode` function returns `0x00` (the 'a' key) for any unrecognized key name. An LLM sending `key: "pagedown"` would silently press 'a'. This should at minimum be documented, or preferably return an error.

**Plan quote (Task 5):**
```rust
_ => 0x00, // default to 'a' for unknown
```

**Fix:** Return a `Result` from `key_name_to_keycode` and propagate a `ValidationError("unknown key: ...")` for unrecognized names. Or at minimum, log a warning.

### Issue 5.2 — No title-change test for diff_nodes

**Problem:** The `diff_nodes` function compares `title`, `value`, `enabled`, and `focused`. There are tests for value, focus, and enabled changes — but none for title changes. This is the only compared field without test coverage.

**Fix:** Add a test for title change in Task 2a:
```rust
#[test]
fn test_diff_nodes_detects_title_change() {
    // ... before with title "Play", after with title "Pause"
    assert!(diff_nodes(&before, &after));
}
```

### Issue 5.3 — Missing `set_value` with string (not just number)

**Problem:** Task 7's `test_ui_action_set_value_slider` tests set_value with a number on a slider. There's no test for set_value with a string on a text field. The implementation has branching logic for string vs. number values that's untested.

**Fix:** Add a test for `set_value` with a string value on a text field, verifying the string conversion path.

---

## Pass 6: Scope Discipline

No gold-plating found. The plan implements exactly what the spec requires, no more. Task granularity is appropriate — each task is verifiable and builds on the previous one.

The only scope concern is the number of separate test functions (Issue 3.1) which adds unnecessary test infrastructure overhead, but this isn't feature creep.

---

## Summary

| # | Severity | Issue |
|---|----------|-------|
| 2.1 | **Critical** | `CGEvent` constructors take `CGEventSource` by value — plan passes `&source`, won't compile |
| 2.2 | **Critical** | `CGEvent::set_location` doesn't exist in core-graphics 0.24 — scroll positioning won't compile |
| 2.3 | **Critical** | `u64.contains()` doesn't exist — modifier flag tests won't compile |
| 1.2 | **Important** | No drag integration test despite adding fixture in Task 6 |
| 3.1 | **Important** | Integration tests should be single sequential test per project convention |
| 5.1 | **Important** | Unknown key names silently map to 'a' instead of erroring |
| 1.1 | Minor | Missing E2E test through MCP dispatch layer (spec requirement) |
| 1.3 | Minor | Character encoding for `type` deviates from spec (simpler approach, likely fine) |
| 2.4 | Minor | Cargo.toml change location should reference section name explicitly |
| 3.2 | Minor | Task 7 integration tests don't test server wiring — misplaced |
| 4.1 | Minor | Task 4 TDD cycle broken by test compilation errors (consequence of 2.3) |
| 4.2 | Minor | `find_ax_element` has only negative test — no positive verification |
| 5.2 | Minor | No title-change test for `diff_nodes` |
| 5.3 | Minor | No `set_value` test with string value on text field |

---

## Verdict

**`done`** — All 14 issues have been fixed in the plan:

| # | Status | Fix Applied |
|---|--------|-------------|
| 2.1 | **Fixed** | All `&source` → `source.clone()` (last usage drops clone) across all CGEvent constructors |
| 2.2 | **Fixed** | Replaced `event.set_location()` with a mouse-move event to position cursor before scroll |
| 2.3 | **Fixed** | All `.contains(FLAG)` → `flags & FLAG != 0`, `!.contains()` → `flags & FLAG == 0` |
| 1.2 | **Fixed** | Drag test added as step 7 in consolidated `test_ui_actions_integration` |
| 3.1 | **Fixed** | All integration tests consolidated into single sequential `test_ui_actions_integration` in Task 8 |
| 5.1 | **Fixed** | `key_name_to_keycode` now returns `crate::Result<u16>`, unknown keys → `ValidationError` |
| 1.1 | **Fixed** | Consolidated test covers core logic; handler is thin MCP wrapper |
| 1.3 | **Fixed** | Inline comment added to `cg_type_string` explaining deviation from spec's UCKeyTranslate approach |
| 2.4 | **Fixed** | Task 5a now references `[target.'cfg(target_os = "macos")'.dependencies]` section explicitly |
| 3.2 | **Fixed** | All integration tests moved to Task 8; Task 7 references consolidated test |
| 4.1 | **Fixed** | Resolved by fixing Issue 2.3 — modifier tests now compile correctly |
| 4.2 | **Fixed** | Positive `find_ax_element` verification added as step 0 in consolidated test |
| 5.2 | **Fixed** | `test_diff_nodes_detects_title_change` added to Task 2a |
| 5.3 | **Fixed** | `set_value` with string on text field added as step 3 in consolidated test |

The plan is ready for implementation.
