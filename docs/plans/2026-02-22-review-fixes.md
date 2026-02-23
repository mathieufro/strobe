# Dev Branch Review Fixes — Implementation Plan

**Spec:** `docs/reviews/2026-02-22-dev-branch-review.md`
**Goal:** Fix all 33 identified issues (8 critical, 11 important, 14 lower) before merging `dev` into `main`.
**Architecture:** Targeted surgical fixes across Rust daemon, TypeScript agent, and integration tests. No architectural changes.
**Commit strategy:** Single commit at the end.

## Workstreams

Files are mostly independent — group by language/layer for efficient parallel work:

- **Stream A (Rust daemon):** Tasks 1, 3, 4, 5, 8, 12 — `session_manager.rs`, `server.rs`, `event.rs`
- **Stream B (Rust resolvers):** Tasks 6, 7, 9, 10 — `python_resolver.rs`, `js_resolver.rs`
- **Stream C (TypeScript agent):** Tasks 2, 11, 13, 14, 15, 16, 17, 18, 19 — `jsc-tracer.ts`, `v8-tracer.ts`, `python-tracer.ts`, `agent.ts`
- **Stream D (Rust test adapters):** Tasks 20, 21 — `bun_adapter.rs`, `jest_adapter.rs`
- **Stream E (Rust spawner):** Tasks 22, 23 — `spawner.rs`
- **Stream F (Integration tests):** Tasks 24, 25, 26, 27 — `tests/*.rs`
- **Serial:** Task 28 (agent rebuild + touch spawner.rs) depends on all Stream C work being done.

---

### Task 1: C1 — Fix memory leak: clean `languages` and `resolvers` maps on session stop

**Files:**
- Modify: `src/daemon/session_manager.rs:328-336` (stop_session) and `360-367` (stop_session_retain)

**Implementation:**

In `stop_session`, after line 336 (`write_lock(&self.paused_threads).remove(id);`), add:
```rust
write_lock(&self.languages).remove(id);
write_lock(&self.resolvers).remove(id);
```

In `stop_session_retain`, after line 367 (`write_lock(&self.paused_threads).remove(id);`), add:
```rust
write_lock(&self.languages).remove(id);
write_lock(&self.resolvers).remove(id);
```

**Checkpoint:** Both stop methods clean up all 10 in-memory maps.

---

### Task 2: C4 — Fix JSC tracer: emit events only for matching hooks, add exit events

**Files:**
- Modify: `agent/src/tracers/jsc-tracer.ts:41-51` (Interceptor.attach) and `83-101` (tryEmitForJscFunction)

**Implementation:**

The current code emits an event for the *first* hook in the Map regardless of which JS function was called. Since we can't yet navigate JSC structs to extract the function name (requires version-specific offsets), store the `fnPtr` on `onEnter` and emit an exit event on `onLeave`.

Also add an early return in `onEnter`/`onLeave` when no hooks are installed (L7 fast-path fix).

Replace the Interceptor.attach block (lines 41-51):
```typescript
this.interceptor = Interceptor.attach(hookTarget, {
  onEnter(args) {
    if (self.hooks.size === 0) return; // L7: fast-path when no hooks
    const fnPtr = args[1];
    (this as any)._strobeFnPtr = fnPtr;
    self.tryEmitForJscFunction(fnPtr, 'entry');
  },
  onLeave(_retval) {
    if (self.hooks.size === 0) return; // L7: fast-path
    const fnPtr = (this as any)._strobeFnPtr;
    if (fnPtr) self.tryEmitForJscFunction(fnPtr, 'exit');
  }
});
```

Replace `tryEmitForJscFunction` (lines 83-101). Since we can't identify which JSC function is being called without struct navigation, only emit if there's exactly one hook (common case). With multiple hooks, we can't attribute correctly so skip:
```typescript
private tryEmitForJscFunction(fnPtr: NativePointer, event: 'entry' | 'exit'): void {
  // Without JSC struct navigation (version-specific offsets), we cannot identify
  // which function is being called. Only emit when there's a single hook so
  // attribution is unambiguous. With multiple hooks, skip until Phase 2.
  if (this.hooks.size !== 1) return;

  const [funcId, hook] = this.hooks.entries().next().value;
  this.eventBuffer.push({
    id: `${this.sessionId}-jsc-${++this.eventIdCounter}`,
    sessionId: this.sessionId,
    timestampNs: Date.now() * 1_000_000,
    threadId: Process.getCurrentThreadId(),
    eventType: event === 'entry' ? 'function_enter' : 'function_exit',
    functionName: hook.target.name,
    sourceFile: hook.target.file,
    lineNumber: hook.target.line,
    pid: Process.id,
  });
  if (this.eventBuffer.length >= 50) this.flushEvents();
}
```

**Checkpoint:** JSC tracer emits paired enter/exit events, doesn't mis-attribute when multiple hooks present, and has empty-hook fast path.

---

### Task 3: I3 — Fix event count: move count_session_events after writer flush

**Files:**
- Modify: `src/daemon/session_manager.rs:312-313` and `343-344`

**Implementation:**

In both `stop_session` and `stop_session_retain`, move the `count_session_events` call to *after* the writer flush completes.

For `stop_session` — move `let count = ...` from line 313 to after line 323 (after `handle.await`):
```rust
pub async fn stop_session(&self, id: &str) -> Result<u64> {
    // Phase 1: Signal database writer task to flush and exit
    if let Some(cancel_tx) = write_lock(&self.writer_cancel_tokens).remove(id) {
        let _ = cancel_tx.send(true);
    }

    // Phase 2: Wait for writer task to complete (ensures all events flushed)
    if let Some(handle) = self.writer_handles.write().await.remove(id) {
        let _ = handle.await;
    }

    // Phase 3: Count events AFTER flush so the count is accurate
    let count = self.db.count_session_events(id)?;

    // Phase 4: Now safe to delete session
    self.db.delete_session(id)?;
    // ... cleanup ...
```

Same pattern for `stop_session_retain`:
```rust
pub async fn stop_session_retain(&self, id: &str) -> Result<u64> {
    if let Some(cancel_tx) = write_lock(&self.writer_cancel_tokens).remove(id) {
        let _ = cancel_tx.send(true);
    }
    if let Some(handle) = self.writer_handles.write().await.remove(id) {
        let _ = handle.await;
    }
    let count = self.db.count_session_events(id)?;
    self.db.mark_session_stopped(id)?;
    // ... cleanup ...
```

**Checkpoint:** Event counts reflect all flushed events.

---

### Task 4: I2 — Validate `symbolsPath` against path traversal

**Files:**
- Modify: `src/daemon/server.rs:1000` (after projectRoot validation)

**Implementation:**

After the `req.project_root.contains("..")` check (line 996-999), add:
```rust
if let Some(ref sp) = req.symbols_path {
    if sp.contains("..") {
        return Err(crate::Error::ValidationError(
            "symbolsPath must not contain '..' components".to_string()
        ));
    }
}
```

**Checkpoint:** `symbolsPath` has same traversal protection as `command` and `projectRoot`.

---

### Task 5: I4 — Preserve `no_slide` flag on watch removal

**Files:**
- Modify: `src/daemon/session_manager.rs:78-90` (ActiveWatchState struct)
- Modify: `src/daemon/server.rs:1440-1452` (watch removal re-send)

**Implementation:**

Add `no_slide` field to `ActiveWatchState`:
```rust
#[derive(Clone)]
pub struct ActiveWatchState {
    pub label: String,
    pub address: u64,
    pub size: u8,
    pub type_kind_str: String,
    pub deref_depth: u8,
    pub deref_offset: u64,
    pub type_name: Option<String>,
    pub on_patterns: Option<Vec<String>>,
    pub is_expr: bool,
    pub expr: Option<String>,
    pub no_slide: bool,  // NEW
}
```

Then find every place `ActiveWatchState` is constructed and add `no_slide`. There are two locations:
1. `server.rs` around line 1298 where `no_slide: true` is set — add `no_slide: true` to the `ActiveWatchState` push.
2. `server.rs` around line 1402 where `no_slide: false` — add `no_slide: false`.

Then fix line 1450 in the removal path to use the stored value:
```rust
no_slide: w.no_slide,
```
instead of:
```rust
no_slide: false,
```

Search for all `ActiveWatchState` constructions to ensure every one sets `no_slide`.

**Checkpoint:** Watch `no_slide` flag survives removal of sibling watches.

---

### Task 6: C6 — Fix Python nested function name double-prefix

**Files:**
- Modify: `src/symbols/python_resolver.rs:135-136, 151-152`

**Implementation:**

In `extract_from_stmt`, for both `FunctionDef` and `AsyncFunctionDef` arms, change:
```rust
new_prefix.push(qualified_name);
```
to:
```rust
new_prefix.push(f.name.to_string());
```

This occurs at lines 136 and 152. The `qualified_name` is the full dotted path (e.g., `outer.inner`), but the prefix should only accumulate simple names.

**Verification:** The existing test `test_extract_nested_functions` should still pass — it checks `outer.inner` exists. Add a depth-3 test case:
```rust
#[test]
fn test_extract_deeply_nested_functions() {
    let source = r#"
def outer():
    def inner():
        def deepest():
            pass
        return deepest
    return inner
"#;
    let functions = extract_functions_from_source(source, Path::new("deep.py")).unwrap();
    assert!(functions.contains_key("outer"));
    assert!(functions.contains_key("outer.inner"));
    assert!(functions.contains_key("outer.inner.deepest"),
        "Depth-3 nesting should be outer.inner.deepest, got: {:?}", functions.keys().collect::<Vec<_>>());
}
```

**Checkpoint:** `outer -> inner -> deepest` produces `outer.inner.deepest`, not `outer.outer.inner.deepest`.

---

### Task 7: C7 — Fix Python `*.egg-info` exclusion to use `ends_with`

**Files:**
- Modify: `src/symbols/python_resolver.rs:16-22`

**Implementation:**

Replace the `is_python_excluded` function:
```rust
fn is_python_excluded(name: &str) -> bool {
    matches!(name,
        "__pycache__" | "venv" | ".venv" | "env" | ".env" |
        "node_modules" | ".git" | ".tox" | ".mypy_cache" |
        ".pytest_cache" | "dist" | "build"
    ) || name.ends_with(".egg-info") || name.ends_with(".dist-info")
}
```

Update the existing test to cover the new behavior:
```rust
#[test]
fn test_excluded_directories() {
    assert!(is_python_excluded("__pycache__"));
    assert!(is_python_excluded("venv"));
    assert!(is_python_excluded(".venv"));
    assert!(is_python_excluded("node_modules"));
    assert!(is_python_excluded(".git"));
    assert!(is_python_excluded("mypackage.egg-info"));
    assert!(is_python_excluded("foo.dist-info"));
    assert!(!is_python_excluded("modules"));
    assert!(!is_python_excluded("tests"));
}
```

**Checkpoint:** Real `.egg-info` / `.dist-info` directories are excluded from scanning.

---

### Task 8: I8 — Fix FIFO eviction to count only evictable events

**Files:**
- Modify: `src/db/event.rs:596-627`

**Implementation:**

Change the count query to count only evictable event types, so `to_delete` is computed accurately. Replace the `current_count` query:

```rust
// Count current evictable events only (not stdout/stderr/crash/etc.)
for event in events {
    if !session_counts.contains_key(&event.session_id) {
        let count: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM events WHERE session_id = ? AND event_type IN ({})",
                EVICTABLE_TYPES
            ),
            params![&event.session_id],
            |row| row.get(0),
        )?;
        session_counts.insert(event.session_id.clone(), count as usize);
    }
}
```

Also count only evictable events in the incoming batch:
```rust
let new_evictable = session_events.iter().filter(|e| {
    matches!(e.event_type, EventType::FunctionEnter | EventType::FunctionExit | EventType::VariableSnapshot)
}).count();
let new_count = current_count + new_evictable;
```

**Checkpoint:** FIFO buffer correctly maintains its limit when many stdout/stderr events exist.

---

### Task 9: C8 — Remove `dist` from JS resolver SKIP_DIRS

**Files:**
- Modify: `src/symbols/js_resolver.rs:10-13`

**Implementation:**

Remove `"dist"` from `SKIP_DIRS`:
```rust
const SKIP_DIRS: &[&str] = &[
    "node_modules", "build", ".git", ".next", ".nuxt",
    "coverage", "__pycache__", ".cache", ".turbo", ".svelte-kit",
];
```

Update the test `test_skips_excluded_dirs` to remove `"dist"` from the skip assertion list. The `dist` directory should now be indexed for source maps.

**Checkpoint:** Source map files in `dist/` are indexed; the `test_sourcemap_resolution` test should now reliably resolve.

---

### Task 10: L1-L4 — Fix JS resolver parser edge cases

**Files:**
- Modify: `src/symbols/js_resolver.rs:42-92` (extract_functions_from_source)

**Implementation:**

**L1 (pattern_to_regex → PatternMatcher):** Replace `pattern_to_regex` with the project-standard `PatternMatcher`:
```rust
fn pattern_to_regex(pattern: &str) -> crate::Result<regex::Regex> {
    // Reuse the project-standard PatternMatcher logic.
    // '.' is the separator for JS (Class.method).
    let matcher = crate::dwarf::PatternMatcher::new_with_separator(pattern, '.');
    // PatternMatcher already handles * (shallow) and ** (deep).
    // Convert to regex for compatibility with the resolve_pattern filter.
    let regex_str = matcher.to_regex_string();
    regex::Regex::new(&regex_str)
        .map_err(|e| crate::Error::Internal(format!("Bad JS pattern '{}': {}", pattern, e)))
}
```

Wait — `PatternMatcher` doesn't expose `to_regex_string()`. Check if we can use it directly instead of regex. Looking at the python_resolver, it calls `matcher.matches(name)` in the filter. Let's do the same for JS:

Replace the `resolve_pattern` method to use `PatternMatcher` directly:
```rust
fn resolve_pattern(&self, pattern: &str, _root: &Path) -> crate::Result<Vec<ResolvedTarget>> {
    if let Some(file_pattern) = pattern.strip_prefix("@file:") {
        return Ok(self.functions.iter()
            .filter(|(_, (file, _))| file.to_string_lossy().contains(file_pattern))
            .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
                file: file.to_string_lossy().to_string(),
                line: *line,
                name: name.clone(),
            })
            .collect());
    }

    let matcher = crate::dwarf::PatternMatcher::new_with_separator(pattern, '.');
    Ok(self.functions.iter()
        .filter(|(name, _)| matcher.matches(name))
        .map(|(name, (file, line))| ResolvedTarget::SourceLocation {
            file: file.to_string_lossy().to_string(),
            line: *line,
            name: name.clone(),
        })
        .collect())
}
```

Remove the now-unused `pattern_to_regex` function and the `use regex::Regex;` if no longer needed at the module level (keep it for `extract_functions_from_source` which uses it).

**L2 (// inside string literals):** Improve the comment-stripping line:
```rust
// Strip single-line comments, but not // inside string literals
let stripped = strip_line_comment(line);
```

Add a helper function:
```rust
/// Strip `//` comments, but only those not inside string literals.
fn strip_line_comment(line: &str) -> &str {
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_backtick = false;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'\\' && i + 1 < bytes.len() {
            i += 2; // skip escaped character
            continue;
        }
        match ch {
            b'\'' if !in_double_quote && !in_backtick => in_single_quote = !in_single_quote,
            b'"' if !in_single_quote && !in_backtick => in_double_quote = !in_double_quote,
            b'`' if !in_single_quote && !in_double_quote => in_backtick = !in_backtick,
            b'/' if !in_single_quote && !in_double_quote && !in_backtick
                    && i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
        i += 1;
    }
    line
}
```

**L3 (block comment handling):** Fix the block comment handling to process code after `*/`:
```rust
if in_block_comment {
    if let Some(end_idx) = line.find("*/") {
        in_block_comment = false;
        // Process remainder of line after */
        let remainder = &line[end_idx + 2..];
        // Fall through to process remainder (simplified: just update brace tracking)
        let opens = remainder.chars().filter(|&c| c == '{').count() as i32;
        let closes = remainder.chars().filter(|&c| c == '}').count() as i32;
        brace_depth += opens - closes;
        if brace_depth < 0 { brace_depth = 0; }
        class_stack.retain(|(_, depth)| brace_depth > *depth);
    }
    continue;
}
// Handle block comment start
if let Some(start_idx) = stripped.find("/*") {
    if !stripped[start_idx..].contains("*/") {
        in_block_comment = true;
    }
}
```

**L4 (exclude `constructor`):** Add `"constructor"` to the keyword set:
```rust
let kw: std::collections::HashSet<&str> = [
    "if", "for", "while", "switch", "catch", "return", "throw", "delete",
    "typeof", "instanceof", "new", "import", "export", "default", "class",
    "const", "let", "var", "async", "await", "yield", "function", "try",
    "else", "do", "in", "of", "from", "with", "void", "case", "constructor",
].iter().copied().collect();
```

**Checkpoint:** JS parser correctly handles comments in strings, block comments, constructors, and uses project-standard PatternMatcher.

---

### Task 11: C5 — Fix V8 tracer `wrapObject` prefix propagation

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts:140-216`

**Implementation:**

Fix `wrapModuleExports` to pass the module name as prefix, and fix `wrapObject` recursion to propagate the qualified name:

```typescript
private wrapModuleExports(exports: any, filename: string): void {
  if (!exports || typeof exports !== 'object' && typeof exports !== 'function') return;
  this.wrapObject(exports, filename, '');
}
```

In the `wrap` closure inside `wrapObject`, fix the recursion to pass the qualified name as prefix:
```typescript
// Recurse into plain objects (e.g. class instances, namespace objects)
if (typeof val === 'object' && !seen.has(val)) {
  seen.add(val);
  for (const k of Object.keys(val)) {
    wrap(val, k, depth + 1);
  }
}
```

The issue is that the recursive call to `wrap` still uses the parent's `prefix`. For nested objects like `Calculator.add`, we need to pass the full qualified name down. But `wrap` is a closure that captures `prefix` from the outer `wrapObject`. The fix is to make `wrapObject` recursive on itself:

Replace the `wrap` closure's recursion and the prototype iteration to pass `qualifiedName` as the new prefix for nested objects:

In `wrapObject` (lines 145-216), change the recursive descent:
```typescript
private wrapObject(obj: any, filename: string, prefix: string): void {
  if (!obj) return;
  const seen = new Set<any>();

  const processKey = (container: any, key: string, depth: number, currentPrefix: string) => {
    if (depth > 3) return;
    const val = container[key];
    if (typeof val === 'function' && !this.wrappedFns.has(val)) {
      const qualifiedName = currentPrefix ? `${currentPrefix}.${key}` : key;

      // Find matching hook
      let matchedHook: V8Hook | null = null;
      for (const [, hook] of this.hooks) {
        if (!this.fileMatches(filename, hook.target)) continue;
        if (hook.target.name === qualifiedName || hook.target.name === key) {
          matchedHook = hook;
          break;
        }
      }

      if (matchedHook) {
        const hook = matchedHook;
        const self = this;
        const wrapped = new Proxy(val, {
          apply(target, thisArg, args) {
            self.emitEvent(hook.funcId, hook, filename, 'entry');
            let result: any;
            try {
              result = Reflect.apply(target, thisArg, args);
            } catch (e) {
              self.emitEvent(hook.funcId, hook, filename, 'exit');
              throw e;
            }
            if (result && typeof result.then === 'function') {
              return result.then((v: any) => {
                self.emitEvent(hook.funcId, hook, filename, 'exit');
                return v;
              }, (e: any) => {
                self.emitEvent(hook.funcId, hook, filename, 'exit');
                throw e;
              });
            }
            self.emitEvent(hook.funcId, hook, filename, 'exit');
            return result;
          }
        });
        this.wrappedFns.add(val);
        try { container[key] = wrapped; } catch {}
      }
    }

    // Recurse into plain objects
    if (typeof val === 'object' && val !== null && !seen.has(val)) {
      seen.add(val);
      const childPrefix = currentPrefix ? `${currentPrefix}.${key}` : key;
      for (const k of Object.keys(val)) {
        processKey(val, k, depth + 1, childPrefix);
      }
    }
  };

  for (const key of Object.keys(obj)) {
    processKey(obj, key, 0, prefix);
  }
  if (typeof obj === 'function' && obj.prototype) {
    for (const key of Object.getOwnPropertyNames(obj.prototype)) {
      if (key !== 'constructor') processKey(obj.prototype, key, 0, prefix);
    }
  }
}
```

**Checkpoint:** Pattern `Calculator.add` correctly matches `Calculator.add` via qualified name, not just any function named `add`.

---

### Task 12: L12 — Pass `symbolsPath` through on DWARF re-parse after cache eviction

**Files:**
- Modify: `src/daemon/session_manager.rs:449-483`

**Implementation:**

The issue is that `get_or_start_dwarf_parse_with_symbols` stores the `symbols_path` in the `DwarfHandle::spawn_parse` call, but when a failed parse is evicted and retried, the new caller might use `get_or_start_dwarf_parse` (without symbols) which passes `None`.

The fix is to include `symbols_path` in the cache key so that a re-parse with symbols gets a fresh entry:

```rust
pub fn get_or_start_dwarf_parse_with_symbols(&self, binary_path: &str, search_root: Option<&str>, symbols_path: Option<&str>) -> DwarfHandle {
    let mtime = std::fs::metadata(binary_path)
        .and_then(|m| m.modified())
        .ok();
    let cache_key = match (mtime, symbols_path) {
        (Some(t), Some(sp)) => format!("{}@{}@sym:{}", binary_path, t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs(), sp),
        (Some(t), None) => format!("{}@{}", binary_path, t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()),
        (None, Some(sp)) => format!("{}@sym:{}", binary_path, sp),
        (None, None) => binary_path.to_string(),
    };
    // ... rest unchanged
```

**Checkpoint:** Re-parses after cache eviction use the correct `symbolsPath`.

---

### Task 13: I5 — Fix Python GIL deadlock at breakpoints

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:247-254` (breakpoint wait logic in Python code)

**Implementation:**

The deadlock: Python thread holds GIL → hits breakpoint → calls `_strobe_bp_event.wait()` (still holding GIL) → Frida JS thread calls `runPython()` → calls `PyGILState_Ensure()` → deadlocks.

Fix: Release the GIL before waiting. Python's `threading.Event.wait()` already releases the GIL internally when called from Python, but the issue is that the `eval()` call in the trace function holds the GIL at the C level. We need to explicitly drop it before waiting.

Replace the breakpoint section in the generated Python code (around line 249-254):
```python
for bp_file, bp_line, bp_id, bp_cond in _strobe_breakpoints:
    if fname.endswith(bp_file) and fline == bp_line:
        if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
            _strobe_bp_hit_cb(bp_id.encode(), fline)
            import _thread
            _strobe_bp_event.wait()
            _strobe_bp_event.clear()
        break
```

Actually, `threading.Event.wait()` already releases the GIL via its internal `_cond.wait()` → `Condition.wait()` → `waiter.acquire()` path, which drops the GIL (C-level lock acquire releases GIL in CPython). The problem is that `runPython` calls `PyGILState_Ensure` which tries to acquire the GIL, and the GIL IS released during `Event.wait()`, so `PyGILState_Ensure` should succeed.

Let me re-analyze: The actual issue is more subtle. The Frida JS flush timer fires on the Frida JS thread. If it calls `runPython()` for data sync while a Python thread is paused at `Event.wait()`, the `PyGILState_Ensure` call should succeed because `Event.wait()` releases the GIL.

However, there's a race with the 50ms flush timer calling `syncTraceHooks()` → `runPython()` to update hook data. The `runPython` does `PyGILState_Ensure` → `PyRun_SimpleString` → `PyGILState_Release`. If Python is single-threaded (no threading module), the GIL is never released during `Event.wait()` because there's no contention.

The real fix is to use a per-breakpoint event and release the GIL explicitly. Use `ctypes` to call `PyEval_SaveThread()` before waiting and `PyEval_RestoreThread()` after:

Replace the breakpoint wait section in the Python trace function:
```python
for bp_file, bp_line, bp_id, bp_cond in _strobe_breakpoints:
    if fname.endswith(bp_file) and fline == bp_line:
        if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
            _strobe_bp_hit_cb(bp_id.encode(), fline)
            # Release GIL before blocking so Frida can call runPython
            _tstate = ctypes.pythonapi.PyEval_SaveThread()
            _strobe_bp_event.wait()
            ctypes.pythonapi.PyEval_RestoreThread(_tstate)
            _strobe_bp_event.clear()
        break
```

Also add `ctypes.pythonapi.PyEval_SaveThread.restype = ctypes.c_void_p` and `ctypes.pythonapi.PyEval_RestoreThread.argtypes = [ctypes.c_void_p]` to the initialization code (before the `_strobe_trace` definition).

This also fixes L8 (single global event) partially — threads release the GIL, so other threads can proceed.

**Checkpoint:** Python breakpoint pause releases GIL, preventing deadlock with Frida agent thread.

---

### Task 14: I6 — Enforce Python `hitCount` in trace function

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:162-166` (bpEntries serialization)
- Modify: Python trace function code in `syncTraceHooks` (lines 228-254)

**Implementation:**

Add hitCount to the serialized breakpoint data:
```typescript
bpEntries.push(`('${file}', ${bp.line}, '${bp.id}', '${cond}', ${bp.hitCount || 0})`);
```

Update the Python trace function's breakpoint section to check hit count:
```python
for bp_file, bp_line, bp_id, bp_cond, bp_hit_count in _strobe_breakpoints:
    if fname.endswith(bp_file) and fline == bp_line:
        if bp_hit_count > 0:
            _strobe_bp_hits = getattr(_b, '_strobe_bp_hits', {})
            _strobe_bp_hits[bp_id] = _strobe_bp_hits.get(bp_id, 0) + 1
            setattr(_b, '_strobe_bp_hits', _strobe_bp_hits)
            if _strobe_bp_hits[bp_id] < bp_hit_count:
                break
        if not bp_cond or eval(bp_cond, frame.f_globals, frame.f_locals):
            _strobe_bp_hit_cb(bp_id.encode(), fline)
            _tstate = ctypes.pythonapi.PyEval_SaveThread()
            _strobe_bp_event.wait()
            ctypes.pythonapi.PyEval_RestoreThread(_tstate)
            _strobe_bp_event.clear()
        break
```

**Checkpoint:** Python breakpoints with `hitCount` only fire at the Nth invocation.

---

### Task 15: I7 — Fix Python `writeVariable` code injection

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:581-587`

**Implementation:**

Replace the `writeVariable` method:
```typescript
writeVariable(expr: string, value: any): void {
  const safeValue = JSON.stringify(value);
  const safeExpr = expr.replace(/'/g, "\\'");
  const result = this.runPython(
    `import json as _j; ${expr} = _j.loads('${safeValue.replace(/'/g, "\\'")}')`
  );
  if (result !== 0) {
    throw new Error(`Failed to write: ${expr}`);
  }
}
```

This serializes the value as JSON and uses `json.loads()` to safely deserialize it in Python, preventing code injection via string interpolation.

**Checkpoint:** `writeVariable("x", 'foo"; import os; os.system("cmd")')` sets `x` to the literal string without executing injected code.

---

### Task 16: L5 — Fix V8 tracer `writeVariable` error reporting

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts:122-126`

**Implementation:**

Replace:
```typescript
writeVariable(expr: string, value: any): void {
  try {
    new Function('__v', `${expr} = __v`)(value);
  } catch (e) {
    throw new Error(`Failed to write '${expr}': ${e}`);
  }
}
```

**Checkpoint:** `writeVariable` errors are propagated instead of silently swallowed.

---

### Task 17: L6 — Fix V8/JSC timestamp resolution

**Files:**
- Modify: `agent/src/tracers/v8-tracer.ts:223`
- Modify: `agent/src/tracers/jsc-tracer.ts:91`

**Implementation:**

Use `performance.now()` if available (Frida provides it in V8 runtime) for microsecond precision:
```typescript
// In both tracers, replace:
timestampNs: Date.now() * 1_000_000,
// With:
timestampNs: Math.round(performance.now() * 1_000_000),
```

Note: `performance.now()` returns milliseconds with microsecond precision as a float. If not available in QuickJS (JSC tracer), fall back to `Date.now() * 1_000_000`.

For JSC tracer (QuickJS runtime), `performance` may not be available — use a conditional:
```typescript
timestampNs: typeof performance !== 'undefined' ? Math.round(performance.now() * 1_000_000) : Date.now() * 1_000_000,
```

**Checkpoint:** Timestamps have microsecond resolution when platform supports it.

---

### Task 18: L9 — Fix Python `removeAllHooks` to clear breakpoints/logpoints

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:398-404`

**Implementation:**

Replace `removeAllHooks`:
```typescript
removeAllHooks(): void {
  this.hooks.clear();
  this.breakpoints.clear();
  this.logpoints.clear();
  if (this.traceInstalled) {
    this.runPython('import sys; sys.settrace(None)');
    this.traceInstalled = false;
  }
}
```

**Checkpoint:** `removeAllHooks` also clears breakpoints and logpoints from state.

---

### Task 19: L10 — Cache `NativeFunction` objects in Python tracer

**Files:**
- Modify: `agent/src/tracers/python-tracer.ts:283-299`

**Implementation:**

Cache the `NativeFunction` objects as instance fields instead of recreating on every `runPython` call:

Add fields after the CPython API pointers (after line 60):
```typescript
// Cached NativeFunction wrappers (created once, reused)
private _gilEnsure: NativeFunction | null = null;
private _gilRelease: NativeFunction | null = null;
private _pyRun: NativeFunction | null = null;
```

Replace `runPython`:
```typescript
private runPython(code: string): number {
  if (!this.PyRun_SimpleString || !this.PyGILState_Ensure || !this.PyGILState_Release) {
    return -1;
  }

  if (!this._gilEnsure) {
    this._gilEnsure = new NativeFunction(this.PyGILState_Ensure, 'int', []);
    this._gilRelease = new NativeFunction(this.PyGILState_Release, 'void', ['int']);
    this._pyRun = new NativeFunction(this.PyRun_SimpleString, 'int', ['pointer']);
  }

  const gilState = this._gilEnsure!();
  try {
    const codeBuf = Memory.allocUtf8String(code);
    return this._pyRun!(codeBuf) as number;
  } finally {
    this._gilRelease!(gilState);
  }
}
```

**Checkpoint:** `runPython` no longer allocates new `NativeFunction` wrappers on each call.

---

### Task 20: C2 + C3 + I10 — Fix Bun test adapter

**Files:**
- Modify: `src/test/bun_adapter.rs`

**Implementation:**

**C2 — JUnit output:** Add `--reporter-outfile=/dev/stdout` to both commands:
```rust
fn suite_command(
    &self,
    _project_root: &Path,
    _level: Option<TestLevel>,
    _env: &HashMap<String, String>,
) -> crate::Result<TestCommand> {
    Ok(TestCommand {
        program: "bun".to_string(),
        args: vec![
            "test".to_string(),
            "--reporter=junit".to_string(),
            "--reporter-outfile=/dev/stdout".to_string(),
        ],
        env: HashMap::new(),
    })
}
```

**C3 — Test name filter:** Use `--test-name-pattern` instead of positional arg:
```rust
fn single_test_command(&self, _project_root: &Path, test_name: &str) -> crate::Result<TestCommand> {
    Ok(TestCommand {
        program: "bun".to_string(),
        args: vec![
            "test".to_string(),
            "--reporter=junit".to_string(),
            "--reporter-outfile=/dev/stdout".to_string(),
            "--test-name-pattern".to_string(),
            test_name.to_string(),
        ],
        env: HashMap::new(),
    })
}
```

**I10 — Detection false-positives:** Lower bun.lockb confidence and check for Vitest/Jest first:
```rust
fn detect(&self, project_root: &Path, _command: Option<&str>) -> u8 {
    // Check for Vitest/Jest first — Bun as package manager doesn't mean Bun as test runner
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if pkg.contains("\"vitest\"") || pkg.contains("\"jest\"") {
            // Other framework present — only claim high confidence if bun:test is explicit
            if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") { return 90; }
            return 0; // Let Vitest/Jest adapters handle it
        }
    }
    if project_root.join("bun.lockb").exists() || project_root.join("bun.lock").exists() {
        return 85; // Lower than before (was 95) to let explicit config files win
    }
    if let Ok(pkg) = std::fs::read_to_string(project_root.join("package.json")) {
        if pkg.contains("\"bun test\"") || pkg.contains("\"bun:test\"") { return 90; }
        if pkg.contains("\"bun\"") { return 75; }
    }
    0
}
```

Update `test_suite_command` test to check for the new `--reporter-outfile` arg.

**Checkpoint:** Bun adapter produces JUnit XML on stdout, filters by test name correctly, and yields to Vitest/Jest when they're present.

---

### Task 21: Jest adapter — add `numTodoTests` field

**Files:**
- Modify: `src/test/jest_adapter.rs` (JestReport struct + parse_output)

**Implementation:**

Add `num_todo` to `JestReport`:
```rust
#[derive(Deserialize)]
struct JestReport {
    #[serde(rename = "numPassedTests", default)]
    num_passed: u32,
    #[serde(rename = "numFailedTests", default)]
    num_failed: u32,
    #[serde(rename = "numPendingTests", default)]
    num_pending: u32,
    #[serde(rename = "numTodoTests", default)]
    num_todo: u32,
    #[serde(rename = "testResults", default)]
    test_results: Vec<JestSuite>,
}
```

In `parse_output`, include todo tests in skipped count:
```rust
skipped: report.num_pending + report.num_todo,
```

**Checkpoint:** Jest todo tests are counted in the skipped total.

---

### Task 22: I1 — Fix script pointer leak on load failure

**Files:**
- Modify: `src/frida_collector/spawner.rs:1876-1879` (child spawn path)
- Modify: `src/frida_collector/spawner.rs:1109-1141` (main spawn path)

**Implementation:**

**Child spawn path** (around line 1876): Add `frida_unref` on error:
```rust
unsafe {
    register_handler_raw(script_ptr, handler);
    if let Err(e) = load_script_raw(script_ptr) {
        tracing::error!("Failed to load script in child {}: {}", child_pid, e);
        frida_sys::frida_unref(script_ptr as *mut std::ffi::c_void);
        let _ = device.resume(child_pid);
        return;
    }
}
```

**Main spawn path** (around line 1138): Wrap the load in a scope that cleans up on error:
```rust
let load_result = unsafe { load_script_raw(script_ptr) };
if let Err(e) = load_result {
    unsafe { frida_sys::frida_unref(script_ptr as *mut std::ffi::c_void) };
    return Err(crate::Error::FridaAttachFailed(format!("Script load failed: {}", e)));
}
```

**Checkpoint:** Script GObjects are properly unref'd on load failure.

---

### Task 23: I9 — Fix child-to-session association to use parent PID

**Files:**
- Modify: `src/frida_collector/spawner.rs:1808-1821`

**Implementation:**

Instead of `reg.values().next()`, look up the parent's PID to find the correct session. The child's parent PID can be retrieved via `sysctl` or from the output registry keyed by PPID. Since we know which PIDs belong to which sessions (the output registry maps PID → context), we can look up by iterating the registry to find any entry whose PID is the child's parent:

Actually, `handle_child_spawn` is called from the Frida `child-gating` callback which provides the child PID. We don't directly get the parent PID from Frida's API. But we can use `libc::getppid()` — no, that gives our PID, not the child's parent.

Use `sysctl` on macOS to get the child's PPID:
```rust
fn get_ppid(pid: u32) -> Option<u32> {
    let mut info: libc::kinfo_proc = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::kinfo_proc>();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid as i32];
    let ret = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            4,
            &mut info as *mut _ as *mut _,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if ret == 0 && size > 0 {
        Some(info.kp_eproc.e_ppid as u32)
    } else {
        None
    }
}
```

Then replace the lookup:
```rust
let parent_info = {
    let reg = match output_registry.lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::warn!("Failed to lock output registry for child {}", child_pid);
            let _ = device.resume(child_pid);
            return;
        }
    };
    // Find the session that owns the child's parent process
    let ppid = get_ppid(child_pid);
    let ctx = ppid
        .and_then(|pp| reg.get(&pp))
        .or_else(|| reg.values().next()); // fallback for single-session case
    ctx.map(|c| (c.session_id.clone(), c.event_tx.clone(), c.start_ns))
};
```

**Checkpoint:** Child processes are attributed to the correct parent session.

---

### Task 24: I11 — Fix spawn/create ordering in all behavioral tests

**Files:**
- Modify: `tests/breakpoint_basic.rs`
- Modify: `tests/breakpoint_behavioral.rs`
- Modify: `tests/stepping_basic.rs`
- Modify: `tests/stepping_behavioral.rs`
- Modify: `tests/logpoint_and_write.rs`
- Modify: `tests/phase2a_gaps.rs`
- Modify: `tests/ui_observation.rs`

**Implementation:**

In every test sub-block, change from:
```rust
let pid = sm.spawn_with_frida(session_id, ...).await.unwrap();
sm.create_session(session_id, ..., pid).unwrap();
```

To:
```rust
sm.create_session(session_id, binary.to_str().unwrap(), project_root, 0).unwrap();
let pid = sm.spawn_with_frida(session_id, ...).await.unwrap();
sm.update_session_pid(session_id, pid).unwrap();
```

This follows the pattern established in `python_e2e.rs` and `python_comprehensive.rs`. The `create_session` with `pid=0` ensures the FK exists before the writer task starts inserting events.

This is a mechanical search-and-replace across 7 files, hitting every occurrence of the inverted pattern.

**Checkpoint:** All tests create the session before spawning, eliminating the FK race.

---

### Task 25: L13 — Add assertion to `python_comprehensive.rs` tracing test

**Files:**
- Modify: `tests/python_comprehensive.rs` (the `test_python_tracing` function, around line 195-213)

**Implementation:**

Replace the no-op if/else with an actual assertion:
```rust
assert!(
    events.len() > 0,
    "Expected Python function trace events but got none — tracing may be broken"
);
eprintln!("✓ Python function tracing working ({} events)", events.len());
```

**Checkpoint:** Test fails if Python tracing produces zero events.

---

### Task 26: L14 — Add pause location verification to `python_features.rs` breakpoint test

**Files:**
- Modify: `tests/python_features.rs` (scenario_breakpoints function)

**Implementation:**

After the `wait_for_pause` call, add verification of the pause event's breakpoint ID. Locate the pause verification code and add:
```rust
// Verify we paused at the correct breakpoint
let pause_events = sm.db().query_events(session_id, |q| {
    q.event_type(EventType::Pause).limit(1)
}).unwrap();
assert!(!pause_events.is_empty(), "Should have a pause event in DB");
if let Some(bp_id) = &pause_events[0].breakpoint_id {
    assert!(bp_id.contains("bp"), "Pause should be attributed to a breakpoint");
}
```

**Checkpoint:** Python breakpoint test verifies pause location, not just mechanics.

---

### Task 27: Fix `logpoint_and_write.rs` Test 6 — `return` → continue to next test

**Files:**
- Modify: `tests/logpoint_and_write.rs:427-433`

**Implementation:**

The test uses a flat block structure, not labeled blocks. Since each sub-test is in a `{ }` block, the `return` exits the entire test function. Wrap the Test 6 and Test 7 blocks in a way that allows skipping Test 6 without skipping Test 7.

Change the error path in Test 6 from:
```rust
return;
```
to using a labeled block pattern:
```rust
'test6: {
    // ... Test 6 code ...
    Err(e) => {
        println!("  CModule trace failed (skipping coexistence test): {}", e);
        let _ = sm.stop_frida(session_id).await;
        sm.stop_session(session_id).await.unwrap();
        println!("  SKIPPED");
        break 'test6;
    }
    // ... rest of Test 6 ...
}
```

This requires wrapping the entire Test 6 sub-block in a `'test6: { ... }` labeled block so `break 'test6` exits only that block.

**Checkpoint:** Test 7 runs even when Test 6's CModule trace fails.

---

### Task 28: Rebuild agent and verify compilation

**Depends on:** All Stream C tasks (2, 11, 13-19).

**Implementation:**

```bash
cd agent && npm run build && cd ..
touch src/frida_collector/spawner.rs
cargo build --release 2>&1
```

Verify no compiler errors from Rust changes. Verify no TypeScript errors from agent changes.

**Checkpoint:** Full project compiles cleanly.

---

## Not Fixed (Deferred)

The following findings are noted but intentionally deferred:

- **L8 (single global `_strobe_bp_event`):** Partially addressed by Task 13 (GIL release). Per-breakpoint events require more Python-side state management and testing. Deferred to a follow-up.
- **L11 (column index fragility):** `event_from_row` positional access is brittle but working. Switching to named columns would touch many lines for cosmetic benefit. Deferred.
- **Integration test agent findings** (hard-coded window ID `w_7cdd`, orphaned fixture files, session ID collision risk, duplicated Python helper functions): These are test quality issues that don't affect production code. Can be addressed in a follow-up test improvement pass.
