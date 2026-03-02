# Progress: Linux UI Observation

**Plan:** `docs/plans/2026-03-02-linux-ui-observation.md`
**Branch:** `linux_compat`

## Pipeline

- [x] Spec
- [x] Plan
- [x] Implement
- [ ] Review

## Tasks

### Phase 1: Foundation
- [x] Task 1: Add Linux dependencies to Cargo.toml — done. `png` moved to shared, `atspi` + `x11rb` added for Linux.
- [x] Task 2: Module declarations and capture_linux stub — done. Added `capture_linux` mod decl + re-export, stub file created.

### Phase 2: Testable Pure Logic
- [x] Task 3: AT-SPI2 role mapping table — done. 30+ role mappings with unit tests.
- [x] Task 4: AT-SPI2 action name mapping — done. click/press/activate/toggle → AXPress.
- [x] Task 5: Pixel format conversion utilities — done. BGRX/BGRA→RGBA + PNG encoding, 6 tests.
- [x] Task 6: Key name to X11 keysym mapping — done. ~50 key names, 7 tests.
- [x] Task 7: Platform-conditional modifier constants in input.rs — done. X11 ShiftMask/ControlMask/Mod1Mask/Mod4Mask.

### Phase 3: AT-SPI2 Accessibility Tree
- [x] Task 8: UID validation helper — done. /proc/{pid}/status UID check, 2 tests.
- [x] Task 9: AT-SPI2 connection, app discovery, and tree walking — done. Async query_ax_tree, build_node, find_element_by_id. Fixed BusName::try_from API.

### Phase 4: X11 Screenshot Capture
- [x] Task 10: X11 window finding by PID — done. find_main_window via _NET_WM_PID, capture + crop, depth 24/32 support.

### Phase 5: Input Actions
- [x] Task 11: XTest input helpers — done. keysym_to_keycode, xtest_key/button/motion.
- [x] Task 12: Full execute_action implementation — done. Two-tier dispatch: AT-SPI2 → XTest for click/set_value/type/scroll/drag/key.

### Phase 6: Server Integration
- [x] Task 13: Remove macOS-only cfg gates in server.rs — done. Vision sidecar unconditional, tree/screenshot per-platform.

### Phase 7: Verification
- [x] Task 14: Full build verification — done. 403 unit tests pass, clean build.
- [x] Task 15: Integration test scaffolding — done. 4 gated tests (AT-SPI2 + X11 availability, error paths).

## Notes
- `BINDGEN_EXTRA_CLANG_ARGS="-I/usr/include -I/usr/include/x86_64-linux-gnu"` required for cargo commands due to frida-sys bindgen.
- Fixed plan's `BusName::from_str_unchecked` → `BusName::try_from` (actual zbus API).
- Fixed plan's `atspi::Role::PushButton` → `Role::Button`, `Role::StaticText` → `Role::Static` (actual atspi API).
- Fixed plan's `n_actions()` → `nactions()` (actual atspi ActionProxy API).
- Fixed plan's `InterfaceSet` iteration — used `.contains(Interface::Action)` instead of string comparison.
