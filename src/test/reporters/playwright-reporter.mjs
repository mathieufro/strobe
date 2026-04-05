// Strobe custom Playwright reporter — streams per-test events to a progress file.
// Strobe polls this file for live progress during test runs.
//
// Protocol: each line is "STROBE_TEST:" followed by a JSON object.
// Events:
//   {e:"module_start", n:"<file>"}
//   {e:"start", n:"<fullName>"}
//   {e:"pass",  n:"<fullName>", d:<ms>}
//   {e:"fail",  n:"<fullName>", d:<ms>, f:"<file>", l:<line>, m:"<error message>"}
//   {e:"skip",  n:"<fullName>"}
//   {e:"module_end", n:"<file>", d:<ms>}

import { appendFileSync, writeFileSync } from "node:fs"

const PROGRESS_FILE = process.env.STROBE_PROGRESS_FILE || "/tmp/.strobe-playwright-progress"

function emit(obj) {
  try {
    appendFileSync(PROGRESS_FILE, "STROBE_TEST:" + JSON.stringify(obj) + "\n")
  } catch {}
}

/** Extract a concise error message from Playwright test result. */
function extractError(result) {
  if (!result.errors || result.errors.length === 0) return ""
  const err = result.errors[0]
  // Playwright error has .message and .stack
  let msg = err.message || ""
  // Trim ANSI codes
  msg = msg.replace(/\x1b\[[0-9;]*m/g, "")
  // Take first 500 chars to keep JSON manageable
  if (msg.length > 500) msg = msg.slice(0, 500) + "..."
  return msg
}

/** @implements {import('@playwright/test/reporter').Reporter} */
export default class StrobePlaywrightReporter {
  _currentSuite = null

  onBegin() {
    try { writeFileSync(PROGRESS_FILE, "") } catch {}
  }

  onTestBegin(test) {
    const file = test.location?.file || ""
    const fullName = test.titlePath().slice(1).join(" > ")
    if (file !== this._currentSuite) {
      this._currentSuite = file
      emit({ e: "module_start", n: file })
    }
    emit({ e: "start", n: fullName })
  }

  onTestEnd(test, result) {
    const fullName = test.titlePath().slice(1).join(" > ")
    const d = result.duration || 0
    const file = test.location?.file || ""
    const line = test.location?.line || 0

    if (result.status === "passed" || result.status === "expected") {
      emit({ e: "pass", n: fullName, d })
    } else if (result.status === "failed" || result.status === "timedOut") {
      emit({ e: "fail", n: fullName, d, f: file, l: line, m: extractError(result) })
    } else if (result.status === "skipped") {
      emit({ e: "skip", n: fullName })
    }
  }

  onEnd() {
    if (this._currentSuite) {
      emit({ e: "module_end", n: this._currentSuite, d: 0 })
    }
  }
}
