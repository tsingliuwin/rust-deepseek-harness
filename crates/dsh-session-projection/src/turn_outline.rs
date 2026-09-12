//! `turnOutline` 投影单元 —— 全日志轮次大纲（对齐上游
//! `packages/session/session-turn-outline`，0.1.2-alpha.3 新增）。
//!
//! 把 `turn/start` 边界、首条人类提问与最终助手回复折叠成整条会话的轮次
//! 大纲，供聊天轮次导航栏渲染「分页窗口之外」的轮次。`turn/start`（而非
//! 提问的 `user/message`）锚定每个条目，因为它的 seq 就是跳转加载的落点：
//! 循环先记 `turn/start` 再记本轮 prompt 与各步，窗口回到该 seq 即包含整轮。
//!
//! 预览镜像导航栏已加载轮次的预览规则（空格拼接文本块、折叠空白、截断加
//! 省略号），预算按导航卡钳制定——prompt 一行（50 字符）、response 三行
//! （120 字符）——保证轮次在事件加载前后显示同样的文字。回复以「最新一条
//! 带文本的助手消息」为草稿，`turn/end` 时才提交进 `turns`；draft-only 的
//! apply 保持 `turns` 数组不变，身份门控变更流因此在轮边界之间保持安静
//! （每轮至多推送三次：边界、prompt、回复）。

use dsh_llm::{ContentBlock, MessageSource};
use dsh_session::{SessionEntry, SessionEvent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ProjectionDefinition;

/// prompt 预算：导航卡一行（13px 于 ~276px 宽）。
pub const PROMPT_PREVIEW_LIMIT: usize = 50;
/// response 预算：导航卡三行（12px 于 ~276px 宽）。
pub const RESPONSE_PREVIEW_LIMIT: usize = 120;

/// 一条已开始轮次的大纲事实（与客户端已分页到多少无关）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnOutlineEntry {
    /// 宿主派发的轮次号（`turn/start` 载荷）。
    pub turn: u64,
    /// 该轮 `turn/start` 事件 seq——窗口回到此 seq 即加载整轮。
    pub seq: u64,
    /// 有界首条人类提问预览（导航卡一行）；合格 prompt 落地前为 `''`。
    pub prompt: String,
    /// 有界最终回复预览（至多导航卡三行）；轮次结束且无助手文本时为 `''`。
    pub response: String,
}

/// 折叠状态：已提交条目 + 打开轮次的回复草稿。草稿缓冲最新带文本的助手
/// 消息，`turn/end` 提交；wire 视图只投影 `turns`——draft-only apply 保持
/// 该数组的值不变，变更流在轮边界之间安静。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct TurnOutlineState {
    /// 已开始轮次，按轮次号升序。
    pub turns: Vec<TurnOutlineEntry>,
    /// 打开轮次的最新带文本助手预览；轮外为 `''`。
    pub draft: String,
}

/// 空格拼接文本块、折叠空白、超出 `limit` 截断并加省略号（上游
/// `preview()` 的逐字移植：逐块 2×limit 上限防止单个大块整段拼接）。
pub fn preview_text(blocks: &[ContentBlock], limit: usize) -> String {
    let mut text = String::new();
    let mut unread = false;
    for block in blocks {
        let ContentBlock::Text { text: block_text } = block else {
            continue;
        };
        if text.chars().count() >= limit * 2 {
            unread = true;
            break;
        }
        // 逐块上限：折叠在每条消息事件上运行，单个多兆字节块不能为了一
        // 条这么短的预览整段拼接（并做正则归一化）。
        let clipped = block_text.chars().count() > limit * 2;
        let chunk: String = if clipped {
            block_text.chars().take(limit * 2).collect()
        } else {
            block_text.clone()
        };
        if text.is_empty() {
            text = chunk;
        } else {
            text.push(' ');
            text.push_str(&chunk);
        }
        if clipped {
            unread = true;
            break;
        }
    }
    normalize_preview(&text, limit, unread)
}

/// 已切分好文本片段的预览（上游 ui-chat turn-navigation.ts `preview()` 的
/// 镜像：上游因 wire 边界禁止与宿主包共享而刻意复制一份；rustdsh 同工作
/// 区，直接共用）。
pub fn preview_parts<'a>(parts: impl IntoIterator<Item = &'a str>, limit: usize) -> String {
    let mut text = String::new();
    let mut unread = false;
    for part in parts {
        if text.chars().count() >= limit * 2 {
            unread = true;
            break;
        }
        // 逐段上限：同 preview_text，超长片段只取前 2×limit 个字符。
        let clipped = part.chars().count() > limit * 2;
        let end = part
            .char_indices()
            .nth(limit * 2)
            .map(|(i, _)| i)
            .unwrap_or(part.len());
        let chunk = &part[..end];
        if text.is_empty() {
            text.push_str(chunk);
        } else {
            text.push(' ');
            text.push_str(chunk);
        }
        if clipped {
            unread = true;
            break;
        }
    }
    normalize_preview(&text, limit, unread)
}

/// 折叠空白（`/\s+/g → ' '` + trim），超预算截断加省略号。
fn normalize_preview(text: &str, limit: usize, unread: bool) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = normalized.chars().count();
    if count > limit - 1 {
        let mut clipped: String = normalized.chars().take(limit - 1).collect();
        // 截断后去掉尾部空白（上游 slice 后 trimEnd）。
        while clipped.ends_with(char::is_whitespace) {
            clipped.pop();
        }
        clipped.push('…');
        return clipped;
    }
    if unread {
        return format!("{normalized}…");
    }
    normalized
}

/// `turnOutline` 投影定义（上游 stateVersion = 2；wire 视图 = `turns` 数组）。
pub fn turn_outline_projection_definition() -> ProjectionDefinition<TurnOutlineState> {
    ProjectionDefinition {
        key: "turnOutline",
        state_version: 2,
        init: TurnOutlineState::default,
        apply: |state, entry| apply_outline(state, entry),
        // wire：state.turns（上游 wire.view: state => state.turns）。
        view: Some(|state: &TurnOutlineState| {
            serde_json::to_value(&state.turns).unwrap_or(Value::Null)
        }),
    }
}

/// 折叠一个事件。所有「无变化」路径返回原状态的克隆（Rust 无引用同一性，
/// wire 视图的值不变即等价于上游的同引用返回）。
fn apply_outline(state: TurnOutlineState, entry: &SessionEntry) -> TurnOutlineState {
    match &entry.event {
        SessionEvent::TurnStart { turn } => {
            // 顺序守卫：不推进轮次号的边界保持大纲有序；重试轮次的预览
            // 落在既有条目上。
            if state.turns.last().is_some_and(|last| last.turn >= *turn) {
                return state;
            }
            let mut next = state;
            next.turns.push(TurnOutlineEntry {
                turn: *turn,
                seq: entry.seq,
                prompt: String::new(),
                response: String::new(),
            });
            next.draft = String::new();
            next
        }
        SessionEvent::UserMessage(m) => {
            // 只有最新轮次还可能在等它的人类开场提问；同一轮里更晚的人类
            // 消息（steering）不改首条预览。
            if !matches!(m.source, MessageSource::User) {
                return state;
            }
            let Some(last) = state.turns.last() else {
                return state;
            };
            if !last.prompt.is_empty() {
                return state;
            }
            let prompt = preview_text(&m.content, PROMPT_PREVIEW_LIMIT);
            if prompt.is_empty() {
                return state;
            }
            let mut next = state;
            if let Some(last) = next.turns.last_mut() {
                last.prompt = prompt;
            }
            next
        }
        SessionEvent::AssistantMessage { message, .. } => {
            // 最新带文本的消息胜出；缓冲到 turn/end 才提交。
            let draft = preview_text(&message.content, RESPONSE_PREVIEW_LIMIT);
            if draft.is_empty() || draft == state.draft {
                return state;
            }
            let mut next = state;
            next.draft = draft;
            next
        }
        SessionEvent::TurnEnd { .. } => {
            if state.draft.is_empty() {
                return state;
            }
            let mut next = state;
            let draft = std::mem::take(&mut next.draft);
            if let Some(last) = next.turns.last_mut() {
                if last.response != draft {
                    last.response = draft;
                }
            }
            next
        }
        _ => state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{ContentBlock, Message, MessageSource, SessionId};
    use dsh_session::Session;
    use std::sync::Arc;

    fn user_message(text: &str) -> dsh_session::SessionEvent {
        SessionEvent::UserMessage(Message::user(vec![ContentBlock::Text { text: text.into() }]))
    }

    fn assistant_message(turn: u64, text: &str) -> dsh_session::SessionEvent {
        SessionEvent::AssistantMessage {
            time_ms: None,
            turn,
            step: 1,
            message: Message::new(
                dsh_llm::Role::Assistant,
                vec![ContentBlock::Text { text: text.into() }],
                MessageSource::Model {
                    provider: "p".into(),
                    model: "m".into(),
                    replay_state: None,
                },
            ),
            interrupted: false,
            usage: None,
        }
    }

    fn state_of(events: &[dsh_session::SessionEvent]) -> TurnOutlineState {
        let mut session = Session::new(SessionId("t".into()));
        for event in events {
            session.append(event.clone());
        }
        let registry = std::sync::Arc::new(crate::SessionProjections::default());
        let _d = registry.register(turn_outline_projection_definition());
        let value = registry.state_of(&session, "turnOutline").unwrap();
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn folds_boundary_prompt_response() {
        let state = state_of(&[
            SessionEvent::TurnStart { turn: 1 },
            user_message("你好，帮我看看这段代码"),
            assistant_message(1, "这是一段示例回复"),
            SessionEvent::TurnEnd { turn: 1, reason: dsh_session::TurnEndReason::Completed },
        ]);
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].turn, 1);
        assert_eq!(state.turns[0].seq, 0);
        assert_eq!(state.turns[0].prompt, "你好，帮我看看这段代码");
        assert_eq!(state.turns[0].response, "这是一段示例回复");
        assert_eq!(state.draft, "");
    }

    #[test]
    fn prompt_only_fills_once_and_requires_user_source() {
        // steering（同轮第二条人类消息）不改首条预览；非 User 源也不填。
        let state = state_of(&[
            SessionEvent::TurnStart { turn: 1 },
            user_message("第一问"),
            user_message("第二问"),
            SessionEvent::TurnEnd { turn: 1, reason: dsh_session::TurnEndReason::Completed },
        ]);
        assert_eq!(state.turns[0].prompt, "第一问");
    }

    #[test]
    fn order_guard_keeps_outline_sorted() {
        let state = state_of(&[
            SessionEvent::TurnStart { turn: 2 },
            SessionEvent::TurnStart { turn: 2 }, // 重复边界被忽略
            SessionEvent::TurnStart { turn: 1 }, // 回退边界被忽略
        ]);
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].turn, 2);
    }

    #[test]
    fn long_previews_clip_with_ellipsis() {
        let long = "字".repeat(300);
        let state = state_of(&[
            SessionEvent::TurnStart { turn: 1 },
            user_message(&long),
            SessionEvent::TurnEnd { turn: 1, reason: dsh_session::TurnEndReason::Completed },
        ]);
        let prompt = &state.turns[0].prompt;
        assert_eq!(prompt.chars().count(), PROMPT_PREVIEW_LIMIT);
        assert!(prompt.ends_with('…'));

        let multi = vec![
            ContentBlock::Text { text: "第一块".into() },
            ContentBlock::Text { text: "第二块".into() },
        ];
        assert_eq!(preview_text(&multi, 50), "第一块 第二块");
    }

    #[test]
    fn wire_view_stays_quiet_across_draft_only_changes() {
        // draft-only（助手消息进入草稿）不改 turns 数组：wire 视图输出
        // 与上个发布相等（上游 Object.is 门控的值等价面）。
        let registry = std::sync::Arc::new(crate::SessionProjections::default());
        let _d = registry.register(turn_outline_projection_definition());
        let mut session = Session::new(SessionId("t".into()));
        session.append(SessionEvent::TurnStart { turn: 1 });
        session.append(user_message("问"));
        registry.drive(&session);
        let before = registry.view_of(&session, "turnOutline").unwrap();
        session.append(assistant_message(1, "答"));
        let after = registry.view_of(&session, "turnOutline").unwrap();
        assert!(Arc::ptr_eq(&before, &after), "draft-only 变化不得更换 wire 输出");

        // drive 变更流：边界+prompt+回复 = 一轮恰好三次推送（draft-only
        // 的助手草稿不推送）。
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let registry2 = std::sync::Arc::new(crate::SessionProjections::default());
        let _d2 = registry2.register(turn_outline_projection_definition());
        let _ld2 = {
            let hits_c = Arc::clone(&hits);
            registry2.on_changed(Arc::new(move |_s, _k, _v, _seq| {
                hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }))
        };
        let mut session2 = Session::new(SessionId("t2".into()));
        session2.append(SessionEvent::TurnStart { turn: 1 });
        session2.append(user_message("问"));
        registry2.drive(&session2);
        session2.append(assistant_message(1, "答"));
        registry2.drive(&session2);
        session2.append(SessionEvent::TurnEnd { turn: 1, reason: dsh_session::TurnEndReason::Completed });
        registry2.drive(&session2);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}
