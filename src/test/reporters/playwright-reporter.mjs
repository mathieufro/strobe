// Strobe custom Playwright reporter — streams per-test events to a progress file.
// Strobe polls this file for live progress during test runs.
//
// Protocol: each line is a JSON object with STROBE_TEST: prefix.
// File path comes from STROBE_PROGRESS_FILE env var.

import { appendFileSync, writeFileSync } from "node:fs"

const PROGRESS_FILE = process.env.STROBE_PROGRESS_FILE || "/tmp/.strobe-playwright-progress"

function emit(obj) {
  try {
    appendFileSync(PROGRESS_FILE, "STROBE_TEST:" + JSON.stringify(obj) + "\n")
  } catch {}
}

/** @implements {import('@playwright/test/reporter').Reporter} */
export default class StrobePlaywrightReporter {
  _currentSuite = null

  onBegin() {
    // Clear the file at the start of a run
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
    let e = "pass"
    if (result.status === "failed" || result.status === "timedOut") e = "fail"
    else if (result.status === "skipped") e = "skip"
    emit({ e, n: fullName, d })
  }

  onEnd() {
    if (this._currentSuite) {
      emit({ e: "module_end", n: this._currentSuite, d: 0 })
    }
  }
}
