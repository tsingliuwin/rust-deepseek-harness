//! 会话日志格式 v2（上游 0.1.3-alpha.1 发布格式）的代数路径与迁移。
//!
//! 镜像 `packages/session/session-persistence-jsonl`（format.ts /
//! generation.ts）与 `session-format-v0-to-v1` / `v1-to-v2` 迁移器的净态语义：
//!
//! - 代数文件名：v0 = `session.jsonl(.zstd)`，v1+ = `session.vN.jsonl(.zstd)`；
//!   当前写目标恒为 `session.v3.jsonl.zstd`（0.1.5-alpha.1 起；v0/v1 先
//!   升 v2 再级联到 v3）。
//! - header：`version: 2`，必填 `isSeeded`（v0 的 `seedLength` 被移除，种子
//!   切点改为落 `session/end-seed {inherited:true}` 事件）。
//! - 流内嵌：`assistant/chunk` 不再是事件；每个 attempt 结算为一行
//!   `assistant/message`（data 带 `stream`）或 `assistant/attempt`。
//! - 迁移永不重写源文件：临时文件 → rename 到目标代文件（v2/v3）；
//!   目标已存在视为冲突。未知必读类型拒绝迁移——fail-closed。

use dsh_llm::{AssistantStreamAccumulator, AssistantStreamRecord};
use dsh_llm::SessionId;
use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};

/// 当前会话格式版本（web `SESSION_FORMAT_VERSION`）。0.1.5-alpha.1 起为 3：
/// system prompt 从 request/header 晋升为 `system/message` 面节点、PTC 词汇
/// 改名（code→ptc）、canonical 信封（replace 富形 startSeq/endSeq）。
pub const SESSION_FORMAT_VERSION: u64 = 3;

/// 从文件名解析代数：`session.jsonl.zstd` → 0，`session.vN.jsonl.zstd` → N。
pub fn generation_of(file_name: &str) -> Option<u64> {
    let stem = file_name.strip_suffix(".zstd").unwrap_or(file_name);
    if stem == "session.jsonl" {
        return Some(0);
    }
    stem.strip_prefix("session.v")
        .and_then(|rest| rest.strip_suffix(".jsonl"))
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
}

/// 目标目录内的会话日志代数清单（升序）。
pub fn generation_files(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(generation) = generation_of(entry.file_name().to_string_lossy().as_ref()) {
                out.push((generation, path));
            }
        }
    }
    out.sort_by_key(|(generation, _)| *generation);
    out
}

/// 目录内最新代的日志路径（无任何代时为 None）。
pub fn latest_generation(dir: &Path) -> Option<PathBuf> {
    generation_files(dir).pop().map(|(_, p)| p)
}

/// 读取日志 header（首个信封行）。
pub fn read_header(file: &Path) -> Option<serde_json::Value> {
    let bytes = crate::read_decompressed(file).ok()?;
    let first = bytes.split(|&b| b == b'\n').next()?;
    serde_json::from_slice(first).ok()
}

/// header 的格式版本（无 header / 非 session 行按 0 处理——legacy 兼容）。
pub fn header_version(header: &serde_json::Value) -> u64 {
    header.get("version").and_then(|v| v.as_u64()).unwrap_or(0)
}

/// 读侧版本门：当前构建理解 0/1/2；更高的版本来自更新版 harness，拒绝解释。
pub fn check_readable_version(file: &Path) -> io::Result<()> {
    let Some(header) = read_header(file) else {
        return Ok(());
    };
    let version = header_version(&header);
    if version > SESSION_FORMAT_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "session log format v{version} is newer than this harness (v{SESSION_FORMAT_VERSION}); refusing to interpret the log"
            ),
        ));
    }
    Ok(())
}

// ---- v0 → v2 迁移 ----

/// 一条待重排的条目：普通行，或 chunk 成员（裸 assistant/chunk 行与 packed
/// 行展开的共同表示）。
struct Item {
    seq: u64,
    time: u64,
    row: Option<serde_json::Value>,
    chunk: Option<(u64, u64, serde_json::Value)>, // (turn, step, chunk)
}

/// 把 v0/v1 日志迁移为 v2，发布到同目录 `session.v2.jsonl.zstd`。
///
/// 语义对齐上游两段迁移的净态：
/// - 头升 2、`isSeeded`（v0 `seedLength` 派生），种子切点插
///   `session/end-seed {inherited:true}`；
/// - chunk（裸事件与 packed 行展开）按信封 `sourceEventSeqs` 认领嵌入
///   `assistant/message.stream`，无人认领的组按最后成员位置落
///   `assistant/attempt`；
/// - legacy 行形升级（缺 id/source 的消息、平铺 tool/result、
///   `{beforeSeq,summary}` 压缩行——rustdsh 旧写形）；
/// - seq 致密化（0 基）并重映射全部 seq 引用；
/// - 未知必读类型 → 拒绝（`Err`），源文件保持原样。
pub fn migrate_to_v2(source: &Path, id: &SessionId) -> io::Result<PathBuf> {
    let bytes = crate::read_decompressed(source)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut header: Option<serde_json::Value> = None;
    let mut raw_rows: Vec<serde_json::Value> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| bad_log(format!("malformed line: {e}")))?;
        if v.get("type").and_then(|t| t.as_str()) == Some("session") {
            header = Some(v);
        } else {
            raw_rows.push(v);
        }
    }
    let Some(mut header) = header else {
        return Err(bad_log("no session header"));
    };
    let version = header_version(&header);
    if version >= 2 {
        return Err(bad_log("source is already v2"));
    }

    // 1) 展开为有序条目
    let mut items: Vec<Item> = Vec::new();
    for row in raw_rows {
        let ty = row
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let ignorable = row.get("ignorable").and_then(|i| i.as_bool()) == Some(true);
        if !crate::is_migration_known_type(&ty) && !ignorable {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session log contains event type \"{ty}\" unknown to this harness and not marked ignorable; refusing to migrate the log — it was likely written by a newer harness"
                ),
            ));
        }
        let time = row.get("time").and_then(|v| v.as_u64()).unwrap_or(0);
        let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        match ty.as_str() {
            "text-chunks" | "reasoning-chunks" | "tool-call-chunks" => {
                // packed 行自身无 seq：成员占据 seq0..seq0+n-1（上游 codec）
                let seq0 = row.get("seq0").and_then(|v| v.as_u64()).unwrap_or(seq);
                expand_packed_row(&row, &ty, seq0, time, &mut items)?;
            }
            "assistant/chunk" => {
                let data = row.get("data").cloned().unwrap_or(serde_json::Value::Null);
                let turn = data.get("turn").and_then(|v| v.as_u64()).unwrap_or(0);
                let step = data.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
                let chunk = data.get("chunk").cloned().unwrap_or(serde_json::Value::Null);
                items.push(Item { seq, time, row: None, chunk: Some((turn, step, chunk)) });
            }
            _ => items.push(Item { seq, time, row: Some(row), chunk: None }),
        }
    }
    items.sort_by_key(|i| i.seq);

    // 2) 认领集与成员索引
    let mut claimed: BTreeSet<u64> = Default::default();
    for item in &items {
        let Some(row) = &item.row else { continue };
        if row.get("type").and_then(|t| t.as_str()) != Some("assistant/message") {
            continue;
        }
        for seq in envelope_claim_seqs(row) {
            claimed.insert(seq);
        }
    }
    let mut members: HashMap<u64, (u64, u64, u64, serde_json::Value)> = Default::default();
    for item in &items {
        if let Some((turn, step, chunk)) = &item.chunk {
            members.insert(item.seq, (item.time, *turn, *step, chunk.clone()));
        }
    }

    // 3) 顺序走条目：未认领 chunk 成组；任何普通行之前冲刷为 attempt
    let session_id = id.as_str().to_string();
    let mut staged: Vec<serde_json::Value> = Vec::new();
    let mut old_to_new: HashMap<u64, u64> = Default::default();
    let mut message_ids: HashMap<u64, String> = Default::default();
    let mut group: Option<(u64, u64, Vec<u64>)> = None; // (turn, step, member seqs)

    for item in items {
        if let Some((turn, step, _)) = item.chunk {
            if claimed.contains(&item.seq) {
                continue; // 由承载 message 行回填映射
            }
            match &mut group {
                Some((g_turn, g_step, seqs)) if *g_turn == turn && *g_step == step => {
                    seqs.push(item.seq);
                }
                _ => {
                    flush_group(
                        group.take(),
                        &members,
                        &mut staged,
                        &mut old_to_new,
                    );
                    group = Some((turn, step, vec![item.seq]));
                }
            }
            continue;
        }
        // 普通行：先冲刷未认领组
        flush_group(group.take(), &members, &mut staged, &mut old_to_new);
        let mut row = item.row.clone().expect("non-chunk item carries a row");
        let ty = row
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        match ty.as_str() {
            "assistant/message" => {
                let claim_seqs = envelope_claim_seqs(&row);
                upgrade_assistant_message(&mut row, &session_id, item.seq, &members)?;
                let new_seq = staged.len() as u64;
                for s in claim_seqs {
                    old_to_new.insert(s, new_seq);
                }
            }
            "tool/result" => upgrade_tool_result(&mut row, &session_id, item.seq, &message_ids),
            "user/message" => upgrade_user_message(&mut row, &session_id, item.seq),
            "compaction/summary" => upgrade_compaction(&mut row, &session_id, item.seq),
            _ => {}
        }
        message_ids.insert(item.seq, row_message_id(&row, &ty));
        old_to_new.insert(item.seq, staged.len() as u64);
        staged.push(row);
    }
    flush_group(group.take(), &members, &mut staged, &mut old_to_new);

    // 4) 重映射剩余 seq 引用
    for row in staged.iter_mut() {
        remap_row_refs(row, &old_to_new);
    }

    // 5) header 升版
    let seed_length = header.get("seedLength").and_then(|v| v.as_u64());
    if let Some(obj) = header.as_object_mut() {
        obj.remove("seedLength");
        // 本迁移的产物恒为 v2（不跟随 SESSION_FORMAT_VERSION——级联的
        // 下一段以该值判定输入代数）
        obj.insert("version".into(), serde_json::json!(2));
        obj.insert(
            "isSeeded".into(),
            serde_json::json!(seed_length.map(|l| l > 0).unwrap_or(false)),
        );
    }

    // 6) 种子切点行（seedLength > 0 时插在切点处）
    if let Some(cut) = seed_length.filter(|l| *l > 0) {
        let pos = staged
            .iter()
            .position(|row| row.get("seq").and_then(|v| v.as_u64()).map(|s| s >= cut).unwrap_or(true))
            .unwrap_or(staged.len());
        let time = staged
            .get(pos)
            .and_then(|row| row.get("time"))
            .cloned()
            .or_else(|| header.get("createdAt").cloned())
            .unwrap_or(serde_json::json!(0));
        staged.insert(
            pos,
            serde_json::json!({
                "type": "session/end-seed", "seq": 0, "time": time,
                "data": {"inherited": true},
            }),
        );
    }

    // 7) 致密化：staged 顺序即新 seq（0 基，与上游 stage() 的 staged.length 一致）
    for (index, row) in staged.iter_mut().enumerate() {
        if let Some(obj) = row.as_object_mut() {
            obj.insert("seq".into(), serde_json::json!(index));
        }
    }

    // 8) 发布：临时文件 → rename（源文件与字节永不改动）
    let parent = source
        .parent()
        .ok_or_else(|| bad_log("source has no parent dir"))?;
    let target = parent.join("session.v2.jsonl.zstd");
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "migration target session.v2.jsonl.zstd already exists",
        ));
    }
    let mut payload =
        serde_json::to_string(&header).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    payload.push('\n');
    for row in &staged {
        payload
            .push_str(&serde_json::to_string(row).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?);
        payload.push('\n');
    }
    let compressed = zstd::stream::encode_all(payload.as_bytes(), 0)?;
    let tmp = tmp_path(parent); // 一次性计算：两次调用会得到不同 nanos 名
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&compressed)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

fn tmp_path(parent: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    parent.join(format!("session.migration.{nanos:016x}.jsonl.zstd.tmp"))
}

fn bad_log(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("migration: {what}"))
}

/// 信封层 sourceEventSeqs（数字或 [start,end] 区间）展开为 seq 集。
fn envelope_claim_seqs(row: &serde_json::Value) -> Vec<u64> {
    let mut out = Vec::new();
    if let Some(ranges) = row.get("sourceEventSeqs").and_then(|v| v.as_array()) {
        for entry in ranges {
            match entry {
                serde_json::Value::Number(n) => {
                    if let Some(s) = n.as_u64() {
                        out.push(s);
                    }
                }
                serde_json::Value::Array(pair) if pair.len() == 2 => {
                    if let (Some(a), Some(b)) = (pair[0].as_u64(), pair[1].as_u64()) {
                        out.extend(a..=b);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// 冲刷未认领组：一行 `assistant/attempt`（位置 = 最后成员，上游同款）。
fn flush_group(
    group: Option<(u64, u64, Vec<u64>)>,
    members: &HashMap<u64, (u64, u64, u64, serde_json::Value)>,
    staged: &mut Vec<serde_json::Value>,
    old_to_new: &mut HashMap<u64, u64>,
) {
    let Some((turn, step, seqs)) = group else { return };
    if seqs.is_empty() {
        return;
    }
    let mut acc = AssistantStreamAccumulator::new();
    let mut last_time = 0u64;
    for seq in &seqs {
        if let Some((time, _, _, chunk)) = members.get(seq) {
            match serde_json::from_value::<dsh_llm::StreamChunk>(chunk.clone()) {
                Ok(c) => acc.push(*time, c),
                Err(_) => {}
            }
            last_time = *time;
        }
    }
    for seq in &seqs {
        old_to_new.insert(*seq, staged.len() as u64);
    }
    let stream: Vec<AssistantStreamRecord> = acc.snapshot();
    staged.push(serde_json::json!({
        "type": "assistant/attempt",
        "seq": 0,
        "time": last_time,
        "data": {
            "turn": turn,
            "step": step,
            "stream": serde_json::to_value(stream).unwrap_or(serde_json::json!([])),
        },
    }));
}

/// v0 packed 行（text-chunks/reasoning-chunks/tool-call-chunks）→ chunk 成员。
/// 行形（上游 codec.ts expandPackedRow）：`{type, seq0, time0,
/// data:{turn, step, index, dt[], texts[]|args[](, name)}}`。
fn expand_packed_row(
    row: &serde_json::Value,
    ty: &str,
    seq0: u64,
    time0: u64,
    items: &mut Vec<Item>,
) -> io::Result<()> {
    let data = row.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let turn = data.get("turn").and_then(|v| v.as_u64()).unwrap_or(0);
    let step = data.get("step").and_then(|v| v.as_u64()).unwrap_or(0);
    let index = data.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let is_tool = ty == "tool-call-chunks";
    let payload_key = if is_tool { "args" } else { "texts" };
    let empty = Vec::new();
    let payload: &Vec<serde_json::Value> = data
        .get(payload_key)
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    let gaps: Vec<i64> = data
        .get("dt")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|g| g.as_i64().unwrap_or(0)).collect())
        .unwrap_or_default();
    let mut time = time0;
    for (i, member) in payload.iter().enumerate() {
        let Some(text) = member.as_str() else {
            return Err(bad_log(format!("{ty} row payload member must be a string")));
        };
        if i > 0 {
            time = (time as i64 + gaps.get(i - 1).copied().unwrap_or(0)) as u64;
        }
        let chunk = match ty {
            "text-chunks" => serde_json::json!({"type": "text-delta", "index": index, "text": text}),
            "reasoning-chunks" => {
                serde_json::json!({"type": "reasoning-delta", "index": index, "text": text})
            }
            _ => {
                let mut c = serde_json::json!({
                    "type": "tool-call-delta", "index": index,
                    "id": data.get("id").cloned().unwrap_or(serde_json::json!("")),
                    "argumentsDelta": text,
                });
                if let Some(name) = data.get("name") {
                    c["name"] = name.clone();
                }
                c
            }
        };
        items.push(Item {
            seq: seq0 + i as u64,
            time,
            row: None,
            chunk: Some((turn, step, chunk)),
        });
    }
    Ok(())
}

/// assistant/message 行升级：message.id/source 补齐（rustdsh 旧写形）、认领
/// 成员重放为 stream、信封 sourceEventSeqs 摘除。
fn upgrade_assistant_message(
    row: &mut serde_json::Value,
    session_id: &str,
    seq: u64,
    members: &HashMap<u64, (u64, u64, u64, serde_json::Value)>,
) -> io::Result<()> {
    let claim_seqs = envelope_claim_seqs(row);
    // 流：认领成员按序重放（先取数，避免与 row 的可变借用交叠）
    let mut acc = AssistantStreamAccumulator::new();
    let mut has_members = false;
    for old_seq in &claim_seqs {
        if let Some((time, _, _, chunk)) = members.get(old_seq) {
            if let Ok(c) = serde_json::from_value::<dsh_llm::StreamChunk>(chunk.clone()) {
                acc.push(*time, c);
                has_members = true;
            }
        }
    }
    if let Some(obj) = row.as_object_mut() {
        obj.remove("sourceEventSeqs");
    }
    let stream_value = if has_members {
        let stream: Vec<AssistantStreamRecord> = acc.snapshot();
        serde_json::to_value(stream).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    } else {
        serde_json::json!([])
    };
    let data = row.get_mut("data").ok_or_else(|| bad_log("assistant/message lacks data"))?;
    let message = data
        .get_mut("message")
        .ok_or_else(|| bad_log("assistant/message lacks message"))?;
    if message
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        if let Some(obj) = message.as_object_mut() {
            obj.insert(
                "id".into(),
                serde_json::json!(format!("legacy-message:{session_id}:{seq}")),
            );
        }
    }
    if message.get("source").map(|s| !s.is_object()).unwrap_or(true) {
        if let Some(obj) = message.as_object_mut() {
            obj.insert(
                "source".into(),
                serde_json::json!({"kind": "model", "provider": "legacy", "model": "legacy"}),
            );
        }
    }
    if let Some(obj) = data.as_object_mut() {
        obj.insert("stream".into(), stream_value);
    }
    Ok(())
}

/// tool/result 平铺形（{toolCallId, content, isError}）→ message 形。
/// surfaceOp.replace 起点可继承消息 id（上游 replacementStart 规则）。
fn upgrade_tool_result(
    row: &mut serde_json::Value,
    session_id: &str,
    seq: u64,
    message_ids: &HashMap<u64, String>,
) {
    // surfaceOp.replace 的继承起点（信封层；先读再借 data）
    let inherited = row
        .get("surfaceOp")
        .filter(|op| op.get("op").and_then(|o| o.as_str()) == Some("replace"))
        .and_then(|op| op.get("start"))
        .and_then(|s| s.as_u64())
        .and_then(|start| message_ids.get(&start).cloned());
    let Some(data) = row.get_mut("data") else { return };
    let flat = data.get("toolCallId").is_some() && data.get("message").is_none();
    if !flat {
        return;
    }
    let Some(obj) = data.as_object_mut() else { return };
    let call_id = obj.get("toolCallId").cloned().unwrap_or(serde_json::json!(""));
    let content = obj.remove("content").unwrap_or(serde_json::json!([]));
    let is_error = obj.remove("isError").unwrap_or(serde_json::json!(false));
    let message_id = inherited.unwrap_or_else(|| format!("legacy-message:{session_id}:{seq}"));
    obj.insert(
        "message".into(),
        serde_json::json!({
            "id": message_id,
            "role": "user",
            "content": [
                {"type": "tool-result", "toolCallId": call_id, "content": content, "isError": is_error}
            ],
            "source": {"kind": "tool", "callId": call_id},
        }),
    );
}

/// user/message 补 id/role；data 内误写的 surfaceOp（rustdsh 旧写形）移除。
fn upgrade_user_message(row: &mut serde_json::Value, session_id: &str, seq: u64) {
    let Some(data) = row.get_mut("data") else { return };
    let Some(obj) = data.as_object_mut() else { return };
    if obj
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.is_empty())
        .unwrap_or(true)
    {
        obj.insert(
            "id".into(),
            serde_json::json!(format!("legacy-message:{session_id}:{seq}")),
        );
    }
    if obj.get("role").is_none() {
        obj.insert("role".into(), serde_json::json!("user"));
    }
    obj.remove("surfaceOp");
}

/// compaction/summary legacy 形（{beforeSeq, summary}）→ v2 富形。
fn upgrade_compaction(row: &mut serde_json::Value, session_id: &str, seq: u64) {
    let Some(data) = row.get_mut("data") else { return };
    let Some(obj) = data.as_object_mut() else { return };
    if obj.get("compactionId").is_some() {
        return; // 已是富形
    }
    let before_seq = obj.get("beforeSeq").and_then(|v| v.as_u64()).unwrap_or(0);
    obj.remove("beforeSeq");
    obj.insert(
        "compactionId".into(),
        serde_json::json!(format!("legacy-compaction:{session_id}:{seq}")),
    );
    obj.insert("shadowedRange".into(), serde_json::json!({"start": 0, "end": before_seq}));
    obj.insert("shadowedSeqs".into(), serde_json::json!([]));
    obj.insert("shadowedTokenCount".into(), serde_json::json!(0));
    obj.insert("provider".into(), serde_json::json!("legacy"));
    obj.insert("model".into(), serde_json::json!("legacy"));
}

/// 重映射一行内所有 seq 引用（致密化前的旧 seq → 新 seq）。
fn remap_row_refs(row: &mut serde_json::Value, map: &HashMap<u64, u64>) {
    let remap = |v: u64| map.get(&v).copied().unwrap_or(v);
    let ty = row
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    if let Some(obj) = row.as_object_mut() {
        if let Some(serde_json::Value::Array(ranges)) = obj.get_mut("sourceEventSeqs") {
            for entry in ranges.iter_mut() {
                match entry {
                    serde_json::Value::Number(_) => {
                        let v = entry.as_u64().unwrap_or(0);
                        *entry = serde_json::json!(remap(v));
                    }
                    serde_json::Value::Array(pair) if pair.len() == 2 => {
                        let a = pair[0].as_u64().unwrap_or(0);
                        let b = pair[1].as_u64().unwrap_or(0);
                        *entry = serde_json::json!([remap(a), remap(b)]);
                    }
                    _ => {}
                }
            }
        }
        if let Some(op) = obj.get_mut("surfaceOp") {
            if let Some(o) = op.as_object_mut() {
                for key in ["start", "end"] {
                    if let Some(v) = o.get(key).and_then(|v| v.as_u64()) {
                        o.insert(key.into(), serde_json::json!(remap(v)));
                    }
                }
            }
        }
    }
    let Some(data) = row.get_mut("data") else { return };
    let Some(obj) = data.as_object_mut() else { return };
    match ty.as_str() {
        "compaction/summary" | "compaction/prune" => {
            if let Some(range) = obj.get_mut("shadowedRange").and_then(|r| r.as_object_mut()) {
                for key in ["start", "end"] {
                    if let Some(v) = range.get(key).and_then(|v| v.as_u64()) {
                        range.insert(key.into(), serde_json::json!(remap(v)));
                    }
                }
            }
            if let Some(serde_json::Value::Array(seqs)) = obj.get_mut("shadowedSeqs") {
                for s in seqs.iter_mut() {
                    let v = s.as_u64().unwrap_or(0);
                    *s = serde_json::json!(remap(v));
                }
            }
        }
        "session/title" | "session/title-llm-request" => {
            if let Some(serde_json::Value::Array(seqs)) = obj.get_mut("messageSeqs") {
                for s in seqs.iter_mut() {
                    let v = s.as_u64().unwrap_or(0);
                    *s = serde_json::json!(remap(v));
                }
            }
        }
        "command/done" => {
            if let Some(v) = obj.get("sourceEventSeq").and_then(|v| v.as_u64()) {
                obj.insert("sourceEventSeq".into(), serde_json::json!(remap(v)));
            }
        }
        _ => {}
    }
}

fn row_message_id(row: &serde_json::Value, ty: &str) -> String {
    let data = row.get("data");
    let message = if ty == "user/message" {
        data
    } else {
        data.and_then(|d| d.get("message"))
    };
    message
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

// ---- v2 → v3 迁移 ----

/// 把 v2 日志迁移为 v3，发布到同目录 `session.v3.jsonl.zstd`。语义对齐上游
/// `session-format-v2-to-v3`（0.1.5-alpha.1）净态：头升 3、agentPreset
/// code→ptc；system prompt 晋升（request/header 的 header.system 抽出，
/// 变化时在该行前插入 system/message；首个 step/start 后补空 head）；PTC
/// 改名（tool/ptc-dispatch*、tools-code-mode→tools-ptc）；canonical 信封
/// （request/header 省略 system/空 tools/空 adapterDefaults）；引用重映射
/// （command/done、compaction、session/title*、信封 sourceEventSeqs 与
/// surfaceOp replace 范围）；未知必读类型拒绝；源文件字节永不改动。
pub fn migrate_v2_to_v3(source: &Path, id: &SessionId) -> io::Result<PathBuf> {
    let bytes = crate::read_decompressed(source)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut header: Option<serde_json::Value> = None;
    let mut raw_rows: Vec<serde_json::Value> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value =
            serde_json::from_str(line).map_err(|e| bad_log(format!("malformed line: {e}")))?;
        if v.get("type").and_then(|t| t.as_str()) == Some("session") {
            header = Some(v);
        } else {
            raw_rows.push(v);
        }
    }
    let Some(mut header) = header else {
        return Err(bad_log("no session header"));
    };
    if header_version(&header) != 2 {
        return Err(bad_log("source is not v2"));
    }
    if let Some(obj) = header.as_object_mut() {
        obj.insert("version".into(), serde_json::json!(3));
        if obj.get("agentPreset").and_then(|v| v.as_str()) == Some("code") {
            obj.insert("agentPreset".into(), serde_json::json!("ptc"));
        }
    }

    let session_id = id.as_str().to_string();
    let mut staged: Vec<serde_json::Value> = Vec::new();
    let mut old_to_new: HashMap<u64, u64> = Default::default();
    let mut head: Option<u64> = None;
    let mut last_prompt = String::new();
    let mut step: Option<(u64, u64)> = None;
    let mut generated_ids: BTreeSet<String> = Default::default();

    // 生成的 system/message 行（上游 emitSystem：id 带 sha256 身份、固定
    // plugin source、append 或精确 replace head + sourceEventSeqs）
    fn emit_system(
        staged: &mut Vec<serde_json::Value>,
        head: &mut Option<u64>,
        generated_ids: &mut BTreeSet<String>,
        session_id: &str,
        prompt: &str,
        anchor_seq: u64,
        anchor_type: &str,
        anchor_time: u64,
        turn: u64,
        step: u64,
    ) -> io::Result<()> {
        let identity = serde_json::json!([
            "session-format-v2-to-v3",
            session_id,
            anchor_seq,
            anchor_type,
        ]);
        let digest = {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(identity.to_string().as_bytes());
            let bytes = hasher.finalize();
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };
        let msg_id = format!("v2-to-v3-system-{digest}");
        if generated_ids.contains(&msg_id) {
            return Err(bad_log("generated system message id collides"));
        }
        generated_ids.insert(msg_id.clone());
        let target_seq = staged.len() as u64;
        let mut row = serde_json::json!({
            "type": "system/message",
            "seq": target_seq,
            "time": anchor_time,
            "data": {
                "turn": turn,
                "step": step,
                "message": {
                    "id": msg_id,
                    "role": "system",
                    "source": {"kind": "plugin", "plugin": "@deepseek-ai/dsh-system-prompt"},
                    "content": if prompt.is_empty() {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([{"type": "text", "text": prompt}])
                    },
                },
            },
        });
        match *head {
            None => row["surfaceOp"] = serde_json::json!("append"),
            Some(h) => {
                row["surfaceOp"] =
                    serde_json::json!({"op": "replace", "startSeq": h, "endSeq": h});
                row["sourceEventSeqs"] = serde_json::json!([h]);
            }
        }
        *head = Some(target_seq);
        staged.push(row);
        Ok(())
    }

    for row in raw_rows {
        let ty = row
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let ignorable = row.get("ignorable").and_then(|i| i.as_bool()) == Some(true);
        if !crate::is_migration_known_type(&ty) && !ignorable {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "session log contains event type \"{ty}\" unknown to this harness and not marked ignorable; refusing to migrate the log — it was likely written by a newer harness"
                ),
            ));
        }
        let source_seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let time = row.get("time").and_then(|v| v.as_u64()).unwrap_or(0);
        let mut row = row;

        // request/header：抽 system 晋升（在该行之前插入），canonical 收尾
        if ty == "request/header" {
            let prompt = row
                .pointer("/data/header/system")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            if prompt != last_prompt {
                let (st_turn, st_step) = step.unwrap_or((0, 0));
                emit_system(
                    &mut staged,
                    &mut head,
                    &mut generated_ids,
                    &session_id,
                    &prompt,
                    source_seq,
                    "request/header",
                    time,
                    st_turn,
                    st_step,
                )?;
                last_prompt = prompt;
            }
            if let Some(header_obj) =
                row.pointer_mut("/data/header").and_then(|h| h.as_object_mut())
            {
                header_obj.remove("system");
                if header_obj.get("tools").and_then(|t| t.as_array()).is_some_and(|a| a.is_empty())
                {
                    header_obj.remove("tools");
                }
                if header_obj
                    .get("adapterDefaults")
                    .and_then(|a| a.as_object())
                    .is_some_and(|o| o.is_empty())
                {
                    header_obj.remove("adapterDefaults");
                }
            }
        }

        // PTC 改名
        match ty.as_str() {
            "tool/code-dispatch-start" => {
                row["type"] = serde_json::json!("tool/ptc-dispatch-start");
            }
            "tool/code-dispatch" => {
                row["type"] = serde_json::json!("tool/ptc-dispatch");
            }
            "agent-preset/selected" => {
                if row.pointer("/data/agentPreset").and_then(|v| v.as_str()) == Some("code") {
                    row["data"]["agentPreset"] = serde_json::json!("ptc");
                }
            }
            "user/message" => {
                if row.pointer("/data/source/plugin").and_then(|v| v.as_str())
                    == Some("tools-code-mode")
                {
                    row["data"]["source"]["plugin"] = serde_json::json!("tools-ptc");
                }
            }
            _ => {}
        }

        // step 上下文
        match ty.as_str() {
            "step/start" => {
                let turn = row.pointer("/data/turn").and_then(|v| v.as_u64()).unwrap_or(0);
                let stp = row.pointer("/data/step").and_then(|v| v.as_u64()).unwrap_or(0);
                step = Some((turn, stp));
            }
            "step/end" | "turn/end" => step = None,
            _ => {}
        }

        // 落行（引用重映射只指向更早的源行）
        remap_v3_row_refs(&mut row, &old_to_new);
        let new_seq = staged.len() as u64;
        old_to_new.insert(source_seq, new_seq);
        staged.push(row);

        // 首个 step/start 后补空 head（上游锚序：step/start 行先 emit）
        if ty == "step/start" && head.is_none() {
            let (st_turn, st_step) = step.unwrap_or((0, 0));
            emit_system(
                &mut staged,
                &mut head,
                &mut generated_ids,
                &session_id,
                "",
                source_seq,
                "step/start",
                time,
                st_turn,
                st_step,
            )?;
        }
    }

    // 致密化（插入行已占目标位；顺序即新 seq，0 基）+ canonical surfaceOp
    // replace 富形（上游 canonicalizeTransformedEvent：start/end → startSeq/endSeq）
    for (index, row) in staged.iter_mut().enumerate() {
        if let Some(obj) = row.as_object_mut() {
            obj.insert("seq".into(), serde_json::json!(index));
            if let Some(op) = obj.get_mut("surfaceOp") {
                if op.get("op").and_then(|o| o.as_str()) == Some("replace") {
                    let start = op.get("startSeq").or_else(|| op.get("start")).and_then(|v| v.as_u64());
                    let end = op.get("endSeq").or_else(|| op.get("end")).and_then(|v| v.as_u64());
                    if let (Some(s), Some(e)) = (start, end) {
                        *op = serde_json::json!({"op": "replace", "startSeq": s, "endSeq": e});
                    }
                }
            }
        }
    }

    // 发布：临时文件 → rename（源文件与字节永不改动）
    let parent = source
        .parent()
        .ok_or_else(|| bad_log("source has no parent dir"))?;
    let target = parent.join("session.v3.jsonl.zstd");
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "migration target session.v3.jsonl.zstd already exists",
        ));
    }
    let mut payload =
        serde_json::to_string(&header).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    payload.push('\n');
    for row in &staged {
        payload.push_str(
            &serde_json::to_string(row).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        );
        payload.push('\n');
    }
    let compressed = zstd::stream::encode_all(payload.as_bytes(), 0)?;
    let tmp = tmp_path(parent);
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&compressed)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

/// v2→v3 的引用重映射（上游 remapEvent 的审计字段集；surfaceOp 范围在
/// v2 存储形 start/end 与 v3 形 startSeq/endSeq 之间读到哪个用哪个）。
fn remap_v3_row_refs(row: &mut serde_json::Value, map: &HashMap<u64, u64>) {
    let one = |v: Option<u64>| -> Option<u64> { v.and_then(|s| map.get(&s).copied()) };
    let ty = row.get("type").and_then(|t| t.as_str()).unwrap_or_default().to_string();
    match ty.as_str() {
        "command/done" => {
            if let Some(seq) = one(row.pointer("/data/sourceEventSeq").and_then(|v| v.as_u64())) {
                row["data"]["sourceEventSeq"] = serde_json::json!(seq);
            }
        }
        "compaction/summary" | "compaction/prune" => {
            let start = one(row.pointer("/data/shadowedRange/start").and_then(|v| v.as_u64()));
            let end = one(row.pointer("/data/shadowedRange/end").and_then(|v| v.as_u64()));
            if let (Some(s), Some(e)) = (start, end) {
                row["data"]["shadowedRange"] = serde_json::json!({"start": s, "end": e});
            }
            if let Some(seqs) = row.pointer("/data/shadowedSeqs").and_then(|v| v.as_array()).cloned() {
                let mapped: Vec<u64> = seqs
                    .iter()
                    .filter_map(|s| s.as_u64())
                    .filter_map(|s| map.get(&s).copied())
                    .collect();
                row["data"]["shadowedSeqs"] = serde_json::json!(mapped);
            }
        }
        "session/title" | "session/title-llm-request" => {
            if let Some(seqs) = row.pointer("/data/messageSeqs").and_then(|v| v.as_array()).cloned() {
                let mapped: Vec<u64> = seqs
                    .iter()
                    .filter_map(|s| s.as_u64())
                    .filter_map(|s| map.get(&s).copied())
                    .collect();
                row["data"]["messageSeqs"] = serde_json::json!(mapped);
            }
        }
        _ => {}
    }
    if let Some(seqs) = row.get("sourceEventSeqs").and_then(|v| v.as_array()).cloned() {
        let mapped: Vec<u64> = seqs
            .iter()
            .filter_map(|s| s.as_u64())
            .filter_map(|s| map.get(&s).copied())
            .collect();
        row["sourceEventSeqs"] = serde_json::json!(mapped);
    }
    if let Some(op) = row.get_mut("surfaceOp") {
        if op.get("op").and_then(|o| o.as_str()) == Some("replace") {
            let start = op
                .get("startSeq")
                .or_else(|| op.get("start"))
                .and_then(|v| v.as_u64());
            let end = op
                .get("endSeq")
                .or_else(|| op.get("end"))
                .and_then(|v| v.as_u64());
            if let (Some(s), Some(e)) = (one(start), one(end)) {
                *op = serde_json::json!({"op": "replace", "start": s, "end": e});
            }
        }
    }
}

#[cfg(test)]
mod v3_tests {
    use super::*;

    /// 写一份最小 v2 日志（header + rows），返回路径。
    fn write_v2(dir: &Path, rows: &[serde_json::Value]) -> PathBuf {
        let session_dir = dir.join("s1");
        std::fs::create_dir_all(&session_dir).unwrap();
        let header = serde_json::json!({
            "type": "session", "version": 2, "id": "s1",
            "createdAt": 1u64, "isSeeded": false,
            "cwd": "/tmp/ws", "delegationDepth": 0,
        });
        let mut payload = serde_json::to_string(&header).unwrap() + "\n";
        for row in rows {
            payload += &(serde_json::to_string(row).unwrap() + "\n");
        }
        let file = session_dir.join("session.v2.jsonl.zstd");
        std::fs::write(&file, zstd::stream::encode_all(payload.as_bytes(), 0).unwrap()).unwrap();
        file
    }

    fn read_rows(file: &Path) -> Vec<serde_json::Value> {
        let bytes = crate::read_decompressed(file).unwrap();
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// v2→v3：system 晋升（空 head + replace 链）、PTC 改名、seq 重映射、
    /// request/header 去 system、源文件原样。
    #[test]
    fn v2_to_v3_promotes_system_renames_ptc_and_densifies() {
        let dir = std::env::temp_dir().join(format!("dsh-v3-mig-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_v2(
            &dir,
            &[
                serde_json::json!({"type":"step/start","seq":0,"time":1,"data":{"turn":1,"step":1}}),
                serde_json::json!({"type":"request/header","seq":1,"time":2,"data":{"header":{"config":{"provider":"p","model":"m"},"system":"Prompt v1","tools":[]},"reason":"initial"}}),
                serde_json::json!({"type":"tool/code-dispatch","seq":2,"time":3,"data":{"turn":1,"step":1}}),
                serde_json::json!({"type":"session/title","seq":3,"time":4,"data":{"title":"t","messageSeqs":[2]}}),
            ],
        );
        let before = std::fs::read(&source).unwrap();
        let id = SessionId::new("s1");

        let target = migrate_v2_to_v3(&source, &id).unwrap();

        assert_eq!(target.file_name().unwrap(), "session.v3.jsonl.zstd");
        assert_eq!(std::fs::read(&source).unwrap(), before, "source bytes must not change");
        let rows = read_rows(&target);
        let header = &rows[0];
        assert_eq!(header["version"], 3);

        // rows[0] 是 header；事件行序：step/start(0) → 空 head(1, append)
        // → system v1(2, replace head) → request/header(3, 无 system、无空
        // tools) → tool/ptc-dispatch(4) → session/title(5, messageSeqs 重映射 2→4)
        assert_eq!(rows[1]["type"], "step/start");
        assert_eq!(rows[2]["type"], "system/message");
        assert_eq!(rows[2]["surfaceOp"], "append");
        assert_eq!(rows[2]["data"]["message"]["content"], serde_json::json!([]));
        assert_eq!(rows[3]["type"], "system/message");
        assert_eq!(rows[3]["data"]["message"]["content"][0]["text"], "Prompt v1");
        assert_eq!(
            rows[3]["surfaceOp"],
            serde_json::json!({"op": "replace", "startSeq": 1, "endSeq": 1})
        );
        assert_eq!(rows[3]["sourceEventSeqs"], serde_json::json!([1]));
        assert!(rows[3]["data"]["message"]["id"]
            .as_str()
            .unwrap()
            .starts_with("v2-to-v3-system-"));
        assert_eq!(rows[4]["type"], "request/header");
        assert!(rows[4]["data"]["header"].get("system").is_none());
        assert!(rows[4]["data"]["header"].get("tools").is_none());
        assert_eq!(rows[5]["type"], "tool/ptc-dispatch");
        assert_eq!(rows[6]["type"], "session/title");
        assert_eq!(rows[6]["data"]["messageSeqs"], serde_json::json!([4]));
        for (index, row) in rows.iter().enumerate().skip(1) {
            assert_eq!(row["seq"], index - 1, "dense seqs from 0");
        }

        // 二次迁移拒绝（已是 v3）
        assert!(migrate_v2_to_v3(&target, &id).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 未知必读类型拒绝迁移（fail-closed）。
    #[test]
    fn v2_to_v3_refuses_unknown_required_events() {
        let dir = std::env::temp_dir().join(format!("dsh-v3-ref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let source = write_v2(
            &dir,
            &[serde_json::json!({
                "type": "brand-new/thing", "seq": 0, "time": 1, "data": {}
            })],
        );
        let id = SessionId::new("s1");
        assert!(migrate_v2_to_v3(&source, &id).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
