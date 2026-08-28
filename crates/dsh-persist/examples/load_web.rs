//! 冒烟：读取一个真实 web 会话并打印事件统计（dsh-persist 验证用）。
use dsh_persist::SessionRecorder;
use dsh_session::SessionEvent;

fn main() {
    let home = std::env::var("USERPROFILE").unwrap();
    let root = format!("{home}\\.dsh\\sessions");
    let rec = SessionRecorder::new(root.into());
    let list = rec.list().unwrap();
    println!("sessions: {}", list.len());
    for e in list.iter() {
        println!("  {} cwd={:?}", e.id.as_str(), e.cwd);
    }
    // 读第一个会话
    let pick = std::env::args().nth(1);
    let found = if let Some(id) = &pick {
        list.iter().find(|e| e.id.as_str() == id)
    } else {
        list.iter().find(|e| e.cwd.is_some())
    };
    if let Some(first) = found {
        let (id, cwd) = (first.id.clone(), first.cwd.clone());
        match rec.load(&id, cwd.as_deref()) {
            Ok((session, cwd)) => {
                println!("loaded {} entries={} cwd={:?}", id.as_str(), session.entries().len(), cwd);
                let mut counts = std::collections::BTreeMap::new();
                for e in session.entries() {
                    let k = match &e.event {
                        SessionEvent::UserMessage(_) => "user",
                        SessionEvent::AssistantMessage { .. } => "assistant",
                        SessionEvent::ToolResult { .. } => "tool-result",
                        SessionEvent::TurnStart { .. } => "turn-start",
                        SessionEvent::TurnEnd { .. } => "turn-end",
                        SessionEvent::StepStart { .. } => "step-start",
                        _ => "other",
                    };
                    *counts.entry(k).or_insert(0) += 1;
                }
                println!("event counts: {counts:?}");
                // 打印第一条用户消息与第一条 assistant 消息预览
                for e in session.entries() {
                    if let SessionEvent::UserMessage(m) = &e.event {
                        let t: String = m.content.iter().filter_map(|b| match b {
                            dsh_llm::ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        }).collect();
                        println!("first user: {t}");
                        break;
                    }
                }
                for e in session.entries() {
                    if let SessionEvent::AssistantMessage { message, .. } = &e.event {
                        let t: String = message.content.iter().filter_map(|b| match b {
                            dsh_llm::ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        }).collect();
                        println!("first assistant text: {}", t.chars().take(60).collect::<String>());
                        break;
                    }
                }
            }
            Err(e) => println!("load failed: {e}"),
        }
    }
}
