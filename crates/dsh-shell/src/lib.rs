//! dsh-shell — a shell-command tool.
//!
//! One `shell` tool that runs a command line (through `cmd /C` on Windows,
//! `sh -c` elsewhere) and returns combined stdout/stderr. Spawn/collect runs on
//! the blocking thread pool so the streaming loop is never stalled.

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell".into(),
            description: "Run a shell command and return its stdout and stderr.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The command line to run." }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let command = input.arguments.get("command").and_then(|v| v.as_str()).unwrap_or("");
        let command = command.to_string();
        tokio::task::spawn_blocking(move || run(&command))
            .await
            .unwrap_or_else(|e| ToolExecutionResult::error(format!("blocking task failed: {e}")))
    }
}

fn run(command: &str) -> ToolExecutionResult {
    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("cmd").args(["/C", command]).output();
    #[cfg(not(target_os = "windows"))]
    let output = std::process::Command::new("sh").args(["-c", command]).output();

    match output {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            if !o.stderr.is_empty() {
                text.push_str("\n[stderr]\n");
                text.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if o.status.success() {
                ToolExecutionResult::text(text)
            } else {
                ToolExecutionResult::error(text)
            }
        }
        Err(e) => ToolExecutionResult::error(format!("spawn failed: {e}")),
    }
}