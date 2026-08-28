//! dsh-llm — the LLM vocabulary and adapter seam.
//!
//! Mirrors [`packages/llm/llm`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm)
//! in the reference harness: the `Message`/`ContentBlock` variants every request
//! and durable history share, the raw `StreamChunk` protocol, the
//! `LlmAdapter` contract, and the shared `BlockAssembler`.

pub mod adapter;
pub mod assembler;
pub mod error;
pub mod events;
pub mod message;
pub mod types;

pub use adapter::{BoxStream, Disposer, LlmAdapter, LlmRuntime};
pub use assembler::BlockAssembler;
pub use error::*;
pub use events::{LlmAdaptersUpdated, LlmStream};
pub use message::*;
pub use types::*;