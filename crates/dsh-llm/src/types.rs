//! Canonical provider-neutral message and streaming vocabulary for the loop,
//! session log, and plugins. Mirrors
//! [`packages/llm/llm/src/types.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/types.ts):
//! the `ContentBlock` variants, the raw `StreamChunk` protocol, the
//! `FinishReason` / `TokenUsage` / `LlmFailure` value types, and the fully
//! assembled `GenerateOptions` request.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Declare a branded identifier newtype wrapping a `String`.
///
/// The reference brands every opaque cross-boundary id (`Branded<B>`) rather
/// than accepting a bare `string`. A `#[serde(transparent)]` newtype keeps the
/// wire representation a plain string while making the boundary explicit.
macro_rules! brand {
    ($name:ident $(,)?) => {
        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

/// Provider-issued tool-call id; correlates with the matching tool result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CallId(pub String);
brand!(CallId);

/// Stable message identity preserved across every representation boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub String);
brand!(MessageId);

impl MessageId {
    /// Generate a fresh random identity (`crypto.randomUUID()` in the reference).
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}

/// Opaque provider-issued request identifier for diagnostics.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderRequestId(pub String);
brand!(ProviderRequestId);

/// Adapter-owned identifier for one model's selectable reasoning effort.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ReasoningEffortId(pub String);
brand!(ReasoningEffortId);

/// Session identity stamped by the loop for request routing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);
brand!(SessionId);

/// A durable raster image reference, valid in user or assistant content.
///
/// Field names mirror upstream `ImageAttachmentRef`
/// (`packages/attachment/attachment/src/types.ts`): the stored bytes are
/// content-addressed (`attachmentId` = `sha256:<hex>`), normalization is owned
/// by the attachment service.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachmentRef {
    pub attachment_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub media_type: String,
    pub bytes: u64,
    pub width: u32,
    pub height: u32,
    /// Input dimensions before normalization scaling; present only when
    /// normalization reduced the image.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_dimensions: Option<ImageDimensions>,
}

/// Intrinsic pixel dimensions (upstream `originalDimensions` member shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

/// A durable verbatim file reference (`FileAttachmentRef` upstream): files are
/// stored byte-for-byte with no normalization; `attachment_id` is the sha256
/// digest of exactly those bytes and `name` is the sanitized display leaf name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentRef {
    pub attachment_id: String,
    pub name: String,
    pub bytes: u64,
}

/// Plain text visible to the end user.
/// Reasoning / thinking content, distinct from visible text.
/// A tool invocation requested by the model.
/// The result of a tool invocation, sent back to the model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Image {
        attachment: ImageAttachmentRef,
    },
    /// A verbatim stored file reference. Providers never receive file bytes:
    /// request assembly replaces every file block (including nested tool
    /// results) with deterministic handle text (`project_files_to_text`).
    File {
        attachment: FileAttachmentRef,
    },
    ToolCall {
        id: CallId,
        name: String,
        /// Raw JSON string as produced by the model.
        arguments: String,
    },
    ToolResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: CallId,
        content: Vec<ContentBlock>,
        #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn reasoning(text: impl Into<String>) -> Self {
        Self::Reasoning { text: text.into() }
    }

    pub fn tool_call(id: CallId, name: String, arguments: String) -> Self {
        Self::ToolCall { id, name, arguments }
    }

    pub fn tool_result(tool_call_id: CallId, content: Vec<ContentBlock>, is_error: bool) -> Self {
        Self::ToolResult {
            tool_call_id,
            content,
            is_error: Some(is_error),
        }
    }

    /// The `type` tag vocabulary.
    pub fn content_type(&self) -> ContentBlockType {
        match self {
            Self::Text { .. } => ContentBlockType::Text,
            Self::Reasoning { .. } => ContentBlockType::Reasoning,
            Self::Image { .. } => ContentBlockType::Image,
            Self::File { .. } => ContentBlockType::File,
            Self::ToolCall { .. } => ContentBlockType::ToolCall,
            Self::ToolResult { .. } => ContentBlockType::ToolResult,
        }
    }
}

/// The block `type` tag vocabulary. The reference derives this as a
/// merge-extensible map key set; Rust keeps a closed enum until the plugin
/// layer adds declaration merging in a later milestone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContentBlockType {
    Text,
    Reasoning,
    Image,
    File,
    ToolCall,
    ToolResult,
}

/// Why a model response stopped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    MaxTokens,
    Aborted {
        failure: LlmFailure,
    },
    Error {
        failure: LlmFailure,
    },
}

/// Per-call token accounting (cache fields are optional).
///
/// Counts are DISJOINT: `input_tokens` is uncached input only; cached input is
/// reported separately as `cache_read_tokens`/`cache_write_tokens` (billed
/// input = sum of the three). Adapters whose providers fold cache hits into a
/// total prompt count (DeepSeek's `prompt_tokens`) subtract them back out.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
}

/// Serializable provider or transport failure facts; policy decides whether
/// they are retryable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmFailure {
    /// Human-readable provider or transport failure.
    pub message: String,
    /// Stable provider-neutral machine-routing code.
    pub code: String,
    /// HTTP status returned by the provider, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Provider-requested delay in milliseconds, when valid and available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_retry_after_ms: Option<u64>,
    /// Opaque provider-issued request identifier for diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ProviderRequestId>,
}

/// Adapter-private lossless-JSON state for replaying a successful response.
///
/// Both halves stay opaque to the harness; only the split is shared vocabulary,
/// so assembly can keep stored metadata aligned with stored content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayEnvelope {
    /// Response-level adapter-private metadata (ids, native stop reason).
    pub response: serde_json::Value,
    /// Per-block adapter-private metadata, one entry per emitted block in
    /// first-seen stream order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<Vec<serde_json::Value>>,
}

/// Raw streaming protocol emitted by adapters.
///
/// Block indexes correlate interleaved deltas, and `block-end` carries the
/// assembled block. `usage` precedes the terminal `finish`; tool arguments
/// remain raw JSON strings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum StreamChunk {
    BlockStart {
        index: usize,
        #[serde(rename = "blockType")]
        block_type: ContentBlockType,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ReasoningDelta {
        index: usize,
        text: String,
    },
    ToolCallDelta {
        index: usize,
        id: CallId,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(rename = "argumentsDelta")]
        arguments_delta: String,
    },
    BlockEnd {
        index: usize,
        block: ContentBlock,
    },
    Usage {
        usage: TokenUsage,
    },
    Finish {
        reason: FinishReason,
        #[serde(rename = "replayState", skip_serializing_if = "Option::is_none")]
        replay_state: Option<ReplayEnvelope>,
    },
}

impl StreamChunk {
    /// A successful `stop` finish (used as the terminal chunk by adapters whose
    /// provider sent no stop reason of its own).
    pub fn stop() -> Self {
        Self::Finish { reason: FinishReason::Stop, replay_state: None }
    }

    /// Terminate a stream in-band with a normalized failure.
    pub fn finish_error(failure: LlmFailure) -> Self {
        Self::Finish { reason: FinishReason::Error { failure }, replay_state: None }
    }

    /// Terminate a stream in-band with an `ABORTED` failure.
    pub fn stream_aborted() -> Self {
        Self::Finish {
            reason: FinishReason::Aborted {
                failure: LlmFailure {
                    message: "aborted".to_string(),
                    code: crate::error::ABORTED.to_string(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
            },
            replay_state: None,
        }
    }
}

/// Lossless compact representation of one model-stream attempt, embedded in
/// durable v2 settlement events (`assistant/message.stream`,
/// `assistant/attempt.stream`). Mirrors upstream `AssistantStreamRecord`
/// (`packages/llm/llm/src/assistant-stream.ts`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AssistantStreamRecord {
    #[serde(rename_all = "camelCase")]
    TextChunks {
        time0: u64,
        index: usize,
        /// ms gaps; `dt[i]` is the gap between member `i` and `i+1`.
        dt: Vec<i64>,
        texts: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    ReasoningChunks {
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        texts: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    ToolCallChunks {
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        id: CallId,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        args: Vec<String>,
    },
    /// A chunk that cannot merge (block boundaries, usage, finish) verbatim.
    Chunk {
        time: u64,
        chunk: StreamChunk,
    },
}

/// JSON-schema description of a tool, as sent to the model.
/// Declared here (not in `dsh-tools`) because it is part of `GenerateOptions`;
/// both `dsh-tools` and `dsh-system-prompt` import it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema object for the arguments.
    pub parameters: serde_json::Value,
}

/// Provider-neutral classification for an auxiliary model call.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuxiliaryPurpose {
    Compaction,
    SessionTitle,
}

/// Cancellation for one model call. Runtime-agnostic stand-in for the
/// reference's `AbortSignal`; adapters honor it between stream items.
#[derive(Clone, Debug, Default)]
pub struct AbortSignal(Arc<AtomicBool>);

impl AbortSignal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn abort(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn aborted(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Return `Err(ABORTED)` when the signal has been aborted.
    pub fn check(&self) -> Result<(), LlmFailure> {
        if self.aborted() {
            Err(LlmFailure {
                message: "aborted".to_string(),
                code: crate::error::ABORTED.to_string(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            })
        } else {
            Ok(())
        }
    }
}

/// A single model request, fully assembled.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateOptions {
    /// Registered provider route selecting the adapter instance.
    pub provider: String,
    pub model: String,
    /// Adapter-owned reasoning effort selected for this exact model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    /// Ordered conversation messages, exactly as the provider sees them.
    pub messages: Vec<crate::message::Message>,
    /// System prompt text (adapters map to the provider's system slot).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Tool schemas (adapters map to the provider's `tools` field).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Cancellation, not serialized.
    #[serde(skip)]
    pub signal: AbortSignal,
    /// Session identity stamped by the loop for request routing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Optional auxiliary-purpose classification.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<AuxiliaryPurpose>,
}

impl GenerateOptions {
    pub fn new(provider: impl Into<String>, model: impl Into<String>, messages: Vec<crate::Message>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            reasoning_effort: None,
            messages,
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: AbortSignal::new(),
            session_id: None,
            purpose: None,
        }
    }
}

/// Display metadata for one registered provider route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmProviderInfo {
    pub id: String,
    pub name: String,
}

/// Provider-owned context capacity for one exact provider/model route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelContext {
    pub context_window: u64,
}

/// One adapter-discovered model; catalog membership is advisory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Exact-route model metadata resolved by its owning adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResolvedModelInfo {
    pub provider: String,
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<LlmModelContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u32>,
}

/// Config fields of one conversation's requests, mapping 1:1 onto the
/// same-named `GenerateOptions` fields. The loop builds requests from the
/// logged header rather than accepting these per call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmCallConfig {
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffortId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

impl LlmCallConfig {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            reasoning_effort: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        }
    }
}

/// Retry policy mode. Full backoff/decision wiring lands with the retry
/// milestone; the value type exists now so adapter registrations carry one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum ResolvedRetryPolicy {
    Normal {
        max_retries: u32,
        retryable_codes: Vec<String>,
        initial_delay_ms: u64,
        max_delay_ms: u64,
        jitter_ratio: f64,
    },
    Always {
        initial_delay_ms: u64,
        max_delay_ms: u64,
        jitter_ratio: f64,
    },
}

impl Default for ResolvedRetryPolicy {
    /// The normal default of five retries.
    fn default() -> Self {
        Self::Normal {
            max_retries: 5,
            retryable_codes: Self::DEFAULT_RETRYABLE_CODES.iter().map(|s| s.to_string()).collect(),
            initial_delay_ms: 1_000,
            max_delay_ms: 60_000,
            jitter_ratio: 0.1,
        }
    }
}

impl ResolvedRetryPolicy {
    /// Transient codes retried by default. Terminal codes
    /// (`INVALID_CREDENTIAL`, `CONTEXT_WINDOW_EXCEEDED`, `QUOTA`, `ABORTED`)
    /// are deliberately absent.
    pub const DEFAULT_RETRYABLE_CODES: &'static [&'static str] =
        &["RATE_LIMIT", "TIMEOUT", "EMPTY_RESPONSE", "UNKNOWN"];

    /// Whether a failure with `code` may be retried under this policy.
    pub fn is_retryable(&self, code: &str) -> bool {
        match self {
            Self::Normal { retryable_codes, .. } => {
                retryable_codes.is_empty() || retryable_codes.iter().any(|c| c == code)
            }
            Self::Always { .. } => true,
        }
    }

    /// The finite retry cap, or `None` for `Always` (unbounded).
    pub fn max_retries(&self) -> Option<u32> {
        match self {
            Self::Normal { max_retries, .. } => Some(*max_retries),
            Self::Always { .. } => None,
        }
    }

    /// Exponential-backoff delay for the given zero-based attempt, capped and
    /// jittered deterministically (no external RNG dependency).
    pub fn delay_for(&self, attempt: u32) -> std::time::Duration {
        let (initial, max, jitter) = match self {
            Self::Normal { initial_delay_ms, max_delay_ms, jitter_ratio, .. } => {
                (*initial_delay_ms, *max_delay_ms, *jitter_ratio)
            }
            Self::Always { initial_delay_ms, max_delay_ms, jitter_ratio } => {
                (*initial_delay_ms, *max_delay_ms, *jitter_ratio)
            }
        };
        let base = initial.saturating_mul(1u64 << attempt.min(20)).min(max).max(1);
        // Deterministic noise in [-1, 1] so retries don't stampede.
        let noise = (attempt as u64)
            .wrapping_mul(2_654_435_761)
            .wrapping_add(0x9E37_79B9)
            % 2000;
        let frac = (noise as f64 / 1000.0) - 1.0;
        let ms = (base as f64 * (1.0 + jitter * frac)).max(1.0) as u64;
        std::time::Duration::from_millis(ms)
    }
}

#[cfg(test)]
mod retry_policy_tests {
    use super::*;

    #[test]
    fn default_retries_transient_not_terminal() {
        let p = ResolvedRetryPolicy::default();
        assert!(p.is_retryable("RATE_LIMIT"));
        assert!(p.is_retryable("EMPTY_RESPONSE"));
        assert!(p.is_retryable("TIMEOUT"));
        assert!(!p.is_retryable("INVALID_CREDENTIAL"));
        assert!(!p.is_retryable("CONTEXT_WINDOW_EXCEEDED"));
    }

    #[test]
    fn always_mode_retries_everything_unbounded() {
        let p = ResolvedRetryPolicy::Always { initial_delay_ms: 10, max_delay_ms: 100, jitter_ratio: 0.0 };
        assert!(p.is_retryable("ANYTHING"));
        assert_eq!(p.max_retries(), None);
    }

    #[test]
    fn delay_grows_then_caps() {
        let p = ResolvedRetryPolicy::Normal {
            max_retries: 5,
            retryable_codes: vec![],
            initial_delay_ms: 100,
            max_delay_ms: 1000,
            jitter_ratio: 0.0,
        };
        assert_eq!(p.delay_for(0).as_millis(), 100);
        assert_eq!(p.delay_for(1).as_millis(), 200);
        assert_eq!(p.delay_for(10).as_millis(), 1000);
    }
}