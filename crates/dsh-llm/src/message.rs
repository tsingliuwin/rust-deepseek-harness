//! Message value types, identity, and immutable construction helpers.
//! Mirrors
//! [`packages/llm/llm/src/message.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/message.ts).

use crate::types::{CallId, ContentBlock, MessageId, StreamChunk};
use serde::{Deserialize, Serialize};

/// Provider-neutral conversation role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// Where a message came from.
///
/// The reference keeps this a merge-extensible sum type; Rust holds it closed
/// for v1 (the `#[non_exhaustive]` marker signals that outward fall-through is
/// expected once the plugin layer arrives).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MessageSource {
    User,
    Plugin {
        plugin: String,
    },
    Model {
        provider: String,
        model: String,
        /// Lossless-JSON adapter state needed to replay the provider response.
        #[serde(rename = "replayState", skip_serializing_if = "Option::is_none")]
        replay_state: Option<serde_json::Value>,
    },
    Tool {
        #[serde(rename = "callId")]
        call_id: CallId,
    },
    /// Producer-injected context (web merge-extensible open vocabulary:
    /// agent-instructions / skill-catalog / session-reference / …; kind is
    /// the web source.kind verbatim). The UI renders this as a context
    /// injection row (ContextInjectionRow); it still belongs to the
    /// model-visible surface (derive_messages keeps it).
    #[serde(rename = "context")]
    Context {
        /// web source.kind original text (kebab-case)
        #[serde(rename = "contextKind")]
        context_kind: String,
        /// producer field carried by the plugin kind
        #[serde(default)]
        plugin: Option<String>,
        /// Semantic information form (instructions/catalog/snapshot/notice/relay/recall)
        #[serde(default)]
        form: Option<String>,
        /// Producer-declared one-line summary attached to the notice form
        #[serde(default)]
        summary: Option<String>,
        /// agent-instruction changed file paths (label source)
        #[serde(default)]
        changes_paths: Vec<String>,
        /// session-reference cited session labels (label source for the recall role)
        #[serde(default)]
        reference_labels: Vec<String>,
        /// skill-invocation name
        #[serde(default)]
        name: Option<String>,
    },
}

/// One immutable message representation shared by delivery, durable history,
/// and model requests.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    /// Stable identity preserved across every representation boundary.
    pub id: MessageId,
    pub role: Role,
    /// Exact model-facing blocks.
    pub content: Vec<ContentBlock>,
    /// Required source fields supplied by the producer.
    pub source: MessageSource,
}

impl Message {
    /// Create one identified message with a fresh stable identity.
    pub fn new(role: Role, content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self { id: MessageId::generate(), role, content, source }
    }

    /// Build a message carrying a pre-assigned identity (replay, re-export).
    pub fn with_id(id: MessageId, role: Role, content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self { id, role, content, source }
    }

    pub fn user(content: Vec<ContentBlock>) -> Self {
        Self::new(Role::User, content, MessageSource::User)
    }

    pub fn user_text(text: impl Into<String>) -> Self {
        Self::user(vec![ContentBlock::text(text)])
    }

    pub fn assistant(content: Vec<ContentBlock>, provider: &str, model: &str) -> Self {
        Self::new(
            Role::Assistant,
            content,
            MessageSource::Model {
                provider: provider.to_string(),
                model: model.to_string(),
                replay_state: None,
            },
        )
    }

    pub fn system(text: impl Into<String>, plugin: &str) -> Self {
        Self::new(
            Role::System,
            vec![ContentBlock::text(text)],
            MessageSource::Plugin { plugin: plugin.to_string() },
        )
    }

    pub fn tool_result(call_id: CallId, content: Vec<ContentBlock>, is_error: bool) -> Self {
        Self::new(
            Role::User,
            vec![ContentBlock::tool_result(call_id.clone(), content, is_error)],
            MessageSource::Tool { call_id },
        )
    }
}

/// Detach and freeze a message whose identity already exists.
///
/// Rust values are owned and immutable by convention; v1 "freeze" is a plain
/// clone that preserves identity.
pub fn freeze_message(message: &Message) -> Message {
    message.clone()
}

/// Create one identified user-role message.
pub fn create_user_message(content: Vec<ContentBlock>) -> Message {
    Message::user(content)
}

/// Create one identified model-produced assistant message.
pub fn create_assistant_message(content: Vec<ContentBlock>, provider: &str, model: &str) -> Message {
    Message::assistant(content, provider, model)
}

/// Create one identified user-role tool-result message.
pub fn create_tool_result_message(call_id: CallId, content: Vec<ContentBlock>, is_error: bool) -> Message {
    Message::tool_result(call_id, content, is_error)
}

/// Whether a stream chunk carries visible model output (the first-token
/// boundary). Empty deltas (heartbeats, empty tool-call frames) do not count.
pub fn is_token_delta(chunk: &StreamChunk) -> bool {
    match chunk {
        StreamChunk::TextDelta { text, .. } | StreamChunk::ReasoningDelta { text, .. } => !text.is_empty(),
        StreamChunk::ToolCallDelta { arguments_delta, name, .. } => !arguments_delta.is_empty() || name.is_some(),
        _ => false,
    }
}

/// Bound one `notice` summary to the reference's 120-character cap.
pub const CONTEXT_SUMMARY_MAX_CHARS: usize = 120;

pub fn bound_context_summary(summary: &str) -> String {
    if summary.chars().count() <= CONTEXT_SUMMARY_MAX_CHARS {
        summary.to_string()
    } else {
        let mut out: String = summary.chars().take(CONTEXT_SUMMARY_MAX_CHARS - 1).collect();
        out.push('…');
        out
    }
}