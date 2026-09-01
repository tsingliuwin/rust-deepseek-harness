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
//!
//! 0.1.2-alpha.3 起 registry 带 wire 视图与变更流：`wire.view` 是「状态 → 客户端
//! 载荷」的读侧投影；变更通知以 raw view 结果是否变化为门（上游用 `Object.is`
//! 比较，契约是 view 对内部态变化复用旧引用；Rust 侧对应物是结构相等比较——
//! 对确定性 view 两者外部行为一致）。turnOutline 这类单元靠「draft-only apply
//! 保持 turns 数组不变」让变更流在轮内保持安静。

pub use dsh_cordis::Disposer;
use dsh_session::{Session, SessionEntry};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

pub mod turn_outline;

/// 变更流监听者：一个单元的 raw view 结果在一次推进后发生了变化。
/// `value` 是 wire 视图输出；`seq` 是引发该变化的最后一个事件序号。
pub type ProjectionChangeListener =
    Arc<dyn Fn(&Session, &str, &Value, u64) + Send + Sync>;

/// 擦除类型后的投影单元。
struct Unit {
    init: Box<dyn Fn() -> Value + Send + Sync>,
    apply: Box<dyn Fn(Value, &SessionEntry) -> Value + Send + Sync>,
    /// wire 视图（`None` = 无客户端面，变更流永不发布该单元）。
    view: Option<Box<dyn Fn(&Value) -> Value + Send + Sync>>,
}

/// 类型化投影定义：`init` 给出初态，`apply` 把一个事件折叠进状态；
/// `view` 是可选的 wire 视图（状态 → 客户端载荷）。
///
/// `state_version` 标记状态形版：换版注册会丢弃旧基线（上游同一契约）。
pub struct ProjectionDefinition<S: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> {
    pub key: &'static str,
    pub state_version: u32,
    pub init: fn() -> S,
    pub apply: fn(S, &SessionEntry) -> S,
    /// wire 视图：只读投影（上游 `wire.view`）。`None` 表示该单元没有
    /// 客户端面——状态可读（[`SessionProjections::state_of`]），但变更流
    /// 永不为它发布。
    pub view: Option<fn(&S) -> Value>,
}

impl<S: Serialize + DeserializeOwned + Clone + Send + Sync + 'static> ProjectionDefinition<S> {
    fn erase(self) -> (&'static str, u32, Arc<Unit>) {
        let init = (self.init)();
        let to_value = |s: &S| serde_json::to_value(s).unwrap_or(Value::Null);
        let view = self.view.map(|f| {
            Box::new(move |state: &Value| -> Value {
                let typed: S = serde_json::from_value(state.clone()).unwrap_or_else(|_| (self.init)());
                f(&typed)
            }) as Box<dyn Fn(&Value) -> Value + Send + Sync>
        });
        let unit = Arc::new(Unit {
            init: Box::new(move || to_value(&init)),
            apply: Box::new(move |state, entry| {
                // 状态只经本单元自己的 to/from 往返，失败仅可能来自换版脏
                // 基线——回退初态重折（正确性由 init 定义保证）。
                let typed: S = serde_json::from_value(state).unwrap_or_else(|_| (self.init)());
                to_value(&(self.apply)(typed, entry))
            }),
            view,
        });
        (self.key, self.state_version, unit)
    }
}

/// 一份已折叠基线：已应用的事件数 + 当前状态 + 双槽 raw view 缓存。
struct Baseline {
    applied: usize,
    state: Value,
    /// `[previous_view, current_view]`：raw view 的比较缓存（web drive 的
    /// `cell.views` 对应物；`None` 槽表示尚无可比值）。
    views: [Option<Arc<Value>>; 2],
}

/// 投影注册表：按 key 归拢单元（注册计数），并提供按会话增量折叠的读取、
/// wire 视图读取与身份门控变更流。
#[derive(Default)]
pub struct SessionProjections {
    /// key → (注册计数, 单元, state_version)。
    units: RwLock<HashMap<&'static str, (usize, Arc<Unit>, u32)>>,
    /// (会话对象地址, key) → 基线（增量折叠进度）。
    baselines: Mutex<HashMap<(usize, String), Baseline>>,
    /// 变更流监听者（[`SessionProjections::on_changed`] 注册）。
    listeners: RwLock<Vec<ProjectionChangeListener>>,
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
        let (unit, mut baseline, baseline_key) = self.cell(session, key)?;
        let entries = session.entries();
        let mut state = baseline.state;
        for entry in &entries[baseline.applied..] {
            state = (unit.apply)(state, entry);
        }
        baseline.applied = entries.len();
        baseline.state = state.clone();
        self.baselines.lock().unwrap().insert(baseline_key, baseline);
        Some(state)
    }

    /// 读取 wire 视图（`wire.view` 的当前输出，按 raw 结果缓存）。
    ///
    /// 与 [`SessionProjections::state_of`] 的差别：走的是读侧投影面。raw
    /// 结果经双槽缓存比较——与上次发布相同时返回同一个 `Arc`（调用方可凭
    /// 指针同一性免重建下游派生，web「view 复用引用压制发布」的 pull 对应）。
    /// key 未注册或单元没有 wire 面时返回 `None`。
    pub fn view_of(&self, session: &Session, key: &str) -> Option<Arc<Value>> {
        let (unit, mut baseline, baseline_key) = self.cell(session, key)?;
        let view = unit.view.as_ref()?;
        let entries = session.entries();
        let mut state = baseline.state;
        for entry in &entries[baseline.applied..] {
            state = (unit.apply)(state, entry);
        }
        baseline.applied = entries.len();
        baseline.state = state.clone();
        let raw = Arc::new(view(&baseline.state));
        // 双槽门：与当前槽相等 → 结果未变，保留旧 Arc；否则前移一格。
        if baseline.views[1].as_ref().is_some_and(|prev| **prev == *raw) {
            let cached = baseline.views[1].clone().unwrap();
            self.baselines.lock().unwrap().insert(baseline_key, baseline);
            return Some(cached);
        }
        baseline.views[0] = baseline.views[1].take();
        baseline.views[1] = Some(Arc::clone(&raw));
        self.baselines.lock().unwrap().insert(baseline_key, baseline);
        Some(raw)
    }

    /// 订阅变更流（上游 `onChanged`）。监听者经 [`SessionProjections::drive`]
    /// 推进时，对每个 raw view 变化了的 wire 单元各收到一次回调。
    /// 返回撤销柄。
    pub fn on_changed(self: &Arc<Self>, listener: ProjectionChangeListener) -> Disposer {
        let mut listeners = self.listeners.write().unwrap();
        let index = listeners.len();
        listeners.push(listener);
        drop(listeners);
        let registry = Arc::clone(self);
        Box::new(move || {
            registry.listeners.write().unwrap().remove(index);
        })
    }

    /// 推进一个会话的全部单元到日志末尾，逐事件对 raw view 变化了的 wire
    /// 单元发布变更（上游 eager drive 的显式版：rustdsh 的事件在会话锁内
    /// 落盘，registry 由调用方在事件后驱动；推拉的间隔里被 `state_of` 推进
    /// 过的事件不补发中间态——与读方共享同一基线）。
    pub fn drive(&self, session: &Session) {
        let listeners = self.listeners.read().unwrap().clone();
        // 先摘 key 再逐个推进：cell() 内部要再取 units 读锁，持锁重入
        // 会与等待中的写锁互相卡死。
        let keys: Vec<&'static str> = self.units.read().unwrap().keys().copied().collect();
        for key in keys {
            let (unit, mut baseline, baseline_key) = match self.cell(session, key) {
                Some(cell) => cell,
                None => continue,
            };
            let entries = session.entries();
            while baseline.applied < entries.len() {
                let event = &entries[baseline.applied];
                let previous = baseline.state.clone();
                let next = (unit.apply)(previous.clone(), event);
                let changed = next != previous;
                baseline.state = next;
                baseline.applied += 1;
                if !changed || listeners.is_empty() {
                    continue;
                }
                let Some(view) = unit.view.as_ref() else {
                    continue;
                };
                let raw = Arc::new(view(&baseline.state));
                // 双槽门：raw view 与上个发布相同（view 对内部态变化复用旧
                // 值）→ 不发布。
                if baseline.views[1].as_ref().is_some_and(|prev| **prev == *raw) {
                    continue;
                }
                baseline.views[0] = baseline.views[1].take();
                baseline.views[1] = Some(Arc::clone(&raw));
                for listener in &listeners {
                    listener(session, key, &raw, event.seq);
                }
            }
            self.baselines.lock().unwrap().insert(baseline_key, baseline);
        }
    }

    /// 取（或惰性建）一个会话在 key 上的基线，连同其单元。
    fn cell(
        &self,
        session: &Session,
        key: &str,
    ) -> Option<(Arc<Unit>, Baseline, (usize, String))> {
        let unit = {
            let units = self.units.read().unwrap();
            Arc::clone(&units.get(key)?.1)
        };
        let entries = session.entries();
        let baseline_key = (session as *const Session as usize, key.to_string());
        let baselines = self.baselines.lock().unwrap();
        let prior = baselines.get(&baseline_key).filter(|b| b.applied <= entries.len());
        let baseline = match prior {
            Some(b) => Baseline {
                applied: b.applied,
                state: b.state.clone(),
                views: [b.views[0].clone(), b.views[1].clone()],
            },
            None => Baseline { applied: 0, state: (unit.init)(), views: [None, None] },
        };
        Some((unit, baseline, baseline_key))
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
            view: None,
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
