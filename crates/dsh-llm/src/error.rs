//! Harness error base with a stable machine-routable code.
//! Mirrors
//! [`packages/llm/llm/src/error.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/error.ts):
//! a `LlmError` carrying `code` (stable, programmatic) distinct from the
//! human-readable message, plus the provider-neutral failure classifiers.

use crate::types::LlmFailure;
use std::sync::LazyLock;

/// Stable machine-routable failure codes. Route on these, never by parsing a
/// message.
pub const NO_ADAPTER: &str = "NO_ADAPTER";
pub const DUPLICATE_ADAPTER: &str = "DUPLICATE_ADAPTER";
pub const REGISTRATION_DISPOSED: &str = "REGISTRATION_DISPOSED";
pub const INVALID_PREPARED_CALL: &str = "INVALID_PREPARED_CALL";
pub const CONTEXT_WINDOW_EXCEEDED: &str = "CONTEXT_WINDOW_EXCEEDED";
pub const QUOTA: &str = "QUOTA";
pub const EMPTY_RESPONSE: &str = "EMPTY_RESPONSE";
pub const INVALID_CREDENTIAL: &str = "INVALID_CREDENTIAL";
pub const INVALID_ARGS: &str = "INVALID_ARGS";
pub const INVARIANT: &str = "INVARIANT";
pub const TIMEOUT: &str = "TIMEOUT";
pub const ABORTED: &str = "ABORTED";
pub const UNKNOWN: &str = "UNKNOWN";

/// The harness error base for the LLM layer: message + code + `LlmFailure`.
/// The failure facts are boxed to keep the `Result` `Err` variant small.
#[derive(Clone, Debug, PartialEq)]
pub struct LlmError {
    /// Human-readable failure text.
    pub message: String,
    /// Stable machine-routable failure class.
    pub code: String,
    /// Full serializable failure facts.
    pub failure: Box<LlmFailure>,
}

impl LlmError {
    pub fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        let message = message.into();
        let code = code.into();
        Self {
            failure: Box::new(LlmFailure {
                message: message.clone(),
                code: code.clone(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            }),
            message,
            code,
        }
    }

    pub fn from_failure(failure: LlmFailure) -> Self {
        let code = failure.code.clone();
        Self { message: failure.message.clone(), code, failure: Box::new(failure) }
    }

    pub fn no_adapter(provider: &str) -> Self {
        Self::new(format!("no adapter registered for provider \"{provider}\""), NO_ADAPTER)
    }

    pub fn duplicate_adapter(provider: &str) -> Self {
        Self::new(format!("provider \"{provider}\" already has an adapter"), DUPLICATE_ADAPTER)
    }

    pub fn aborted() -> Self {
        Self::new("aborted", ABORTED)
    }

    pub fn invariant(message: impl Into<String>) -> Self {
        Self::new(message, INVARIANT)
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for LlmError {}

// --- Provider-neutral failure classifiers ----------------------------------
//
// A static regex must initialize through `LazyLock::new(|| ...)` with the
// literal inlined (no captures), so a small macro expands each classifier.

macro_rules! regex_static {
    ($name:ident, $re:literal) => {
        static $name: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new($re).unwrap());
    };
}

regex_static!(
    STRUCTURED_CONTEXT_OVERFLOW,
    r"(?i)(?:^|[^a-z0-9])context[\s_-](?:length|window)[\s_-](?:exceed(?:ed|s)?|overflow(?:ed)?|limit[\s_-]exceeded)(?:$|[^a-z0-9])"
);
regex_static!(
    MAX_CONTEXT_LENGTH,
    r"(?i)\b(?:maximum|max)(?:\s+(?:allowed|supported))?\s+context\s+(?:length|window)\b"
);
regex_static!(
    TOO_LARGE_FOR_CONTEXT,
    r"(?i)\b(?:request|prompt|input|messages?)\s+(?:is\s+|are\s+)?too\s+(?:large|long)\s+for\s+(?:(?:this|the)\s+)?(?:model(?:'s)?\s+)?context(?:\s+window)?\b"
);
regex_static!(
    INPUT_TOO_LONG_FOR_MODEL,
    r"(?i)\b(?:input|prompt|request)\s+(?:is\s+)?too\s+(?:long|large)\s+for\s+(?:this|the)\s+model\b"
);
regex_static!(
    EXCEEDS_MODEL_CONTEXT,
    r"(?i)\b(?:input|prompt|request|messages?)\b.{0,40}\b(?:exceed(?:s|ed)?|overflows?|is\s+larger\s+than)\b.{0,40}\b(?:the\s+)?(?:model(?:'s)?\s+)?context(?:\s+(?:length|window))?\b"
);

/// Recognize context-overflow wording used by OpenAI-compatible providers and
/// library adapters.
pub fn is_context_window_exceeded(detail: &str) -> bool {
    STRUCTURED_CONTEXT_OVERFLOW.is_match(detail)
        || MAX_CONTEXT_LENGTH.is_match(detail)
        || TOO_LARGE_FOR_CONTEXT.is_match(detail)
        || INPUT_TOO_LONG_FOR_MODEL.is_match(detail)
        || EXCEEDS_MODEL_CONTEXT.is_match(detail)
}

regex_static!(
    INSUFFICIENT_QUOTA,
    r"(?i)\binsufficient[\s_-]+(?:quota|balance|credits?)\b"
);
regex_static!(
    QUOTA_EXCEEDED,
    r"(?i)\b(?:quota|usage[\s_-]+limit)[\s_-]+(?:exceeded|exhausted|reached)\b"
);
regex_static!(
    EXCEED_QUOTA,
    r"(?i)\bexceed(?:ed|s)?[\s_-]+(?:(?:your|the)[\s_-]+)?(?:current[\s_-]+)?quota\b"
);
regex_static!(
    BALANCE_EXHAUSTED,
    r"(?i)\b(?:balance|credits?)[\s_-]+(?:exhausted|depleted)\b"
);
regex_static!(
    OUT_OF_CREDITS,
    r"(?i)\bout[\s_-]+of[\s_-]+(?:credits?|budget)\b"
);

/// Recognize provider wording that identifies an exhausted account quota
/// rather than a transient request-rate limit.
pub fn is_quota_exceeded(detail: &str) -> bool {
    INSUFFICIENT_QUOTA.is_match(detail)
        || QUOTA_EXCEEDED.is_match(detail)
        || EXCEED_QUOTA.is_match(detail)
        || BALANCE_EXHAUSTED.is_match(detail)
        || OUT_OF_CREDITS.is_match(detail)
}

/// Render a thrown value with its full cause chain, so transport wrappers
/// surface the underlying failure instead of masking it. Diagnostic-surface
/// rendering only — never parse the result; route on `LlmError.code`.
pub fn error_chain(value: &dyn std::error::Error) -> String {
    let mut parts = vec![format!("{value}")];
    let mut next = value.source();
    while let Some(cause) = next {
        let text = format!("{cause}");
        if text != *parts.last().unwrap() {
            parts.push(text);
        }
        next = cause.source();
    }
    parts.join(": ")
}