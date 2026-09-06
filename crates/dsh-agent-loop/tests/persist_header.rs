//! 端到端验收：mock 一轮对话经真实持久层落盘后，`request/header`
//! （模型实际看到的 system/tools/config）必须出现在日志里。
//!
//! 回归背景：event_to_web_line 曾把 RequestHeader 丢进 `_ => None`，
//! agent-loop 明明发了、盘上却永远没有——「用-查-改-用」的「查」环节
//! 因此无法验证 prompt 修复是否到达模型。此测试把发射（build_request）
//! 与落盘（SessionRecorder）两段串起来，防再次断链。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlockType, FinishReason, GenerateOptions, LlmAdapter, LlmError,
    LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::Arc;

struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, _options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "done".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn turn_persists_request_header() {
    let dir = std::env::temp_dir().join(format!("dsh-loop-header-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));

    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let _adapter = llm.register_adapter(&["mock".to_string()], Arc::new(MockAdapter)).unwrap();

    let agent = ReactLoopAgent::new(
        SessionId::new("session-header-e2e"),
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
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );

    // 与 gpui 宿主同一条落盘缝：sink 原样追加到 recorder
    let sink_agent = Arc::clone(&agent);
    let sink_recorder = Arc::clone(&recorder);
    agent.set_event_sink(move |event| {
        let id = sink_agent.session().lock().unwrap().id.clone();
        let _ = sink_recorder.append(&id, "/tmp/ws", &event);
    });

    agent.spawn();
    agent.followup("hi");
    agent.when_idle().await;

    // 内存 session 里有 header（发射侧存在）
    let logged = {
        let s = agent.session();
        let s = s.lock().unwrap();
        s.request_header().map(|h| h.system.clone()).flatten()
    };
    assert_eq!(logged.as_deref(), Some("You are a concise assistant."));

    // 盘上也有（持久侧不再丢弃），且 system 内容完整
    let (loaded, _) = recorder
        .load(&dsh_llm::SessionId::new("session-header-e2e"), Some("/tmp/ws"))
        .expect("session should load");
    let on_disk = loaded.entries().iter().any(|e| matches!(e.event, SessionEvent::RequestHeader { .. }));
    assert!(on_disk, "request/header must be persisted; entries: {:?}", loaded.entries().iter().map(|e| format!("{:?}", e.event).chars().take(60).collect::<String>()).collect::<Vec<_>>());
    let sys = loaded.request_header().and_then(|h| h.system.clone());
    assert_eq!(sys.as_deref(), Some("You are a concise assistant."));

    std::fs::remove_dir_all(&dir).ok();
}
