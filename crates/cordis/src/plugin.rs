//! Fiber lifecycle: plugins mounted into a scoped context with reversible
//! effects and dependency injection.
//!
//! Mirrors Cordis's "a plugin is a Service; a fiber is its lifecycle":
//! [`Scope`] is the per-plugin context whose registrations auto-unwind on
//! disposal; [`Fiber`] owns that scope; [`PluginManager`] mounts plugins in
//! `inject`-satisfied order and applies patches to the config rows.

use crate::context::{Context, Effect};
use serde_json::Value;
use std::any::{Any, TypeId};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A plugin's application error.
#[derive(Debug, Clone)]
pub struct PluginError(pub String);

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for PluginError {}

/// A per-plugin context whose registrations unwind on disposal.
///
/// Plugin code registers listeners and services through `scope.*`; every
/// registration is recorded as an [`Effect`] on the owning fiber.
#[derive(Clone)]
pub struct Scope {
    ctx: Context,
    effects: Arc<Mutex<Vec<Effect>>>,
}

impl Scope {
    pub(crate) fn new(ctx: Context) -> Self {
        Self { ctx, effects: Arc::new(Mutex::new(Vec::new())) }
    }

    pub fn ctx(&self) -> &Context {
        &self.ctx
    }

    /// Record one reversible effect owned by the fiber.
    pub fn effect(&self, effect: Effect) {
        self.effects.lock().unwrap().push(effect);
    }

    pub fn provide<T: Any + Send + Sync>(&self, service: T) {
        let disposer = self.ctx.provide(service);
        self.effect(disposer);
    }

    pub fn provide_arc<T: Any + Send + Sync>(&self, service: Arc<T>) {
        let disposer = self.ctx.provide_arc(service);
        self.effect(disposer);
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.ctx.get::<T>()
    }

    pub fn on_emit<E: crate::EmitEvent>(&self, f: impl Fn(&E::Payload) + Send + Sync + 'static) {
        let disposer = self.ctx.on_emit::<E>(f);
        self.effect(Effect::from_box(disposer));
    }

    pub fn on_waterfall<E: crate::WaterfallEvent>(
        &self,
        f: impl Fn(E::Payload, crate::BoxFuture<E::Output>) -> crate::BoxFuture<E::Output> + Send + Sync + 'static,
    ) {
        let disposer = self.ctx.on_waterfall::<E>(f);
        self.effect(Effect::from_box(disposer));
    }

    pub fn on_serial<E: crate::SerialEvent>(
        &self,
        f: impl Fn(E::Payload) -> crate::BoxFuture<Option<E::Output>> + Send + Sync + 'static,
    ) {
        let disposer = self.ctx.on_serial::<E>(f);
        self.effect(Effect::from_box(disposer));
    }
}

/// A plugin: a stable id, optional service-type dependencies, and `apply`.
pub trait Plugin: Send + Sync {
    /// Stable row id used for patching config.
    fn id(&self) -> &str;

    /// Service types this plugin requires to be present before it applies.
    fn inject(&self) -> Vec<TypeId> {
        Vec::new()
    }

    /// Apply the plugin against its scoped context, reading `config` (if any).
    fn apply(&self, scope: &Scope, config: Option<&Value>) -> Result<(), PluginError>;
}

/// One fiber: a mounted plugin's lifecycle.
pub struct Fiber {
    id: String,
    scope: Scope,
}

impl Fiber {
    fn new(id: String, scope: Scope) -> Self {
        Self { id, scope }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Unwind every registration in reverse order.
    pub fn dispose(self) {
        let effects: Vec<Effect> = {
            let mut guard = self.scope.effects.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        for effect in effects.into_iter().rev() {
            effect.dispose();
        }
    }
}

/// A config row naming one plugin to mount.
#[derive(Clone)]
pub struct PluginRow {
    pub id: String,
    pub plugin: Arc<dyn Plugin>,
    pub config: Option<Value>,
}

impl PluginRow {
    pub fn new(id: impl Into<String>, plugin: Arc<dyn Plugin>) -> Self {
        Self { id: id.into(), plugin, config: None }
    }

    pub fn with_config(mut self, config: Value) -> Self {
        self.config = Some(config);
        self
    }
}

/// One patch operation over a row list (config is replaced by id or rows are
/// inserted/removed).
#[derive(Clone)]
pub enum Patch {
    Replace { id: String, config: Value },
    Insert { after: Option<String>, row: PluginRow },
    Remove { id: String },
}

/// Mounts plugins as fibers and applies patches.
#[derive(Default)]
pub struct PluginManager {
    fibers: Mutex<Vec<Fiber>>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mount one plugin whose `inject` is already satisfied.
    pub fn mount(&self, ctx: &Context, plugin: Arc<dyn Plugin>, config: Option<Value>) -> Result<(), PluginError> {
        let missing: Vec<TypeId> = plugin.inject().into_iter().filter(|t| !ctx.contains(*t)).collect();
        if !missing.is_empty() {
            return Err(PluginError(format!(
                "plugin \"{}\" has unmet dependencies ({missing:?})",
                plugin.id()
            )));
        }
        let scope = Scope::new(ctx.clone());
        plugin.apply(&scope, config.as_ref())?;
        self.fibers.lock().unwrap().push(Fiber::new(plugin.id().to_string(), scope));
        Ok(())
    }

    /// Mount rows in order, deferring any row whose `inject` isn't satisfied
    /// yet until an earlier-mounted plugin provides it.
    pub fn mount_all(&self, ctx: &Context, rows: Vec<PluginRow>) -> Result<(), PluginError> {
        let mut pending: VecDeque<PluginRow> = rows.into();
        let mut stalled = 0usize;
        while let Some(row) = pending.pop_front() {
            let satisfied = row.plugin.inject().into_iter().all(|t| ctx.contains(t));
            if satisfied {
                self.mount(ctx, row.plugin, row.config)?;
                stalled = 0;
            } else {
                pending.push_back(row);
                stalled += 1;
                if stalled >= pending.len() {
                    return Err(PluginError(format!(
                        "unmet dependencies: {} plugin(s) could not be mounted",
                        pending.len()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Apply config patches to a row list, returning the composed rows.
    pub fn patch(rows: Vec<PluginRow>, patches: Vec<Patch>) -> Vec<PluginRow> {
        let mut rows = rows;
        for patch in patches {
            match patch {
                Patch::Replace { id, config } => {
                    if let Some(row) = rows.iter_mut().find(|r| r.id == id) {
                        row.config = Some(config);
                    }
                }
                Patch::Insert { after, row } => match after {
                    None => rows.insert(0, row),
                    Some(id) => {
                        let pos = rows.iter().position(|r| r.id == id).map(|i| i + 1).unwrap_or(rows.len());
                        rows.insert(pos, row);
                    }
                },
                Patch::Remove { id } => rows.retain(|r| r.id != id),
            }
        }
        rows
    }

    /// Unwind all mounted fibers in reverse mount order.
    pub fn dispose_all(self) {
        let fibers: Vec<Fiber> = {
            let mut guard = self.fibers.lock().unwrap();
            std::mem::take(&mut *guard)
        };
        for fiber in fibers.into_iter().rev() {
            fiber.dispose();
        }
    }
}