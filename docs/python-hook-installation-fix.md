# Python Hook Installation Fix

**Date:** 2026-02-11
**Issue:** Python function hooks matched patterns (matched=4) but weren't being installed (installed=0)
**Status:** FIXED

## Root Cause

The FridaSpawner was sending all targets (both native and interpreted) in the `functions` JSON array. The agent expected interpreted language targets in a separate `targets` array.

###  Original Flow (Broken)

1. PythonResolver resolves pattern `modules.timing.*` → 4 functions with file:line info
2. Spawner creates FunctionTarget structs with `address: 0` for Python targets
3. `handle_add_patterns()` sent ALL targets in `functions` array:
   ```json
   {
     "type": "hooks",
     "action": "add",
     "functions": [
       {"address": "0x0", "name": "fast", "sourceFile": "timing.py", "lineNumber": 4},
       ...
     ]
   }
   ```
4. Agent's `handleMessage()` tried to install these as address-based hooks
5. PythonTracer couldn't install hooks without proper file:line targets
6. Result: matched=4, installed=0

## Fix

Modified `handle_add_patterns()` in `src/frida_collector/spawner.rs` to split targets:

```rust
// Split targets: native (address > 0) vs interpreted (address == 0)
let mut native_funcs: Vec<serde_json::Value> = Vec::new();
let mut interpreted_targets: Vec<serde_json::Value> = Vec::new();

for f in functions {
    if f.address == 0 {
        // Interpreted language target (Python, JS, etc.)
        interpreted_targets.push(serde_json::json!({
            "file": f.source_file,
            "line": f.line_number,
            "name": f.name,
        }));
    } else {
        // Native binary target
        native_funcs.push(serde_json::json!({
            "address": format!("0x{:x}", f.address),
            "name": f.name,
            ...
        }));
    }
}

// Build message with separate arrays
let mut hooks_msg = serde_json::json!({
    "type": "hooks",
    "action": "add",
    "mode": mode_str,
});

if !native_funcs.is_empty() {
    hooks_msg["functions"] = serde_json::json!(native_funcs);
    hooks_msg["imageBase"] = serde_json::json!(format!("0x{:x}", image_base));
}

if !interpreted_targets.is_empty() {
    hooks_msg["targets"] = serde_json::json!(interpreted_targets);
}
```

### New Flow (Fixed)

1. PythonResolver resolves pattern `modules.timing.*` → 4 functions with file:line info
2. Spawner creates FunctionTarget structs with `address: 0`
3. `handle_add_patterns()` splits based on address:
   - address==0 → `interpreted_targets` array
   - address>0 → `native_funcs` array
4. Sends JSON with `targets` field for interpreted:
   ```json
   {
     "type": "hooks",
     "action": "add",
     "mode": "full",
     "targets": [
       {"file": "timing.py", "line": 4, "name": "fast"},
       {"file": "timing.py", "line": 7, "name": "medium"},
       {"file": "timing.py", "line": 10, "name": "slow"},
       {"file": "timing.py", "line": 13, "name": "very_slow"}
     ]
   }
   ```
5. Agent's `handleMessage()` checks for `message.targets` (line 388)
6. Calls `tracer.installHook()` with file:line targets
7. PythonTracer installs hooks via frame evaluation
8. Result: matched=4, installed=4 ✅

## Files Changed

- `src/frida_collector/spawner.rs`: Modified `handle_add_patterns()` to split targets
- `agent/src/agent.ts`: Already had support for `message.targets` (no changes needed)
- `agent/src/tracers/python-tracer.ts`: Already implemented (no changes needed)

## Testing

Run Python comprehensive tests:
```bash
cargo test --release test_python_comprehensive -- --nocapture
```

Expected output for Test 2/8:
```
Hook result: installed=4 matched=4
✓ Python function tracing working
```

## Notes

- The agent already had infrastructure for interpreted targets (added in earlier commits)
- The HookInstruction interface already defined the `targets` field (line 11-15 of agent.ts)
- This fix completes the Python hook installation pipeline
- No changes needed to PythonTracer - it was already correctly implemented
