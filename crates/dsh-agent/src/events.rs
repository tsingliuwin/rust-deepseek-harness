//! `agent/*` event vocabulary (the live-agent extension points).

use crate::AgentStatus;
use dsh_cordis::{EmitEvent, SerialEvent, WaterfallEvent};
use dsh_llm::{AbortSignal, LlmCallConfig, LlmFailure, Message, ResolvedRetryPolicy};

// --- emit (observer) events -------------------------------------------------

pub struct AgentInboxInserted;
impl EmitEvent for AgentInboxInserted {
    type Payload = Message;
}

pub struct AgentInboxDiscarded;
impl EmitEvent for AgentInboxDiscarded {
    type Payload = Message;
}

#[derive(Clone, Debug)]
pub struct InboxClaimedPayload {
    pub message: Message,
    pub turn: u64,
}

pub struct AgentInboxClaimed;
impl EmitEvent for AgentInboxClaimed {
    type Payload = InboxClaimedPayload;
}

pub struct AgentStatusChanged;
impl EmitEvent for AgentStatusChanged {
    type Payload = AgentStatus;
}

#[derive(Clone, Debug)]
pub struct AgentErrorPayload {
    pub turn: u64,
    pub step: u64,
    pub message: String,
    pub code: String,
}

pub struct AgentErrorOccurred;
impl EmitEvent for AgentErrorOccurred {
    type Payload = AgentErrorPayload;
}

// --- waterfall events -------------------------------------------------------

/// `agent/pre-step`: decide what the model sees. `next()` returns the default
/// `enter` decision; return [`PreStepDecision::Reject`] without `next()` to
/// close the turn before a model call.
#[derive(Clone, Debug)]
pub struct PreStepInput {
    pub messages: Vec<Message>,
    pub turn: u64,
    pub step: u64,
    pub signal: AbortSignal,
}

#[derive(Clone, Debug)]
pub enum PreStepDecision {
    Reject,
    Enter { messages: Vec<Message> },
}

pub struct AgentPreStep;
impl WaterfallEvent for AgentPreStep {
    type Payload = PreStepInput;
    type Output = PreStepDecision;
}

/// `agent/request`: rewrite the proposed call config. `next()` returns the
/// config-so-far (the seed); a listener may replace or amend it.
#[derive(Clone, Debug)]
pub struct AgentRequestPayload {
    pub turn: u64,
    pub step: u64,
    pub signal: AbortSignal,
}

pub struct AgentRequest;
impl WaterfallEvent for AgentRequest {
    type Payload = AgentRequestPayload;
    type Output = LlmCallConfig;
}

/// `agent/request-error`: offer a failed step for recovery. Return
/// `Some(Retry)` to retry; `None` (or `next()`'s default) fails the turn.
#[derive(Clone, Debug)]
pub struct RequestErrorPayload {
    pub turn: u64,
    pub step: u64,
    pub provider: String,
    pub failure: LlmFailure,
    pub retry_policy: ResolvedRetryPolicy,
    pub signal: AbortSignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestErrorAction {
    Retry,
}

pub struct AgentRequestError;
impl WaterfallEvent for AgentRequestError {
    type Payload = RequestErrorPayload;
    type Output = Option<RequestErrorAction>;
}

// --- serial events ----------------------------------------------------------

/// `agent/turn-stopping`: stops a turn. Listeners run in registration order.
#[derive(Clone, Debug)]
pub struct TurnStoppingPayload {
    pub turn: u64,
    pub signal: AbortSignal,
}

pub struct AgentTurnStopping;
impl SerialEvent for AgentTurnStopping {
    type Payload = TurnStoppingPayload;
    type Output = ();
}