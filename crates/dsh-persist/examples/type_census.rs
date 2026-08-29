//! 诊断：统计 web 日志的精确 type 分布。
use dsh_persist::SessionRecorder;
fn main() {
    let home = std::env::var("HOME").unwrap();
    let rec = SessionRecorder::new(format!("{home}/.dsh/sessions").into());
    let list = rec.list().unwrap();
    let target = list.iter().find(|e| e.id.as_str().contains("7aac")).expect("not found");
    let cwd = target.cwd.clone().unwrap_or_default();
    let raw = format!("{home}/.dsh/sessions/--{}/{}", cwd.replace('/', "-"), target.id.as_str());
    let _ = raw;
    // 借 load 拿不到原始行——直接读文件解压
    let file = format!("{home}/.dsh/sessions/--Users-liuyq-aiproject-deepseek-harness--/{}/session.jsonl.zstd", target.id.as_str());
    let bytes = std::fs::read(&file).unwrap();
    let text = String::from_utf8(zstd::decode_all(&bytes[..]).unwrap()).unwrap();
    let mut counts = std::collections::BTreeMap::new();
    for l in text.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            *counts.entry(v.get("type").and_then(|t| t.as_str()).unwrap_or("?").to_string()).or_insert(0) += 1;
        }
    }
    println!("{counts:?}");
    for l in text.lines() {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            if v.get("type").and_then(|t| t.as_str()) == Some("assistant/chunk") {
                println!("CHUNK SAMPLE: {}", serde_json::to_string(&v).unwrap());
                break;
            }
        }
    }
}
