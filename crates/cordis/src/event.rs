//! A typed event bus with Cordis dispatch semantics.
//!
//! Mirrors the vendored Cordis [`EventsService`](https://github.com/deepseek-ai/deepseek-harness/blob/main/vendor/cordis/src/events.ts):
//! - [`emit`](EventBus::emit) runs listeners synchronously in registration
//!   order, ignoring return values;
//! - [`waterfall`](EventBus::waterfall) composes listeners around an innermost
//!   `next`, where a listener that does not call `next` short-circuits the rest
//!   of the chain (including the built-in default);
//! - [`serial`](EventBus::serial) awaits listeners in order until one returns a
//!   bail value (`Some`).
//!
//! Events are concrete Rust marker types (`struct AgentRequest;` etc.) that
//! implement one of [`EmitEvent`] / [`WaterfallEvent`] / [`SerialEvent`]; the
//! bus keys listeners by the event type's [`TypeId`].

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A boxed `'static` future.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// A one-shot unregistration (the reference returns `() => boolean`).
pub type Disposer = Box<dyn FnOnce() + Send>;

/// Marker for a synchronous broadcast event (observer-only, no return value).
pub trait EmitEvent: 'static {
    type Payload: Send + 'static;
}

/// Marker for an around-middleware event whose result flows through `next`.
pub trait WaterfallEvent: 'static {
    type Payload: Clone + Send + 'static;
    type Output: Send + 'static;
}

/// Marker for an ordered, awaited event that bails on the first `Some`.
pub trait SerialEvent: 'static {
    type Payload: Clone + Send + 'static;
    type Output: Send + 'static;
}

type EmitFn<P> = Box<dyn Fn(&P) + Send + Sync>;
type WaterfallFn<P, O> = Arc<dyn Fn(P, BoxFuture<O>) -> BoxFuture<O> + Send + Sync>;
type SerialFn<P, O> = Arc<dyn Fn(P) -> BoxFuture<Option<O>> + Send + Sync>;

type Listener = (u64, Box<dyn Any + Send + Sync>);
type ListenerSlot = Arc<Mutex<Vec<Listener>>>;
type SlotMap = Arc<Mutex<HashMap<TypeId, ListenerSlot>>>;

/// The event bus. All state is shared, so it is cheap to clone and share across
/// services (the reference mixes one bus into every context).
#[derive(Clone, Default)]
pub struct EventBus {
    emit_slots: SlotMap,
    waterfall_slots: SlotMap,
    serial_slots: SlotMap,
    next_token: Arc<AtomicU64>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    fn token(&self) -> u64 {
        self.next_token.fetch_add(1, Ordering::Relaxed)
    }

    fn slot(map: &SlotMap, key: TypeId) -> ListenerSlot {
        Arc::clone(
            map.lock()
                .unwrap()
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(Vec::new()))),
        )
    }

    // --- emit ---------------------------------------------------------------

    pub fn on_emit<E: EmitEvent>(&self, f: impl Fn(&E::Payload) + Send + Sync + 'static) -> Disposer {
        let token = self.token();
        let listener: EmitFn<E::Payload> = Box::new(f);
        let slot = Self::slot(&self.emit_slots, TypeId::of::<E>());
        slot.lock().unwrap().push((token, Box::new(listener)));
        Box::new(move || {
            slot.lock().unwrap().retain(|(t, _)| *t != token);
        })
    }

    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        let key = TypeId::of::<E>();
        let slot = self.emit_slots.lock().unwrap().get(&key).cloned();
        let Some(slot) = slot else { return };
        let listeners = slot.lock().unwrap();
        for (_, l) in listeners.iter() {
            let f = l.downcast_ref::<EmitFn<E::Payload>>().expect("emit listener type");
            f(&payload);
        }
    }

    // --- waterfall ----------------------------------------------------------

    pub fn on_waterfall<E: WaterfallEvent>(
        &self,
        f: impl Fn(E::Payload, BoxFuture< E::Output>) -> BoxFuture< E::Output>
            + Send
            + Sync
            + 'static,
    ) -> Disposer {
        let token = self.token();
        let listener: WaterfallFn<E::Payload, E::Output> = Arc::new(f);
        let slot = Self::slot(&self.waterfall_slots, TypeId::of::<E>());
        slot.lock().unwrap().push((token, Box::new(listener)));
        Box::new(move || {
            slot.lock().unwrap().retain(|(t, _)| *t != token);
        })
    }

    pub async fn waterfall<E: WaterfallEvent>(
        &self,
        payload: E::Payload,
        default: impl FnOnce(E::Payload) -> BoxFuture< E::Output> + Send + 'static,
    ) -> E::Output {
        let key = TypeId::of::<E>();
        let slot = self.waterfall_slots.lock().unwrap().get(&key).cloned();
        let listeners: Vec<WaterfallFn<E::Payload, E::Output>> = match slot {
            None => Vec::new(),
            Some(s) => s
                .lock()
                .unwrap()
                .iter()
                .map(|(_, l)| {
                    l.downcast_ref::<WaterfallFn<E::Payload, E::Output>>()
                        .expect("waterfall listener type")
                        .clone()
                })
                .collect(),
        };
        descend::<E>(&listeners, 0, payload, Box::new(default)).await
    }

    // --- serial -------------------------------------------------------------

    pub fn on_serial<E: SerialEvent>(
        &self,
        f: impl Fn(E::Payload) -> BoxFuture<Option<E::Output>> + Send + Sync + 'static,
    ) -> Disposer {
        let token = self.token();
        let listener: SerialFn<E::Payload, E::Output> = Arc::new(f);
        let slot = Self::slot(&self.serial_slots, TypeId::of::<E>());
        slot.lock().unwrap().push((token, Box::new(listener)));
        Box::new(move || {
            slot.lock().unwrap().retain(|(t, _)| *t != token);
        })
    }

    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        let key = TypeId::of::<E>();
        let slot = self.serial_slots.lock().unwrap().get(&key).cloned()?;
        let listeners: Vec<SerialFn<E::Payload, E::Output>> = slot
            .lock()
            .unwrap()
            .iter()
            .map(|(_, l)| {
                l.downcast_ref::<SerialFn<E::Payload, E::Output>>()
                    .expect("serial listener type")
                    .clone()
            })
            .collect();
        for l in listeners {
            if let Some(out) = l(payload.clone()).await {
                return Some(out);
            }
        }
        None
    }
}

/// Build the innermost-first `next` continuation for a waterfall chain.
fn descend<E: WaterfallEvent>(
    listeners: &[WaterfallFn<E::Payload, E::Output>],
    idx: usize,
    payload: E::Payload,
    default: Box<dyn FnOnce(E::Payload) -> BoxFuture<E::Output> + Send>,
) -> BoxFuture<E::Output> {
    if let Some(l) = listeners.get(idx) {
        let next_payload = payload.clone();
        let next = descend::<E>(listeners, idx + 1, next_payload, default);
        l(payload, next)
    } else {
        default(payload)
    }
}