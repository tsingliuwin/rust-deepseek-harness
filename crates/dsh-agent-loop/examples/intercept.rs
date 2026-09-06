//! Demonstrates the Cordis event/waterfall layer: plugins intercept the loop
//! through typed `emit` / `waterfall` / `serial` extension points.
//!
//! Registered listeners:
//! - `agent/status` (emit)          — prints every status transition;
//! - `agent/pre-step` (waterfall)   — injects a reminder message (delegates via `next`);
//! - `agent/request` (waterfall)    — amends the call config (sets `max_tokens`);
//! - `llm/stream` (waterfall)       — SHORT-CIRCUITS the model call with its own answer;
//! - `agent/turn-stopping` (serial) — observes the turn's stop boundary.

use async_trait::async_trait;
use dsh_agent::{
    AgentPreStep, AgentRequest, AgentStatusChanged, AgentTurnStopping, PreStepDecision,
};
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError, LlmProviderInfo,
    LlmRuntime, LlmStream, Message, SessionId, StreamChunk,
};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::Arc;

/// Never reached: the `llm/stream` listener short-circuits before adapter
/// resolution. Present so the runtime has a registered route.
struct DummyAdapter;

#[async_trait]
impl LlmAdapter for DummyAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        Ok(Box::pin(stream::empty()))
    }
}

#[tokio::main]
async fn main() {
    let events = EventBus::new();

    // emit: observe status transitions.
    let _status = events.on_emit::<AgentStatusChanged>(|status| {
        println!("[agent/status] {status:?}");
    });

    // waterfall: inject a reminder before the model sees the turn (delegates).
    let _prestep = events.on_waterfall::<AgentPreStep>(|_input, next| {
        Box::pin(async move {
            match next.await {
                PreStepDecision::Enter { mut messages } => {
                    messages.insert(0, Message::user_text(
                        "(system reminder: injected by an agent/pre-step plugin)",
                    ));
                    PreStepDecision::Enter { messages }
                }
                reject @ PreStepDecision::Reject => reject,
            }
        })
    });

    // waterfall: amend the proposed call config (delegates, then mutates).
    let _request = events.on_waterfall::<AgentRequest>(|_payload, next| {
        Box::pin(async move {
            let mut config = next.await;
            config.max_tokens = Some(128);
            config
        })
    });

    // waterfall: short-circuit the model call entirely (no `next()`).
    let _stream = events.on_waterfall::<LlmStream>(|_options, _next| {
        Box::pin(async move {
            let chunks = vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta {
                    index: 0,
                    text: "[short-circuited by an llm/stream plugin]".into(),
                },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ];
            Ok(Box::pin(stream::iter(chunks)) as BoxStream)
        })
    });

    // serial: observe the turn-stopping boundary.
    let _turn_stopping = events.on_serial::<AgentTurnStopping>(|p| {
        Box::pin(async move {
            println!("[agent/turn-stopping] turn={}", p.turn);
            None
        })
    });

    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _adapter = llm.register_adapter(&["mock".to_string()], Arc::new(DummyAdapter)).unwrap();
    let tools = Arc::new(ToolRegistry::new());
    let prompt = Arc::new(SystemPrompt::new());

    let agent = ReactLoopAgent::new(
        SessionId::new("intercept-session"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: Some("You are a concise assistant.".into()),
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        tools,
        prompt,
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );
    let _rx = agent.subscribe();
    agent.spawn();

    agent.followup("what is 1 + 2?");
    agent.when_idle().await;

    let session = agent.session();
    let session = session.lock().unwrap();

    println!("\n=== derived messages ===");
    for m in session.derive_messages() {
        println!("  role={:?} blocks={:?}", m.role, m.content);
    }

    println!("\n=== request header (config was amended) ===");
    if let Some(h) = session.request_header() {
        println!("  {:?}", h.config);
    }

    println!("\nOK");
}