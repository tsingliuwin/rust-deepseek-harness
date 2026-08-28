//! dsh-web — an HTTP fetch tool.
//!
//! One `web_fetch` tool that GETs a URL and returns its text body (truncated).
//! This is the milestone shape of the reference's web capability (search +
//! fetch providers and their policy arrive later).

use async_trait::async_trait;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult};
use serde_json::json;

pub struct WebTool {
    client: reqwest::Client,
}

impl Default for WebTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebTool {
    pub fn new() -> Self {
        Self { client: reqwest::Client::new() }
    }
}

#[async_trait]
impl Tool for WebTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch a URL and return its text content.".into(),
            parameters: json!({
                "type": "object",
                "properties": { "url": { "type": "string", "description": "The URL to fetch." } },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let url = input.arguments.get("url").and_then(|v| v.as_str()).unwrap_or("");
        match self.client.get(url).send().await {
            Ok(resp) => match resp.text().await {
                Ok(body) => {
                    let truncated: String = body.chars().take(8000).collect();
                    ToolExecutionResult::text(truncated)
                }
                Err(e) => ToolExecutionResult::error(format!("read body failed: {e}")),
            },
            Err(e) => ToolExecutionResult::error(format!("fetch failed: {e}")),
        }
    }
}