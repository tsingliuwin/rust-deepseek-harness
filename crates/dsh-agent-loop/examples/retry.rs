//! Demonstrates `agent/request-error` backoff retry: a flaky adapter fails the
//! first attempt with a transient `RATE_LIMIT`, then `attach_retry` (installed
//! as an `agent/request-error` listener) sleeps the resolved backoff delay and
//! retries, so the turn still completes.

use async_trait::async_trait;
use dsh_agent_loop::retry::attach_retry;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError, LlmFailure,
    LlmProviderInfo, LlmRuntime, ResolvedRetryPolicy, SessionId, StreamChunk,
};
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

struct FlakyAdapter {
    calls: AtomicU32,
}

#[async_trait]
impl LlmAdapter for FlakyAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Flaky".into() }
    }

    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        Some(ResolvedRetryPolicy::Normal {
            max_retries: 3,
            retryable_codes: vec!["RATE_LIMIT".into()],
            initial_delay_ms: 10,
            max_delay_ms: 40,
            jitter_ratio: 0.0,
        })
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(Box::pin(stream::iter(vec![StreamChunk::Finish {
                reason: FinishReason::Error {
                    failure: LlmFailure {
                        message: "rate limited (transient)".into(),
                        code: "RATE_LIMIT".into(),
                        status: Some(429),
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                },
                replay_state: None,
            }])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
                StreamChunk::TextDelta { index: 0, text: "recovered after retry".into() },
                StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
            ])))
        }
    }
}

#[tokio::main]
async fn main() {
    let events = EventBus::new();
    // 0.1.2-alpha.2 起：重试进度持久进会话日志（llm/retry 事件），计数从
    // llmRetry 投影读取——安装点需要投影注册表与 agent 槽。
    let projections = Arc::new(dsh_session_projection::SessionProjections::default());
    let agent_slot: Arc<std::sync::OnceLock<Arc<ReactLoopAgent>>> = Arc::new(std::sync::OnceLock::new());
    let _retry = attach_retry(&events, Arc::clone(&projections), Arc::clone(&agent_slot));

    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(FlakyAdapter { calls: AtomicU32::new(0) });
    let _h = llm.register_adapter(&["mock".to_string()], adapter.clone()).unwrap();

    let agent = ReactLoopAgent::new(
        SessionId::new("retry-session"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: None,
        },
        llm,
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );
    let _ = agent_slot.set(Arc::clone(&agent));
    let _rx = agent.subscribe();
    agent.spawn();

    agent.followup("please answer");
    agent.when_idle().await;

    println!("adapter calls: {}", adapter.calls.load(Ordering::SeqCst));
    println!("=== derived ===");
    let session = agent.session();
    let session = session.lock().unwrap();
    for m in session.derive_messages() {
        println!("  role={:?} blocks={:?}", m.role, m.content);
    }
    assert_eq!(adapter.calls.load(Ordering::SeqCst), 2);
    println!("\nOK (two attempts, one retry)");
}