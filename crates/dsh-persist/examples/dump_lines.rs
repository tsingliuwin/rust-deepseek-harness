//! 诊断：打印所有会话文件的行类型统计与头信息。
use dsh_persist::SessionRecorder;
fn main() {
    let home = std::env::var("HOME").unwrap();
    let rec = SessionRecorder::new(format!("{home}/.dsh/sessions").into());
    for e in rec.list().unwrap() {
        let cwd = e.cwd.clone().unwrap_or_default();
        match rec.load(&e.id, Some(cwd.as_str())) {
            Ok((session, _)) => {
                let mut counts = std::collections::BTreeMap::new();
                for entry in session.entries() {
                    let k = match &entry.event {
                        dsh_session::SessionEvent::TurnStart { .. } => "turn/start",
                        dsh_session::SessionEvent::TurnEnd { .. } => "turn/end",
                        dsh_session::SessionEvent::StepStart { .. } => "step/start",
                        dsh_session::SessionEvent::StepEnd { .. } => "step/end",
                        dsh_session::SessionEvent::SystemMessage { .. } => "system/message",
                        dsh_session::SessionEvent::UserMessage(m) => {
                            match &m.source {
                                dsh_llm::MessageSource::User => "user/message(user)",
                                dsh_llm::MessageSource::Context { .. } => "user/message(ctx)",
                                _ => "user/message(?)",
                            }
                        }
                        dsh_session::SessionEvent::AssistantMessage { .. } => "assistant/message",
                        dsh_session::SessionEvent::AssistantChunk { .. } => "chunk",
                        dsh_session::SessionEvent::ToolCall { .. } => "tool/call",
                        dsh_session::SessionEvent::ToolResult { .. } => "tool/result",
                        dsh_session::SessionEvent::RequestHeader { .. } => "request/header",
                        dsh_session::SessionEvent::RequestContext(_) => "request/context",
                        dsh_session::SessionEvent::SessionTitle { .. } => "session/title",
                        dsh_session::SessionEvent::LlmRetry { .. } => "llm/retry",
                        dsh_session::SessionEvent::LlmRetryStarted { .. } => "llm/retry-started",
                        dsh_session::SessionEvent::Compaction { .. } => "compaction",
                    };
                    *counts.entry(k).or_insert(0) += 1;
                }
                println!("{} cwd={} events={:?}", e.id.as_str(), cwd, counts);
            }
            Err(err) => println!("{} cwd={} LOAD FAILED: {err}", e.id.as_str(), cwd),
        }
    }
}
