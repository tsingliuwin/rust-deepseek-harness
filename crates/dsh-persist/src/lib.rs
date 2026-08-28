//! dsh-persist — 与 web 版 dsh 完全共享的会话持久化。
//!
//! 磁盘布局（web `session-persistence-jsonl` 同款）：
//! `{root}/--{projectKey(cwd)}--/{session-id}/session.jsonl.zstd`
//! - 首行 header：`{"type":"session","version":0,"id",…,"cwd","delegationDepth"}`
//! - 事件行信封：`{"type":"<event>","seq":N,"time":ms,"data":{…}}`
//! 读取时转换为本仓库 `SessionEvent` 词汇表；写入时同样转回 web 信封，
//! 两端互读互通。旧的散文件 `{root}/{id}.jsonl` 仍可读（迁移兼容）。

use dsh_llm::{
    CallId, ContentBlock, Message, MessageId, MessageSource, SessionId,
};
use dsh_session::{Session, SessionEvent};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// web `projectKey(cwd)`：`/`、`\`、`:` 连续合并为 `-`；ASCII 安全字符
/// （字母数字 `._-`，非 `~`）保留；其余按 UTF-16 code unit 转 `~XXXX`
/// （大写 4 位 hex）；去前导 `-`；`--…--` 包裹；251 截断。
pub fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut sep_run = false;
    for ch in cwd.encode_utf16() {
        let c = char::from_u32(ch as u32).unwrap_or('\u{fffd}');
        if c == '/' || c == '\\' || c == ':' {
            if !sep_run {
                readable.push('-');
            }
            sep_run = true;
        } else if c != '~' && (c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') {
            readable.push(c);
            sep_run = false;
        } else {
            readable.push_str(&format!("~{:04X}", ch));
            sep_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let truncated: String = slug.chars().take(251).collect();
    format!("--{}--", truncated)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// 侧栏列表条目：会话 id + 它的 project cwd（目录名反推不可靠，从 header 读）。
pub struct SessionEntry {
    pub id: SessionId,
    pub cwd: Option<String>,
    pub modified: SystemTime,
}

pub struct SessionRecorder {
    root: PathBuf,
}

impl SessionRecorder {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn project_dir(&self, cwd: &str) -> PathBuf {
        self.root.join(project_key(cwd))
    }

    fn session_file(&self, id: &SessionId, cwd: &str) -> PathBuf {
        self.project_dir(cwd).join(id.as_str()).join("session.jsonl.zstd")
    }

    fn legacy_file(&self, id: &SessionId) -> PathBuf {
        self.root.join(format!("{}.jsonl", id.as_str()))
    }

    /// 新建会话：建目录并写 header（web 读取的最小要求）。
    pub fn create(&self, id: &SessionId, cwd: &str, agent_preset: &str) -> io::Result<()> {
        let file = self.session_file(id, cwd);
        fs::create_dir_all(file.parent().unwrap())?;
        let header = serde_json::json!({
            "type": "session",
            "version": 0,
            "id": id.as_str(),
            "createdAt": now_ms(),
            "cwd": cwd,
            "delegationDepth": 0,
            "agentPreset": agent_preset,
        });
        let mut buf = Vec::new();
        writeln!(buf, "{}", header)?;
        self.write_compressed(&file, &buf)?;
        // projcache 身份行：web 侧栏冷启动先读投影（identity 足够定位）
        self.touch_projcache(id, cwd, 0, None);
        Ok(())
    }

    fn projcache_path(&self) -> PathBuf {
        self.root
            .parent()
            .unwrap_or(&self.root)
            .join("storages")
            .join("session_projcache.json")
    }

    /// 把会话投影写入 `storages/session_projcache.json`（web 是该文档的
    /// 另一个写者：读-改-写合并，只动 `tables.sessions.<id>` 的 identity
    /// 与 rows.title）。`title = None` 只建/保 identity（web SessionTitle
    /// 投影行形：`rows.title = {ver:1, seq, val}`，latest-wins）。
    pub fn touch_projcache(&self, id: &SessionId, cwd: &str, seq: u64, title: Option<&str>) {
        let path = self.projcache_path();
        let mut doc: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| {
                serde_json::json!({
                    "unit": {"name": "session_projcache", "version": 3},
                    "global": null,
                    "tables": {"sessions": {}}
                })
            });
        let Some(tables) = doc.get_mut("tables").and_then(|t| t.as_object_mut()) else { return };
        let sessions = tables.entry("sessions".to_string()).or_insert_with(|| serde_json::json!({}));
        let Some(sessions) = sessions.as_object_mut() else { return };
        let row = sessions.entry(id.as_str().to_string()).or_insert_with(|| serde_json::json!({}));
        let Some(row) = row.as_object_mut() else { return };
        let identity = row.entry("identity".to_string()).or_insert_with(|| {
            serde_json::json!({"createdAt": now_ms(), "cwd": cwd})
        });
        if let Some(obj) = identity.as_object_mut() {
            // cwd 以最近一次写入为准（会话可跨工作区移动）
            obj.insert("cwd".into(), serde_json::json!(cwd));
        }
        if let Some(title) = title {
            let rows = row.entry("rows".to_string()).or_insert_with(|| serde_json::json!({}));
            if let Some(rows) = rows.as_object_mut() {
                rows.insert(
                    "title".into(),
                    serde_json::json!({"ver": 1, "seq": seq, "val": title}),
                );
            }
        }
        if let Ok(text) = serde_json::to_string_pretty(&doc) {
            let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
            let _ = std::fs::write(&path, text);
        }
    }

    /// 追加一个事件（web 信封行；读-改-写整文件压缩）。
    pub fn append(&self, id: &SessionId, cwd: &str, event: &SessionEvent) -> io::Result<()> {
        let file = self.session_file(id, cwd);
        let (mut lines, last_seq) = self.read_lines(&file);
        if lines.is_empty() {
            // 目录被外部清掉时自愈：重建 header
            self.create(id, cwd, "standard")?;
            lines = self.read_lines(&file).0;
        }
        let seq = last_seq + 1;
        if let SessionEvent::SessionTitle { title } = event {
            self.touch_projcache(id, cwd, seq, Some(title));
        }
        let row = event_to_web_line(event, seq, now_ms());
        let mut buf = String::new();
        for l in &lines {
            buf.push_str(l);
            buf.push('\n');
        }
        if let Some(row) = row {
            buf.push_str(&row.to_string());
            buf.push('\n');
        }
        let bytes = buf.into_bytes();
        self.write_compressed(&file, &bytes)
    }

    /// 会话标题（`session/title` 事件的最新值）。
    pub fn title_of(&self, id: &SessionId, cwd: &str) -> Option<String> {
        let file = self.session_file(id, cwd);
        let (lines, _) = self.read_lines(&file);
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("session/title"))
            .filter_map(|o| o.get("data")?.get("title")?.as_str().map(|s| s.to_string()))
            .next_back()
    }

    /// 加载会话：优先 web 布局；返回 (Session, 真实 cwd)。
    /// cwd 传空时自动扫描目录定位。
    pub fn load(&self, id: &SessionId, cwd_hint: Option<&str>) -> io::Result<(Session, Option<String>)> {
        // 1) 按 hint
        if let Some(cwd) = cwd_hint {
            let file = self.session_file(id, cwd);
            if file.exists() {
                let events = self.read_events(&file);
                return Ok((Session::from_events(id.clone(), events), Some(cwd.to_string())));
            }
        }
        // 2) 扫描所有 project 目录找该 id
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                let file = dir.join("session.jsonl.zstd");
                if file.exists() {
                    let cwd = read_header_cwd(&file);
                    let events = self.read_events(&file);
                    return Ok((Session::from_events(id.clone(), events), cwd));
                }
            }
        }
        // 3) legacy 散文件
        let legacy = self.legacy_file(id);
        if legacy.exists() {
            let reader = BufReader::new(File::open(&legacy)?);
            let mut events = Vec::new();
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(ev) = serde_json::from_str::<SessionEvent>(&line) {
                    events.push(ev);
                }
            }
            return Ok((Session::from_events(id.clone(), events), None));
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "session not found"))
    }

    /// 列出全部会话：web 布局（含每会话 header 的 cwd）+ 旧散文件。
    pub fn list(&self) -> io::Result<Vec<SessionEntry>> {
        let mut out = Vec::new();
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    if proj.file_type()?.is_file() {
                        // 旧散文件
                        let name = proj.file_name().to_string_lossy().into_owned();
                        if let Some(stem) = name.strip_suffix(".jsonl") {
                            if !stem.starts_with("session-") {
                                let modified = proj
                                    .metadata()
                                    .and_then(|m| m.modified())
                                    .unwrap_or(UNIX_EPOCH);
                                out.push(SessionEntry {
                                    id: SessionId::new(stem.to_string()),
                                    cwd: None,
                                    modified,
                                });
                            }
                        }
                    }
                    continue;
                }
                for sess in fs::read_dir(proj.path())? {
                    let sess = sess?;
                    if !sess.file_type()?.is_dir() {
                        continue;
                    }
                    let name = sess.file_name().to_string_lossy().into_owned();
                    if !name.starts_with("session-") {
                        continue;
                    }
                    let file = sess.path().join("session.jsonl.zstd");
                    if !file.exists() {
                        continue;
                    }
                    let modified = fs::metadata(&file)
                        .and_then(|m| m.modified())
                        .unwrap_or(UNIX_EPOCH);
                    let cwd = read_header_cwd(&file);
                    out.push(SessionEntry {
                        id: SessionId::new(name),
                        cwd,
                        modified,
                    });
                }
            }
        }
        out.sort_by(|a, b| b.modified.cmp(&a.modified));
        Ok(out)
    }

    /// 删除会话（web 布局整目录；散文件兜底）。
    pub fn delete(&self, id: &SessionId, cwd: Option<&str>) -> io::Result<()> {
        if let Some(cwd) = cwd {
            let dir = self.project_dir(cwd).join(id.as_str());
            if dir.exists() {
                return fs::remove_dir_all(dir);
            }
        }
        if self.root.exists() {
            for proj in fs::read_dir(&self.root)? {
                let proj = proj?;
                if !proj.file_type()?.is_dir() {
                    continue;
                }
                let dir = proj.path().join(id.as_str());
                if dir.exists() {
                    return fs::remove_dir_all(dir);
                }
            }
        }
        fs::remove_file(self.legacy_file(id))
    }

    // ---- 内部 ----

    fn read_events(&self, file: &Path) -> Vec<SessionEvent> {
        let (lines, _) = self.read_lines(file);
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| web_line_to_event(&v))
            .collect()
    }

    fn read_lines(&self, file: &Path) -> (Vec<String>, u64) {
        let Ok(bytes) = read_decompressed(file) else { return (Vec::new(), 0) };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut last_seq = 0u64;
        let lines: Vec<String> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
                    if let Some(s) = v.get("seq").and_then(|s| s.as_u64()) {
                        last_seq = last_seq.max(s);
                    }
                }
                l.to_string()
            })
            .collect();
        (lines, last_seq)
    }

    fn write_compressed(&self, file: &Path, bytes: &[u8]) -> io::Result<()> {
        let tmp = file.with_extension("zstd.tmp");
        {
            let mut out = File::create(&tmp)?;
            let mut enc = zstd::stream::Encoder::new(&mut out, 3)?;
            enc.write_all(bytes)?;
            enc.finish()?;
        }
        fs::rename(&tmp, file)
    }
}

fn read_decompressed(file: &Path) -> io::Result<Vec<u8>> {
    let f = File::open(file)?;
    let mut dec = zstd::stream::Decoder::new(f)?;
    let mut out = Vec::new();
    dec.read_to_end(&mut out)?;
    Ok(out)
}

fn read_header_cwd(file: &Path) -> Option<String> {
    let bytes = read_decompressed(file).ok()?;
    let first = bytes.split(|&b| b == b'\n').next()?;
    let v: serde_json::Value = serde_json::from_slice(first).ok()?;
    v.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string())
}

// ---- web 行 ↔ SessionEvent ----

fn blocks_from_web(arr: Option<&serde_json::Value>) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let Some(arr) = arr.and_then(|a| a.as_array()) else { return out };
    for b in arr {
        let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match ty {
            "text" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::text(t));
                }
            }
            "reasoning" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    out.push(ContentBlock::reasoning(t));
                }
            }
            "tool-call" => {
                let id = b.get("id").and_then(|v| v.as_str()).unwrap_or_default();
                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                let args = b.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}");
                out.push(ContentBlock::ToolCall {
                    id: CallId(id.to_string()),
                    name: name.to_string(),
                    arguments: args.to_string(),
                });
            }
            "tool-result" => {
                let call = b.get("toolCallId").and_then(|v| v.as_str()).unwrap_or_default();
                let content = blocks_from_web(b.get("content"));
                let is_error = b.get("isError").and_then(|v| v.as_bool());
                out.push(ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                });
            }
            _ => {}
        }
    }
    out
}

fn blocks_to_web(blocks: &[ContentBlock]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({"type":"text","text":text}),
            ContentBlock::Reasoning { text } => serde_json::json!({"type":"reasoning","text":text}),
            ContentBlock::ToolCall { id, name, arguments } => serde_json::json!({
                "type":"tool-call","id":id.0,"name":name,"arguments":arguments
            }),
            ContentBlock::ToolResult { tool_call_id, content, is_error } => serde_json::json!({
                "type":"tool-result","toolCallId":tool_call_id.0,
                "content":blocks_to_web(content),
                "isError":is_error.unwrap_or(false)
            }),
            ContentBlock::Image { .. } => serde_json::json!({"type":"image"}),
        })
        .collect();
    serde_json::Value::Array(arr)
}

/// web 信封行 → 我们的事件（消息/回合类；chunk 与辅助事件跳过——
/// assistant/message 已含最终内容）。
pub fn web_line_to_event(v: &serde_json::Value) -> Option<SessionEvent> {
    let ty = v.get("type")?.as_str()?;
    let data = v.get("data");
    match ty {
        "turn/start" => Some(SessionEvent::TurnStart { turn: num(data, "turn") }),
        "turn/end" => Some(SessionEvent::TurnEnd {
            turn: num(data, "turn"),
            reason: dsh_session::TurnEndReason::Completed,
        }),
        "step/start" => Some(SessionEvent::StepStart { turn: num(data, "turn"), step: num(data, "step") }),
        "user/message" => {
            let d = data?;
            let content = blocks_from_web(d.get("content"));
            let mut msg = Message::user(content);
            if let Some(id) = d.get("id").and_then(|i| i.as_str()) {
                msg.id = MessageId(id.to_string());
            }
            msg.source = MessageSource::User;
            Some(SessionEvent::UserMessage(msg))
        }
        "assistant/message" => {
            let d = data?;
            let m = d.get("message")?;
            let content = blocks_from_web(m.get("content"));
            let msg = Message::assistant(content, "web", "web");
            Some(SessionEvent::AssistantMessage {
                turn: num(data, "turn"),
                step: num(data, "step"),
                message: msg,
                interrupted: false,
                usage: None,
            })
        }
        "tool/result" => {
            let d = data?;
            let call = d.get("toolCallId").and_then(|v| v.as_str()).unwrap_or_default();
            let content = blocks_from_web(d.get("content"));
            let is_error = d.get("isError").and_then(|v| v.as_bool());
            let msg = Message::assistant(
                vec![ContentBlock::ToolResult {
                    tool_call_id: CallId(call.to_string()),
                    content,
                    is_error,
                }],
                "web",
                "web",
            );
            Some(SessionEvent::ToolResult { turn: 0, step: 0, message: msg })
        }
        // 我们自己的压缩/步末事件（step/end 落盘保证 seq 1:1 对齐，
        // 压缩的 beforeSeq 依赖它；web 端按未知类型忽略）
        "step/end" => Some(SessionEvent::StepEnd { turn: num(data, "turn"), step: num(data, "step") }),
        // 会话标题（web SessionTitleEventData；只取 title，latest-wins）
        "session/title" => Some(SessionEvent::SessionTitle {
            title: data?
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        }),
        "compaction/summary" => Some(SessionEvent::Compaction {
            before_seq: num(data, "beforeSeq"),
            summary: data?.get("summary").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
        }),
        _ => None,
    }
}

fn num(data: Option<&serde_json::Value>, key: &str) -> u64 {
    data.and_then(|d| d.get(key)).and_then(|v| v.as_u64()).unwrap_or_default()
}

/// 我们的事件 → web 信封行（None = 不落盘的辅助事件）。
pub fn event_to_web_line(ev: &SessionEvent, seq: u64, time: u64) -> Option<serde_json::Value> {
    let row = |ty: &str, data: serde_json::Value| {
        serde_json::json!({"type": ty, "seq": seq, "time": time, "data": data})
    };
    match ev {
        SessionEvent::TurnStart { turn } => Some(row("turn/start", serde_json::json!({"turn": turn}))),
        SessionEvent::TurnEnd { turn, .. } => Some(row("turn/end", serde_json::json!({"turn": turn, "reason": {"kind": "completed"}}))),
        SessionEvent::StepStart { turn, step } => Some(row("step/start", serde_json::json!({"turn": turn, "step": step}))),
        SessionEvent::UserMessage(m) => Some(row("user/message", serde_json::json!({
            "content": blocks_to_web(&m.content),
            "source": {"kind": "user"},
            "role": "user",
            "id": m.id.0,
            "surfaceOp": "append",
        }))),
        SessionEvent::AssistantMessage { turn, step, message, .. } => Some(row("assistant/message", serde_json::json!({
            "turn": turn, "step": step,
            "message": {"role": "assistant", "content": blocks_to_web(&message.content)},
        }))),
        SessionEvent::ToolResult { message, .. } => {
            let block = message.content.iter().find_map(|b| match b {
                ContentBlock::ToolResult { tool_call_id, content, is_error } => {
                    Some((tool_call_id, content, is_error))
                }
                _ => None,
            })?;
            Some(row("tool/result", serde_json::json!({
                "toolCallId": block.0 .0,
                "content": blocks_to_web(block.1),
                "isError": block.2.unwrap_or(false),
            })))
        }
        SessionEvent::StepEnd { turn, step } => {
            Some(row("step/end", serde_json::json!({"turn": turn, "step": step})))
        }
        SessionEvent::Compaction { before_seq, summary } => {
            Some(row("compaction/summary", serde_json::json!({"beforeSeq": before_seq, "summary": summary})))
        }
        SessionEvent::SessionTitle { title } => Some(row(
            "session/title",
            serde_json::json!({"title": title, "messageSeqs": [], "source": {"kind": "fallback"}}),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn title_round_trips_to_web_artifacts() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let id = SessionId::new("session-test");
        rec.create(&id, "/tmp/ws", "standard").unwrap();
        rec.append(&id, "/tmp/ws", &SessionEvent::SessionTitle { title: "你好".into() }).unwrap();

        // 事件可读回（web session/title 行）
        assert_eq!(rec.title_of(&id, "/tmp/ws"), Some("你好".into()));
        // projcache 投影行落盘（web 侧栏冷启动来源）
        let pc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.join("storages").join("session_projcache.json")).unwrap(),
        ).unwrap();
        let row = &pc["tables"]["sessions"]["session-test"];
        assert_eq!(row["identity"]["cwd"], "/tmp/ws");
        assert_eq!(row["rows"]["title"]["val"], "你好");
        assert_eq!(row["rows"]["title"]["ver"], 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_scans_all_project_buckets() {
        let dir = std::env::temp_dir().join(format!("dsh-persist-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = SessionRecorder::new(dir.join("sessions"));
        let a = SessionId::new("session-a");
        let b = SessionId::new("session-b");
        rec.create(&a, "/tmp/ws-a", "standard").unwrap();
        rec.create(&b, "/tmp/ws-b", "standard").unwrap();
        let list = rec.list().unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|e| e.id == a && e.cwd.as_deref() == Some("/tmp/ws-a")));
        assert!(list.iter().any(|e| e.id == b && e.cwd.as_deref() == Some("/tmp/ws-b")));
        std::fs::remove_dir_all(&dir).ok();
    }
}
