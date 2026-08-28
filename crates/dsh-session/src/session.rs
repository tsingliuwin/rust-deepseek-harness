//! Append-only session event log and its model-history projection.

use serde::{Deserialize, Serialize};

/// Why a turn ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndReason {
    Completed,
    MaxTokens,
    Blocked,
    Aborted,
    Error { failure: dsh_llm::LlmFailure },
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
}

/// A log entry: the event plus its monotonic sequence number.
#[derive(Clone, Debug)]
pub struct SessionEntry {
    pub seq: u64,
    pub event: SessionEvent,
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
    pub fn derive_messages(&self) -> Vec<dsh_llm::Message> {
        self.entries
            .iter()
            .filter_map(|e| match &e.event {
                SessionEvent::UserMessage(m) => Some(m.clone()),
                SessionEvent::AssistantMessage { message, .. } => {
                    if message.content.is_empty() { None } else { Some(message.clone()) }
                }
                SessionEvent::ToolResult { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect()
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