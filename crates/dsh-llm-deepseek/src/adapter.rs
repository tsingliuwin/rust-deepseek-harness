//! The [`DeepSeekAdapter`] transport and its SSE → [`StreamChunk`] mapper.

use async_stream::stream;
use async_trait::async_trait;
use bytes::Bytes;
use dsh_llm::{
    AbortSignal, BoxStream, CallId, ContentBlock, ContentBlockType, FinishReason, GenerateOptions,
    LlmAdapter, LlmError, LlmFailure, LlmProviderInfo, Role, StreamChunk, TokenUsage,
};
use dsh_llm::Message;
use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::pin::Pin;
use std::time::Duration;

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

const DEFAULT_BASE_URL: &str = "https://api.deepseek.com";

/// DeepSeek provider adapter over the OpenAI-compatible chat-completions API.
#[derive(Clone)]
pub struct DeepSeekAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl DeepSeekAdapter {
    /// Build an adapter for a fixed API key and optional base URL override.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .user_agent(concat!("dsh-rust/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            client,
            base_url: base_url.into(),
            api_key: api_key.into(),
        }
    }

    /// Build an adapter from `DEEPSEEK_API_KEY` (optional `DEEPSEEK_BASE_URL`).
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("DEEPSEEK_API_KEY").ok()?;
        let base = std::env::var("DEEPSEEK_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Some(Self::with_base_url(key, base))
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    fn finish_stream(failure: LlmFailure) -> BoxStream {
        Box::pin(stream! {
            yield StreamChunk::finish_error(failure);
        })
    }

    fn build_body(&self, options: &GenerateOptions) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        if let Some(system) = &options.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        for m in &options.messages {
            messages.push(Self::map_message(m));
        }

        let mut body = json!({
            "model": options.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        });
        if let Some(tools) = &options.tools
            && !tools.is_empty() {
                let tools_json: Vec<Value> = tools
                    .iter()
                    .map(|t| {
                        json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect();
                body["tools"] = Value::Array(tools_json);
            }
        if let Some(temp) = options.temperature {
            body["temperature"] = Value::from(temp);
        }
        if let Some(max) = options.max_tokens {
            body["max_tokens"] = Value::from(max);
        }
        if let Some(stop) = &options.stop {
            body["stop"] = Value::Array(stop.iter().map(|s| Value::String(s.clone())).collect());
        }
        body
    }

    /// Map one provider-neutral [`Message`] onto DeepSeek's wire message.
    fn map_message(m: &Message) -> Value {
        match m.role {
            Role::System => json!({ "role": "system", "content": Self::blocks_to_text(&m.content) }),
            Role::User => {
                if let [ContentBlock::ToolResult { tool_call_id, content, .. }] = m.content.as_slice() {
                    json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id.as_str(),
                        "content": Self::blocks_to_text(content),
                    })
                } else {
                    json!({ "role": "user", "content": Self::blocks_to_text(&m.content) })
                }
            }
            Role::Assistant => {
                let mut text = String::new();
                let mut reasoning = String::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                for block in &m.content {
                    match block {
                        ContentBlock::Text { text: t } => text.push_str(t),
                        ContentBlock::Reasoning { text: t } => reasoning.push_str(t),
                        ContentBlock::ToolCall { id, name, arguments } => tool_calls.push(json!({
                            "id": id.as_str(),
                            "type": "function",
                            "function": { "name": name, "arguments": arguments },
                        })),
                        ContentBlock::Image { .. } | ContentBlock::ToolResult { .. } => {}
                    }
                }
                let mut obj = serde_json::Map::new();
                obj.insert("role".into(), Value::String("assistant".into()));
                obj.insert(
                    "content".into(),
                    if text.is_empty() { Value::Null } else { Value::String(text) },
                );
                if !reasoning.is_empty() {
                    obj.insert("reasoning_content".into(), Value::String(reasoning));
                }
                if !tool_calls.is_empty() {
                    obj.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                Value::Object(obj)
            }
        }
    }

    /// Flatten text/reasoning blocks into one model-facing string.
    fn blocks_to_text(content: &[ContentBlock]) -> String {
        let mut out = String::new();
        for b in content {
            match b {
                ContentBlock::Text { text } | ContentBlock::Reasoning { text } => out.push_str(text),
                ContentBlock::ToolResult { content, .. } => out.push_str(&Self::blocks_to_text(content)),
                ContentBlock::ToolCall { .. } | ContentBlock::Image { .. } => {}
            }
        }
        out
    }

    fn map_usage(u: &Value) -> TokenUsage {
        let n = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let prompt = n("prompt_tokens");
        let cache_hit = n("prompt_cache_hit_tokens");
        let cache_miss = n("prompt_cache_miss_tokens");
        TokenUsage {
            input_tokens: prompt.saturating_sub(cache_hit + cache_miss),
            output_tokens: n("completion_tokens"),
            cache_read_tokens: Some(cache_hit),
            cache_write_tokens: Some(cache_miss),
            reasoning_tokens: None,
        }
    }

    fn classify_http_failure(status: u16, detail: &str) -> LlmFailure {
        let code = if dsh_llm::is_quota_exceeded(detail) {
            dsh_llm::QUOTA
        } else if dsh_llm::is_context_window_exceeded(detail) {
            dsh_llm::CONTEXT_WINDOW_EXCEEDED
        } else if status == 401 || status == 403 {
            dsh_llm::INVALID_CREDENTIAL
        } else if status == 429 {
            "RATE_LIMIT"
        } else {
            dsh_llm::UNKNOWN
        };
        LlmFailure {
            message: detail.to_string(),
            code: code.to_string(),
            status: Some(status),
            provider_retry_after_ms: None,
            request_id: None,
        }
    }

    /// Fold the raw HTTP byte stream into the harness `StreamChunk` protocol.
    fn chunk_stream(bytes: ByteStream, signal: AbortSignal) -> BoxStream {
        Box::pin(stream! {
            let mut bytes = bytes;
            let mut buffer: Vec<u8> = Vec::new();
            let mut ctx = StreamCtx::default();

            loop {
                // Drain every complete SSE event buffered so far.
                while let Some(event) = take_event(&mut buffer) {
                    if signal.aborted() {
                        yield StreamChunk::stream_aborted();
                        return;
                    }
                    for chunk in ctx.handle_event(&event) {
                        let terminal = matches!(&chunk, StreamChunk::Finish { .. });
                        yield chunk;
                        if terminal {
                            return;
                        }
                    }
                }

                // Refill the buffer.
                match bytes.next().await {
                    Some(Ok(b)) => buffer.extend_from_slice(&b),
                    Some(Err(e)) => {
                        yield StreamChunk::finish_error(LlmFailure {
                            message: format!("transport failure: {e}"),
                            code: dsh_llm::UNKNOWN.to_string(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                        });
                        return;
                    }
                    None => {
                        // EOF: finalize a trailing event that lacked a blank line.
                        if !buffer.is_empty() {
                            let rest = std::mem::take(&mut buffer);
                            for chunk in ctx.handle_event(&String::from_utf8_lossy(&rest)) {
                                let terminal = matches!(&chunk, StreamChunk::Finish { .. });
                                yield chunk;
                                if terminal {
                                    return;
                                }
                            }
                        }
                        yield ctx.final_finish();
                        return;
                    }
                }
            }
        })
    }
}

#[async_trait]
impl LlmAdapter for DeepSeekAdapter {
    fn provider_info(&self, _provider: &str) -> LlmProviderInfo {
        LlmProviderInfo { id: "deepseek".to_string(), name: "DeepSeek".to_string() }
    }

    async fn stream(&self, options: GenerateOptions) -> Result<BoxStream, LlmError> {
        let signal = options.signal.clone();
        let body = self.build_body(&options);
        let resp = self
            .client
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::new(format!("request failed: {e}"), dsh_llm::UNKNOWN))?;

        let status = resp.status();
        if !status.is_success() {
            let status_code = status.as_u16();
            let detail = resp
                .text()
                .await
                .unwrap_or_else(|_| format!("HTTP {status_code}"));
            return Ok(Self::finish_stream(Self::classify_http_failure(status_code, &detail)));
        }

        let bytes = Box::pin(resp.bytes_stream()) as ByteStream;
        Ok(Self::chunk_stream(bytes, signal))
    }
}

// --- SSE tokenizer and chunk mapper ----------------------------------------

#[derive(Default)]
struct StreamCtx {
    reasoning_started: bool,
    text_started: bool,
    tool_blocks: HashMap<usize, ToolBlock>,
    any_content: bool,
    finish_reason: Option<String>,
}

struct ToolBlock {
    id: Option<CallId>,
    name_sent: bool,
    started: bool,
}

impl StreamCtx {
    /// Parse one SSE event into the chunks it yields.
    fn handle_event(&mut self, event: &str) -> Vec<StreamChunk> {
        let mut datas: Vec<&str> = Vec::new();
        for line in event.lines() {
            let line = line.strip_prefix('\r').unwrap_or(line);
            if let Some(rest) = line.strip_prefix("data:") {
                datas.push(rest.trim_start());
            }
        }
        if datas.is_empty() {
            return Vec::new();
        }
        let data = datas.join("\n");
        if data == "[DONE]" {
            return vec![self.final_finish()];
        }
        match serde_json::from_str::<Value>(&data) {
            Ok(v) => {
                let mut out = Vec::new();
                self.handle_chunk(&v, &mut out);
                out
            }
            Err(_) => Vec::new(),
        }
    }

    fn handle_chunk(&mut self, v: &Value, out: &mut Vec<StreamChunk>) {
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            out.push(StreamChunk::Usage { usage: DeepSeekAdapter::map_usage(usage) });
        }

        let Some(choices) = v.get("choices").and_then(|c| c.as_array()) else {
            return;
        };
        for choice in choices {
            let delta = choice.get("delta").cloned().unwrap_or(Value::Null);

            if let Some(r) = delta.get("reasoning_content").and_then(|x| x.as_str())
                && !r.is_empty() {
                    self.any_content = true;
                    if !self.reasoning_started {
                        out.push(StreamChunk::BlockStart {
                            index: 0,
                            block_type: ContentBlockType::Reasoning,
                        });
                        self.reasoning_started = true;
                    }
                    out.push(StreamChunk::ReasoningDelta { index: 0, text: r.to_string() });
                }

            if let Some(c) = delta.get("content").and_then(|x| x.as_str())
                && !c.is_empty() {
                    self.any_content = true;
                    let index = if self.reasoning_started { 1 } else { 0 };
                    if !self.text_started {
                        out.push(StreamChunk::BlockStart { index, block_type: ContentBlockType::Text });
                        self.text_started = true;
                    }
                    out.push(StreamChunk::TextDelta { index, text: c.to_string() });
                }

            if let Some(tcs) = delta.get("tool_calls").and_then(|x| x.as_array()) {
                self.any_content = true;
                let base = (self.reasoning_started as usize) + (self.text_started as usize);
                for tc in tcs {
                    let provider_index = tc.get("index").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                    let block_index = base + provider_index;
                    let block = self.tool_blocks.entry(provider_index).or_insert(ToolBlock {
                        id: None,
                        name_sent: false,
                        started: false,
                    });
                    let id = tc
                        .get("id")
                        .and_then(|x| x.as_str())
                        .map(CallId::new)
                        .or_else(|| block.id.clone());
                    let name = tc.get("function").and_then(|f| f.get("name")).and_then(|x| x.as_str());
                    let arguments = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");

                    if !block.started {
                        out.push(StreamChunk::BlockStart {
                            index: block_index,
                            block_type: ContentBlockType::ToolCall,
                        });
                        block.started = true;
                    }
                    let name_once = if block.name_sent { None } else { name.map(|n| n.to_string()) };
                    if name_once.is_some() {
                        block.name_sent = true;
                    }
                    if let Some(i) = id {
                        block.id = Some(i.clone());
                    }
                    out.push(StreamChunk::ToolCallDelta {
                        index: block_index,
                        id: block.id.clone().unwrap_or_else(|| CallId::new(format!("call-{block_index}"))),
                        name: name_once,
                        arguments_delta: arguments.to_string(),
                    });
                }
            }

            if let Some(fr) = choice.get("finish_reason").and_then(|x| x.as_str())
                && !fr.is_empty() && self.finish_reason.is_none() {
                    self.finish_reason = Some(fr.to_string());
                }
        }
    }

    fn final_finish(&self) -> StreamChunk {
        let reason = match self.finish_reason.as_deref() {
            Some("length") => FinishReason::MaxTokens,
            Some("tool_calls") => FinishReason::ToolCalls,
            _ => {
                if !self.any_content {
                    FinishReason::Error {
                        failure: LlmFailure {
                            message: "empty model response".to_string(),
                            code: dsh_llm::EMPTY_RESPONSE.to_string(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                        },
                    }
                } else {
                    FinishReason::Stop
                }
            }
        };
        StreamChunk::Finish { reason, replay_state: None }
    }
}

/// Extract one complete SSE event (up to and including a blank line) from the
/// front of `buf`, if present. Handles both `\n\n` and `\r\n\r\n`.
fn take_event(buf: &mut Vec<u8>) -> Option<String> {
    let (pos, sep_len) = if let Some(p) = buf.windows(2).position(|w| w == b"\n\n") {
        (p, 2)
    } else if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        (p, 4)
    } else {
        return None;
    };
    let event: Vec<u8> = buf.drain(..pos + sep_len).collect();
    Some(String::from_utf8_lossy(&event).into_owned())
}