// MANUAL validation of WebKit Inspector Protocol for Bun tracing.
// Run: bun run tests/fixtures/bun_inspector_validation.ts
//
// This is a manual developer script, NOT an automated test.
// If validation passes, a separate integration task is needed to wire
// Inspector Protocol into SessionManager/FridaSpawner.

import { spawn } from "child_process";

async function validate() {
  // 1. Spawn bun --inspect-wait with dynamic port
  const proc = spawn("bun", [
    "--inspect-wait=127.0.0.1:0",
    "tests/fixtures/bun_inspector_target.ts"
  ]);

  // 2. Parse inspector URL from stderr — accumulate chunks, async wait with timeout
  let stderrBuf = "";
  let wsUrl = "";

  proc.stderr?.on("data", (data: Buffer) => {
    stderrBuf += data.toString();
    // Match full URL including hyphens in UUID
    const match = stderrBuf.match(/ws:\/\/[^\s]+/);
    if (match) wsUrl = match[0];
  });

  // Wait up to 10s for URL (not a hard 1s timeout)
  const deadline = Date.now() + 10_000;
  while (!wsUrl && Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 100));
  }

  if (!wsUrl) {
    console.error("FAIL: No WebSocket URL found in stderr after 10s");
    console.error("stderr:", stderrBuf);
    proc.kill();
    process.exit(1);
  }

  console.log(`Connecting to: ${wsUrl}`);

  // 3. Connect via WebSocket
  const ws = new WebSocket(wsUrl);
  let msgId = 1;
  const wsSend = (method: string, params?: any) => {
    ws.send(JSON.stringify({ id: msgId++, method, params }));
  };

  const responses: any[] = [];
  const results: Map<number, any> = new Map();

  ws.onmessage = (event: MessageEvent) => {
    const data = JSON.parse(event.data as string);
    responses.push(data);
    if (data.id) results.set(data.id, data);
    if (data.method) {
      console.log(`  Event: ${data.method}`, JSON.stringify(data.params || {}).slice(0, 150));
    }
  };

  await new Promise<void>((resolve, reject) => {
    ws.addEventListener('open', () => resolve());
    ws.addEventListener('error', (e) => reject(e));
  });

  // 4. Enable debugger
  wsSend("Debugger.enable");
  await new Promise(r => setTimeout(r, 500));

  // 5. Test MULTI-HOOK attribution: add symbolic breakpoints for TWO functions
  const bp1Id = msgId;
  wsSend("Debugger.addSymbolicBreakpoint", {
    symbol: "handleRequest",
    caseSensitive: true,
    isRegex: false,
    options: {
      autoContinue: true,
      actions: [{
        type: "evaluate",
        data: "globalThis.__strobe_bp1_fired = true; console.log('[strobe-trace] handleRequest called')",
        emulateUserGesture: false,
      }]
    }
  });

  const bp2Id = msgId;
  wsSend("Debugger.addSymbolicBreakpoint", {
    symbol: "computeHash",
    caseSensitive: true,
    isRegex: false,
    options: {
      autoContinue: true,
      actions: [{
        type: "evaluate",
        data: "globalThis.__strobe_bp2_fired = true; console.log('[strobe-trace] computeHash called')",
        emulateUserGesture: false,
      }]
    }
  });

  // 6. Wait for BOTH breakpoint responses before resuming
  const bpDeadline = Date.now() + 5_000;
  while ((!results.has(bp1Id) || !results.has(bp2Id)) && Date.now() < bpDeadline) {
    await new Promise(r => setTimeout(r, 50));
  }

  if (!results.has(bp1Id) || !results.has(bp2Id)) {
    console.error("FAIL: addSymbolicBreakpoint responses not received");
    ws.close(); proc.kill(); process.exit(1);
  }

  console.log(`BP1 response:`, JSON.stringify(results.get(bp1Id)));
  console.log(`BP2 response:`, JSON.stringify(results.get(bp2Id)));

  // 7. Resume execution (target setTimeout fires after 1s)
  wsSend("Debugger.resume");

  // 8. Wait for target to complete (target does work at +1s, exits at +10s)
  await new Promise(r => setTimeout(r, 4000));

  // 9. Analyze results
  const scriptParsed = responses.filter(r => r.method === "Debugger.scriptParsed");
  const paused = responses.filter(r => r.method === "Debugger.paused");
  const consoleMessages = responses.filter(r =>
    r.method === "Console.messageAdded" || r.method === "Runtime.consoleAPICalled"
  );

  // Check if action callbacks fired by looking for our marker console logs
  const allText = responses.map(r => JSON.stringify(r)).join('\n');
  const bp1Fired = allText.includes('handleRequest called');
  const bp2Fired = allText.includes('computeHash called');

  console.log(`\n=== Results ===`);
  console.log(`Scripts parsed: ${scriptParsed.length}`);
  console.log(`Paused events: ${paused.length}`);
  console.log(`Console messages: ${consoleMessages.length}`);
  console.log(`handleRequest breakpoint action fired: ${bp1Fired}`);
  console.log(`computeHash breakpoint action fired: ${bp2Fired}`);
  console.log(`autoContinue working: ${paused.length === 0 && (bp1Fired || bp2Fired) ? "YES" : "UNCLEAR"}`);

  ws.close();
  proc.kill();

  // Success criteria:
  // 1. BOTH breakpoint actions must fire (proves multi-hook attribution works)
  // 2. No Debugger.paused events (proves autoContinue works)
  // 3. Function names identifiable from the fired actions
  const pass = bp1Fired && bp2Fired && paused.length === 0;

  if (pass) {
    console.log("\nVALIDATION: PASS — Inspector Protocol works with multi-hook attribution");
    console.log("NEXT STEPS: Integration requires tokio-tungstenite WebSocket client,");
    console.log("  SessionManager inspector connection field, CDP-to-Event translation,");
    console.log("  and debug_trace dispatch to inspector channel.");
  } else if (bp1Fired && !bp2Fired) {
    console.log("\nVALIDATION: PARTIAL — Single hook works but multi-hook attribution fails");
    console.log("  The Inspector Protocol may not support multiple symbolic breakpoints.");
  } else {
    console.log("\nVALIDATION: FAIL — Inspector Protocol not working for tracing");
    console.log("  Fallback: Bun.plugin() + onLoad source transformation via --preload");
    console.log("  (requires AST transformation library, separate multi-week effort)");
    process.exit(1);
  }
}

validate().catch(e => {
  console.error("Validation error:", e);
  process.exit(1);
});
