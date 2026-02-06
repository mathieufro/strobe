# Lazy DWARF Parsing

**Date:** 2026-02-06
**Status:** Approved
**Commit strategy:** Single commit at the end

## Problem

DWARF parsing takes **19.5 seconds** (95% of launch time) for a 79MB binary with 141MB dSYM containing 104,714 functions. The recommended workflow — launch with no patterns, read stderr first — doesn't need DWARF at all. This makes zero-pattern launches unacceptably slow.

## Goal

Zero-pattern launches complete in ~1 second. DWARF is parsed in the background and ready by the time the user calls `debug_trace` to add patterns.

## Architecture

```
BEFORE:  debug_launch → DwarfParser::parse() [19.5s] → Frida spawn [1s] → response
AFTER:   debug_launch → extract_image_base() [<10ms] → Frida spawn [1s] → response
                         └→ spawn_blocking(parse) [19.5s, background] → cached
```

## Changes

### 1. `src/dwarf/parser.rs` — Make `extract_image_base` public
- Line 54: `fn` → `pub fn`

### 2. `src/dwarf/handle.rs` — NEW: `DwarfHandle` type
- `tokio::sync::watch` channel wrapping `Option<Result<Arc<DwarfParser>, String>>`
- `spawn_parse(binary_path)` — starts `spawn_blocking`, returns immediately
- `ready(dwarf)` — wraps already-parsed result (cache hit)
- `get()` — async, awaits parse completion, returns `Result<Arc<DwarfParser>>`

Why `watch`: cloneable, retains last value for late subscribers, async-aware.
Why `String` error: `crate::Error` isn't `Clone`.

### 3. `src/dwarf/mod.rs` — Add module export
- `mod handle; pub use handle::DwarfHandle;`

### 4. `src/daemon/session_manager.rs` — DWARF cache + cheap launch
- Change `dwarf_cache` type: `HashMap<String, Arc<DwarfParser>>` → `HashMap<String, DwarfHandle>`
- New `get_or_start_dwarf_parse()`: check cache, if miss start background parse, cache handle, return immediately
- Rewrite `spawn_with_frida()`: call `extract_image_base()` (~10ms), get handle, pass both to spawner

### 5. `src/frida_collector/spawner.rs` — Deferred DWARF in sessions
- `FridaSession.dwarf: Option<Arc<DwarfParser>>` → `dwarf_handle: DwarfHandle` + `image_base: u64`
- `spawn()` accepts `dwarf_handle` + `image_base` instead of doing its own parsing
- `add_patterns()` / `remove_patterns()` await `session.dwarf_handle.get()` before resolving

### 6. `src/daemon/server.rs` — Deferred hook installation
- Always pass `&[]` for initial_patterns (launch is always fast)
- After launch, if pending patterns exist: `tokio::spawn` background task calling `update_frida_patterns()`
- Hooks arrive after DWARF parse completes

## Thread Safety
- `DwarfHandle` uses `tokio::sync::watch` — `Send + Sync + Clone`
- `dwarf_cache` std `RwLock` holds only short HashMap lookups
- Frida worker thread unchanged — receives resolved `FunctionTarget` vec
- Background `tokio::spawn` for deferred hooks acquires `frida_spawner` write lock after spawn releases it

## Error Handling
- DWARF parse fails → `add_patterns()` returns error, session stays valid (output capture works)
- `spawn_blocking` panics → watch sender drops → `get()` returns error
- `extract_image_base` fails → defaults to 0

## Verification
1. `cargo test` — all existing tests pass
2. `cargo build` — clean compile
3. Launch via MCP with no patterns → ~1s response
4. `debug_trace` with patterns on running session → hooks install after DWARF ready
5. Relaunch same binary → DWARF cache hit (instant)
