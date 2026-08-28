//! dsh-agent — the `Agent` interface vocabulary: inbox, status, and the live
//! `agent/*` event vocabulary.

pub mod events;

pub use events::{
    AgentErrorOccurred, AgentErrorPayload, AgentInboxClaimed, AgentInboxDiscarded,
    AgentInboxInserted, AgentPreStep, AgentRequest, AgentRequestError, AgentRequestPayload,
    AgentStatusChanged, AgentTurnStopping, InboxClaimedPayload, PreStepDecision, PreStepInput,
    RequestErrorAction, RequestErrorPayload, TurnStoppingPayload,
};

use dsh_llm::Message;

/// One of the two ordered pending-message lists owned by an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxTarget {
    NextTurn,
    NextStep,
}

/// Whether an agent is working (`Running`) or between turns (`Idle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Running,
}

/// Incremental projection of the agent's pending input.
///
/// v1 keeps the two lists in memory; the reference also replays durable
/// `agent/inbox/spliced` log events, which arrives with the persistence
/// milestone. Splice/claim semantics are identical to the reference.
#[derive(Default)]
pub struct Inbox {
    next_turn: Vec<Message>,
    next_step: Vec<Message>,
}

impl Inbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Prompts awaiting individual turns.
    pub fn next_turn(&self) -> &[Message] {
        &self.next_turn
    }

    /// Input awaiting the next step boundary.
    pub fn next_step(&self) -> &[Message] {
        &self.next_step
    }

    /// Whether either pending-message list contains work.
    pub fn has_pending(&self) -> bool {
        !self.next_turn.is_empty() || !self.next_step.is_empty()
    }

    /// Whether the next-step list contains work.
    pub fn has_pending_next_step(&self) -> bool {
        !self.next_step.is_empty()
    }

    /// Remove every pending message, next-step before next-turn.
    pub fn clear(&mut self) {
        self.next_step.clear();
        self.next_turn.clear();
    }

    /// Apply standard splice semantics to one pending list.
    ///
    /// `usize::MAX` for `start` means "append" (the reference uses `Infinity`).
    pub fn splice(&mut self, target: InboxTarget, start: usize, delete_count: usize, inserted: Vec<Message>) {
        let list = match target {
            InboxTarget::NextTurn => &mut self.next_turn,
            InboxTarget::NextStep => &mut self.next_step,
        };
        let start = start.min(list.len());
        let delete = delete_count.min(list.len() - start);
        list.splice(start..start + delete, inserted);
    }

    /// Append one message to a pending list.
    pub fn append(&mut self, target: InboxTarget, message: Message) {
        self.splice(target, usize::MAX, 0, vec![message]);
    }

    /// Remove and return the complete batch proposed for one step: all
    /// next-step input followed by one queued turn when `target` is next-turn.
    pub fn claim(&mut self, target: InboxTarget, _turn: u64) -> Vec<Message> {
        let mut claimed = std::mem::take(&mut self.next_step);
        if target == InboxTarget::NextTurn && !self.next_turn.is_empty() {
            claimed.push(self.next_turn.remove(0));
        }
        claimed
    }
}