//! Exercises a real capability tool end-to-end: the mock model asks the `fs`
//! tool to read a temp file, the tool reads it, and the transcript carries the
//! actual file content as the tool result.

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_fs::FsTool;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError, LlmProviderInfo,
    LlmRuntime, MessageSource, SessionId, StreamChunk,
};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::Arc;

struct FsMockAdapter;

#[async_trait]
impl LlmAdapter for FsMockAdapter {
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
                StreamChunk::TextDelta { index: 0, text: "read the file successfully".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ]
        } else {
            vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall },
                StreamChunk::ToolCallDelta {
                    index: 0,
                    id: dsh_llm::CallId::new("call_fs"),
                    name: Some("fs".into()),
                    arguments_delta: format!(
                        r#"{{"op":"read","path":"{}"}}"#,
                        path_for_demo()
                    ),
                },
                StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None },
            ]
        };
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn path_for_demo() -> String {
    let p = std::env::temp_dir().join("dsh_fs_demo.txt");
    std::fs::write(&p, "hello from the fs tool").unwrap();
    p.to_string_lossy().replace('\\', "\\\\").to_string()
}

#[tokio::main]
async fn main() {
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _h = llm.register_adapter(&["mock".to_string()], Arc::new(FsMockAdapter)).unwrap();

    let tools = Arc::new(ToolRegistry::new());
    let _fs = tools.register(Arc::new(FsTool::default())).unwrap();

    let agent = ReactLoopAgent::new(
        SessionId::new("fs-demo"),
        AgentOptions { provider: "mock".into(), model: "mock".into(), max_tokens: None, system_prompt: None, compaction: Default::default(), workdir: Default::default() },
        llm,
        tools,
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );
    let _rx = agent.subscribe();
    agent.spawn();

    agent.followup("read the file");
    agent.when_idle().await;

    let session = agent.session();
    let session = session.lock().unwrap();
    println!("=== derived ===");
    for m in session.derive_messages() {
        println!("  role={:?} blocks={:?}", m.role, m.content);
    }

    // The tool result block must carry the real file content.
    let has_content = session.derive_messages().iter().any(|m| {
        m.content.iter().any(|b| matches!(b, dsh_llm::ContentBlock::ToolResult { content, .. }
            if content.iter().any(|c| matches!(c, dsh_llm::ContentBlock::Text { text } if text.contains("hello from the fs tool")))))
    });
    assert!(has_content, "fs tool must return the file content");
    println!("\nOK");
}