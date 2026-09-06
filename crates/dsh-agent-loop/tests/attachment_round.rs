//! 端到端验收：带 FileBlock 的用户消息经 ReactLoopAgent 一整轮后——
//!
//! 1. 请求组装把 FileBlock 投影为确定性 handle 文本（provider 只见文本，
//!    内含只读保存路径；上游 projectFilesToText 语义）；
//! 2. 落盘的 user/message 保留 FileBlock（附件块在前、文本在后）；
//! 3. 附件存储的只读别名路径真实存在（模型用文件工具可读）。

use async_trait::async_trait;
use dsh_agent_loop::{AgentOptions, ReactLoopAgent};
use dsh_cordis::EventBus;
use dsh_llm::{
    BoxStream, ContentBlock, ContentBlockType, FileAttachmentRef, FinishReason, GenerateOptions,
    LlmAdapter, LlmError, LlmProviderInfo, LlmRuntime, SessionId, StreamChunk,
};
use dsh_persist::{AttachmentStore, SessionRecorder};
use dsh_session::SessionEvent;
use dsh_system_prompt::SystemPrompt;
use dsh_tools::ToolRegistry;
use futures::stream;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct CapturingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for CapturingAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "mock".into(), name: "Mock".into() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        self.requests.lock().unwrap().push(options);
        let chunks = vec![
            StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text },
            StreamChunk::TextDelta { index: 0, text: "got it".into() },
            StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None },
        ];
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[tokio::test]
async fn file_block_projects_to_handle_text_and_persists() {
    let dir = std::env::temp_dir().join(format!("dsh-loop-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let attachments_root = dir.join("attachments").join("v1");
    let store = Arc::new(AttachmentStore::new(attachments_root.clone()));

    // 先落一份附件字节（模拟 composer 附加时的存储写入）
    let reference = store
        .save_file_verbatim(b"PDF-BYTES", Some("report.pdf"))
        .expect("store file");

    let recorder = Arc::new(SessionRecorder::new(dir.join("sessions")));
    let events = EventBus::new();
    let llm = Arc::new(LlmRuntime::with_events(events.clone()));
    let adapter = Arc::new(CapturingAdapter::default());
    let _reg = llm.register_adapter(&["mock".to_string()], Arc::clone(&adapter) as Arc<dyn LlmAdapter>).unwrap();

    let agent = ReactLoopAgent::new(
        SessionId::new("session-file-e2e"),
        AgentOptions {
            provider: "mock".into(),
            model: "mock".into(),
            max_tokens: None,
            system_prompt: None,
            compaction: Default::default(),
            workdir: Default::default(),
            attachments_root: Some(attachments_root),
        },
        llm,
        Arc::new(ToolRegistry::new()),
        Arc::new(SystemPrompt::new()),
        Arc::new(dsh_session_projection::SessionProjections::default()),
        events,
    );

    let sink_agent = Arc::clone(&agent);
    let sink_recorder = Arc::clone(&recorder);
    agent.set_event_sink(move |event| {
        let id = sink_agent.session().lock().unwrap().id.clone();
        let _ = sink_recorder.append(&id, "/tmp/ws", &event);
    });

    agent.spawn();

    // 模拟 composer 发送：附件块在前、文本在后（上游 sendSession 顺序）
    let msg = dsh_llm::Message::user(vec![
        ContentBlock::File { attachment: reference.clone() },
        ContentBlock::text("请阅读附件"),
    ]);
    agent.send(msg, dsh_agent::InboxTarget::NextTurn);
    agent.when_idle().await;

    // 1) 请求侧：provider 收到的是 handle 文本（含只读路径），无 FileBlock
    let requests = adapter.requests.lock().unwrap();
    assert!(!requests.is_empty(), "adapter must be called");
    let projected = &requests[0].messages;
    let user = projected
        .iter()
        .find(|m| m.content.iter().any(|b| matches!(b, ContentBlock::Text { .. })))
        .expect("projected user message");
    let handle_text = user
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .expect("handle text");
    assert!(handle_text.starts_with("[File \"report.pdf\" (9 bytes, sha256:"), "{handle_text}");
    assert!(handle_text.contains("verbatim read-only copy saved at"), "{handle_text}");
    assert!(
        handle_text.contains("When delegating file work, include this saved path"),
        "{handle_text}"
    );
    // 只读路径指向真实存在的别名文件
    let saved = handle_text
        .split("saved at \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .map(std::path::PathBuf::from)
        .expect("saved path in handle text");
    assert!(saved.exists(), "alias must exist: {}", saved.display());
    assert_eq!(std::fs::read(&saved).unwrap(), b"PDF-BYTES");
    assert!(
        !projected.iter().any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::File { .. }))),
        "provider must never see file blocks"
    );
    drop(requests);

    // 2) 落盘侧：user/message 保留 FileBlock（web 互读的字节级前提）
    let (loaded, _) = recorder
        .load(&SessionId::new("session-file-e2e"), Some("/tmp/ws"))
        .expect("session should load");
    let logged_user = loaded.entries().iter().find_map(|e| match &e.event {
        SessionEvent::UserMessage(m) => Some(m),
        _ => None,
    });
    let logged_user = logged_user.expect("user message persisted");
    match &logged_user.content[0] {
        ContentBlock::File { attachment } => {
            assert_eq!(attachment.attachment_id, reference.attachment_id);
            assert_eq!(attachment.name, "report.pdf");
        }
        other => panic!("file block must persist, got {other:?}"),
    }
    assert!(matches!(&logged_user.content[1], ContentBlock::Text { .. }));

    std::fs::remove_dir_all(&dir).ok();
}
