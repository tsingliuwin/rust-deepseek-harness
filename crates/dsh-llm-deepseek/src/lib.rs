//! dsh-llm-deepseek — the DeepSeek provider adapter.
//!
//! One [`LlmAdapter`] implementation over the DeepSeek HTTP chat-completions
//! endpoint, using `reqwest` for transport and a hand-written SSE tokenizer for
//! the streaming response. Honors the reference contract: `reasoning`
//! (`reasoning_content`) is a first-class block, tool-call `arguments` stay raw
//! JSON strings end-to-end, and cache token accounting subtracts
//! `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` back out of the prompt
//! total.

mod adapter;

pub use adapter::DeepSeekAdapter;