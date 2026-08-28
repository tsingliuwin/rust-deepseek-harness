//! dsh-fs — a local-filesystem tool.
//!
//! One `fs` tool exposing `read` / `write` / `list` / `exists` operations over
//! `std::fs`. This is the milestone shape of the reference's filesystem
//! capability (its policy seam and sandbox provider arrive later).

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;
use std::path::Path;

pub struct FsTool;

#[async_trait]
impl Tool for FsTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs".into(),
            description: "Read, write, list, or check files on the local filesystem.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string", "enum": ["read", "write", "list", "exists"] },
                    "path": { "type": "string", "description": "Filesystem path." },
                    "content": { "type": "string", "description": "Content to write (write op only)." }
                },
                "required": ["op", "path"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let op = input.arguments.get("op").and_then(|v| v.as_str()).unwrap_or("");
        let path = input.arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
        match op {
            "read" => match std::fs::read_to_string(Path::new(path)) {
                Ok(text) => ToolExecutionResult::text(text),
                Err(e) => ToolExecutionResult::error(format!("read failed: {e}")),
            },
            "write" => {
                let content = input.arguments.get("content").and_then(|v| v.as_str()).unwrap_or("");
                match std::fs::write(Path::new(path), content) {
                    Ok(()) => ToolExecutionResult::text(format!("wrote {} bytes", content.len())),
                    Err(e) => ToolExecutionResult::error(format!("write failed: {e}")),
                }
            }
            "list" => match std::fs::read_dir(Path::new(path)) {
                Ok(entries) => {
                    let mut out = String::new();
                    for entry in entries.flatten() {
                        out.push_str(&entry.file_name().to_string_lossy());
                        out.push('\n');
                    }
                    ToolExecutionResult::text(out)
                }
                Err(e) => ToolExecutionResult::error(format!("list failed: {e}")),
            },
            "exists" => ToolExecutionResult::text(format!("{}", Path::new(path).exists())),
            other => ToolExecutionResult::error(format!("unknown op \"{other}\"")),
        }
    }
}