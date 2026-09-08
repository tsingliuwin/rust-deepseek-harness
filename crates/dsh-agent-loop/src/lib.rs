//! dsh-agent-loop — the default agent driver.
//!
//! Mirrors [`packages/core/agent-loop`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/core/agent-loop):
//! the `ReactLoopAgent` state machine that drives one session through turn and
//! step boundaries, deriving every model request from the session log.
//!
//! Milestone-2 wiring: live `agent/*` extension points now dispatch through a
//! shared [`EventBus`] — `agent/pre-step` (waterfall), `agent/request`
//! (waterfall), `agent/request-error` (waterfall, retry), `agent/turn-stopping`
//! (serial), and the `agent/status`, `agent/error`, `agent/inbox/*` emits.
//! Remaining simplifications: no backoff behind `agent/request-error` retry,
//! and sequential (rather than pooled-parallel) tool dispatch.

use dsh_agent::{
    AgentErrorOccurred, AgentErrorPayload, AgentInboxClaimed, AgentInboxDiscarded,
    AgentInboxInserted, AgentPreStep, AgentRequest, AgentRequestError, AgentRequestPayload,
    AgentStatus, AgentStatusChanged, AgentTurnStopping, Inbox, InboxClaimedPayload,
    PreStepDecision, PreStepInput, RequestErrorAction, RequestErrorPayload, TurnStoppingPayload,
};
pub use dsh_agent::InboxTarget;
use dsh_cordis::EventBus;
use dsh_llm::{
    AbortSignal, BlockAssembler, CallId, ContentBlock, FinishReason, GenerateOptions, LlmCallConfig,
    LlmRuntime, Message, MessageSource, Role, SessionId, StreamChunk,
};
use dsh_session::SessionEntry;
use dsh_session::{EpochHeader, HeaderReason, Session, SessionEvent, TurnEndReason};
use dsh_session_projection::{Disposer, ProjectionDefinition, SessionProjections};
use dsh_system_prompt::{PromptAssembly, SystemPrompt};
use dsh_tools::{ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use flume::{Receiver, Sender};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::Notify;

pub mod retry;

/// 一步的边界事实（`step/start` 或 `step/end` 及其 seq）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepBoundary {
    pub kind: StepBoundaryKind,
    pub seq: u64,
}

/// 边界种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepBoundaryKind {
    Start,
    End,
}

/// 从单个 agent 会话日志折叠出的轮次/步边界事实（web
/// `TurnBoundaryProjection`，由 dsh-agent-loop 注册 `turnBoundary` 单元）。
///
/// 读方契约：key 由 agent-loop 注册，缺席即「无打开轮次 / 无边界」的能力
/// 缺席而非损坏状态；对缺席无安全回退的读方（如 step-open 判定）可以
/// 显式报错（上游同一注释）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnBoundaryProjection {
    /// 打开轮次的 `turn/start` seq；轮次之间为 null。
    pub open_turn_start_seq: Option<u64>,
    /// 最近一次 `step/start` 的 seq；首步前为 null。
    pub last_step_start_seq: Option<u64>,
    /// 最近的步边界（start 或 end）及其 seq。
    pub last_step_boundary: Option<StepBoundary>,
    /// 最近 `turn/start` 的轮次号；首轮前为 0。
    pub last_turn: u64,
}

/// `turnBoundary` 投影定义（stateVersion 与上游一致为 2；注册放在全部
/// config 校验之后是上游注释的语义——被拒绝的构造不留下投影单元）。
pub fn turn_boundary_projection_definition() -> ProjectionDefinition<TurnBoundaryProjection> {
    ProjectionDefinition {
        key: "turnBoundary",
        state_version: 2,
        init: TurnBoundaryProjection::default,
        apply: |state, entry: &SessionEntry| {
            let mut next = state.clone();
            match &entry.event {
                SessionEvent::TurnStart { turn } => {
                    next.open_turn_start_seq = Some(entry.seq);
                    next.last_turn = *turn;
                }
                SessionEvent::TurnEnd { .. } => {
                    next.open_turn_start_seq = None;
                }
                SessionEvent::StepStart { .. } => {
                    next.last_step_start_seq = Some(entry.seq);
                    next.last_step_boundary =
                        Some(StepBoundary { kind: StepBoundaryKind::Start, seq: entry.seq });
                }
                SessionEvent::StepEnd { .. } => {
                    next.last_step_boundary =
                        Some(StepBoundary { kind: StepBoundaryKind::End, seq: entry.seq });
                }
                _ => {}
            }
            next
        },
        // turnBoundary 无客户端 wire 面（宿主内消费）。
        view: None,
    }
}

/// Static configuration for one agent.
#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    /// Base system instructions, rendered ahead of registered prompt sections.
    pub system_prompt: Option<String>,
    /// 上下文压缩配置（threshold_tokens = 0 禁用；默认 60k/6 条）。
    pub compaction: dsh_compaction::CompactionConfig,
    /// 会话工作目录（web session.header.cwd → 模型可见的运行时上下文 +
    /// 工具执行基准；宿主切换工作区时更新同一句柄）。
    pub workdir: dsh_tools::Workdir,
    /// 附件存储根（`DSH_HOME/attachments/v1`）：请求组装把 FileBlock 投影
    /// 为 handle 文本时解析只读保存路径；None = 无可用路径（handle 落
    /// 「无法访问」分支）。
    pub attachments_root: Option<std::path::PathBuf>,
}

/// A live event emitted to UI/observers as the loop progresses.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted { turn: u64 },
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCall { tool_call_id: CallId, name: String, arguments: String },
    ToolResult { tool_call_id: CallId, is_error: bool },
    AssistantMessage { message: Message, usage: Option<dsh_llm::TokenUsage> },
    TurnEnded { turn: u64, reason: TurnEndReason },
    /// 压缩事务完成：影子区前 `shadowed_messages` 条消息已由检查点替换。
    Compacted { shadowed_messages: usize },
    Error { message: String, code: String },
}

/// Convenience for a client-submitted text prompt.
pub fn user_message(text: impl Into<String>) -> Message {
    Message::user_text(text)
}

/// 上游 routeLabel：与对侧 provider 相同则只显示 model，否则 `provider/model`。
fn route_label(provider: &str, model: &str, other_provider: &str) -> String {
    if provider == other_provider { model.to_string() } else { format!("{provider}/{model}") }
}

/// v3 system prompt 面节点消息（上游 createSystemMessage(text, SOURCE)）：
/// role system、固定 plugin source；空 prompt 落空 content（dormant head）。
fn system_prompt_message(rendered: &str) -> Message {
    let content = if rendered.is_empty() {
        Vec::new()
    } else {
        vec![ContentBlock::text(rendered)]
    };
    Message::new(
        Role::System,
        content,
        MessageSource::Plugin {
            plugin: "@deepseek-ai/dsh-system-prompt".into(),
            form: None,
            summary: None,
            sections: None,
        },
    )
}

/// 构造模型切换公告消息（上游 `modelSwitchNotice`）：user/plugin
/// `model-selection` 形态，notice form 携带摘要。
fn model_switch_notice_message(previous: (&str, &str), selected: (&str, &str)) -> Message {
    let from = route_label(previous.0, previous.1, selected.0);
    let to = route_label(selected.0, selected.1, previous.0);
    Message::user_plugin(
        vec![ContentBlock::text(format!(
            "[model changed: assistant turns above this point were generated by {from}; the session continues with {to}]",
        ))],
        "model-selection",
        Some("notice".into()),
        Some(dsh_llm::bound_context_summary(&format!("{from} → {to}"))),
        None,
    )
}

enum StepEnd {
    /// Tools owe another request: open another step.
    NeedsAnotherStep,
    /// The turn concluded with this reason.
    Concluded(TurnEndReason),
}

/// The default `Agent` implementation: a turn/step driver over an inbox.
/// 简单唯一 id（时间纳秒 hex；compactionId 这类不透明标识够用）。
fn uuid_simple() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:032x}")
}

pub struct ReactLoopAgent {
    options: Arc<RwLock<AgentOptions>>,
    session: Arc<Mutex<Session>>,
    inbox: Arc<Mutex<Inbox>>,
    llm: Arc<LlmRuntime>,
    tools: Arc<ToolRegistry>,
    prompt: Arc<SystemPrompt>,
    projections: Arc<SessionProjections>,
    events: EventBus,
    wake: Arc<Notify>,
    abort: Arc<Mutex<AbortSignal>>,
    status: Arc<Mutex<AgentStatus>>,
    ui_events: Mutex<Option<Sender<AgentEvent>>>,
    on_event: Mutex<Option<EventSink>>,
    /// `turnBoundary` 注册的撤销柄（注册随 agent 存活，上游 rode the fiber）。
    /// Mutex 使整体保持 Sync（Disposer 本身只是 Send）。
    _turn_boundary_registration: Mutex<Option<Disposer>>,
    /// v3 system prompt 面节点 head：(日志 seq, 当前文本)。None = 本会话
    /// 尚无 system/message 行（上游 SystemPromptProjection 的 head 跟踪）。
    system_head: Mutex<Option<(u64, String)>>,
}

/// Side-channel observer for every appended session event.
type EventSink = Box<dyn Fn(SessionEvent) + Send + Sync>;

impl ReactLoopAgent {
    pub fn new(
        id: SessionId,
        options: AgentOptions,
        llm: Arc<LlmRuntime>,
        tools: Arc<ToolRegistry>,
        prompt: Arc<SystemPrompt>,
        projections: Arc<SessionProjections>,
        events: EventBus,
    ) -> Arc<Self> {
        // 注册柄由 agent 持有：注册随 agent 存活，disposer 的强引用反过来
        // 保证注册表不会先于 agent 消失。
        let turn_boundary_registration = projections.register(turn_boundary_projection_definition());
        Arc::new(Self {
            options: Arc::new(RwLock::new(options)),
            session: Arc::new(Mutex::new(Session::new(id))),
            inbox: Arc::new(Mutex::new(Inbox::new())),
            llm,
            tools,
            prompt,
            projections,
            events,
            wake: Arc::new(Notify::new()),
            abort: Arc::new(Mutex::new(AbortSignal::new())),
            status: Arc::new(Mutex::new(AgentStatus::Idle)),
            ui_events: Mutex::new(None),
            on_event: Mutex::new(None),
            _turn_boundary_registration: Mutex::new(Some(turn_boundary_registration)),
            system_head: Mutex::new(None),
        })
    }

    /// 运行期热更新模型路由（设置面板保存时调用）。
    pub fn set_provider_and_model(&self, provider: impl Into<String>, model: impl Into<String>) {
        let mut o = self.options.write().unwrap();
        o.provider = provider.into();
        o.model = model.into();
    }

    pub fn session(&self) -> Arc<Mutex<Session>> {
        Arc::clone(&self.session)
    }

    /// Replace the agent's session (session switching). The inbox is cleared.
    /// The v3 system head is rebuilt from the log (last `system/message` row).
    pub fn set_session(&self, session: Session) {
        self.projections.forget_session(&self.session.lock().unwrap());
        let head = session.entries().iter().rev().find_map(|e| match &e.event {
            SessionEvent::SystemMessage { message, .. } => {
                let text = message
                    .content
                    .first()
                    .and_then(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((e.seq, text))
            }
            _ => None,
        });
        *self.system_head.lock().unwrap() = head;
        *self.session.lock().unwrap() = session;
        self.inbox.lock().unwrap().clear();
    }

    /// Append one event to the session log and fan it out to the sink
    /// (persistence). Public seam for log-writing plugins (upstream
    /// `agent.session.append(...)`): retry events ride this too.
    pub fn append_session_event(&self, event: SessionEvent) {
        self.append_event(event);
    }

    /// The projection registry this agent folds boundary facts into.
    pub fn projections(&self) -> Arc<SessionProjections> {
        Arc::clone(&self.projections)
    }

    /// Register a side-channel observer for every appended session event
    /// (e.g. JSONL persistence). Called with the session lock released.
    pub fn set_event_sink(&self, sink: impl Fn(SessionEvent) + Send + Sync + 'static) {
        *self.on_event.lock().unwrap() = Some(Box::new(sink));
    }

    /// Append one event to the session log and fan it out to the sink.
    /// Returns the log seq the event took (0 基致密).
    fn append_event(&self, event: SessionEvent) -> u64 {
        let seq = {
            let mut s = self.session.lock().unwrap();
            s.append(event.clone())
        };
        if let Some(sink) = self.on_event.lock().unwrap().as_ref() {
            sink(event);
        }
        seq
    }

    pub fn status(&self) -> AgentStatus {
        *self.status.lock().unwrap()
    }

    /// Subscribe to live UI events, returning the receiver half.
    pub fn subscribe(&self) -> Receiver<AgentEvent> {
        let (tx, rx) = flume::unbounded();
        *self.ui_events.lock().unwrap() = Some(tx);
        rx
    }

    fn emit_ui(&self, event: AgentEvent) {
        if let Some(tx) = self.ui_events.lock().unwrap().as_ref() {
            let _ = tx.send(event);
        }
    }

    fn set_status(&self, status: AgentStatus) {
        let changed = {
            let mut cur = self.status.lock().unwrap();
            if *cur == status {
                false
            } else {
                *cur = status;
                true
            }
        };
        if changed {
            self.events.emit::<AgentStatusChanged>(status);
        }
    }

    /// Queue a message and wake the driver.
    pub fn send(&self, message: Message, target: InboxTarget) {
        self.inbox.lock().unwrap().append(target, message.clone());
        self.events.emit::<AgentInboxInserted>(message);
        self.wake.notify_one();
    }

    /// Queue a user prompt as the next turn.
    pub fn followup(&self, text: impl Into<String>) {
        self.send(user_message(text), InboxTarget::NextTurn);
    }

    /// Queue input for the next step boundary.
    pub fn steer(&self, text: impl Into<String>) {
        self.send(user_message(text), InboxTarget::NextStep);
    }

    /// Inject context without waking the driver.
    pub fn inject(&self, message: Message) {
        self.inbox.lock().unwrap().append(InboxTarget::NextStep, message.clone());
        self.events.emit::<AgentInboxInserted>(message);
    }

    /// Cancel in-flight work and clear pending input.
    pub fn cancel(&self) {
        let discarded = {
            let mut inbox = self.inbox.lock().unwrap();
            let mut all = inbox.next_turn().to_vec();
            all.extend_from_slice(inbox.next_step());
            inbox.clear();
            all
        };
        for m in discarded {
            self.events.emit::<AgentInboxDiscarded>(m);
        }
        self.abort.lock().unwrap().abort();
    }

    /// Spawn the driver task on the ambient tokio runtime.
    pub fn spawn(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move { this.drive().await });
    }

    /// Wait until the driver is idle with no pending inbox work.
    pub async fn when_idle(&self) {
        while self.status() == AgentStatus::Idle && self.inbox.lock().unwrap().has_pending() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        while self.status() != AgentStatus::Idle {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    async fn drive(&self) {
        loop {
            self.wake.notified().await;
            *self.abort.lock().unwrap() = AbortSignal::new();
            self.set_status(AgentStatus::Running);
            while self.inbox.lock().unwrap().has_pending() && !self.abort.lock().unwrap().aborted() {
                self.run_turn().await;
            }
            self.set_status(AgentStatus::Idle);
        }
    }

    fn assemble_prompt(&self) -> PromptAssembly {
        let mut parts: Vec<String> = Vec::new();
        let base = {
            let o = self.options.read().unwrap();
            o.system_prompt.clone().unwrap_or_default()
        };
        if !base.is_empty() {
            parts.push(base);
        }
        let sections = self.prompt.render();
        if !sections.is_empty() {
            parts.push(sections);
        }
        // 运行时上下文：会话工作目录（web sandbox:policy 快照的同一措辞——
        // 模型据此知道自己在哪个工作区工作）
        {
            let o = self.options.read().unwrap();
            if let Some(ws) = o.workdir.get() {
                parts.push(format!(
                    "Current DSH file policy: workspace-write. Any available operation enforced by the DSH file sandbox may modify files under the session workspace: \"{}\". Some platform temporary areas may also be writable.",
                    ws.display()
                ));
            }
        }
        PromptAssembly {
            system: parts.join("\n\n"),
            tools: self.tools.schemas(),
        }
    }

    /// 轮次起点的上下文压缩：估算 token 超阈值时，选影子区（工具配对安全
    /// 边界）、以当前路由原样重放前缀 + 压缩指令发起辅助调用
    /// （`AuxiliaryPurpose::Compaction`），摘要落 `SessionEvent::Compaction`
    /// （derive_messages 侧生效）。失败静默跳过。
    async fn maybe_compact(&self, assembly: &PromptAssembly) {
        let config = { self.options.read().unwrap().compaction };
        if config.threshold_tokens == 0 {
            return;
        }
        let pairs = {
            let s = self.session.lock().unwrap();
            s.derive_messages_with_seqs()
        };
        if pairs.len() <= config.keep_recent {
            return;
        }
        let msgs: Vec<Message> = pairs.iter().map(|(_, m)| m.clone()).collect();
        if dsh_compaction::estimate_tokens(&msgs) <= config.threshold_tokens {
            return;
        }
        let boundary = dsh_compaction::select_boundary(&msgs, config.keep_recent);
        if boundary == 0 {
            return;
        }
        let before_seq = pairs[boundary - 1].0;
        let shadowed: Vec<Message> = pairs[..boundary].iter().map(|(_, m): &(u64, Message)| m.clone()).collect();
        let (provider, model) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone())
        };
        let signal = self.abort.lock().unwrap().clone();
        match dsh_compaction::summarize_with_llm(
            &self.llm,
            &provider,
            &model,
            Some(assembly.system.clone()),
            Some(assembly.tools.clone()),
            &shadowed,
            signal,
        )
        .await
        {
            Ok(summary) => {
                // v2 富形：影子区边界/清单/token 估算/路由（上游
                // compaction/summary 必填七件套）
                self.append_event(SessionEvent::compaction(
                    before_seq,
                    summary,
                    format!("cp-{}", uuid_simple()),
                    pairs.first().map(|(s, _)| *s).unwrap_or(0),
                    pairs[..boundary].iter().map(|(s, _)| *s).collect(),
                    dsh_compaction::estimate_tokens(&shadowed) as u64,
                    provider.clone(),
                    model.clone(),
                ));
                self.emit_ui(AgentEvent::Compacted { shadowed_messages: boundary });
            }
            Err(_) => {}
        }
    }

    async fn run_turn(&self) {
        // 上游 agent.ts：lastTurn 从 turnBoundary 投影读取（key 恒在——
        // 本 crate 注册了它），缺席回退 0 只是类型层面的兜底。
        let turn = {
            let s = self.session.lock().unwrap();
            self.projections
                .state_of(&s, "turnBoundary")
                .and_then(|v| v.get("lastTurn").and_then(|t| t.as_u64()))
                .unwrap_or(0)
                + 1
        };
        self.append_event(SessionEvent::TurnStart { turn });
        self.emit_ui(AgentEvent::TurnStarted { turn });

        // 上下文压缩：轮次起点检查估算 token，超阈值则摘要影子区
        // （失败尽力而为——本轮按未压缩继续）
        let assembly = self.assemble_prompt();
        self.maybe_compact(&assembly).await;

        let mut target = InboxTarget::NextTurn;
        let mut turn_ends: Option<TurnEndReason> = None;
        let mut step = 0u64;

        loop {
            let signal = self.abort.lock().unwrap().clone();
            if signal.aborted() {
                turn_ends = Some(TurnEndReason::Aborted { cancel_cause: dsh_session::CancelCause::Legacy });
                break;
            }

            step += 1;
            let claimed = { self.inbox.lock().unwrap().claim(target, turn) };
            for m in &claimed {
                self.events.emit::<AgentInboxClaimed>(InboxClaimedPayload { message: m.clone(), turn });
            }

            // agent/pre-step waterfall: reject or rewrite the claimed batch.
            let input = PreStepInput { messages: claimed, turn, step, signal: signal.clone() };
            let decision = self
                .events
                .waterfall::<AgentPreStep>(input, |input| {
                    Box::pin(async move { PreStepDecision::Enter { messages: input.messages } })
                })
                .await;

            let messages = match decision {
                PreStepDecision::Reject => {
                    turn_ends = Some(TurnEndReason::Blocked);
                    break;
                }
                PreStepDecision::Enter { messages } => messages,
            };

            if step > 1 && turn_ends.is_some() && messages.is_empty() {
                break;
            }
            if step == 1 && messages.is_empty() {
                turn_ends = Some(TurnEndReason::Completed);
                break;
            }

            let assembly = self.assemble_prompt();
            // 上游 agent.ts 落盘序：step/start → system commits → user 批。
            self.append_event(SessionEvent::StepStart { turn, step });
            // v3 system prompt 面节点：rustdsh 的 provider 路线走请求 system
            // 参数（非 in-history），按上游 SystemPromptProjection 归一化语义
            // ——无 head 则 append（空 prompt 也保留 head），变化则精确替换
            // head（上游验证要求 replace 端点恰为当前 head）。
            {
                let rendered = assembly.system.clone();
                let mut head = self.system_head.lock().unwrap();
                match head.as_ref() {
                    None => {
                        let seq = self.append_event(SessionEvent::SystemMessage {
                            turn,
                            step,
                            message: system_prompt_message(&rendered),
                            replace: None,
                        });
                        *head = Some((seq, rendered));
                    }
                    Some((seq, text)) if *text != rendered => {
                        let seq = *seq;
                        let new_seq = self.append_event(SessionEvent::SystemMessage {
                            turn,
                            step,
                            message: system_prompt_message(&rendered),
                            replace: Some(seq),
                        });
                        *head = Some((new_seq, rendered));
                    }
                    _ => {}
                }
            }
            // 模型切换公告（上游 model-selection.ts pre-step 通知）：本步有新增
            // 消息且本轮路由与上一持久化 request/header 不同时，在消息批尾部追加
            // 一条公告；首次请求与 effort-only 变化不公告。公告随消息落盘并自然
            // 进入由日志派生的请求历史。
            let mut messages = messages;
            {
                let selected = {
                    let o = self.options.read().unwrap();
                    (o.provider.clone(), o.model.clone())
                };
                let previous = {
                    let s = self.session.lock().unwrap();
                    s.request_header().map(|h| (h.config.provider.clone(), h.config.model.clone()))
                };
                if let Some(prev) = previous
                    && (prev.0 != selected.0 || prev.1 != selected.1)
                {
                    messages.push(model_switch_notice_message(
                        (&prev.0, &prev.1),
                        (&selected.0, &selected.1),
                    ));
                }
            }
            for m in &messages {
                self.append_event(SessionEvent::UserMessage(m.clone()));
            }

            let step_end = self.run_step(turn, step, &assembly, &signal).await;

            self.append_event(SessionEvent::StepEnd { turn, step });

            if let StepEnd::Concluded(reason) = step_end
                && !matches!(turn_ends, Some(TurnEndReason::MaxTokens)) {
                    turn_ends = Some(reason);
                }

            if turn_ends.is_some() && !self.inbox.lock().unwrap().has_pending_next_step() {
                self.events
                    .serial::<AgentTurnStopping>(TurnStoppingPayload { turn, signal: signal.clone() })
                    .await;
                break;
            }
            target = InboxTarget::NextStep;
        }

        let reason = turn_ends.unwrap_or(TurnEndReason::Completed);
        self.append_event(SessionEvent::TurnEnd { turn, reason: reason.clone() });
        self.emit_ui(AgentEvent::TurnEnded { turn, reason });
    }

    async fn run_step(
        &self,
        turn: u64,
        step: u64,
        assembly: &PromptAssembly,
        signal: &AbortSignal,
    ) -> StepEnd {
        let (provider, model) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone())
        };

        loop {
            let options = self.build_request(turn, step, assembly, signal).await;
            let mut stream = match self.llm.stream(options).await {
                Ok(s) => s,
                Err(e) => {
                    self.report_error(turn, step, &e.message, &e.code);
                    return StepEnd::Concluded(TurnEndReason::Error { failure: *e.failure });
                }
            };

            let mut assembler = BlockAssembler::new();
            while let Some(chunk) = stream.next().await {
                if signal.aborted() {
                    let content = assembler.interrupted_blocks();
                    if !content.is_empty() {
                        let message = Message::assistant(content, &provider, &model);
                        self.append_event(SessionEvent::AssistantMessage {
                            turn,
                            step,
                            message: message.clone(),
                            interrupted: true,
                            usage: assembler.usage().cloned(),
                        });
                    }
                    return StepEnd::Concluded(TurnEndReason::Aborted { cancel_cause: dsh_session::CancelCause::Legacy });
                }

                self.append_event(SessionEvent::AssistantChunk {
                    turn,
                    step,
                    chunk: chunk.clone(),
                });
                match &chunk {
                    StreamChunk::TextDelta { text, .. } => {
                        self.emit_ui(AgentEvent::TextDelta { text: text.clone() });
                    }
                    StreamChunk::ReasoningDelta { text, .. } => {
                        self.emit_ui(AgentEvent::ReasoningDelta { text: text.clone() });
                    }
                    _ => {}
                }
                assembler.push(chunk);
            }

            let finish = assembler.finish();
            if let FinishReason::Error { failure } | FinishReason::Aborted { failure } = &finish {
                // agent/request-error waterfall: a listener may elect to retry.
                let action = self
                    .events
                    .waterfall::<AgentRequestError>(
                        RequestErrorPayload {
                            turn,
                            step,
                            provider: provider.clone(),
                            failure: failure.clone(),
                            retry_policy: self.llm.provider_retry_policy(&provider),
                            signal: signal.clone(),
                        },
                        |_| Box::pin(async move { None }),
                    )
                    .await;
                if matches!(action, Some(RequestErrorAction::Retry)) {
                    continue;
                }
                self.report_error(turn, step, &failure.message, &failure.code);
                return StepEnd::Concluded(TurnEndReason::Error { failure: failure.clone() });
            }

            let blocks = match assembler.blocks() {
                Ok(b) => b,
                Err(e) => {
                    self.report_error(turn, step, &e.message, &e.code);
                    return StepEnd::Concluded(TurnEndReason::Error { failure: *e.failure });
                }
            };

            let usage = assembler.usage().cloned();
            let message = Message::assistant(blocks, &provider, &model);
            self.append_event(SessionEvent::AssistantMessage {
                turn,
                step,
                message: message.clone(),
                interrupted: false,
                usage: usage.clone(),
            });
            self.emit_ui(AgentEvent::AssistantMessage { message: message.clone(), usage });

            if matches!(finish, FinishReason::MaxTokens) {
                return StepEnd::Concluded(TurnEndReason::MaxTokens);
            }

            let tool_calls: Vec<(CallId, String, String)> = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { id, name, arguments } => {
                        Some((id.clone(), name.clone(), arguments.clone()))
                    }
                    _ => None,
                })
                .collect();

            if tool_calls.is_empty() {
                return StepEnd::Concluded(TurnEndReason::Completed);
            }

            let concluded = self.execute_tool_calls(turn, step, &tool_calls, signal).await;
            return if concluded {
                StepEnd::Concluded(TurnEndReason::Completed)
            } else {
                StepEnd::NeedsAnotherStep
            };
        }
    }

    fn report_error(&self, turn: u64, step: u64, message: &str, code: &str) {
        self.events.emit::<AgentErrorOccurred>(AgentErrorPayload {
            turn,
            step,
            message: message.to_string(),
            code: code.to_string(),
        });
        self.emit_ui(AgentEvent::Error { message: message.to_string(), code: code.to_string() });
    }

    async fn execute_tool_calls(
        &self,
        turn: u64,
        step: u64,
        tool_calls: &[(CallId, String, String)],
        _signal: &AbortSignal,
    ) -> bool {
        let mut concluded = false;
        for (id, name, arguments) in tool_calls {
            self.emit_ui(AgentEvent::ToolCall {
                tool_call_id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            });
            self.append_event(SessionEvent::ToolCall {
                turn,
                step,
                call_id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            });

            let input =
                ToolExecutionInput::with_raw_arguments(id.clone(), name.clone(), arguments.clone());
            let result: ToolExecutionResult = match self.tools.execute(&input).await {
                Some(r) => r,
                None => ToolExecutionResult::error(format!("no tool \"{name}\"")),
            };
            concluded |= result.concludes_turn;

            let msg = Message::tool_result(id.clone(), result.content.clone(), result.is_error);
            self.append_event(SessionEvent::ToolResult { turn, step, message: msg });
            self.emit_ui(AgentEvent::ToolResult {
                tool_call_id: id.clone(),
                is_error: result.is_error,
            });
        }
        concluded
    }

    async fn build_request(
        &self,
        turn: u64,
        step: u64,
        assembly: &PromptAssembly,
        signal: &AbortSignal,
    ) -> GenerateOptions {
        let (provider, model, max_tokens) = {
            let o = self.options.read().unwrap();
            (o.provider.clone(), o.model.clone(), o.max_tokens)
        };
        let mut seed = LlmCallConfig::new(&provider, &model);
        seed.max_tokens = max_tokens;

        // agent/request waterfall: replace or amend the proposed config.
        let config = {
            let seed = seed.clone();
            self.events
                .waterfall::<AgentRequest>(
                    AgentRequestPayload { turn, step, signal: signal.clone() },
                    move |_| Box::pin(async move { seed }),
                )
                .await
        };

        let system = if assembly.system.is_empty() { None } else { Some(assembly.system.clone()) };
        let tools = if assembly.tools.is_empty() { None } else { Some(assembly.tools.clone()) };

        let (boundary, session_id) = {
            let s = self.session.lock().unwrap();
            (s.derive_messages(), s.id.clone())
        };
        // 请求组装：FileBlock → 确定性 handle 文本（上游
        // projectFilesToText 在所有 provider 路由前无条件执行——provider
        // 永远收不到文件字节，模型按需用现有文件工具读存储副本）
        let boundary = {
            let root = self.options.read().unwrap().attachments_root.clone();
            match root {
                Some(root) => {
                    let store = dsh_persist::AttachmentStore::new(root);
                    dsh_llm::project_files_to_text(boundary, &|ref_: &dsh_llm::FileAttachmentRef| -> Option<String> {
                        store
                            .file_host_path(ref_)
                            .map(|p| p.to_string_lossy().into_owned())
                    })
                }
                None => dsh_llm::project_files_to_text(boundary, &|_| None),
            }
        };

        // v3：system prompt 经 system/message 行持久化，request/header 不再
        // 携带 system 字段（上游 canonicalizeTransformedEvent 语义）；模型
        // 请求本身仍走 options.system。
        let header = EpochHeader {
            config: config.clone(),
            system: None,
            tools: tools.clone(),
        };
        let (changed, reason) = {
            let s = self.session.lock().unwrap();
            match s.request_header() {
                None => (true, HeaderReason::Initial),
                Some(h) => (*h != header, HeaderReason::Change),
            }
        };
        if changed {
            self.append_event(SessionEvent::RequestHeader { header, reason });
        }

        let mut options = GenerateOptions::new(provider, model, boundary);
        options.system = system;
        options.tools = tools;
        options.max_tokens = config.max_tokens;
        options.temperature = config.temperature;
        options.signal = signal.clone();
        options.session_id = Some(session_id);
        options
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use dsh_llm::{MessageSource, Role};

    #[test]
    fn route_label_shows_model_only_for_same_provider() {
        assert_eq!(route_label("p", "m1", "p"), "m1");
        assert_eq!(route_label("p1", "m", "p2"), "p1/m");
    }

    /// 上游 modelSwitchNotice 模板与 source 形（48cc1cf1d6）。
    #[test]
    fn model_switch_notice_matches_upstream_template() {
        let m = model_switch_notice_message(
            ("deepseek-official", "deepseek-v4-flash"),
            ("deepseek-official", "deepseek-v4-pro"),
        );
        assert_eq!(m.role, Role::User);
        match &m.source {
            MessageSource::Plugin { plugin, form, summary, sections } => {
                assert_eq!(plugin, "model-selection");
                assert_eq!(form.as_deref(), Some("notice"));
                assert_eq!(summary.as_deref(), Some("deepseek-v4-flash → deepseek-v4-pro"));
                assert!(sections.is_none());
            }
            other => panic!("plugin source expected, got {other:?}"),
        }
        match &m.content[0] {
            ContentBlock::Text { text } => assert_eq!(
                text,
                "[model changed: assistant turns above this point were generated by deepseek-v4-flash; the session continues with deepseek-v4-pro]",
            ),
            other => panic!("text block expected, got {other:?}"),
        }
    }
}
