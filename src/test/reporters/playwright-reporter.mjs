// Strobe custom Playwright reporter.
//
// Two jobs:
//  1. Stream live progress to STROBE_PROGRESS_FILE ("STROBE_TEST:" + JSON lines)
//     and emit a list-reporter-style marker on stderr so Strobe's log_router
//     builds summary.md / failures.md and tracks status live.
//  2. Write COMPLETE per-test artifacts into <STROBE_RUN_DIR>/tests/<id>/ —
//     full stdout, full stderr + error + stack + code frame, and the copied
//     browser-trace / screenshot / video / error-context files. The live stream
//     can't carry these (the log_router's pending buffer is capped), so the
//     reporter writes the files itself into the same sanitized dir the
//     log_router uses (matching its `sanitize`), so summary.md links resolve.

import { appendFileSync, copyFileSync, existsSync, mkdirSync, writeFileSync, writeSync } from "node:fs"
import { basename, join } from "node:path"

const PROGRESS_FILE = process.env.STROBE_PROGRESS_FILE || "/tmp/.strobe-playwright-progress"
const RUN_DIR = process.env.STROBE_RUN_DIR || ""

function emit(obj) {
  try {
    appendFileSync(PROGRESS_FILE, "STROBE_TEST:" + JSON.stringify(obj) + "\n")
  } catch {}
}

const errSync = (s) => {
  try { writeSync(2, s) } catch {}
}

const stripAnsi = (s) => (s || "").replace(/\x1b\[[0-9;]*m/g, "")

/** Mirror of Strobe's Rust `sanitize` (src/test/artifacts.rs) so the reporter's
 *  per-test dir matches the log_router's. SipHash truncation for names >=200 is
 *  not replicable in JS — those (rare) long names fall back to a plain trim. */
function sanitize(id) {
  const SENT = ""
  let s = id.split("::").join(SENT + SENT)
  s = Array.from(s)
    .map((c) => (c === SENT || /[A-Za-z0-9-]/.test(c) ? c : "_"))
    .join("")
  s = s.replace(/_+/g, "_")
  s = s.split(SENT + SENT).join("__")
  s = s.replace(/^_+/, "").replace(/_+$/, "")
  if (s.length < 200) return s
  return s.slice(0, 191).replace(/_+$/, "")
}

function extractError(result) {
  if (!result.errors || result.errors.length === 0) return ""
  let msg = stripAnsi(result.errors[0].message || "")
  if (msg.length > 500) msg = msg.slice(0, 500) + "..."
  return msg
}

const chunkStr = (c) =>
  typeof c === "string" ? c : Buffer.isBuffer(c) ? c.toString("utf8") : String(c)

/** Write the complete per-test artifacts into <RUN_DIR>/tests/<id>/. */
function writeArtifacts(test, result) {
  if (!RUN_DIR) return
  try {
    const nameOnly = (typeof test.titlePath === "function" ? test.titlePath() : []).slice(2).join(" > ")
    if (!nameOnly) return
    const dir = join(RUN_DIR, "tests", sanitize(nameOnly))
    mkdirSync(dir, { recursive: true })

    const stdout = (result.stdout || []).map(chunkStr).join("")
    if (stdout) appendFileSync(join(dir, "stdout.log"), stdout)

    let stderr = (result.stderr || []).map(chunkStr).join("")
    for (const err of result.errors || []) {
      stderr += `\n──── error ────\n${stripAnsi(err.message)}\n`
      if (err.stack && err.stack !== err.message) stderr += `${stripAnsi(err.stack)}\n`
      if (err.snippet) stderr += `${stripAnsi(err.snippet)}\n`
    }

    // Copy each attachment INTO the per-test dir + reference it. This is the
    // full browser trace, the failure screenshot, the video, the page snapshot.
    for (const att of result.attachments || []) {
      const ct = att.contentType || ""
      if (att.path && existsSync(att.path)) {
        const dest = basename(att.path)
        try { copyFileSync(att.path, join(dir, dest)) } catch {}
        stderr += `\n──── attachment: ${att.name} (${ct}) → ${dest} ────\n`
        if (att.name === "trace") {
          stderr += `open with: npx playwright show-trace "${join(dir, dest)}"\n`
        }
      } else if (att.body) {
        const text = Buffer.isBuffer(att.body) ? att.body.toString("utf8") : String(att.body)
        const fname = `${(att.name || "attachment").replace(/[^A-Za-z0-9-]/g, "_")}.txt`
        try { writeFileSync(join(dir, fname), text) } catch {}
        stderr += `\n──── attachment: ${att.name} (${ct}) → ${fname} ────\n`
        if (text.length < 8000) stderr += `${text}\n`
      }
    }

    if (stderr) appendFileSync(join(dir, "stderr.log"), stderr)
  } catch {
    /* best-effort per-test artifact capture */
  }
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

    // Complete per-test artifacts (full output + copied trace/screenshot/etc).
    writeArtifacts(test, result)

    // List-reporter-style marker on stderr — the log_router parses this to build
    // summary.md / failures.md and to create the per-test dir. Keep it LAST.
    try {
      const projectName = test.parent?.project?.()?.name || "chromium"
      const fileBase = file ? file.split("/").pop() : ""
      const glyph =
        result.status === "passed" || result.status === "expected"
          ? "✓"
          : result.status === "skipped"
            ? "-"
            : "✘"
      const path = typeof test.titlePath === "function" ? test.titlePath() : []
      const nameOnly = path.length > 2 ? path.slice(2).join(" > ") : (test.title || fullName)
      const loc = fileBase ? `${fileBase}:${line || 1}:1 › ` : ""
      errSync(`  ${glyph}  1 [${projectName}] › ${loc}${nameOnly} (${Math.round(d)}ms)\n`)
    } catch {
      /* best-effort live marker */
    }
  }

  onEnd() {
    if (this._currentSuite) {
      emit({ e: "module_end", n: this._currentSuite, d: 0 })
    }
  }
}
