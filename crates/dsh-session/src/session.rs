//! Append-only session event log and its model-history projection.

use serde::{Deserialize, Serialize};

/// Why a turn ended.
///
/// serde 形状对齐上游 v2 词汇（`TurnEndReasonMap`）：`aborted` 携带
/// `reason`（rustdsh 的取消一律落上游迁移自己合成的 `{kind:"legacy"}`
/// 取消原因——保证嵌套变体可被 web 校验接受），`error` 的载荷键是
/// `error`（web 命名；rustdsh 内部字段名保持 failure）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndReason {
    Completed,
    MaxTokens,
    Blocked,
    Aborted {
        #[serde(rename = "reason")]
        cancel_cause: CancelCause,
    },
    Error {
        #[serde(rename = "error")]
        failure: dsh_llm::LlmFailure,
    },
}

/// 取消原因（web `TurnEndCancelCause` 的已知保底形；web 迁移对无因取消
/// 合成 `{kind:"legacy"}`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CancelCause {
    Legacy,
}

/// Whether a request header was appended initially, on resume, or on change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HeaderReason {
    Initial,
    Change,
    Resume,
}

/// The logged request epoch: config plus the rendered prompt and tool order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpochHeader {
    pub config: dsh_llm::LlmCallConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<dsh_llm::ToolSchema>>,
}

/// The logged request context (provider/model/context window) epoch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestContext {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// One durable session event.
///
/// Serialized as tagged JSON (one line per event) so a session can be
/// persisted to JSONL and rebuilt verbatim from the log; the model-visible
/// history is always re-derived from these events (`derive_messages`), never
/// stored separately.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum SessionEvent {
    TurnStart { turn: u64 },
    TurnEnd { turn: u64, reason: TurnEndReason },
    StepStart { turn: u64, step: u64 },
    StepEnd { turn: u64, step: u64 },
    /// `surfaceOp: append` — the message itself is the model-visible node.
    UserMessage(dsh_llm::Message),
    AssistantMessage {
        turn: u64,
        step: u64,
        message: dsh_llm::Message,
        interrupted: bool,
        usage: Option<dsh_llm::TokenUsage>,
    },
    /// Raw chunk, preserved for replay fidelity (log-only, not model-visible).
    AssistantChunk { turn: u64, step: u64, chunk: dsh_llm::StreamChunk },
    ToolCall {
        turn: u64,
        step: u64,
        call_id: dsh_llm::CallId,
        name: String,
        arguments: String,
    },
    /// `surfaceOp: append` — the tool-result message is the model-visible node.
    ToolResult {
        turn: u64,
        step: u64,
        message: dsh_llm::Message,
    },
    RequestHeader {
        header: EpochHeader,
        reason: HeaderReason,
    },
    RequestContext(RequestContext),
    /// 压缩事务：影子区（来源 seq <= before_seq 的消息）由检查点消息替换
    /// （web compaction/start→summary→end 的合并承载）。v2 日志的
    /// `compaction/summary` 富形字段一并携带（web RELEASED_V2 处置表要求
    /// compactionId/shadowedRange/shadowedSeqs/shadowedTokenCount/provider/
    /// model 必填）。
    Compaction {
        before_seq: u64,
        summary: String,
        /// 事务标识（web compactionId；rustdsh 每次压缩生成一个）。
        compaction_id: String,
        /// 影子区起点（首个被替换消息的事件 seq；终点即 `before_seq`）。
        shadowed_start: u64,
        /// 被替换消息的事件 seq 清单。
        shadowed_seqs: Vec<u64>,
        /// 影子区估算 token 数。
        shadowed_token_count: u64,
        /// 发起压缩时的路由（web 必填字段）。
        provider: String,
        model: String,
    },
    /// 会话标题（log-only，latest-wins；不进模型可见面）。
    /// web SessionTitleEventData 的 title 部分（messageSeqs/source 不落盘）。
    SessionTitle { title: String },
    /// 一次 provider 路由重试等待排定前的持久记录（web `llm/retry`，
    /// LlmRetryEventData：normal 模式带 maxRetries，always 模式不带）。
    LlmRetry {
        retry_id: String,
        turn: u64,
        step: u64,
        provider: String,
        mode: String,
        policy_key: String,
        retry: u32,
        max_retries: Option<u32>,
        delay_ms: u64,
        failure: dsh_llm::LlmFailure,
    },
    /// 一次重试等待完成、下一次请求尝试开始前的持久迁移（web
    /// `llm/retry-started`）。
    LlmRetryStarted { retry_id: String, turn: u64, step: u64, retry: u32 },
}

/// A log entry: the event plus its monotonic sequence number.
#[derive(Clone, Debug)]
pub struct SessionEntry {
    pub seq: u64,
    pub event: SessionEvent,
}

impl SessionEvent {
    /// 构造一次压缩事务（v2 富形；`compaction_id` 由调用方生成）。
    pub fn compaction(
        before_seq: u64,
        summary: String,
        compaction_id: String,
        shadowed_start: u64,
        shadowed_seqs: Vec<u64>,
        shadowed_token_count: u64,
        provider: String,
        model: String,
    ) -> Self {
        Self::Compaction {
            before_seq,
            summary,
            compaction_id,
            shadowed_start,
            shadowed_seqs,
            shadowed_token_count,
            provider,
            model,
        }
    }
}

/// 检查点消息帧形（web CHECKPOINT_PREAMBLE + SUMMARY 标签；与
/// dsh-compaction::frame_checkpoint 保持一致——压缩替换消息的权威构形）。
pub fn compaction_checkpoint_message(summary: &str) -> dsh_llm::Message {
    const PREAMBLE: &str = "\
This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. \
Treat the captured context as established background and build on it without restating it. \
Continue the task directly from the messages that follow, without acknowledging this checkpoint.";
    dsh_llm::Message::user_text(format!(
        "{PREAMBLE}\n\n<compacted-summary>\n{summary}\n</compacted-summary>"
    ))
}

/// The append-only session log and in-memory store.
///
/// `derive_messages` is the single projection of model-visible history. The
/// reference enforces "model-visible means logged"; v1 honors this strictly by
/// deriving every request from the log.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: dsh_llm::SessionId,
    entries: Vec<SessionEntry>,
    next_seq: u64,
}

impl Session {
    pub fn new(id: dsh_llm::SessionId) -> Self {
        Self { id, entries: Vec::new(), next_seq: 0 }
    }

    /// Rebuild a session from a persisted event stream.
    pub fn from_events(id: dsh_llm::SessionId, events: Vec<SessionEvent>) -> Self {
        let next_seq = events.len() as u64;
        let entries = events.into_iter().enumerate().map(|(seq, event)| SessionEntry { seq: seq as u64, event }).collect();
        Self { id, entries, next_seq }
    }

    /// Append one event and return its sequence number.
    pub fn append(&mut self, event: SessionEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(SessionEntry { seq, event });
        seq
    }

    pub fn entries(&self) -> &[SessionEntry] {
        &self.entries
    }

    /// The current turn number (0 when no turn has opened yet).
    pub fn last_turn(&self) -> u64 {
        self.entries
            .iter()
            .rev()
            .find_map(|e| match &e.event {
                SessionEvent::TurnStart { turn } => Some(*turn),
                _ => None,
            })
            .unwrap_or(0)
    }

    /// Project the model-visible history from the log.
    ///
    /// Only message-producing surface events (`user/message`,
    /// `assistant/message` — skipping empty content — and `tool/result`)
    /// derive to a message; boundaries, chunks, and headers do not.
    /// 压缩事件（`Compaction`）丢弃影子区（来源 seq <= before_seq）并注入
    /// 检查点消息。
    pub fn derive_messages(&self) -> Vec<dsh_llm::Message> {
        self.derive_messages_with_seqs()
            .into_iter()
            .map(|(_, m)| m)
            .collect()
    }

    /// [`Self::derive_messages`] 的带序版本：每条消息附带其来源事件 seq
    /// （压缩选区需要知道影子区落点）。
    pub fn derive_messages_with_seqs(&self) -> Vec<(u64, dsh_llm::Message)> {
        // is_checkpoint 标记由 Compaction 事件派生的消息：新压缩无条件
        // 替换所有旧检查点（web 同一语义——合并更新而非叠加）。
        let mut out: Vec<(u64, dsh_llm::Message, bool)> = Vec::new();
        for e in &self.entries {
            match &e.event {
                SessionEvent::UserMessage(m) => out.push((e.seq, m.clone(), false)),
                SessionEvent::AssistantMessage { message, .. } => {
                    if !message.content.is_empty() {
                        out.push((e.seq, message.clone(), false));
                    }
                }
                SessionEvent::ToolResult { message, .. } => out.push((e.seq, message.clone(), false)),
                SessionEvent::Compaction { before_seq, summary, .. } => {
                    out.retain(|(seq, _, checkpoint)| !checkpoint && *seq > *before_seq);
                    // 影子区是历史前缀：检查点插在保留消息之前（替换其位置）
                    out.insert(0, (e.seq, compaction_checkpoint_message(summary), true));
                }
                _ => {}
            }
        }
        out.into_iter().map(|(seq, m, _)| (seq, m)).collect()
    }

    /// The most recent request header epoch, if one was logged.
    pub fn request_header(&self) -> Option<&EpochHeader> {
        self.entries.iter().rev().find_map(|e| match &e.event {
            SessionEvent::RequestHeader { header, .. } => Some(header),
            _ => None,
        })
    }

    /// The most recent request context epoch, if one was logged.
    pub fn request_context(&self) -> Option<&RequestContext> {
        self.entries.iter().rev().find_map(|e| match &e.event {
            SessionEvent::RequestContext(c) => Some(c),
            _ => None,
        })
    }

    pub fn tool_calls_for_step(&self, turn: u64, step: u64) -> Vec<dsh_llm::CallId> {
        self.entries
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::ToolCall { turn: t, step: s, call_id, .. } if *t == turn && *s == step => {
                    Some(call_id.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The first user message text, for display titles.
    pub fn first_user_text(&self) -> Option<String> {
        self.entries.iter().find_map(|e| match &e.event {
            SessionEvent::UserMessage(m) => {
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        dsh_llm::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                if text.is_empty() { None } else { Some(text) }
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{ContentBlock, Message, Role, SessionId};

    #[test]
    fn derive_messages_and_from_events_round_trip() {
        let mut s = Session::new(SessionId::new("t"));
        s.append(SessionEvent::TurnStart { turn: 1 });
        s.append(SessionEvent::UserMessage(Message::user_text("hello")));
        let assistant = Message::assistant(vec![ContentBlock::text("hi there")], "mock", "mock");
        s.append(SessionEvent::AssistantMessage {
            turn: 1,
            step: 1,
            message: assistant,
            interrupted: false,
            usage: None,
        });
        s.append(SessionEvent::TurnEnd { turn: 1, reason: TurnEndReason::Completed });

        let msgs = s.derive_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert_eq!(s.first_user_text().as_deref(), Some("hello"));

        // Round-trip through from_events.
        let events: Vec<SessionEvent> = s.entries().iter().map(|e| e.event.clone()).collect();
        let s2 = Session::from_events(SessionId::new("t"), events);
        assert_eq!(s2.derive_messages().len(), 2);
        assert_eq!(s2.first_user_text().as_deref(), Some("hello"));
    }

    #[test]
    fn compaction_shadows_region_and_injects_checkpoint() {
        let mut s = Session::new(SessionId::new("t"));
        s.append(SessionEvent::UserMessage(Message::user_text("old-1"))); // seq 0
        s.append(SessionEvent::UserMessage(Message::user_text("old-2"))); // seq 1
        s.append(SessionEvent::UserMessage(Message::user_text("keep"))); // seq 2
        s.append(SessionEvent::compaction(
            1,
            "## Current Work\n- x".into(),
            "cp-1".into(),
            0,
            vec![0, 1],
            42,
            "deepseek".into(),
            "test-model".into(),
        ));

        let msgs = s.derive_messages();
        assert_eq!(msgs.len(), 2);
        assert!(!msgs.iter().any(|m| m.content.iter().any(|b| matches!(b, ContentBlock::Text { text } if text == "old-1" || text == "old-2"))));
        let ContentBlock::Text { text } = &msgs[0].content[0] else { panic!() };
        assert!(text.contains("<compacted-summary>"));
        assert!(text.contains("## Current Work"));
        assert_eq!(msgs[1].content[0], ContentBlock::Text { text: "keep".into() });

        // 第二次压缩替换到更晚的落点
        s.append(SessionEvent::compaction(
            2,
            "merged".into(),
            "cp-2".into(),
            1,
            vec![1, 2],
            30,
            "deepseek".into(),
            "test-model".into(),
        ));
        let msgs = s.derive_messages();
        assert_eq!(msgs.len(), 1);
        let ContentBlock::Text { text } = &msgs[0].content[0] else { panic!() };
        assert!(text.contains("merged") && !text.contains("keep"));
    }

    #[test]
    fn tool_result_is_model_visible_user_message() {
        let mut s = Session::new(SessionId::new("t"));
        let result_msg = Message::tool_result(
            dsh_llm::CallId::new("c1"),
            vec![ContentBlock::text("42")],
            false,
        );
        s.append(SessionEvent::ToolResult { turn: 1, step: 1, message: result_msg });
        let msgs = s.derive_messages();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
        assert!(matches!(&msgs[0].content[0], ContentBlock::ToolResult { .. }));
    }
}