//! dsh-cordis — a minimal plugin/context layer.
//!
//! The reference harness is built on Cordis ("everything is a plugin"). This
//! crate provides the context (a repository of services), the reversible
//! `Effect`, a typed [`EventBus`] with `emit` / `waterfall` / `serial`
//! dispatch, and the fiber lifecycle ([`Fiber`] / [`Scope`] / [`Plugin`] /
//! [`PluginManager`]) with `inject` dependency ordering and patchable config.

pub mod context;
pub mod event;
pub mod plugin;

pub use context::{Context, Effect};
pub use event::{
    BoxFuture, Disposer, EmitEvent, EventBus, SerialEvent, WaterfallEvent,
};
pub use plugin::{Fiber, Patch, Plugin, PluginError, PluginManager, PluginRow, Scope};