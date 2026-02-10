---
name: strobe-debugging
description: Investigate bugs using Strobe's dynamic instrumentation — observe first, trace surgically, fix with evidence
---

# Bug Investigation with Strobe

You have Strobe's dynamic instrumentation tools. Use them. Do NOT guess at bugs by reading code — **observe runtime behavior first**, then fix with evidence.

## The Loop: Observe → Trace → Narrow → Fix

### Step 1: Reproduce

Pick the right entry point based on what the user reported:

**A) There's a failing test** — run it:
```
debug_test({ projectRoot, test: "<test_name>" })
```
Poll `debug_test_status` until complete. You get structured failures (file, line, message, `suggested_traces`), stuck warnings, and crash data.

**B) It's a runtime bug** (UI glitch, server error, crash, wrong behavior) — launch the program:
```
debug_launch({ command: "<binary>", args: [...], projectRoot })
```
If you already have a hypothesis from the user's description or from reading code, stage traces *before* launch so you capture the bug on first trigger:
```
debug_trace({ add: ["suspect_module::*"] })  // no sessionId = staged for next launch
debug_launch({ command: "<binary>", args: [...], projectRoot })
```
Then tell the user what to do to trigger the bug (click a button, send a request, open a file, etc). Wait for them to confirm they've triggered it.

**C) No test infrastructure and the bug needs a test to reproduce** — stop this workflow. Suggest the user brainstorm a test harness first. Don't try to scaffold one inline.

### Step 2: Check stderr/stdout FIRST

Most bugs are visible in output alone. Before adding any traces:

```
debug_query({ sessionId, eventType: "stderr" })
debug_query({ sessionId, eventType: "stdout" })
debug_query({ sessionId, eventType: "crash" })
```

Look for: panics, assertion messages, error logs, ASAN reports, segfaults, HTTP error responses, unexpected output. If the root cause is already clear, skip to Step 5.

### Step 3: Trace surgically — on the LIVE session

If you have `suggested_traces` from a test failure, use those. Otherwise, form a hypothesis from the error output and trace the relevant module:

```
debug_trace({ sessionId, add: ["<pattern>"] })
```

Then query what happened:

```
debug_query({ sessionId, function: { contains: "<suspect>" } })
```

For runtime bugs where the user triggers the action, use time filters to isolate the relevant window:
```
debug_query({ sessionId, timeFrom: "-5s" })
```

**Pattern strategy**: Start narrow (1-3 patterns, target <50 hooks). Only widen if the narrow view isn't enough. Never use `@usercode` or `*` as a first move.

### Step 4: Go deeper if needed

Pick based on what you learned:

**Watches** — track a global/variable across function calls:
```
debug_trace({ sessionId, watches: { add: [{ variable: "g_state", on: ["suspect::fn"] }] } })
```

**Breakpoints** — pause at a specific condition to inspect state:
```
debug_breakpoint({ sessionId, add: [{ function: "suspect::fn", condition: "args[0] == 0" }] })
```

**Logpoints** — non-blocking printf-style tracing without pausing:
```
debug_logpoint({ sessionId, add: [{ function: "suspect::fn", message: "called with {args[0]}" }] })
```

**Memory reads** — inspect a variable's value right now:
```
debug_read({ sessionId, targets: [{ variable: "g_counter" }] })
```

**Slow function search** — find performance bottlenecks:
```
debug_query({ sessionId, minDurationNs: 1000000 })
```

After each tool, re-query to see the new data. This is an iterative loop — keep going until you understand the root cause.

### Step 5: Fix with evidence

Now you know **exactly** what's wrong from runtime observation. Make the minimal fix.

### Step 6: Verify

If you started from a test, re-run it, then the full suite:
```
debug_test({ projectRoot, test: "<test_name>" })   // fixed?
debug_test({ projectRoot })                          // regressions?
```

If you started from `debug_launch`, rebuild and relaunch. Ask the user to trigger the bug again and confirm it's resolved.

## Rules

- **If the cause is obvious, just fix it.** Don't instrument for the sake of it. But if you're guessing, observe runtime behavior before changing code.
- **NEVER run tests via bash** — always use `debug_test`. It gives you structured data + a live Frida session.
- **Traces are free to add mid-flight** — don't restart the session to add instrumentation.
- **Start narrow, widen incrementally** — broad patterns generate noise and can crash targets (>100 hooks).
- **Use `suggested_traces` when available** — they're generated from the failure location, don't ignore them.
- **For interactive bugs, tell the user what to trigger** — Strobe instruments the live process, but the user needs to drive the UI/requests.
