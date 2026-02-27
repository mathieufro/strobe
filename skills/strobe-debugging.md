---
name: strobe-debugging
description: Investigate bugs using Strobe's dynamic instrumentation — run first, trace fast, fix with evidence
---

# Bug Investigation with Strobe

**Start with static analysis — most bugs are obvious from reading code.** When it doesn't work, switch to instrumentation. Two equally valid tools:

- **Strobe traces** — discover which functions ran and with what arguments, without touching source
- **Source log injection** — add `printf`/logging at the exact location you care about, rebuild, rerun

## Hard Rules

**1. When static analysis hasn't found it: instrument, don't keep reading.**
Runtime bugs (wrong execution path, compile-time guard, unregistered handler, wrong instance) are invisible in source. Instrument them.

**2. A hypothesis must be tested at runtime, not confirmed by reading more files.**
Verify with a trace or log — if right, one run proves it; if wrong, you still have new data.

**3. Never re-run a test without new instrumentation.**
Re-running without a new trace, log, or code change gives the same failure with no new information.

---

## The Loop

### Step 0: Get a Failing Test (if you don't have one)

If you have a **failing test** — skip to Step 1.

If you have a **bug report or user scenario**, the order is:

**First: static analysis.** Read the relevant code, trace the execution path. Most bugs are obvious and you can fix it right here. If you find it — fix it and verify.

**If static analysis didn't find it: write a reproduction test.**

- **Unit test** when the bug is in a specific, isolatable function
- **E2e test** when the scenario requires real system state: UI interaction, multi-step workflow, complex object construction. When in doubt, use e2e — it puts you in the exact same situation as the user

Write the smallest test that exercises the exact path. Verify it fails with `debug_test` — a passing test means wrong reproduction, not a fix.

### Step 1: Reproduce and Check Output

```
debug_test({ projectRoot, test: "<test_name>" })
debug_query({ sessionId, eventType: "stderr" })
```

If the cause is clear — fix it. If ambiguous or silent — go to Step 2 immediately.

### Step 2: Instrument

**Strobe trace** to discover which functions ran:
```
debug_trace({ sessionId, add: ["SuspectedClass::*"] })
debug_query({ sessionId, eventType: "function_enter" })
```
If the suspected function never appears — something upstream blocked it. That's the lead.

**Log injection** for specific values or control flow at a precise location:
```cpp
printf("[DEBUG] called: id=%d, active=%d\n", id, active);  // remove after fix
```
Prefer logs when you know the exact line, or for inlined/templated code Strobe can't hook.

### Step 3: Narrow, Fix, Verify

Use observed evidence to narrow. Watches for variable state: `debug_trace({ sessionId, watches: { add: [{ variable: "g_state", on: ["MyClass::method"] }] } })`

**Fix with evidence.** State it explicitly: "The trace showed X was never called — the `#ifdef` on line 47 prevents it in test builds." If you can't state the evidence, instrument more.

```
debug_test({ projectRoot, test: "<test_name>" })  // fixed?
debug_test({ projectRoot })                         // regressions?
```

---

## When hookedFunctions: 0

Quick investigation before falling back to logs:
1. Try `@file:filename.cpp` — bypasses name mangling
2. Glob `**/*.dSYM` — if found, re-launch with `symbolsPath`
3. Try without namespace prefix
4. Templates/lambdas rarely hook — go straight to log injection

After 2-3 attempts, switch to source logging. Don't keep trying pattern variations.
