//! Generate `summary.md` and `failures.md` from an NDJSON event stream.
//!
//! Invoked at run end (typically when emitting `RunComplete`). Reads
//! `<run_dir>/events.ndjson` and emits two markdown files in the same dir.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io;
use std::path::Path;

#[derive(Default)]
struct CaseSummary {
    test_id: String,
    status: String, // "pass" | "fail" | "stalled" | "skipped"
    duration_ms: u64,
    artifact_dir: String,
    file: String,
    error_message: Option<String>,
    expected: Option<String>,
    actual: Option<String>,
    code_context: Vec<String>,
    stall_reason: Option<String>,
}

pub fn generate(run_dir: &Path) -> io::Result<()> {
    let events_path = run_dir.join("events.ndjson");
    let text = std::fs::read_to_string(&events_path)?;

    // Insertion order matters for deterministic table output.
    let mut order: Vec<String> = Vec::new();
    let mut cases: BTreeMap<String, CaseSummary> = BTreeMap::new();

    for line in text.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "test_started" => {
                let id = v
                    .get("test_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                if !cases.contains_key(&id) {
                    order.push(id.clone());
                    cases.insert(
                        id.clone(),
                        CaseSummary {
                            test_id: id,
                            ..Default::default()
                        },
                    );
                }
            }
            "test_passed" | "test_failed" | "test_skipped" | "test_stalled" => {
                let id = v
                    .get("test_id")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string();
                let entry = cases.entry(id.clone()).or_insert_with(|| {
                    order.push(id.clone());
                    CaseSummary {
                        test_id: id.clone(),
                        ..Default::default()
                    }
                });
                entry.duration_ms = v
                    .get("duration_ms")
                    .and_then(|n| n.as_u64())
                    .unwrap_or(entry.duration_ms);
                entry.artifact_dir = v
                    .get("artifact_dir")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&entry.artifact_dir)
                    .to_string();
                entry.file = v
                    .get("file")
                    .and_then(|s| s.as_str())
                    .unwrap_or(&entry.file)
                    .to_string();
                entry.status = match kind {
                    "test_passed" => "pass".into(),
                    "test_failed" => "fail".into(),
                    "test_skipped" => "skipped".into(),
                    "test_stalled" => "stalled".into(),
                    _ => entry.status.clone(),
                };
                if kind == "test_failed" {
                    if let Some(err) = v.get("error") {
                        entry.error_message = err
                            .get("message")
                            .and_then(|s| s.as_str())
                            .map(String::from);
                        entry.expected = err
                            .get("expected")
                            .and_then(|s| s.as_str())
                            .map(String::from);
                        entry.actual =
                            err.get("actual").and_then(|s| s.as_str()).map(String::from);
                        entry.code_context = err
                            .get("code_context")
                            .and_then(|a| a.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                }
                if kind == "test_stalled" {
                    let elapsed = v.get("elapsed_ms").and_then(|n| n.as_u64()).unwrap_or(0);
                    let median = v.get("median_ms").and_then(|n| n.as_u64()).unwrap_or(0);
                    entry.stall_reason = Some(format!(
                        "exceeded 5× median ({} ms vs {} ms baseline)",
                        elapsed, median
                    ));
                }
            }
            _ => {}
        }
    }

    write_summary(run_dir, &order, &cases)?;
    write_failures(run_dir, &order, &cases)?;
    Ok(())
}

fn rank(status: &str) -> u8 {
    match status {
        "fail" => 0,
        "stalled" => 1,
        "skipped" => 2,
        "pass" => 3,
        _ => 4,
    }
}

fn write_summary(
    run_dir: &Path,
    order: &[String],
    cases: &BTreeMap<String, CaseSummary>,
) -> io::Result<()> {
    let mut sorted: Vec<&CaseSummary> = order.iter().filter_map(|id| cases.get(id)).collect();
    sorted.sort_by_key(|c| (rank(&c.status), c.test_id.clone()));

    let totals_pass = sorted.iter().filter(|c| c.status == "pass").count();
    let totals_fail = sorted.iter().filter(|c| c.status == "fail").count();
    let totals_stalled = sorted.iter().filter(|c| c.status == "stalled").count();
    let totals_skipped = sorted.iter().filter(|c| c.status == "skipped").count();
    let total_duration: u64 = sorted.iter().map(|c| c.duration_ms).sum();

    let mut out = String::new();
    out.push_str("# Test Run Summary\n\n");
    out.push_str(&format!(
        "**Totals:** {} pass, {} fail, {} stalled, {} skipped — {} ms\n\n",
        totals_pass, totals_fail, totals_stalled, totals_skipped, total_duration
    ));
    out.push_str("| Test | Status | Duration | Artifacts |\n");
    out.push_str("|------|--------|----------|-----------|\n");
    for c in &sorted {
        let artifacts_cell = if c.artifact_dir.is_empty() {
            "—".to_string()
        } else {
            format!("[{0}]({0})", c.artifact_dir)
        };
        out.push_str(&format!(
            "| {} | {} | {} ms | {} |\n",
            c.test_id, c.status, c.duration_ms, artifacts_cell
        ));
    }
    std::fs::write(run_dir.join("summary.md"), out)
}

fn write_failures(
    run_dir: &Path,
    order: &[String],
    cases: &BTreeMap<String, CaseSummary>,
) -> io::Result<()> {
    let mut out = String::new();
    out.push_str("# Failures\n\n");
    let mut wrote_any = false;
    for id in order {
        let c = match cases.get(id) {
            Some(c) => c,
            None => continue,
        };
        if c.status != "fail" && c.status != "stalled" {
            continue;
        }
        wrote_any = true;
        out.push_str(&format!("## {}\n\n", c.test_id));
        if let Some(reason) = &c.stall_reason {
            out.push_str(&format!("**Reason:** {}\n\n", reason));
        }
        if let Some(msg) = &c.error_message {
            out.push_str(&format!("**Error:** {}\n\n", msg));
        }
        if c.expected.is_some() || c.actual.is_some() {
            out.push_str("```diff\n");
            if let Some(e) = &c.expected {
                out.push_str(&format!("- {}\n", e));
            }
            if let Some(a) = &c.actual {
                out.push_str(&format!("+ {}\n", a));
            }
            out.push_str("```\n\n");
        }
        if !c.code_context.is_empty() {
            let lang = code_lang(&c.file);
            out.push_str(&format!("```{}\n", lang));
            for line in &c.code_context {
                out.push_str(line);
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
        if !c.artifact_dir.is_empty() {
            out.push_str(&format!("[Artifacts]({})\n\n", c.artifact_dir));
        }
    }
    if !wrote_any {
        out.push_str("No failures.\n");
    }
    std::fs::write(run_dir.join("failures.md"), out)
}

fn code_lang(file: &str) -> &'static str {
    if file.ends_with(".ts") || file.ends_with(".tsx") {
        "ts"
    } else if file.ends_with(".js") || file.ends_with(".jsx") || file.ends_with(".mjs") {
        "js"
    } else if file.ends_with(".rs") {
        "rust"
    } else if file.ends_with(".py") {
        "python"
    } else {
        ""
    }
}
