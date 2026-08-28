//! The shared context: a repository of replaceable services plus the event bus.
//!
//! Mirrors Cordis's "a context is a repository of services" with the event bus
//! mixed in. v1 keeps services keyed by [`TypeId`]; typed event dispatch is
//! added directly so call sites read like the reference (`ctx.on(...)`,
//! `ctx.waterfall(...)`, `ctx.emit(...)`).

use crate::event::{BoxFuture, Disposer, EmitEvent, EventBus, SerialEvent, WaterfallEvent};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A reversible registration. `dispose` unwinds whatever `apply` mounted, and
/// is idempotent.
pub struct Effect(Option<Box<dyn FnOnce() + Send>>);

impl Effect {
    pub fn new(undo: impl FnOnce() + Send + 'static) -> Self {
        Self(Some(Box::new(undo)))
    }

    pub fn from_box(undo: Box<dyn FnOnce() + Send>) -> Self {
        Self(Some(undo))
    }

    pub fn dispose(mut self) {
        if let Some(undo) = self.0.take() {
            undo();
        }
    }
}

/// A shared context holding replaceable services and a typed event bus.
#[derive(Clone, Default)]
pub struct Context {
    services: Arc<Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>>,
    events: EventBus,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    // --- services -----------------------------------------------------------

    pub fn provide<T: Any + Send + Sync>(&self, service: T) -> Effect {
        self.provide_arc(Arc::new(service))
    }

    pub fn provide_arc<T: Any + Send + Sync>(&self, service: Arc<T>) -> Effect {
        let key = TypeId::of::<T>();
        let services = Arc::clone(&self.services);
        services.lock().unwrap().insert(key, Box::new(service));
        Effect::new(move || {
            services.lock().unwrap().remove(&key);
        })
    }

    pub fn get<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        let map = self.services.lock().unwrap();
        let any = map.get(&TypeId::of::<T>())?;
        any.downcast_ref::<Arc<T>>().cloned()
    }

    /// Whether a service of the given type has been provided.
    pub fn contains(&self, type_id: TypeId) -> bool {
        self.services.lock().unwrap().contains_key(&type_id)
    }

    /// The set of service types currently provided.
    pub fn provided_types(&self) -> Vec<TypeId> {
        self.services.lock().unwrap().keys().copied().collect()
    }

    // --- events -------------------------------------------------------------

    pub fn events(&self) -> &EventBus {
        &self.events
    }

    pub fn on_emit<E: EmitEvent>(&self, f: impl Fn(&E::Payload) + Send + Sync + 'static) -> Disposer {
        self.events.on_emit::<E>(f)
    }

    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        self.events.emit::<E>(payload);
    }

    pub fn on_waterfall<E: WaterfallEvent>(
        &self,
        f: impl Fn(E::Payload, BoxFuture< E::Output>) -> BoxFuture< E::Output>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        self.events.on_waterfall::<E>(f)
    }

    pub async fn waterfall<E: WaterfallEvent>(
        &self,
        payload: E::Payload,
        default: impl FnOnce(E::Payload) -> BoxFuture< E::Output> + Send + 'static,
    ) -> E::Output {
        self.events.waterfall::<E>(payload, default).await
    }

    pub fn on_serial<E: SerialEvent>(
        &self,
        f: impl Fn(E::Payload) -> BoxFuture< Option<E::Output>> + Send + Sync + 'static,
    ) -> Disposer {
        self.events.on_serial::<E>(f)
    }

    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        self.events.serial::<E>(payload).await
    }
}