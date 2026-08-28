//! dsh-compaction — 会话上下文压缩（对齐 `packages/compaction/compaction-basic`）。
//!
//! 机制：估算会话 token 超过阈值时，选定一个「影子区」（尽量多的历史前缀，
//! 边界回退保证工具调用/结果对不拆散），用当前路由原样重放该前缀 + 追加
//! 压缩指令作为最后一条用户消息发起辅助 LLM 调用（复用 provider 前缀缓存，
//! `AuxiliaryPurpose::Compaction` 标记），把产出摘要落成检查点消息替换影子区。
//! 会话侧由 [`dsh_session::SessionEvent::Compaction`] 事件承载，回放时
//! `derive_messages` 据此丢弃影子区并注入检查点。
//!
//! token 估算用 chars/4 启发式（参考实现用按路由计价的 TokenMeter，本实现
//! 无 tokenizer 依赖）。

use dsh_llm::message::Message;
use dsh_llm::types::{AuxiliaryPurpose, ContentBlock, GenerateOptions, StreamChunk};
use dsh_llm::LlmRuntime;

/// 包裹结构化摘要的标签（web SUMMARY_OPEN/CLOSE_TAG）。会话日志里已存在
/// 该块时视为先前的检查点：合并更新而非逐字复制。
pub const SUMMARY_OPEN_TAG: &str = "<compacted-summary>";
pub const SUMMARY_CLOSE_TAG: &str = "</compacted-summary>";

/// 压缩指令：作为重放会话后的最后一条用户消息发送（web COMPACTION_INSTRUCTION，
/// 逐字对齐——保持会话自身 system/tools/前缀使辅助调用成为真实前缀）。
pub const COMPACTION_INSTRUCTION: &str = "\
You are now acting as a compaction engine for this AI coding assistant. Condense the conversation ABOVE into a structured checkpoint that lets another model resume the work with no loss of essential context.

Output EXACTLY the Markdown structure below: keep every section, in order. Use terse bullets, not prose paragraphs. Write \"(none)\" for an empty section — never drop a section.

## Primary Request and Intent
- [the user's original and evolving goals; quote verbatim where the exact wording matters]

## Key Technical Concepts
- [technologies, frameworks, patterns, and conventions in play]

## Files and Code
- [exact path: why it matters, key changes or snippets]

## Errors and Fixes
- [error: how it was resolved, plus any related user feedback]

## Pending Jobs
- [explicitly requested work not yet completed]

## Current Work
- [precisely what was in progress at this checkpoint]

## Next Step
- [the single next action, directly in line with the most recent request, or \"(none)\"]

## Critical Context
- [decisions and their rationale, constraints, user preferences, open questions, data needed to continue]

Rules:
- Write concise English engineering prose. Preserve exact file paths, commands, error strings, identifiers, numeric values, function signatures, and syntax fragments.
- Capture user feedback and explicit instructions faithfully, especially corrections.
- Do NOT mention this summarization request or that the context was compacted.
- Output only the checkpoint text: do not call any tool or take any other action.";

/// 检查点消息的定场白（web CHECKPOINT_PREAMBLE）。
pub const CHECKPOINT_PREAMBLE: &str = "\
This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. \
Treat the captured context as established background and build on it without restating it. \
Continue the task directly from the messages that follow, without acknowledging this checkpoint.";

/// 压缩配置。
#[derive(Clone, Copy, Debug)]
pub struct CompactionConfig {
    /// 估算 token 超过该值触发压缩（0 = 禁用）。
    pub threshold_tokens: usize,
    /// 压缩后保留的最近消息条数（影子区 = 其余前缀）。
    pub keep_recent: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { threshold_tokens: 60_000, keep_recent: 6 }
    }
}

/// chars/4 启发式 token 估算（中英混合文本的粗粒度近似）。
pub fn estimate_tokens(messages: &[Message]) -> usize {
    let chars: usize = messages
        .iter()
        .map(|m| {
            m.content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } | ContentBlock::Reasoning { text } => text.chars().count(),
                    ContentBlock::ToolCall { arguments, .. } => arguments.chars().count(),
                    _ => 0,
                })
                .sum::<usize>()
        })
        .sum();
    chars / 4
}

fn has_tool_call(m: &Message) -> bool {
    m.content.iter().any(|b| matches!(b, ContentBlock::ToolCall { .. }))
}

fn has_tool_result(m: &Message) -> bool {
    m.content.iter().any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

/// 影子区边界选择：候选边界 = len - keep_recent（不早于 0）；随后向前回退，
/// 直到边界不落在工具调用/结果对中间（结果消息的调用在影子区、或调用消息
/// 紧邻边界会造成悬空引用）。web toolPairingBalanced 的同一约束。
pub fn select_boundary(messages: &[Message], keep_recent: usize) -> usize {
    let mut boundary = messages.len().saturating_sub(keep_recent);
    while boundary > 0 {
        let next_splits_pair = messages.get(boundary).is_some_and(|m| has_tool_result(m));
        let prev_leaves_call = messages.get(boundary - 1).is_some_and(|m| has_tool_call(m));
        if next_splits_pair || prev_leaves_call {
            boundary -= 1;
        } else {
            break;
        }
    }
    boundary
}

/// 检查点消息正文：定场白 + 标签包裹的摘要。
pub fn frame_checkpoint(summary: &str) -> String {
    format!("{CHECKPOINT_PREAMBLE}\n\n{SUMMARY_OPEN_TAG}\n{summary}\n{SUMMARY_CLOSE_TAG}")
}

/// 由会话事件摘要构造检查点消息（dsh-session 回放同用此构形）。
pub fn checkpoint_message(summary: &str) -> Message {
    Message::user_text(frame_checkpoint(summary))
}

/// 辅助摘要调用：重放影子区（保持会话自身的 system/tools 以复用 provider
/// 前缀缓存）+ 压缩指令作为最后一条用户消息；收集纯文本输出。
/// 参考实现在此之上还有 provider/model 路由覆盖与 usage 记账，这里取当前
/// 路由（`AuxiliaryPurpose::Compaction` 标记辅助用途）。
pub async fn summarize_with_llm(
    llm: &LlmRuntime,
    provider: &str,
    model: &str,
    system: Option<String>,
    tools: Option<Vec<dsh_llm::types::ToolSchema>>,
    shadowed: &[Message],
    signal: dsh_llm::types::AbortSignal,
) -> Result<String, String> {
    let mut messages: Vec<Message> = shadowed.to_vec();
    messages.push(Message::user_text(COMPACTION_INSTRUCTION));
    let mut options = GenerateOptions::new(provider, model, messages);
    options.system = system;
    options.tools = tools;
    options.signal = signal;
    options.purpose = Some(AuxiliaryPurpose::Compaction);
    let mut stream = llm.stream(options).await.map_err(|e| e.to_string())?;
    let mut out = String::new();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        match chunk {
            StreamChunk::TextDelta { text, .. } => out.push_str(&text),
            StreamChunk::Finish { reason, .. } => {
                if matches!(reason, dsh_llm::types::FinishReason::Aborted { .. }) {
                    return Err("compaction aborted".into());
                }
            }
            _ => {}
        }
    }
    let text = out.trim().to_string();
    if text.is_empty() {
        return Err("compaction produced no text".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::message::Message;
    use dsh_llm::types::{CallId, ContentBlock};

    #[test]
    fn estimates_tokens_by_quarter_chars() {
        let msgs = vec![Message::user_text("abcdefgh")]; // 8 chars → 2 tokens
        assert_eq!(estimate_tokens(&msgs), 2);
    }

    fn user_result(call: &str) -> Message {
        Message::user(vec![ContentBlock::ToolResult {
            tool_call_id: CallId(call.into()),
            content: vec![ContentBlock::text("ok")],
            is_error: None,
        }])
    }

    fn assistant_call(call: &str) -> Message {
        Message::assistant(
            vec![ContentBlock::ToolCall {
                id: CallId(call.into()),
                name: "shell".into(),
                arguments: "{}".into(),
            }],
            "mock",
            "mock",
        )
    }

    #[test]
    fn boundary_backs_off_tool_pairs() {
        // [user, call, result, user, call, result, ...keep 3]
        let msgs = vec![
            Message::user_text("hi"),
            assistant_call("1"),
            user_result("1"),
            Message::user_text("more"),
            assistant_call("2"),
            user_result("2"),
            Message::user_text("recent1"),
            Message::user_text("recent2"),
            Message::user_text("recent3"),
        ];
        // 候选边界 6：recent1 非结果消息、result2 的调用对完整落在影子区
        // → 无需回退
        assert_eq!(select_boundary(&msgs, 3), 6);
    }

    #[test]
    fn boundary_never_splits_call_from_result() {
        // 候选边界 2 落在 result 前 → 回退到 1：影子区 [user]，call+result
        // 对完整保留（再往前才需要拆对，故停在 1）
        let msgs = vec![Message::user_text("hi"), assistant_call("1"), user_result("1")];
        assert_eq!(select_boundary(&msgs, 1), 1);
    }

    #[test]
    fn boundary_never_negative() {
        let msgs = vec![assistant_call("1"), user_result("1")];
        assert_eq!(select_boundary(&msgs, 4), 0);
    }

    #[test]
    fn checkpoint_frames_summary() {
        let m = checkpoint_message("## Current Work\n- x");
        let ContentBlock::Text { text } = &m.content[0] else { panic!() };
        assert!(text.starts_with(CHECKPOINT_PREAMBLE));
        assert!(text.contains(SUMMARY_OPEN_TAG));
        assert!(text.ends_with(SUMMARY_CLOSE_TAG));
    }
}
