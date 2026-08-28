//! The adapter contract and the adapter registry (`LlmRuntime`). Mirrors the
//! `LlmAdapter` seam and `LlmRuntime` of the reference: subclass-equivalents
//! implement `stream()` and register one instance for a set of provider routes;
//! `GenerateOptions.provider` selects the serving adapter.

use crate::error::LlmError;
use crate::events::{LlmAdaptersUpdated, LlmStream};
use crate::types::{
    AbortSignal, GenerateOptions, LlmModelInfo, LlmProviderInfo, LlmResolvedModelInfo,
    ResolvedRetryPolicy, StreamChunk,
};
use async_trait::async_trait;
use dsh_cordis::EventBus;
use futures::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

/// A boxed chunk stream, the return type of one streaming model call.
pub type BoxStream = Pin<Box<dyn Stream<Item = StreamChunk> + Send>>;

/// A one-shot unregistration; mirrors the disposer `registerAdapter` returns.
pub type Disposer = Box<dyn FnOnce() + Send>;

/// Provider-wire adapter for the harness message and stream vocabulary.
///
/// Every provider HTTP request must include the app-attribution `User-Agent`
/// header; adapters honor `options.signal` between stream items.
#[async_trait]
pub trait LlmAdapter: Send + Sync {
    /// Describe one provider route owned by this adapter.
    fn provider_info(&self, provider: &str) -> LlmProviderInfo;

    /// Return the provider-owned retry policy, or `None` for normal defaults.
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        None
    }

    /// List models this adapter can currently advertise for one owned provider.
    /// Advisory: an adapter may accept unlisted model ids.
    async fn list_models(&self, _provider: &str) -> Vec<LlmModelInfo> {
        vec![]
    }

    /// Resolve all metadata available for one exact model.
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<AbortSignal>,
    ) -> LlmResolvedModelInfo {
        LlmResolvedModelInfo {
            provider: provider.to_string(),
            id: model.to_string(),
            name: model.to_string(),
            context: None,
            default_max_tokens: None,
        }
    }

    /// Stream one model call as raw chunks. The only required method.
    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError>;
}

/// An adapter registry plus a streaming model-call API.
pub struct LlmRuntime {
    adapters: Arc<RwLock<HashMap<String, Arc<dyn LlmAdapter>>>>,
    order: Arc<RwLock<Vec<String>>>,
    events: EventBus,
}

impl LlmRuntime {
    pub fn new() -> Self {
        Self::with_events(EventBus::new())
    }

    pub fn with_events(events: EventBus) -> Self {
        Self {
            adapters: Arc::new(RwLock::new(HashMap::new())),
            order: Arc::new(RwLock::new(Vec::new())),
            events,
        }
    }

    /// The event bus this runtime dispatches its seams on.
    pub fn events(&self) -> &EventBus {
        &self.events
    }

    /// Register an adapter for the given provider routes, all-or-nothing.
    pub fn register_adapter(
        &self,
        providers: &[String],
        adapter: Arc<dyn LlmAdapter>,
    ) -> Result<Disposer, LlmError> {
        {
            let map = self.adapters.read().unwrap();
            for p in providers {
                if map.contains_key(p.as_str()) {
                    return Err(LlmError::duplicate_adapter(p));
                }
            }
        }
        {
            let mut map = self.adapters.write().unwrap();
            for p in providers {
                map.insert(p.clone(), adapter.clone());
            }
        }
        {
            let mut order = self.order.write().unwrap();
            for p in providers {
                order.push(p.clone());
            }
        }

        let providers = providers.to_vec();
        let adapters = Arc::clone(&self.adapters);
        let order = Arc::clone(&self.order);
        let events = self.events.clone();
        events.emit::<LlmAdaptersUpdated>(());
        Ok(Box::new(move || {
            let mut map = adapters.write().unwrap();
            for p in &providers {
                map.remove(p.as_str());
            }
            let mut ord = order.write().unwrap();
            ord.retain(|p| !providers.contains(p));
            events.emit::<LlmAdaptersUpdated>(());
        }))
    }

    /// Describe provider routes with a registered adapter, in registration order.
    pub fn list_providers(&self) -> Vec<LlmProviderInfo> {
        let order = self.order.read().unwrap();
        let map = self.adapters.read().unwrap();
        order
            .iter()
            .filter_map(|p| map.get(p.as_str()).map(|a| a.provider_info(p)))
            .collect()
    }

    /// Resolve the adapter serving one provider route.
    pub fn get_adapter(&self, provider: &str) -> Option<Arc<dyn LlmAdapter>> {
        self.adapters.read().unwrap().get(provider).cloned()
    }

    /// Resolve the retry policy captured with one provider route.
    pub fn provider_retry_policy(&self, provider: &str) -> ResolvedRetryPolicy {
        self.get_adapter(provider)
            .and_then(|a| a.provider_retry_policy(provider))
            .unwrap_or_default()
    }

    /// Stream one model call. This is the `llm/stream` waterfall: `next()` reaches
/// the resolved adapter; a listener may wrap or short-circuit the stream.
    pub async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let adapters = Arc::clone(&self.adapters);
        self.events
            .waterfall::<LlmStream>(options, move |options| {
                let provider = options.provider.clone();
                Box::pin(async move {
                    let adapter = adapters
                        .read()
                        .unwrap()
                        .get(&provider)
                        .cloned()
                        .ok_or_else(|| LlmError::no_adapter(&provider))?;
                    adapter.stream(options).await
                })
            })
            .await
    }
}

impl Default for LlmRuntime {
    fn default() -> Self {
        Self::new()
    }
}