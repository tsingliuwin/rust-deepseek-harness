//! dsh-session-projection — 声明式会话投影单元注册表。
//!
//! 对齐 [`packages/session/session-projection`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/session/session-projection)：
//! 每个单元用 `{ key, stateVersion, init, apply }` 声明「如何把事件日志折叠成
//! 类型化状态」，注册表按 key 归拢（同 key 注册计数，最后一个 disposer 撤销
//! 后 key 才消失），`state_of` 对会话日志增量折叠出当前状态。
//!
//! 0.1.2-alpha.2 起 agent-loop（turnBoundary）、llm-retry（llmRetry）等把
//! 「从日志派生状态」统一收敛到这里：读取 O(新增事件)，不再每次 `findLast`
//! 扫全量日志。
//!
//! 骨架简化：状态经 `serde_json::Value` 擦除存储（上游用 zod schema 在边界
//! 校验）；基线缓存按会话对象地址 + 已折叠加载进度失效（上游用 WeakMap 按
//! 会话身份持有），同 id 整体重载的会话走 [`SessionProjections::forget_session`]
//! 兜底。

pub use dsh_cordis::Disposer;
use dsh_session::{Session, SessionEntry};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// 擦除类型后的投影单元。
struct Unit {
    init: Box<dyn Fn() -> Value + Send + Sync>,
    apply: Box<dyn Fn(Value, &SessionEntry) -> Value + Send + Sync>,
}

/// 类型化投影定义：`init` 给出初态，`apply` 把一个事件折叠进状态。
///
/// `state_version` 标记状态形版：换版注册会丢弃旧基线（上游同一契约）。
pub struct ProjectionDefinition<S: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> {
    pub key: &'static str,
    pub state_version: u32,
    pub init: fn() -> S,
    pub apply: fn(S, &SessionEntry) -> S,
}

impl<S: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> ProjectionDefinition<S> {
    fn erase(self) -> (&'static str, u32, Arc<Unit>) {
        let init = (self.init)();
        let to_value = |s: &S| serde_json::to_value(s).unwrap_or(Value::Null);
        let unit = Arc::new(Unit {
            init: Box::new(move || to_value(&init)),
            apply: Box::new(move |state, entry| {
                // 状态只经本单元自己的 to/from 往返，失败仅可能来自换版脏
                // 基线——回退初态重折（正确性由 init 定义保证）。
                let typed: S = serde_json::from_value(state).unwrap_or_else(|_| (self.init)());
                to_value(&(self.apply)(typed, entry))
            }),
        });
        (self.key, self.state_version, unit)
    }
}

/// 一份已折叠基线：已应用的事件数 + 当前状态。
struct Baseline {
    applied: usize,
    state: Value,
}

/// 投影注册表：按 key 归拢单元（注册计数），并提供按会话增量折叠的读取。
#[derive(Default)]
pub struct SessionProjections {
    /// key → (注册计数, 单元, state_version)。
    units: RwLock<HashMap<&'static str, (usize, Arc<Unit>, u32)>>,
    /// (会话对象地址, key) → 基线（增量折叠进度）。
    baselines: Mutex<HashMap<(usize, String), Baseline>>,
}

impl SessionProjections {
    /// 注册一个投影单元并返回撤销一半注册的 disposer（同 key 计数，最后
    /// 一个撤销才移除 key——上游同一契约：同 key 的 N 个挂载共享一个单元）。
    pub fn register<S: Serialize + DeserializeOwned + Clone + Send + Sync + 'static>(
        self: &Arc<Self>,
        definition: ProjectionDefinition<S>,
    ) -> Disposer {
        let (key, version, unit) = definition.erase();
        {
            let mut units = self.units.write().unwrap();
            let entry = units.entry(key).or_insert((0, Arc::clone(&unit), version));
            entry.0 += 1;
            // 换版重挂载：旧基线全部作废（fold 语义已变）。
            if entry.2 != version {
                entry.2 = version;
                self.drop_baselines(key);
            }
        }
        let registry = Arc::clone(self);
        Box::new(move || {
            let mut units = registry.units.write().unwrap();
            if let Some(entry) = units.get_mut(key) {
                entry.0 -= 1;
                if entry.0 == 0 {
                    units.remove(key);
                    registry.drop_baselines(key);
                }
            }
        })
    }

    /// 读取一个会话在 key 上的当前投影状态；key 未注册时返回 `None`
    /// （能力缺席语义：读方按「无该投影」处理而非报错）。
    pub fn state_of(&self, session: &Session, key: &str) -> Option<Value> {
        let unit = {
            let units = self.units.read().unwrap();
            Arc::clone(&units.get(key)?.1)
        };
        let entries = session.entries();
        let baseline_key = (session as *const Session as usize, key.to_string());
        let mut baselines = self.baselines.lock().unwrap();
        let prior = baselines.get(&baseline_key).filter(|b| b.applied <= entries.len());
        let mut state = match prior {
            Some(b) => b.state.clone(),
            None => (unit.init)(),
        };
        let start = prior.map(|b| b.applied).unwrap_or(0);
        for entry in &entries[start..] {
            state = (unit.apply)(state, entry);
        }
        baselines.insert(baseline_key, Baseline { applied: entries.len(), state: state.clone() });
        Some(state)
    }

    /// 丢弃一个会话的全部基线（会话被整体替换/重载时调用；骨架兜底：
    /// 换会话的对象地址不同自然错开，同地址复用才需要显式清理）。
    pub fn forget_session(&self, session: &Session) {
        let addr = session as *const Session as usize;
        self.baselines.lock().unwrap().retain(|(a, _), _| *a != addr);
    }

    fn drop_baselines(&self, key: &str) {
        self.baselines.lock().unwrap().retain(|(_, k), _| k != key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::SessionId;
    use dsh_session::SessionEvent;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
    struct CountState {
        turns: u64,
        #[serde(rename = "lastTurn")]
        last_turn: u64,
    }

    fn count_def(key: &'static str) -> ProjectionDefinition<CountState> {
        ProjectionDefinition {
            key,
            state_version: 1,
            init: CountState::default,
            apply: |state, entry| {
                let mut next = state.clone();
                if let SessionEvent::TurnStart { turn } = &entry.event {
                    next.turns += 1;
                    next.last_turn = *turn;
                }
                next
            },
        }
    }

    #[test]
    fn fold_incrementally_and_count_registrations() {
        let registry = Arc::new(SessionProjections::default());
        let d1 = registry.register(count_def("count"));
        let d2 = registry.register(count_def("count"));

        let mut s = Session::new(SessionId("p".into()));
        // 已注册、零事件：返回初态（缺席只发生在 key 未注册时）。
        let state: CountState =
            serde_json::from_value(registry.state_of(&s, "count").unwrap()).unwrap();
        assert_eq!(state, CountState::default());
        s.append(SessionEvent::TurnStart { turn: 1 });
        s.append(SessionEvent::TurnStart { turn: 2 });
        let state: CountState =
            serde_json::from_value(registry.state_of(&s, "count").unwrap()).unwrap();
        assert_eq!((state.turns, state.last_turn), (2, 2));

        // 增量：再追加一个事件只折新增部分。
        s.append(SessionEvent::TurnStart { turn: 3 });
        let state: CountState =
            serde_json::from_value(registry.state_of(&s, "count").unwrap()).unwrap();
        assert_eq!((state.turns, state.last_turn), (3, 3));

        // 注册计数：撤销一个仍可读，撤销全部后 key 消失（基线同步清理）。
        d1();
        assert!(registry.state_of(&s, "count").is_some());
        d2();
        assert_eq!(registry.state_of(&s, "count"), None);
    }
}
