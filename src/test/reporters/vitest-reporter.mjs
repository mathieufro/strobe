// Strobe custom Vitest reporter — streams per-test events to stderr.
// Used alongside --reporter=json for real-time progress tracking.
//
// Protocol: each line is prefixed with STROBE_TEST: followed by a JSON object.
// Events:
//   {e:"module_start", n:"<relativeModuleId>"}  — test file execution begins
//   {e:"module_end",   n:"<relativeModuleId>", d:<ms>} — test file finished
//   {e:"start", n:"<fullName>"}           — test is about to run
//   {e:"pass",  n:"<fullName>", d:<ms>}   — test passed
//   {e:"fail",  n:"<fullName>", d:<ms>}   — test failed
//   {e:"skip",  n:"<fullName>"}           — test skipped/pending/todo
//
// Vitest 3.x reporter API — hooks are silently ignored by Vitest 2.x.

export default class StrobeReporter {
  onTestModuleStart(testModule) {
    process.stderr.write(
      "\nSTROBE_TEST:" + JSON.stringify({
        e: "module_start",
        n: testModule.relativeModuleId || testModule.moduleId
      }) + "\n"
    );
  }

  onTestModuleEnd(testModule) {
    const d = testModule.diagnostic();
    process.stderr.write(
      "\nSTROBE_TEST:" + JSON.stringify({
        e: "module_end",
        n: testModule.relativeModuleId || testModule.moduleId,
        d: d ? d.duration : 0
      }) + "\n"
    );
  }

  onTestCaseReady(testCase) {
    // Skip emitting "start" for tests that won't actually execute.
    // onTestCaseReady fires for skipped/todo tests too (Vitest 3.x docs).
    const mode = testCase.options && testCase.options.mode;
    if (mode === "skip" || mode === "todo") {
      return; // onTestCaseResult will emit the "skip" event
    }
    const r = testCase.result();
    if (r && r.state === "skipped") {
      return; // already resolved as skipped
    }
    process.stderr.write(
      "\nSTROBE_TEST:" + JSON.stringify({ e: "start", n: testCase.fullName }) + "\n"
    );
  }

  onTestCaseResult(testCase) {
    const r = testCase.result();
    const e = r.state === "passed" ? "pass" : r.state === "failed" ? "fail" : "skip";
    const d = testCase.diagnostic();
    process.stderr.write(
      "\nSTROBE_TEST:" + JSON.stringify({
        e,
        n: testCase.fullName,
        d: d ? d.duration : (r.duration || 0)
      }) + "\n"
    );
  }
}
