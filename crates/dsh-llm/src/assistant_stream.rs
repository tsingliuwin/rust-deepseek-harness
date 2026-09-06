//! Lossless compact encoding of one model-stream attempt.
//!
//! Mirrors upstream
//! [`packages/llm/llm/src/assistant-stream.ts`](https://github.com/deepseek-ai/deepseek-harness/blob/main/packages/llm/llm/src/assistant-stream.ts):
//! the v2 session format embeds every settled attempt's exact timed chunk
//! sequence in the settlement event as [`AssistantStreamRecord`]s, so the
//! durable log reproduces streaming timing without one event per chunk.
//! Merge rule is strictly adjacent: only the trailing record may grow.

use crate::types::{AssistantStreamRecord, CallId, StreamChunk};

/// One accumulator record being built; `last_time` is dropped on snapshot.
enum MutableRecord {
    Deltas {
        kind: DeltaKind,
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        texts: Vec<String>,
        last_time: u64,
    },
    ToolCall {
        time0: u64,
        index: usize,
        dt: Vec<i64>,
        id: String,
        name: Option<String>,
        args: Vec<String>,
        last_time: u64,
    },
    Chunk {
        time: u64,
        chunk: StreamChunk,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeltaKind {
    Text,
    Reasoning,
}

/// Incrementally compacts one attempt without retaining a raw chunk list.
#[derive(Default)]
pub struct AssistantStreamAccumulator {
    records: Vec<MutableRecord>,
}

/// ms gap between two timestamps; always representable for wall-clock times.
fn safe_gap(previous: u64, next: u64) -> i64 {
    next as i64 - previous as i64
}

impl AssistantStreamAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one timed chunk. Merge semantics follow the upstream accumulator
    /// exactly: same-kind same-index trailing records join; tool-call deltas
    /// with an empty id or an empty `name` degrade to verbatim chunk records,
    /// as do block boundaries, usage, and finish.
    pub fn push(&mut self, time: u64, chunk: StreamChunk) {
        match chunk {
            StreamChunk::TextDelta { index, text } => self.push_delta(DeltaKind::Text, time, index, text),
            StreamChunk::ReasoningDelta { index, text } => {
                self.push_delta(DeltaKind::Reasoning, time, index, text)
            }
            StreamChunk::ToolCallDelta {
                index,
                id: CallId(id),
                name,
                arguments_delta,
            } => {
                if id.is_empty() || name.as_deref() == Some("") {
                    self.records.push(MutableRecord::Chunk {
                        time,
                        chunk: StreamChunk::ToolCallDelta {
                            index,
                            id: CallId(id),
                            name,
                            arguments_delta,
                        },
                    });
                    return;
                }
                if let Some(MutableRecord::ToolCall {
                    index: last_index,
                    dt,
                    id: last_id,
                    name: last_name,
                    args,
                    last_time,
                    ..
                }) = self.records.last_mut()
                {
                    let same_name = last_name.is_some() == name.is_some()
                        && last_name.as_deref() == name.as_deref();
                    if *last_index == index && *last_id == id && same_name {
                        dt.push(safe_gap(*last_time, time));
                        args.push(arguments_delta.clone());
                        *last_time = time;
                        return;
                    }
                }
                self.records.push(MutableRecord::ToolCall {
                    time0: time,
                    index,
                    dt: Vec::new(),
                    id,
                    name,
                    args: vec![arguments_delta],
                    last_time: time,
                });
            }
            StreamChunk::BlockStart { .. }
            | StreamChunk::BlockEnd { .. }
            | StreamChunk::Usage { .. }
            | StreamChunk::Finish { .. } => self.records.push(MutableRecord::Chunk { time, chunk }),
        }
    }

    fn push_delta(&mut self, kind: DeltaKind, time: u64, index: usize, text: String) {
        if let Some(MutableRecord::Deltas {
            kind: last_kind,
            index: last_index,
            dt,
            texts,
            last_time,
            ..
        }) = self.records.last_mut()
        {
            if *last_kind == kind && *last_index == index {
                dt.push(safe_gap(*last_time, time));
                texts.push(text);
                *last_time = time;
                return;
            }
        }
        self.records.push(MutableRecord::Deltas {
            kind,
            time0: time,
            index,
            dt: Vec::new(),
            texts: vec![text],
            last_time: time,
        });
    }

    /// The current compact attempt stream, ready for a durable settlement.
    pub fn snapshot(&self) -> Vec<AssistantStreamRecord> {
        self.records
            .iter()
            .map(|record| match record {
                MutableRecord::Deltas {
                    kind,
                    time0,
                    index,
                    dt,
                    texts,
                    ..
                } => match kind {
                    DeltaKind::Text => AssistantStreamRecord::TextChunks {
                        time0: *time0,
                        index: *index,
                        dt: dt.clone(),
                        texts: texts.clone(),
                    },
                    DeltaKind::Reasoning => AssistantStreamRecord::ReasoningChunks {
                        time0: *time0,
                        index: *index,
                        dt: dt.clone(),
                        texts: texts.clone(),
                    },
                },
                MutableRecord::ToolCall {
                    time0,
                    index,
                    dt,
                    id,
                    name,
                    args,
                    ..
                } => AssistantStreamRecord::ToolCallChunks {
                    time0: *time0,
                    index: *index,
                    dt: dt.clone(),
                    id: crate::types::CallId(id.clone()),
                    name: name.clone(),
                    args: args.clone(),
                },
                MutableRecord::Chunk { time, chunk } => AssistantStreamRecord::Chunk {
                    time: *time,
                    chunk: chunk.clone(),
                },
            })
            .collect()
    }

    /// Whether any chunk has been recorded for this attempt.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// Expand compact records back into the exact timed chunk sequence (upstream
/// `expandAssistantStream`; used by tests and log forensics).
pub fn expand_assistant_stream(stream: &[AssistantStreamRecord]) -> Vec<(u64, StreamChunk)> {
    let mut chunks = Vec::new();
    for record in stream {
        match record {
            AssistantStreamRecord::Chunk { time, chunk } => chunks.push((*time, chunk.clone())),
            AssistantStreamRecord::TextChunks {
                time0, index, dt, texts, ..
            } => push_expanded(&mut chunks, *time0, dt, texts, |text| StreamChunk::TextDelta {
                index: *index,
                text,
            }),
            AssistantStreamRecord::ReasoningChunks {
                time0, index, dt, texts, ..
            } => push_expanded(&mut chunks, *time0, dt, texts, |text| {
                StreamChunk::ReasoningDelta {
                    index: *index,
                    text,
                }
            }),
            AssistantStreamRecord::ToolCallChunks {
                time0,
                index,
                dt,
                id,
                name,
                args,
            } => push_expanded(&mut chunks, *time0, dt, args, |arg| StreamChunk::ToolCallDelta {
                index: *index,
                id: id.clone(),
                name: name.clone(),
                arguments_delta: arg,
            }),
        }
    }
    chunks
}

fn push_expanded(
    chunks: &mut Vec<(u64, StreamChunk)>,
    time0: u64,
    dt: &[i64],
    members: &[String],
    make: impl Fn(String) -> StreamChunk,
) {
    let mut time = time0;
    for (i, member) in members.iter().enumerate() {
        if i > 0 {
            time = (time as i64 + dt[i - 1]) as u64;
        }
        chunks.push((time, make(member.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CallId, FinishReason, TokenUsage};

    fn ms(n: u64) -> u64 {
        1_000_000 + n
    }

    #[test]
    fn adjacent_deltas_merge_and_boundaries_break() {
        let mut acc = AssistantStreamAccumulator::new();
        acc.push(ms(0), StreamChunk::BlockStart { index: 0, block_type: crate::types::ContentBlockType::Text });
        acc.push(ms(10), StreamChunk::TextDelta { index: 0, text: "a".into() });
        acc.push(ms(12), StreamChunk::TextDelta { index: 0, text: "b".into() });
        acc.push(ms(20), StreamChunk::TextDelta { index: 1, text: "c".into() });
        acc.push(
            ms(30),
            StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            },
        );
        let records = acc.snapshot();
        assert_eq!(records.len(), 4);
        assert_eq!(
            records[0],
            AssistantStreamRecord::Chunk {
                time: ms(0),
                chunk: StreamChunk::BlockStart {
                    index: 0,
                    block_type: crate::types::ContentBlockType::Text
                }
            }
        );
        assert_eq!(
            records[1],
            AssistantStreamRecord::TextChunks {
                time0: ms(10),
                index: 0,
                dt: vec![2],
                texts: vec!["a".into(), "b".into()]
            }
        );
        assert_eq!(
            records[2],
            AssistantStreamRecord::TextChunks {
                time0: ms(20),
                index: 1,
                dt: vec![],
                texts: vec!["c".into()]
            }
        );
        // 展开还原精确时序
        let expanded = expand_assistant_stream(&records);
        assert_eq!(expanded[1].0, ms(10));
        assert_eq!(expanded[2].0, ms(12));
        assert_eq!(expanded[3].0, ms(20));
    }

    #[test]
    fn tool_call_deltas_merge_by_id_and_name() {
        let mut acc = AssistantStreamAccumulator::new();
        acc.push(
            ms(1),
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId("call-1".into()),
                name: Some("read".into()),
                arguments_delta: "{\"p".into(),
            },
        );
        acc.push(
            ms(4),
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId("call-1".into()),
                name: None, // name 缺席与 Some("read") 不同 → 断开
                arguments_delta: ":1}".into(),
            },
        );
        acc.push(
            ms(9),
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId("call-1".into()),
                name: None,
                arguments_delta: "!".into(),
            },
        );
        let records = acc.snapshot();
        assert_eq!(records.len(), 2);
        assert_eq!(
            records[0],
            AssistantStreamRecord::ToolCallChunks {
                time0: ms(1),
                index: 0,
                dt: vec![],
                id: CallId("call-1".into()),
                name: Some("read".into()),
                args: vec!["{\"p".into()]
            }
        );
        assert_eq!(
            records[1],
            AssistantStreamRecord::ToolCallChunks {
                time0: ms(4),
                index: 0,
                dt: vec![5],
                id: CallId("call-1".into()),
                name: None,
                args: vec![":1}".into(), "!".into()]
            }
        );
    }

    #[test]
    fn empty_id_or_name_degrades_to_verbatim_chunk() {
        let mut acc = AssistantStreamAccumulator::new();
        acc.push(
            ms(1),
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId(String::new()),
                name: None,
                arguments_delta: "x".into(),
            },
        );
        acc.push(
            ms(2),
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId("c".into()),
                name: Some(String::new()),
                arguments_delta: "y".into(),
            },
        );
        let records = acc.snapshot();
        assert_eq!(records.len(), 2);
        for record in &records {
            assert!(matches!(record, AssistantStreamRecord::Chunk { .. }));
        }
    }

    #[test]
    fn usage_round_trips_through_snapshot() {
        let mut acc = AssistantStreamAccumulator::new();
        acc.push(ms(1), StreamChunk::TextDelta { index: 0, text: "hi".into() });
        acc.push(
            ms(2),
            StreamChunk::Usage {
                usage: TokenUsage {
                    input_tokens: 3,
                    output_tokens: 5,
                    cache_read_tokens: None,
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                },
            },
        );
        let records = acc.snapshot();
        let json = serde_json::to_value(&records).unwrap();
        assert_eq!(json[0]["type"], "text-chunks");
        assert_eq!(json[0]["time0"], ms(1));
        assert_eq!(json[1]["type"], "chunk");
        assert_eq!(json[1]["chunk"]["type"], "usage");
        let back: Vec<AssistantStreamRecord> = serde_json::from_value(json).unwrap();
        assert_eq!(back, records);
        let expanded = expand_assistant_stream(&back);
        assert_eq!(expanded.len(), 2);
    }
}
