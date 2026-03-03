use std::path::{Path, PathBuf};
use crate::Result;

#[derive(Debug)]
enum AgentSystem {
    ClaudeCode { config_dir: PathBuf },
}

/// Detect which coding agent system is installed.
fn detect_agent() -> Option<AgentSystem> {
    let home = dirs::home_dir()?;

    // Claude Code: detect by ~/.claude/ dir or ~/.claude.json
    let claude_dir = home.join(".claude");
    let claude_json = home.join(".claude.json");
    if claude_dir.exists() || claude_json.exists() {
        return Some(AgentSystem::ClaudeCode { config_dir: claude_dir });
    }

    None
}

/// Get the path to the strobe binary.
fn strobe_binary_path() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().to_string())
}

/// Install Strobe MCP config + TDD skill for the detected agent.
pub fn install() -> Result<()> {
    let agent = detect_agent();

    match agent {
        Some(AgentSystem::ClaudeCode { config_dir }) => {
            install_claude_code(&config_dir)?;
            println!("Strobe installed for Claude Code.");
            println!();
            println!("Optional: Enable AI vision for UI observation:");
            println!("  strobe setup-vision");
        }
        None => {
            println!("No supported coding agent detected.");
            println!("Supported: Claude Code (~/.claude/)");
            println!("\nManual setup: add strobe to your MCP config with:");
            println!("  command: \"strobe\"");
            println!("  args: [\"mcp\"]");
            println!();
            println!("Optional: Enable AI vision for UI observation:");
            println!("  strobe setup-vision");
        }
    }

    Ok(())
}

fn install_claude_code(config_dir: &Path) -> Result<()> {
    let binary = strobe_binary_path()?;

    // Write/update MCP config in ~/.claude.json (local scope for current project)
    let claude_json_path = config_dir.parent()
        .unwrap_or(config_dir)
        .join(".claude.json");

    let mut config: serde_json::Value = if claude_json_path.exists() {
        let content = std::fs::read_to_string(&claude_json_path)?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let server_entry = serde_json::json!({
        "type": "stdio",
        "command": binary,
        "args": ["mcp"],
        "env": {}
    });

    // User scope: top-level mcpServers in ~/.claude.json (available across all projects)
    config
        .as_object_mut()
        .and_then(|root| {
            root.entry("mcpServers")
                .or_insert(serde_json::json!({}))
                .as_object_mut()
        })
        .map(|servers| servers.insert("strobe".to_string(), server_entry));

    std::fs::write(&claude_json_path, serde_json::to_string_pretty(&config)?)?;

    // Install debugging skill
    let skills_dir = config_dir.join("skills").join("strobe-debugging");
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::write(
        skills_dir.join("strobe-debugging.md"),
        include_str!("../skills/strobe-debugging.md"),
    )?;

    Ok(())
}
