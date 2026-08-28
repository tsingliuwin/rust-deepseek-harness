//! Headless smoke test of the agent spine: a mock adapter that first asks for
//! a tool call and then returns a final answer, plus a tiny calculator tool.
//!
//! This exercises the full turn/step machine, streaming assembly, tool
//! execution, and transcript derivation without a network or a GUI.

use async_trait::async_trait;
use dsh_agent_loop::{AgentEvent, AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, MessageSource, SessionId, StreamChunk,
};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::{Tool, ToolDefinition, ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use futures::stream;
use serde_json::json;
use std::sync::Arc;

/// A scripted adapter: tool call on first step, final answer once a tool result
/// is in history.
struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let has_tool_result = options
            .messages
            .iter()
            .any(|m| matches!(m.source, MessageSource::Tool { .. }));

        let chunks = if has_tool_result {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta { index: 0, text: "The answer is ".into() },
                StreamChunk::TextDelta { index: 0, text: "3.".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ]
        } else {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: dsh_llm::CallId::new("call_1"),
                    name: Some("calculator".into()),
                    arguments_delta: r#"{"op":"add","a":1,"b":2}"#.into(),
                },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

/// A trivial `calculator` tool: `{"op":"add","a":1,"b":2}` -> "3".
struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "calculator".into(),
            description: "Add two integers".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "op": { "type": "string" },
                    "a": { "type": "integer" },
                    "b": { "type": "integer" }
                },
                "required": ["op", "a", "b"]
            }),
        }
    }

    async fn execute(&self, input: &ToolExecutionInput) -> ToolExecutionResult {
        let a = input.arguments.get("a").and_then(|v| v.as_i64()).unwrap_or(0);
        let b = input.arguments.get("b").and_then(|v| v.as_i64()).unwrap_or(0);
        ToolExecutionResult::text(format!("{}", a + b))
    }
}

#[tokio::main]
async fn main() {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _adapter = llm.register_adapter(&["mock".to_string()], Arc::new(MockAdapter)).unwrap();

    let tools = Arc::new(ToolRegistry::new());
    let _tool = tools.register(Arc::new(CalculatorTool)).unwrap();

    let prompt = Arc::new(SystemPrompt::new());

    let agent = ReactLoopAgent::new(
        SessionId::new("mock-session"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: Some("You are a concise assistant.".into()),
        },
        llm,
        tools,
        prompt,
        events,
    );

    let rx = agent.subscribe();
    agent.spawn();

    let printer = tokio::spawn(async move {
        while let Ok(ev) = rx.recv_async().await {
            match ev {
                AgentEvent::TextDelta { text } => print!("{text}"),
                AgentEvent::ReasoningDelta { text } => print!(" [reasoning:{text}]"),
                AgentEvent::ToolCall { name, arguments, .. } => {
                    println!("\n  [tool-call] {name}({arguments})");
                }
                AgentEvent::ToolResult { tool_call_id, is_error } => {
                    println!("\n  [tool-result] {} error={is_error}", tool_call_id.as_str());
                }
                AgentEvent::TurnEnded { turn, reason } => {
                    println!("\n  [turn-end] turn={turn} reason={reason:?}");
                }
                other => println!("\n[event] {other:?}"),
            }
        }
    });

    agent.followup("what is 1 + 2?");
    agent.when_idle().await;

    let session = agent.session();
    let session = session.lock().unwrap();

    println!("\n=== derived messages ===");
    for m in session.derive_messages() {
        println!("  role={:?} source={:?} blocks={:?}", m.role, m.source, m.content);
    }

    println!("\n=== log ({} events) ===", session.entries().len());
    for e in session.entries() {
        println!("  #{:02} {:?}", e.seq, e.event);
    }

    drop(session);
    std::mem::forget(printer);
    println!("\nOK");
}