//! dsh-session — the append-only session event log.
//!
//! Mirrors [`packages/core/session`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/session):
//! the durable `SessionEvent` stream, the in-memory store, `derive_messages()`
//! projection of model-visible history, and the `request/header` /
//! `request/context` epochs that make a request reconstructable from the log.

pub mod session;

pub use session::{
    EpochHeader, HeaderReason, RequestContext, Session, SessionEntry, SessionEvent, TurnEndReason,
};