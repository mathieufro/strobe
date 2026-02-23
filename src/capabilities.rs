//! Runtime capability derivation.
//!
//! Maps detected language + command to a RuntimeCapabilities struct with
//! prescriptive guidance on how to enable full Strobe functionality.

use crate::mcp::{CapabilityLevel, RuntimeCapabilities};
use crate::symbols::Language;

/// Derive baseline capabilities from the detected language and command.
///
/// All limitation messages are prescriptive — they tell the LLM exactly
/// what to do to achieve full Strobe instrumentation.
pub fn derive_capabilities(language: Language, command: &str) -> RuntimeCapabilities {
    let cmd_lower = command.to_lowercase();
    let is_bun = std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains("bun"))
        .unwrap_or(false);

    match language {
        Language::Native => RuntimeCapabilities {
            runtime: "native".to_string(),
            runtime_detail: None,
            function_tracing: CapabilityLevel::Full,
            breakpoints: CapabilityLevel::Full,
            stepping: CapabilityLevel::Full,
            output_capture: CapabilityLevel::Full,
            limitations: vec![],
        },
        Language::Python => RuntimeCapabilities {
            runtime: "cpython".to_string(),
            runtime_detail: Some("Python (CPython)".to_string()),
            function_tracing: CapabilityLevel::Full,
            breakpoints: CapabilityLevel::Full,
            stepping: CapabilityLevel::Partial,
            output_capture: CapabilityLevel::Full,
            limitations: vec![
                "Python stepping supports step-over only. For finer control, set breakpoints at specific lines instead of stepping.".to_string(),
                "Memory read/write (raw addresses) not available for Python. Use debug_memory with variable names to inspect Python objects.".to_string(),
            ],
        },
        Language::JavaScript if is_bun => RuntimeCapabilities {
            runtime: "jsc".to_string(),
            runtime_detail: Some("Bun (JSC)".to_string()),
            function_tracing: CapabilityLevel::None,
            breakpoints: CapabilityLevel::None,
            stepping: CapabilityLevel::None,
            output_capture: CapabilityLevel::Full,
            limitations: vec![
                "Bun's release binary strips all JSC symbols, which disables function tracing, breakpoints, and stepping. \
                 To get full Strobe instrumentation, build Bun from source in debug mode: \
                 git clone https://github.com/oven-sh/bun && cd bun && bun run build — \
                 then use ./build/debug/bun-debug instead of bun. \
                 Debug builds preserve JSC symbols needed for function tracing.".to_string(),
            ],
        },
        Language::JavaScript => {
            let detail = if cmd_lower.contains("node") {
                "Node.js (V8)"
            } else {
                "JavaScript (V8)"
            };
            RuntimeCapabilities {
                runtime: "v8".to_string(),
                runtime_detail: Some(detail.to_string()),
                function_tracing: CapabilityLevel::Full,
                breakpoints: CapabilityLevel::Full,
                stepping: CapabilityLevel::Full,
                output_capture: CapabilityLevel::Full,
                limitations: vec![],
            }
        }
    }
}

/// Merge agent-reported capabilities into the baseline.
///
/// The agent sends a `capabilities` message after tracer.initialize() with
/// runtime-detected details (e.g., actual Python version, whether JSC API
/// symbols were found). This enriches the Rust-side baseline.
pub fn merge_agent_capabilities(
    baseline: &mut RuntimeCapabilities,
    agent_payload: &serde_json::Value,
) {
    // Update runtime_detail if agent provides one
    if let Some(detail) = agent_payload.get("runtimeDetail").and_then(|v| v.as_str()) {
        baseline.runtime_detail = Some(detail.to_string());
    }

    // Update capability levels from agent
    if let Some(ft) = agent_payload.get("functionTracing").and_then(|v| v.as_bool()) {
        baseline.function_tracing = if ft { CapabilityLevel::Full } else { CapabilityLevel::None };
    }
    if let Some(bp) = agent_payload.get("breakpoints").and_then(|v| v.as_bool()) {
        baseline.breakpoints = if bp { CapabilityLevel::Full } else { CapabilityLevel::None };
    }
    if let Some(st) = agent_payload.get("stepping").and_then(|v| v.as_bool()) {
        baseline.stepping = if st { CapabilityLevel::Full } else { CapabilityLevel::None };
    }

    // Replace limitations if agent provides them
    if let Some(lims) = agent_payload.get("limitations").and_then(|v| v.as_array()) {
        let agent_lims: Vec<String> = lims
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !agent_lims.is_empty() {
            baseline.limitations = agent_lims;
        }
    }
}
