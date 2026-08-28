//! Built-in retry policy for `agent/request-error`.
//!
//! Mirrors the reference's `dsh-llm-retry` role: a listener on the
//! `agent/request-error` waterfall that, when the failure is retryable and
//! attempts remain, sleeps the resolved backoff delay and returns
//! [`RequestErrorAction::Retry`]. Otherwise it delegates via `next()` so the
//! loop fails the turn.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dsh_agent::{AgentRequestError, AgentTurnStopping, RequestErrorAction};
use dsh_cordis::{Disposer, EventBus};

/// Install the retry listener and return a disposer that withdraws it.
///
/// Attempts are tracked per `(turn, step)` and cleared when the turn stops.
pub fn attach_retry(events: &EventBus) -> Disposer {
    let attempts: Arc<Mutex<HashMap<(u64, u64), u32>>> = Arc::new(Mutex::new(HashMap::new()));

    let a_req = Arc::clone(&attempts);
    let request_disposer = events.on_waterfall::<AgentRequestError>(move |payload, next| {
        let attempts = Arc::clone(&a_req);
        Box::pin(async move {
            let attempt = {
                let mut map = attempts.lock().unwrap();
                let e = map.entry((payload.turn, payload.step)).or_insert(0);
                *e += 1;
                *e
            };

            let retryable = payload.retry_policy.is_retryable(&payload.failure.code);
            let within_cap = payload
                .retry_policy
                .max_retries()
                .is_none_or(|max| attempt <= max);

            if retryable && within_cap && !payload.signal.aborted() {
                tokio::time::sleep(payload.retry_policy.delay_for(attempt - 1)).await;
                return Some(RequestErrorAction::Retry);
            }

            next.await
        })
    });

    let a_ts = Arc::clone(&attempts);
    let turn_disposer = events.on_serial::<AgentTurnStopping>(move |payload| {
        let attempts = Arc::clone(&a_ts);
        Box::pin(async move {
            attempts.lock().unwrap().retain(|(turn, _), _| *turn != payload.turn);
            None
        })
    });

    Box::new(move || {
        request_disposer();
        turn_disposer();
    })
}