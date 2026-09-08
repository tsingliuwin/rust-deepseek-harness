//! 会话日志格式 v2（0.1.3-alpha.1 同步）的持久层验收：
//!
//! - 写形：header v2 + isSeeded、settlement 内嵌 stream、assistant/attempt、
//!   tool/result message 形、compaction 富形、seq 0 基致密；
//! - 读形：多代文件取最高代（session.lock/临时文件忽略）、未来版本拒绝；
//! - 迁移：rustdsh legacy v0 日志（无 id 消息/平铺 tool-result/裸 chunk）
//!   与 web v0 语料形（packed 行 + 信封 sourceEventSeqs 认领）→ v2；
//!   源文件永不重写；未知必读类型 fail-closed；
//! - projcache v7：stamp 7 + identity.formatVersion。

use dsh_llm::{CallId, ContentBlock, Message, MessageSource, SessionId, StreamChunk};
use dsh_session::SessionEvent;
use dsh_persist::{SessionRecorder, PROJCACHE_DOMAIN_VERSION};
use serde_json::{json, Value};
use std::path::Path;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-v2-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn write_log(path: &Path, rows: &[Value]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut payload = String::new();
    for r in rows {
        payload.push_str(&r.to_string());
        payload.push('\n');
    }
    let compressed = zstd::stream::encode_all(payload.as_bytes(), 0).unwrap();
    std::fs::write(path, compressed).unwrap();
}

fn read_rows(path: &Path) -> Vec<Value> {
    let bytes = zstd::stream::decode_all(std::fs::File::open(path).unwrap()).unwrap();
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage(Message::user_text(text))
}

#[test]
fn v2_new_session_header_and_filename() {
    let dir = temp_dir("header");
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-v2-shape");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    rec.append(&id, "/tmp/ws", &user_msg("你好")).unwrap();

    let file = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id.as_str())
        .join("session.v3.jsonl.zstd");
    assert!(file.exists(), "v3 代文件名必须落盘");

    let rows = read_rows(&file);
    assert_eq!(rows[0]["type"], "session");
    assert_eq!(rows[0]["version"], 3, "header version = 3");
    assert_eq!(rows[0]["isSeeded"], false, "isSeeded 必填（rustdsh 恒 false）");
    assert!(rows[0].get("seedLength").is_none(), "seedLength 已退役");

    // user/message：v2 data 必含 role/id/content/source；surfaceOp 是信封层
    // 成员（append 缺省不写），data 内不得出现
    let um = &rows[1];
    assert_eq!(um["type"], "user/message");
    assert_eq!(um["seq"], 0, "v2 seq 0 基");
    let keys: Vec<&str> = um["data"].as_object().unwrap().keys().map(String::as_str).collect();
    for k in ["content", "source", "role", "id"] {
        assert!(keys.contains(&k), "user/message data 缺 {k}: {keys:?}");
    }
    assert!(!keys.contains(&"surfaceOp"), "surfaceOp 不在 data 内");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn v2_chunks_embed_into_settlement_stream() {
    let dir = temp_dir("stream");
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-v2-stream");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    // 流式 chunk（不再落行）→ 结算行内嵌压缩流
    for text in ["你", "好", "！"] {
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::AssistantChunk {
                turn: 1,
                step: 1,
                chunk: StreamChunk::TextDelta { index: 0, text: text.into() },
            },
        )
        .unwrap();
    }
    rec.append(
        &id,
        "/tmp/ws",
        &SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            message: Message::assistant(vec![ContentBlock::text("你好！")], "mock", "mock"),
            interrupted: false,
            usage: None,
        },
    )
    .unwrap();

    let file = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id.as_str())
        .join("session.v3.jsonl.zstd");
    let rows = read_rows(&file);
    // 日志里没有 chunk 行
    assert!(
        !rows.iter().any(|r| r["type"] == "assistant/chunk"),
        "v2 不再落 assistant/chunk 行"
    );
    let settlement = rows.iter().find(|r| r["type"] == "assistant/message").unwrap();
    let stream = &settlement["data"]["stream"];
    assert!(stream.is_array());
    assert_eq!(stream.as_array().unwrap().len(), 1, "相邻同 index delta 合并");
    assert_eq!(stream[0]["type"], "text-chunks");
    assert_eq!(stream[0]["texts"], json!(["你", "好", "！"]));
    assert_eq!(stream[0]["index"], 0);
    // settlement message 全形（id/role/source）
    let message = &settlement["data"]["message"];
    assert!(!message["id"].as_str().unwrap_or_default().is_empty(), "message.id 必填");
    assert_eq!(message["role"], "assistant");
    assert_eq!(message["source"]["kind"], "model");
    assert_eq!(message["source"]["provider"], "mock");
    // seq 致密：header 后 0..N-1 连续
    let seqs: Vec<u64> = rows[1..].iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    let expect: Vec<u64> = (0..seqs.len() as u64).collect();
    assert_eq!(seqs, expect, "seq 必须 0 基致密");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn v2_failed_attempt_flushes_as_assistant_attempt() {
    let dir = temp_dir("attempt");
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-v2-attempt");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    // 第一次尝试流出两个 delta 后失败（重试排定）
    for text in ["a", "b"] {
        rec.append(
            &id,
            "/tmp/ws",
            &SessionEvent::AssistantChunk {
                turn: 1,
                step: 1,
                chunk: StreamChunk::TextDelta { index: 0, text: text.into() },
            },
        )
        .unwrap();
    }
    rec.append(
        &id,
        "/tmp/ws",
        &SessionEvent::LlmRetry {
            retry_id: "r1".into(),
            turn: 1,
            step: 1,
            provider: "mock".into(),
            mode: "normal".into(),
            policy_key: "k".into(),
            retry: 1,
            max_retries: Some(3),
            delay_ms: 100,
            failure: dsh_llm::LlmFailure {
                message: "boom".into(),
                code: "E".into(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
        },
    )
    .unwrap();
    // 第二次尝试成功结算
    rec.append(
        &id,
        "/tmp/ws",
        &SessionEvent::AssistantChunk {
            turn: 1,
            step: 1,
            chunk: StreamChunk::TextDelta { index: 0, text: "ok".into() },
        },
    )
    .unwrap();
    rec.append(
        &id,
        "/tmp/ws",
        &SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            message: Message::assistant(vec![ContentBlock::text("ok")], "mock", "mock"),
            interrupted: false,
            usage: None,
        },
    )
    .unwrap();

    let file = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id.as_str())
        .join("session.v3.jsonl.zstd");
    let rows = read_rows(&file);
    let attempt = rows.iter().find(|r| r["type"] == "assistant/attempt").expect("失败尝试必须落 assistant/attempt");
    assert_eq!(attempt["data"]["turn"], 1);
    assert_eq!(attempt["data"]["step"], 1);
    assert_eq!(attempt["data"]["stream"][0]["texts"], json!(["a", "b"]));
    // attempt 在 llm/retry 之前（上游 settle 先于重试排定）
    let attempt_seq = attempt["seq"].as_u64().unwrap();
    let retry = rows.iter().find(|r| r["type"] == "llm/retry").unwrap();
    assert!(attempt_seq < retry["seq"].as_u64().unwrap());
    // 结算行只带第二次尝试的流
    let settlement = rows.iter().find(|r| r["type"] == "assistant/message").unwrap();
    assert_eq!(settlement["data"]["stream"][0]["texts"], json!(["ok"]));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn v2_tool_result_and_compaction_shapes() {
    let dir = temp_dir("tool");
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-v2-tool");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    let msg = Message::tool_result(
        CallId("c1".into()),
        vec![ContentBlock::text("r")],
        false,
    );
    rec.append(&id, "/tmp/ws", &SessionEvent::ToolResult { turn: 1, step: 1, message: msg })
        .unwrap();
    rec.append(
        &id,
        "/tmp/ws",
        &SessionEvent::compaction(
            2,
            "摘要".into(),
            "cp-1".into(),
            0,
            vec![0, 1, 2],
            321,
            "mock".into(),
            "mock-model".into(),
        ),
    )
    .unwrap();

    let file = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id.as_str())
        .join("session.v3.jsonl.zstd");
    let rows = read_rows(&file);
    let tr = rows.iter().find(|r| r["type"] == "tool/result").unwrap();
    // v2 形：{turn, step, message:{id, role:'user', content, source}}
    assert_eq!(tr["data"]["turn"], 1);
    assert_eq!(tr["data"]["step"], 1);
    assert_eq!(tr["data"]["message"]["role"], "user");
    assert_eq!(tr["data"]["message"]["source"]["kind"], "tool");
    assert_eq!(tr["data"]["message"]["source"]["callId"], "c1");
    assert_eq!(tr["data"]["message"]["content"][0]["type"], "tool-result");
    assert!(!tr["data"]["message"]["id"].as_str().unwrap_or_default().is_empty());

    let cs = rows.iter().find(|r| r["type"] == "compaction/summary").unwrap();
    // RELEASED_V2 必填七件套
    for k in ["compactionId", "summary", "shadowedRange", "shadowedSeqs", "shadowedTokenCount", "provider", "model"] {
        assert!(cs["data"].get(k).is_some(), "compaction/summary 缺 {k}");
    }
    assert_eq!(cs["data"]["shadowedRange"]["end"], 2);
    assert_eq!(cs["data"]["shadowedSeqs"], json!([0, 1, 2]));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migration_upgrades_legacy_rustdsh_log() {
    // rustdsh 旧版写形：header v0、消息无 id/source、tool/result 平铺、
    // 裸 assistant/chunk、seq 1 基
    let dir = temp_dir("migrate-legacy");
    let id = "session-mig-legacy";
    let log = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id)
        .join("session.jsonl.zstd");
    write_log(
        &log,
        &[
            json!({"type": "session", "version": 0, "id": id, "createdAt": 1, "cwd": "/tmp/ws", "delegationDepth": 0}),
            json!({"type": "user/message", "seq": 1, "time": 10, "data": {"content": [{"type": "text", "text": "q"}], "source": {"kind": "user"}, "role": "user", "id": "m1", "surfaceOp": "append"}}),
            json!({"type": "assistant/message", "seq": 2, "time": 20, "data": {"turn": 1, "step": 1, "message": {"role": "assistant", "content": [{"type": "text", "text": "hi"}]}}}),
            json!({"type": "assistant/chunk", "seq": 3, "time": 21, "data": {"turn": 1, "step": 2, "chunk": {"type": "text-delta", "index": 0, "text": "st"}}}),
            json!({"type": "assistant/chunk", "seq": 4, "time": 23, "data": {"turn": 1, "step": 2, "chunk": {"type": "text-delta", "index": 0, "text": "re"}}}),
            json!({"type": "tool/result", "seq": 5, "time": 30, "data": {"toolCallId": "c1", "content": [{"type": "text", "text": "r"}], "isError": false}}),
            json!({"type": "turn/end", "seq": 6, "time": 40, "data": {"turn": 1, "reason": {"kind": "completed"}}}),
        ],
    );
    let source_bytes = std::fs::read(&log).unwrap();

    // 写打开触发迁移；追加落 v2
    let rec = SessionRecorder::new(dir.join("sessions"));
    let sid = SessionId::new(id);
    rec.append(&sid, "/tmp/ws", &SessionEvent::SessionTitle { title: "t".into() })
        .unwrap();

    let v2_file = log.parent().unwrap().join("session.v2.jsonl.zstd");
    assert!(v2_file.exists(), "级联中间产物必须发布到 session.v2.jsonl.zstd");
    assert!(log.parent().unwrap().join("session.v3.jsonl.zstd").exists(), "级联终产物必须发布到 session.v3.jsonl.zstd");
    // 源文件字节原封不动（上游：source vN artifact remains unchanged）
    assert_eq!(
        std::fs::read(&log).unwrap(),
        source_bytes,
        "迁移绝不重写源文件"
    );
    let rows = read_rows(&v2_file);
    assert_eq!(rows[0]["version"], 2);
    assert_eq!(rows[0]["isSeeded"], false);
    // 裸 chunk 无人认领 → assistant/attempt（携带压缩流）
    let attempt = rows.iter().find(|r| r["type"] == "assistant/attempt").unwrap();
    assert_eq!(attempt["data"]["stream"][0]["texts"], json!(["st", "re"]));
    // 消息补 id/source
    let am = rows.iter().find(|r| r["type"] == "assistant/message").unwrap();
    assert_eq!(am["data"]["message"]["id"], "legacy-message:session-mig-legacy:2");
    assert_eq!(am["data"]["message"]["source"]["kind"], "model");
    assert_eq!(am["data"]["stream"], json!([]), "无人认领 → 空流（校验器对空流跳过内容一致性）");
    // 平铺 tool/result → message 形
    let tr = rows.iter().find(|r| r["type"] == "tool/result").unwrap();
    assert_eq!(tr["data"]["message"]["source"]["kind"], "tool");
    assert_eq!(tr["data"]["message"]["content"][0]["toolCallId"], "c1");
    // data 内误写的 surfaceOp 摘除
    let um = rows.iter().find(|r| r["type"] == "user/message").unwrap();
    assert!(um["data"].get("surfaceOp").is_none());
    // seq 致密（0 基连续）
    let seqs: Vec<u64> = rows[1..].iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    let expect: Vec<u64> = (0..seqs.len() as u64).collect();
    assert_eq!(seqs, expect);
    // 迁移后可读：tool-result 与标题事件都在
    let (session, _) = rec.load(&sid, Some("/tmp/ws")).unwrap();
    assert!(session.entries().iter().any(|e| matches!(
        e.event,
        SessionEvent::ToolResult { .. }
    )));
    assert!(session
        .entries()
        .iter()
        .any(|e| matches!(e.event, SessionEvent::SessionTitle { .. })));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migration_claims_packed_rows_via_source_event_seqs() {
    // web alpha.5 语料主形：顶层 packed 行 + 信封 sourceEventSeqs 区间认领
    let dir = temp_dir("migrate-claim");
    let id = "session-mig-claim";
    let log = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id)
        .join("session.jsonl.zstd");
    write_log(
        &log,
        &[
            json!({"type": "session", "version": 0, "id": id, "createdAt": 1, "cwd": "/tmp/ws", "delegationDepth": 0}),
            json!({"type": "turn/start", "seq": 1, "time": 5, "data": {"turn": 1}}),
            json!({"type": "text-chunks", "seq0": 2, "time0": 100, "data": {"turn": 1, "step": 1, "index": 0, "dt": [5], "texts": ["a", "b"]}}),
            json!({"type": "assistant/message", "seq": 4, "time": 120, "sourceEventSeqs": [[2, 3]], "surfaceOp": "append",
                   "data": {"turn": 1, "step": 1,
                            "message": {"id": "m1", "role": "assistant", "content": [{"type": "text", "text": "ab"}],
                                        "source": {"kind": "model", "provider": "p", "model": "m"}}}}),
        ],
    );
    let rec = SessionRecorder::new(dir.join("sessions"));
    let sid = SessionId::new(id);
    rec.append(&sid, "/tmp/ws", &SessionEvent::SessionTitle { title: "t".into() })
        .unwrap();

    let v2_file = log.parent().unwrap().join("session.v2.jsonl.zstd");
    let rows = read_rows(&v2_file);
    let am = rows.iter().find(|r| r["type"] == "assistant/message").unwrap();
    // 认领成员重放为内嵌流
    assert_eq!(am["data"]["stream"][0]["type"], "text-chunks");
    assert_eq!(am["data"]["stream"][0]["texts"], json!(["a", "b"]));
    assert_eq!(am["data"]["stream"][0]["dt"], json!([5]));
    // 信封 sourceEventSeqs 已摘除
    assert!(am.get("sourceEventSeqs").is_none());
    // 被认领的 chunk 不产生 attempt 行
    assert!(!rows.iter().any(|r| r["type"] == "assistant/attempt"));
    // 致密化：turn/start=0、assistant/message=1（chunk 消费进 message）
    let start = rows.iter().find(|r| r["type"] == "turn/start").unwrap();
    assert_eq!(start["seq"], 0);
    assert_eq!(am["seq"], 1);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn migration_refuses_unknown_required_type() {
    let dir = temp_dir("migrate-refuse");
    let id = "session-mig-refuse";
    let log = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id)
        .join("session.jsonl.zstd");
    write_log(
        &log,
        &[
            json!({"type": "session", "version": 0, "id": id, "createdAt": 1, "cwd": "/tmp/ws", "delegationDepth": 0}),
            json!({"type": "plugin/required-thing", "seq": 1, "time": 5, "data": {}}),
        ],
    );
    let source_bytes = std::fs::read(&log).unwrap();
    let rec = SessionRecorder::new(dir.join("sessions"));
    let sid = SessionId::new(id);
    let err = rec
        .append(&sid, "/tmp/ws", &SessionEvent::SessionTitle { title: "t".into() })
        .unwrap_err();
    assert!(err.to_string().contains("plugin/required-thing"), "{err}");
    // fail-closed：无 v2 产物、源文件原样
    assert!(!log.parent().unwrap().join("session.v2.jsonl.zstd").exists());
    assert_eq!(std::fs::read(&log).unwrap(), source_bytes);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn generation_scan_prefers_latest_and_ignores_lock() {
    let dir = temp_dir("generations");
    let id_str = "session-gens";
    let bucket = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id_str);
    // v0 旧代 + v2 新代并存（web 迁移后的家现状）+ lease 文件
    write_log(
        &bucket.join("session.jsonl.zstd"),
        &[
            json!({"type": "session", "version": 0, "id": id_str, "createdAt": 1, "cwd": "/tmp/ws", "delegationDepth": 0}),
            json!({"type": "user/message", "seq": 1, "time": 1, "data": {"content": [{"type": "text", "text": "旧"}], "source": {"kind": "user"}, "role": "user", "id": "m0"}}),
        ],
    );
    write_log(
        &bucket.join("session.v2.jsonl.zstd"),
        &[
            json!({"type": "session", "version": 2, "id": id_str, "createdAt": 1, "isSeeded": false, "cwd": "/tmp/ws", "delegationDepth": 0}),
            json!({"type": "user/message", "seq": 0, "time": 2, "data": {"content": [{"type": "text", "text": "新"}], "source": {"kind": "user"}, "role": "user", "id": "m1"}}),
        ],
    );
    std::fs::write(bucket.join("session.lock"), b"lease").unwrap();

    let rec = SessionRecorder::new(dir.join("sessions"));
    let (session, _) = rec.load(&SessionId::new(id_str), None).unwrap();
    let texts: Vec<String> = session
        .entries()
        .iter()
        .filter_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => m.content.iter().find_map(|b| match b {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["新".to_string()], "必须读最高代");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_refuses_future_format_version() {
    let dir = temp_dir("future");
    let id_str = "session-future";
    let bucket = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id_str);
    write_log(
        &bucket.join("session.v4.jsonl.zstd"),
        &[
            json!({"type": "session", "version": 4, "id": id_str, "createdAt": 1, "isSeeded": false}),
            json!({"type": "user/message", "seq": 0, "time": 1, "data": {"content": [], "source": {"kind": "user"}, "role": "user", "id": "m"}}),
        ],
    );
    let rec = SessionRecorder::new(dir.join("sessions"));
    let err = rec
        .load(&SessionId::new(id_str), None)
        .expect_err("未来版本必须拒绝");
    assert!(err.to_string().contains("newer"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn projcache_stamps_v7_with_format_version() {
    let dir = temp_dir("projcache-v7");
    // 预置 per-record 树（无树家只写旧整档——bootstrap 延后语义）
    std::fs::create_dir_all(
        dir.join("storages")
            .join("session_projcache")
            .join("sessions"),
    )
    .unwrap();
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-v7");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    rec.append(&id, "/tmp/ws", &SessionEvent::SessionTitle { title: "题".into() })
        .unwrap();
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dir.join("storages")
                .join("session_projcache")
                .join("sessions")
                .join("session-v7.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(doc["version"], PROJCACHE_DOMAIN_VERSION);
    assert_eq!(doc["version"], 7);
    assert_eq!(doc["record"]["identity"]["formatVersion"], 3, "v3 语义：identity 携带会话格式版本");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn user_message_file_block_round_trips() {
    // 附件链路：FileBlock 落盘 → 读回（web 互读互通的字节级前提）
    let dir = temp_dir("file-block");
    let rec = SessionRecorder::new(dir.join("sessions"));
    let id = SessionId::new("session-file");
    rec.create(&id, "/tmp/ws", "standard").unwrap();
    let mut msg = Message::user(vec![
        ContentBlock::File {
            attachment: dsh_llm::FileAttachmentRef {
                attachment_id: "sha256:0011223344556677889900aabbccddeeff00112233445566778899aabbccddee".into(),
                name: "report.pdf".into(),
                bytes: 2048,
            },
        },
        ContentBlock::text("请看附件"),
    ]);
    msg.source = MessageSource::User;
    rec.append(&id, "/tmp/ws", &SessionEvent::UserMessage(msg)).unwrap();

    let file = dir
        .join("sessions")
        .join(dsh_persist::project_key("/tmp/ws"))
        .join(id.as_str())
        .join("session.v3.jsonl.zstd");
    let rows = read_rows(&file);
    let um = rows.iter().find(|r| r["type"] == "user/message").unwrap();
    // 附件块在前、文本在后（上游 sendSession content 顺序）
    assert_eq!(um["data"]["content"][0]["type"], "file");
    assert_eq!(um["data"]["content"][0]["attachment"]["attachmentId"], "sha256:0011223344556677889900aabbccddeeff00112233445566778899aabbccddee");
    assert_eq!(um["data"]["content"][0]["attachment"]["name"], "report.pdf");
    assert_eq!(um["data"]["content"][0]["attachment"]["bytes"], 2048);
    assert_eq!(um["data"]["content"][1]["type"], "text");

    let (session, _) = rec.load(&id, Some("/tmp/ws")).unwrap();
    let SessionEvent::UserMessage(m) = &session.entries()[0].event else {
        panic!("expected user message");
    };
    match &m.content[0] {
        ContentBlock::File { attachment } => {
            assert_eq!(attachment.name, "report.pdf");
            assert_eq!(attachment.bytes, 2048);
        }
        other => panic!("expected file block, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}
