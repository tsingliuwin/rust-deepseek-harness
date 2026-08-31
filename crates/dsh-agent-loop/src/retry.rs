//! 内置重试执行器（`agent/request-error` 的 waterfall 监听者）。
//!
//! 对齐 0.1.2-alpha.2 的 `dsh-llm-retry`：重试进度不再放在监听者的内存
//! 里，而是持久进会话日志——每段重试链以 [`SessionEvent::LlmRetry`]（重试
//! 等待排定前）与 [`SessionEvent::LlmRetryStarted`]（等待结束、下一次请求
//! 尝试开始前）记录，计数与 retryId 从 `llmRetry` 投影读取（按
//! `[provider, policyKey]` 分桶，`step/start` 与 `turn/end` 清零）。会话
//! 崩溃/恢复后重试状态不丢也不重放，retryId 在同一段链内稳定。
//!
//! 延迟：provider 的 `retry-after`（`provider_retry_after_ms`）优先，封顶
//! 策略 `max_delay_ms`；否则按策略的指数退避。

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use dsh_agent::{AgentRequestError, RequestErrorAction};
use dsh_cordis::{Disposer, EventBus};
use dsh_llm::ResolvedRetryPolicy;
use dsh_session::{SessionEntry, SessionEvent};
use dsh_session_projection::{ProjectionDefinition, SessionProjections};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ReactLoopAgent;

/// 一次重试链的稳定标识（同一段链复用首个 id；web `RetryId` 品牌的承载）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetryId(pub String);

/// `llmRetry` 投影的单条状态：当前步某 provider+policy 的重试进度。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryStateEntry {
    retry: u32,
    retry_id: String,
}

/// 投影状态：key 为 `JSON.stringify([provider, policyKey])`（上游同形）。
type LlmRetryState = HashMap<String, RetryStateEntry>;

/// 策略的稳定 key：解析后策略的全字段 JSON（上游 `retryPolicyKey` 同义——
/// 字段集合一致即同一段重试链）。
fn retry_policy_key(policy: &ResolvedRetryPolicy) -> String {
    match policy {
        ResolvedRetryPolicy::Normal {
            max_retries,
            retryable_codes,
            initial_delay_ms,
            max_delay_ms,
            jitter_ratio,
        } => json!({
            "mode": "normal",
            "maxRetries": max_retries,
            "retryableCodes": retryable_codes,
            "initialDelayMs": initial_delay_ms,
            "maxDelayMs": max_delay_ms,
            "jitterRatio": jitter_ratio,
        })
        .to_string(),
        ResolvedRetryPolicy::Always { initial_delay_ms, max_delay_ms, jitter_ratio } => json!({
            "mode": "always",
            "initialDelayMs": initial_delay_ms,
            "maxDelayMs": max_delay_ms,
            "jitterRatio": jitter_ratio,
        })
        .to_string(),
    }
}

/// 投影分桶 key：`JSON.stringify([provider, policyKey])`。
fn retry_state_key(provider: &str, policy_key: &str) -> String {
    json!([provider, policy_key]).to_string()
}

/// `llmRetry` 投影定义（上游 stateVersion = 1）：`step/start` 与
/// `turn/end` 清空整个状态——重试计数天然按步归零，无需外部清理。
fn llm_retry_projection_definition() -> ProjectionDefinition<LlmRetryState> {
    ProjectionDefinition {
        key: "llmRetry",
        state_version: 1,
        init: HashMap::new,
        apply: |state, entry: &SessionEntry| {
            match &entry.event {
                SessionEvent::StepStart { .. } | SessionEvent::TurnEnd { .. } => HashMap::new(),
                SessionEvent::LlmRetry { provider, policy_key, retry, retry_id, .. } => {
                    let mut next = state.clone();
                    let key = retry_state_key(provider, policy_key);
                    let changed = !matches!(next.get(&key),
                        Some(e) if e.retry == *retry && e.retry_id == *retry_id);
                    if changed {
                        next.insert(
                            key,
                            RetryStateEntry { retry: *retry, retry_id: retry_id.clone() },
                        );
                    }
                    next
                }
                _ => state,
            }
        },
    }
}

/// 安装重试执行器，返回撤销柄（连同 `llmRetry` 投影注册一起撤销）。
///
/// `agent_slot` 在 agent 构造完成后由宿主填入（web：监听者从 payload 的
/// `agent` 取会话；骨架里事件不携带 agent，由槽间接线）。
pub fn attach_retry(
    events: &EventBus,
    projections: Arc<SessionProjections>,
    agent_slot: Arc<OnceLock<Arc<ReactLoopAgent>>>,
) -> Disposer {
    let registration = projections.register(llm_retry_projection_definition());

    let request_disposer = events.on_waterfall::<AgentRequestError>(move |payload, next| {
        let projections = Arc::clone(&projections);
        let agent_slot = Arc::clone(&agent_slot);
        Box::pin(async move {
            let Some(agent) = agent_slot.get() else { return next.await };
            let policy = &payload.retry_policy;
            if payload.signal.aborted() {
                return next.await;
            }
            if !policy.is_retryable(&payload.failure.code) {
                return next.await;
            }

            let policy_key = retry_policy_key(policy);
            let state: LlmRetryState = {
                let session = agent.session();
                let previous = projections.state_of(&session.lock().unwrap(), "llmRetry");
                previous
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default()
            };
            let previous = state.get(&retry_state_key(&payload.provider, &policy_key));
            let previous_retry = previous.map(|e| e.retry).unwrap_or(0);
            let max_retries = policy.max_retries();
            if let Some(cap) = max_retries
                && previous_retry >= cap
            {
                return next.await;
            }
            let retry = previous_retry + 1;
            let retry_id = previous
                .map(|e| e.retry_id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            // provider retry-after 优先，封顶策略 max_delay_ms；否则指数退避。
            let max_delay_ms = match policy {
                ResolvedRetryPolicy::Normal { max_delay_ms, .. }
                | ResolvedRetryPolicy::Always { max_delay_ms, .. } => *max_delay_ms,
            };
            let delay_ms = match payload.failure.provider_retry_after_ms {
                Some(after) if after > 0 => after.min(max_delay_ms),
                _ => policy.delay_for(retry - 1).as_millis() as u64,
            };

            let (mode, max_retries_field) = match policy {
                ResolvedRetryPolicy::Normal { max_retries, .. } => ("normal", Some(*max_retries)),
                ResolvedRetryPolicy::Always { .. } => ("always", None),
            };
            agent.append_session_event(SessionEvent::LlmRetry {
                retry_id: retry_id.clone(),
                turn: payload.turn,
                step: payload.step,
                provider: payload.provider.clone(),
                mode: mode.into(),
                policy_key: policy_key.clone(),
                retry,
                max_retries: max_retries_field,
                delay_ms,
                failure: payload.failure.clone(),
            });

            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            if payload.signal.aborted() {
                // 中止即放弃本次重试：不写 retry-started，交回下游。
                return next.await;
            }
            agent.append_session_event(SessionEvent::LlmRetryStarted {
                retry_id,
                turn: payload.turn,
                step: payload.step,
                retry,
            });
            Some(RequestErrorAction::Retry)
        })
    });

    Box::new(move || {
        registration();
        request_disposer();
    })
}
