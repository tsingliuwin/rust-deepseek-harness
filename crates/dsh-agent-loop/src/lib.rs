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
    AgentStatus, AgentStatusChanged, AgentTurnStopping, Inbox, InboxClaimedPayload, InboxTarget,
    PreStepDecision, PreStepInput, RequestErrorAction, RequestErrorPayload, TurnStoppingPayload,
};
use dsh_cordis::EventBus;
use dsh_llm::{
    AbortSignal, BlockAssembler, CallId, ContentBlock, FinishReason, GenerateOptions, LlmCallConfig,
    LlmRuntime, Message, SessionId, StreamChunk,
};
use dsh_session::{EpochHeader, HeaderReason, Session, SessionEvent, TurnEndReason};
use dsh_system_prompt::{PromptAssembly, SystemPrompt};
use dsh_tools::{ToolExecutionInput, ToolExecutionResult, ToolRegistry};
use flume::{Receiver, Sender};
use futures::StreamExt;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::Notify;

pub mod retry;

/// Static configuration for one agent.
#[derive(Clone, Debug)]
pub struct AgentOptions {
    pub provider: String,
    pub model: String,
    pub max_tokens: Option<u32>,
    /// Base system instructions, rendered ahead of registered prompt sections.
    pub system_prompt: Option<String>,
}

/// A live event emitted to UI/observers as the loop progresses.
#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted { turn: u64 },
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCall { tool_call_id: CallId, name: String, arguments: String },
    ToolResult { tool_call_id: CallId, is_error: bool },
    AssistantMessage { message: Message },
    TurnEnded { turn: u64, reason: TurnEndReason },
    Error { message: String, code: String },
}

/// Convenience for a client-submitted text prompt.
pub fn user_message(text: impl Into<String>) -> Message {
    Message::user_text(text)
}

enum StepEnd {
    /// Tools owe another request: open another step.
    NeedsAnotherStep,
    /// The turn concluded with this reason.
    Concluded(TurnEndReason),
}

/// The default `Agent` implementation: a turn/step driver over an inbox.
pub struct ReactLoopAgent {
    options: Arc<RwLock<AgentOptions>>,
    session: Arc<Mutex<Session>>,
    inbox: Arc<Mutex<Inbox>>,
    llm: Arc<LlmRuntime>,
    tools: Arc<ToolRegistry>,
    prompt: Arc<SystemPrompt>,
    events: EventBus,
    wake: Arc<Notify>,
    abort: Arc<Mutex<AbortSignal>>,
    status: Arc<Mutex<AgentStatus>>,
    ui_events: Mutex<Option<Sender<AgentEvent>>>,
    on_event: Mutex<Option<EventSink>>,
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
        events: EventBus,
    ) -> Arc<Self> {
        Arc::new(Self {
            options: Arc::new(RwLock::new(options)),
            session: Arc::new(Mutex::new(Session::new(id))),
            inbox: Arc::new(Mutex::new(Inbox::new())),
            llm,
            tools,
            prompt,
            events,
            wake: Arc::new(Notify::new()),
            abort: Arc::new(Mutex::new(AbortSignal::new())),
            status: Arc::new(Mutex::new(AgentStatus::Idle)),
            ui_events: Mutex::new(None),
            on_event: Mutex::new(None),
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
    pub fn set_session(&self, session: Session) {
        *self.session.lock().unwrap() = session;
        self.inbox.lock().unwrap().clear();
    }

    /// Register a side-channel observer for every appended session event
    /// (e.g. JSONL persistence). Called with the session lock released.
    pub fn set_event_sink(&self, sink: impl Fn(SessionEvent) + Send + Sync + 'static) {
        *self.on_event.lock().unwrap() = Some(Box::new(sink));
    }

    /// Append one event to the session log and fan it out to the sink.
    fn append_event(&self, event: SessionEvent) {
        {
            let mut s = self.session.lock().unwrap();
            s.append(event.clone());
        }
        if let Some(sink) = self.on_event.lock().unwrap().as_ref() {
            sink(event);
        }
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
        PromptAssembly {
            system: parts.join("\n\n"),
            tools: self.tools.schemas(),
        }
    }

    async fn run_turn(&self) {
        let turn = {
            let s = self.session.lock().unwrap();
            s.last_turn() + 1
        };
        self.append_event(SessionEvent::TurnStart { turn });
        self.emit_ui(AgentEvent::TurnStarted { turn });

        let mut target = InboxTarget::NextTurn;
        let mut turn_ends: Option<TurnEndReason> = None;
        let mut step = 0u64;

        loop {
            let signal = self.abort.lock().unwrap().clone();
            if signal.aborted() {
                turn_ends = Some(TurnEndReason::Aborted);
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
            for m in &messages {
                self.append_event(SessionEvent::UserMessage(m.clone()));
            }
            self.append_event(SessionEvent::StepStart { turn, step });

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
                    return StepEnd::Concluded(TurnEndReason::Aborted);
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
                usage,
            });
            self.emit_ui(AgentEvent::AssistantMessage { message: message.clone() });

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

        let header = EpochHeader {
            config: config.clone(),
            system: system.clone(),
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