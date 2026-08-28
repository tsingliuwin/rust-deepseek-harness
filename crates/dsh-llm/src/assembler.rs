//! Incremental chunk-to-message assembler — the single canonical assembly
//! algorithm. Mirrors
//! [`packages/llm/llm/src/assembler.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/assembler.ts):
//! folds a `StreamChunk` stream back into `ContentBlock`s plus usage, finish
//! reason, and replay state, with the one shared keep/drop decision (max-token
//! truncation drops unexecutable tool calls).

use crate::error::LlmError;
use crate::message::{Message, MessageSource, Role};
use crate::types::{
    CallId, ContentBlock, ContentBlockType, FinishReason, ReplayEnvelope, StreamChunk, TokenUsage,
};
use std::collections::HashMap;

struct PartialBlock {
    block_type: ContentBlockType,
    text: String,
    tool_call_id: Option<CallId>,
    tool_call_name: Option<String>,
    tool_call_arguments: String,
    /// Set by `block-end` — authoritative, and freezes the partial.
    block: Option<ContentBlock>,
}

impl PartialBlock {
    fn new(block_type: ContentBlockType) -> Self {
        Self {
            block_type,
            text: String::new(),
            tool_call_id: None,
            tool_call_name: None,
            tool_call_arguments: String::new(),
            block: None,
        }
    }
}

/// Incrementally assembles raw [`StreamChunk`]s into complete
/// [`ContentBlock`]s and a final assistant [`Message`].
#[derive(Default)]
pub struct BlockAssembler {
    partials: HashMap<usize, PartialBlock>,
    order: Vec<usize>,
    usage: Option<TokenUsage>,
    finish: Option<FinishReason>,
    replay_state: Option<ReplayEnvelope>,
}

impl BlockAssembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk into the assembly state.
    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::BlockStart { index, block_type } => {
                if !self.partials.contains_key(&index) {
                    self.order.push(index);
                    self.partials.insert(index, PartialBlock::new(block_type));
                }
            }
            StreamChunk::TextDelta { index, text } => {
                let p = self.ensure(index, ContentBlockType::Text);
                if p.block.is_none() {
                    p.text.push_str(&text);
                }
            }
            StreamChunk::ReasoningDelta { index, text } => {
                let p = self.ensure(index, ContentBlockType::Reasoning);
                if p.block.is_none() {
                    p.text.push_str(&text);
                }
            }
            StreamChunk::ToolCallDelta { index, id, name, arguments_delta } => {
                let p = self.ensure(index, ContentBlockType::ToolCall);
                if p.block.is_none() {
                    p.tool_call_id = Some(id);
                    if let Some(n) = name {
                        p.tool_call_name = Some(n);
                    }
                    p.tool_call_arguments.push_str(&arguments_delta);
                }
            }
            StreamChunk::BlockEnd { index, block } => {
                let ty = block.content_type();
                let p = self.ensure(index, ty);
                // First close wins; ignoring re-close stragglers keeps streamed
                // output and the final assembled block in agreement.
                if p.block.is_none() {
                    p.block = Some(block);
                }
            }
            StreamChunk::Usage { usage } => {
                self.usage = Some(usage);
            }
            StreamChunk::Finish { reason, replay_state } => {
                self.finish = Some(reason);
                self.replay_state = replay_state;
            }
        }
    }

    fn ensure(&mut self, index: usize, block_type: ContentBlockType) -> &mut PartialBlock {
        if !self.partials.contains_key(&index) {
            self.order.push(index);
            self.partials.insert(index, PartialBlock::new(block_type));
        }
        self.partials.get_mut(&index).expect("partial just inserted")
    }

    fn must_get(&self, index: usize) -> &PartialBlock {
        self.partials
            .get(&index)
            .unwrap_or_else(|| panic!("BlockAssembler invariant violated: no partial for index {index}"))
    }

    fn assemble(&self, partial: &PartialBlock, index: usize) -> Result<ContentBlock, LlmError> {
        if let Some(block) = &partial.block {
            return Ok(block.clone());
        }
        match partial.block_type {
            ContentBlockType::Text => Ok(ContentBlock::text(partial.text.clone())),
            ContentBlockType::Reasoning => Ok(ContentBlock::reasoning(partial.text.clone())),
            ContentBlockType::ToolCall => Ok(ContentBlock::tool_call(
                partial.tool_call_id.clone().unwrap_or_else(|| CallId::new(format!("call-{index}"))),
                partial.tool_call_name.clone().unwrap_or_default(),
                partial.tool_call_arguments.clone(),
            )),
            other => Err(LlmError::invariant(format!(
                "cannot assemble incomplete block of type \"{other:?}\""
            ))),
        }
    }

    /// The one shared keep/drop decision over all seen blocks: max-token
    /// truncation drops tool calls that cannot be executed safely.
    fn assembled(&self) -> Result<(Vec<ContentBlock>, Option<ReplayEnvelope>), LlmError> {
        let all: Vec<ContentBlock> = self
            .order
            .iter()
            .map(|i| self.assemble(self.must_get(*i), *i))
            .collect::<Result<_, _>>()?;

        let kept: Option<Vec<bool>> = if matches!(self.finish(), FinishReason::MaxTokens) {
            Some(all.iter().map(|b| !matches!(b, ContentBlock::ToolCall { .. })).collect())
        } else {
            None
        };

        let blocks: Vec<ContentBlock> = match &kept {
            None => all.clone(),
            Some(k) => all.iter().zip(k).filter(|(_, keep)| **keep).map(|(b, _)| b.clone()).collect(),
        };

        let replay = self.pruned_replay(&all, kept.as_deref(), blocks.len());
        Ok((blocks, replay))
    }

    fn pruned_replay(
        &self,
        all: &[ContentBlock],
        kept: Option<&[bool]>,
        blocks_len: usize,
    ) -> Option<ReplayEnvelope> {
        let env = self.replay_state.as_ref()?;
        let Some(entries) = &env.blocks else {
            return Some(env.clone());
        };
        // Entries whose length does not match the emitted block count discard
        // the whole envelope.
        if entries.len() != all.len() {
            return None;
        }
        match kept {
            None => Some(env.clone()),
            Some(_k) if blocks_len == all.len() => Some(env.clone()),
            Some(k) => Some(ReplayEnvelope {
                response: env.response.clone(),
                blocks: Some(
                    entries
                        .iter()
                        .zip(k)
                        .filter(|(_, keep)| **keep)
                        .map(|(e, _)| e.clone())
                        .collect(),
                ),
            }),
        }
    }

    /// Assemble all blocks seen so far, in stream order; an open block of a
    /// non-delta type throws (it was never closed by `block-end`).
    pub fn blocks(&self) -> Result<Vec<ContentBlock>, LlmError> {
        self.assembled().map(|(b, _)| b)
    }

    /// Assemble the prefix an interrupted stream can safely finalize: closed and
    /// open text/reasoning blocks with non-whitespace content.
    pub fn interrupted_blocks(&self) -> Vec<ContentBlock> {
        self.order
            .iter()
            .filter_map(|i| {
                let p = self.must_get(*i);
                let ty = p.block.as_ref().map(|b| b.content_type()).unwrap_or(p.block_type);
                if !matches!(ty, ContentBlockType::Text | ContentBlockType::Reasoning) {
                    return None;
                }
                match self.assemble(p, *i) {
                    Ok(ContentBlock::Text { text }) if !text.trim().is_empty() => Some(ContentBlock::text(text)),
                    Ok(ContentBlock::Reasoning { text }) if !text.trim().is_empty() => Some(ContentBlock::reasoning(text)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Usage from the `usage` chunk; `None` until one arrives.
    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// Finish reason; `Stop` when the stream ended without a terminal finish.
    pub fn finish(&self) -> FinishReason {
        self.finish.clone().unwrap_or(FinishReason::Stop)
    }

    /// Replay metadata, pruned in step with `blocks`.
    pub fn replay_state(&self) -> Option<ReplayEnvelope> {
        self.assembled().ok().and_then(|(_, r)| r)
    }

    /// The assembled assistant message.
    pub fn message(&self, source: MessageSource) -> Result<Message, LlmError> {
        Ok(Message::new(Role::Assistant, self.blocks()?, source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContentBlockType, FinishReason, StreamChunk};
    use crate::CallId;

    #[test]
    fn assembles_text_from_deltas() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text });
        a.push(StreamChunk::TextDelta { index: 0, text: "Hello ".into() });
        a.push(StreamChunk::TextDelta { index: 0, text: "world".into() });
        a.push(StreamChunk::Finish { reason: FinishReason::Stop, replay_state: None });
        assert_eq!(a.blocks().unwrap(), vec![ContentBlock::Text { text: "Hello world".into() }]);
        assert_eq!(a.finish(), FinishReason::Stop);
    }

    #[test]
    fn assembles_tool_call_from_deltas() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::ToolCall });
        a.push(StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::new("c1"),
            name: Some("fs".into()),
            arguments_delta: "{\"op\":".into(),
        });
        a.push(StreamChunk::ToolCallDelta {
            index: 0,
            id: CallId::new("c1"),
            name: None,
            arguments_delta: "\"read\"}".into(),
        });
        a.push(StreamChunk::Finish { reason: FinishReason::ToolCalls, replay_state: None });
        let blocks = a.blocks().unwrap();
        assert_eq!(blocks.len(), 1);
        match &blocks[0] {
            ContentBlock::ToolCall { id, name, arguments } => {
                assert_eq!(id.as_str(), "c1");
                assert_eq!(name, "fs");
                assert_eq!(arguments, "{\"op\":\"read\"}");
            }
            other => panic!("expected tool call, got {other:?}"),
        }
    }

    #[test]
    fn drops_tool_calls_on_max_tokens() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text });
        a.push(StreamChunk::TextDelta { index: 0, text: "hi".into() });
        a.push(StreamChunk::BlockStart { index: 1, block_type: ContentBlockType::ToolCall });
        a.push(StreamChunk::ToolCallDelta {
            index: 1,
            id: CallId::new("c1"),
            name: Some("fs".into()),
            arguments_delta: "{}".into(),
        });
        a.push(StreamChunk::Finish { reason: FinishReason::MaxTokens, replay_state: None });
        let blocks = a.blocks().unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(matches!(blocks[0], ContentBlock::Text { .. }));
    }

    #[test]
    fn interrupted_blocks_returns_partial_text() {
        let mut a = BlockAssembler::new();
        a.push(StreamChunk::BlockStart { index: 0, block_type: ContentBlockType::Text });
        a.push(StreamChunk::TextDelta { index: 0, text: "partial".into() });
        // No finish chunk: the stream was interrupted.
        assert_eq!(a.interrupted_blocks(), vec![ContentBlock::Text { text: "partial".into() }]);
    }
}