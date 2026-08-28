//! 冒烟：统计 web 会话里 user/message 的 source.kind 分布与内容形态。
use dsh_persist::SessionRecorder;

fn main() {
    let home = std::env::var("HOME").unwrap();
    let rec = SessionRecorder::new(format!("{home}/.dsh/sessions").into());
    let list = rec.list().unwrap();
    let target = std::env::args().nth(1).map(|id| list.iter().find(|e| e.id.as_str() == id).expect("id not found").clone()).unwrap_or_else(|| list.iter().find(|e| e.cwd.as_deref().unwrap_or("").contains("deepseek-harness")).expect("no web session").clone());
    let (session, _) = rec.load(&target.id, target.cwd.as_deref()).unwrap();
    let raw = home + "/.dsh/sessions/--Users-liuyq-aiproject-deepseek-harness--/" + target.id.as_str() + "/session.jsonl.zstd";
    let bytes = std::fs::read(&raw).unwrap();
    let text = String::from_utf8(zstd::decode_all(&bytes[..]).unwrap()).unwrap();
    let mut kinds = std::collections::BTreeMap::new();
    for l in text.lines() {
        let v: serde_json::Value = match serde_json::from_str(l) { Ok(v) => v, Err(_) => continue };
        if v.get("type").and_then(|t| t.as_str()) != Some("user/message") { continue; }
        {
            let kind = v.pointer("/data/source/kind").and_then(|k| k.as_str()).unwrap_or("<none>").to_string();
            let producer = v.pointer("/data/source").map(|p| serde_json::to_string(p).unwrap_or_default()).unwrap_or_default();
            println!("SRC {} {}", kind, producer);
            let producer = v.pointer("/data/source").map(|p| serde_json::to_string(p).unwrap_or_default()).unwrap_or_default();
            let content_head = v.pointer("/data/content").map(|c| serde_json::to_string(c).unwrap_or_default()).unwrap_or_default();
            *kinds.entry(kind.clone()).or_insert(0usize) += 1;
            if kind != "user" && kinds.get(&kind).copied().unwrap_or(0) <= 1 {
                println!("kind={} source={} content_head={}", kind, producer, &content_head[..content_head.len().min(150)]);
            }
        }
    }
    println!("user/message source kinds: {:?}", kinds);
    // 走 rec.load 的解析路径：确认注入消息已带 Context source
    let mut ctx = 0;
    for e in session.entries() {
        if let dsh_session::SessionEvent::UserMessage(m) = &e.event {
            if matches!(m.source, dsh_llm::MessageSource::Context { .. }) {
                ctx += 1;
            }
        }
    }
    println!("loaded entries={} context injections={}", session.entries().len(), ctx);
}
