//! Anthropic Messages streaming protocol.
//!
//! Codex does not talk to Anthropic natively (it routes everything through the
//! OpenAI Responses API), so there is no codex precedent. This module ports the
//! project's existing Anthropic parsing into a [`SseProtocol`] impl.
//!
//! Per-frame state machine over the `type` field: `message_start` captures
//! input tokens; `content_block_start` opens a text / `tool_use` / `thinking`
//! block; `content_block_delta` streams `text_delta` / `input_json_delta` /
//! `thinking_delta` / `signature_delta`; `content_block_stop` finalizes a tool
//! call and emits the accumulated thinking signature; `message_delta` carries
//! the final output tokens; `error` is terminal.

use serde_json::Value;

use crate::provider::sse_protocol::FrameOutcome;
use crate::provider::sse_protocol::SseProtocol;
use crate::provider::types::{LLMStreamChunk, ToolCall, Usage};

#[derive(Default)]
struct ThinkingAcc {
    text: String,
    signature: String,
}

struct BlockState {
    tool: Option<(String, String, String)>,
    thinking: Option<ThinkingAcc>,
}

/// Stateful Anthropic Messages parser. Owns per-block state, the assembled
/// tool-call list, and the running usage.
#[derive(Default)]
pub(crate) struct AnthropicProtocol {
    current: Option<BlockState>,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
}

impl AnthropicProtocol {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl SseProtocol for AnthropicProtocol {
    fn parse_frame(&mut self, payload: &str) -> FrameOutcome {
        let Ok(ev) = serde_json::from_str::<Value>(payload) else {
            return FrameOutcome::Ignore;
        };
        let event_type = ev.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match event_type {
            "content_block_start" => {
                let block = ev.pointer("/content_block").cloned().unwrap_or(Value::Null);
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => self.current = Some(BlockState { tool: None, thinking: None }),
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.current = Some(BlockState {
                            tool: Some((id, name, String::new())),
                            thinking: None,
                        });
                    }
                    Some("thinking") => {
                        // Extended-thinking block: text streams via
                        // `thinking_delta`, signature via `signature_delta`.
                        // Both must be echoed back to continue the turn.
                        self.current = Some(BlockState {
                            tool: None,
                            thinking: Some(ThinkingAcc::default()),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = ev.get("delta").cloned().unwrap_or(Value::Null);
                match delta.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = delta.get("text").and_then(|x| x.as_str()) {
                            return FrameOutcome::Chunks(vec![LLMStreamChunk {
                                delta: Some(t.to_string()),
                                ..empty_chunk()
                            }]);
                        }
                    }
                    Some("input_json_delta") => {
                        if let (Some(s), Some((_, _, acc))) = (
                            delta.get("partial_json").and_then(|x| x.as_str()),
                            self.current.as_mut().and_then(|c| c.tool.as_mut()),
                        ) {
                            acc.push_str(s);
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = delta.get("thinking").and_then(|x| x.as_str()) {
                            if let Some(acc) =
                                self.current.as_mut().and_then(|c| c.thinking.as_mut())
                            {
                                acc.text.push_str(t);
                            }
                            return FrameOutcome::Chunks(vec![LLMStreamChunk {
                                thinking_delta: Some(t.to_string()),
                                ..empty_chunk()
                            }]);
                        }
                    }
                    Some("signature_delta") => {
                        // Anthropic delivers the thinking block's signature as
                        // a separate delta; accumulate it and emit once at
                        // content_block_stop.
                        if let Some(sig) = delta.get("signature").and_then(|x| x.as_str()) {
                            if let Some(acc) =
                                self.current.as_mut().and_then(|c| c.thinking.as_mut())
                            {
                                acc.signature.push_str(sig);
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let mut chunks: Vec<LLMStreamChunk> = Vec::new();
                if let Some(state) = self.current.take() {
                    if let Some((id, name, json_str)) = state.tool {
                        let args = if json_str.trim().is_empty() {
                            Value::Object(Default::default())
                        } else {
                            // A parse failure usually means the tool arguments
                            // were truncated mid-stream (max_tokens). Fall back
                            // to an empty object so the tool surfaces a clean
                            // "missing argument" error instead of executing with
                            // null arguments.
                            serde_json::from_str(&json_str).unwrap_or_else(|e| {
                                log::warn!(
                                    "anthropic tool `{name}` args parse failed ({e}); likely truncated"
                                );
                                Value::Object(Default::default())
                            })
                        };
                        self.tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments: args,
                        });
                    }
                    // Emit the accumulated thinking signature once the thinking
                    // block closes, so the agent loop can store it and echo it
                    // back on the next turn.
                    if let Some(acc) = state.thinking {
                        if !acc.signature.is_empty() {
                            chunks.push(LLMStreamChunk {
                                thinking_signature: Some(acc.signature),
                                ..empty_chunk()
                            });
                        }
                    }
                }
                if chunks.is_empty() {
                    return FrameOutcome::Ignore;
                }
                return FrameOutcome::Chunks(chunks);
            }
            "message_start" => {
                // Anthropic streams input_tokens here (on /message/usage);
                // message_delta only carries output_tokens. Without this, every
                // streamed turn reports input_tokens = 0.
                if let Some(u) = ev.pointer("/message/usage") {
                    self.usage = Some(Usage {
                        input_tokens: u
                            .get("input_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                        output_tokens: u
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                    });
                }
            }
            "message_delta" => {
                // message_delta carries the final output_tokens (and
                // stop_reason). Preserve input_tokens from message_start.
                if let Some(stop_reason) = ev
                    .pointer("/delta/stop_reason")
                    .and_then(|value| value.as_str())
                {
                    log::debug!("anthropic stream stop_reason={stop_reason}");
                }
                let in_tok = self.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0);
                if let Some(u) = ev.pointer("/usage") {
                    self.usage = Some(Usage {
                        input_tokens: in_tok,
                        output_tokens: u
                            .get("output_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                    });
                }
            }
            "message_stop" => {}
            "error" => {
                let msg = ev
                    .pointer("/error/message")
                    .and_then(|x| x.as_str())
                    .unwrap_or("anthropic stream error")
                    .to_string();
                return FrameOutcome::Error(anyhow::anyhow!(msg));
            }
            _ => {}
        }
        FrameOutcome::Ignore
    }

    fn flush(&mut self) -> FrameOutcome {
        FrameOutcome::Terminal(LLMStreamChunk {
            tool_calls: if self.tool_calls.is_empty() {
                None
            } else {
                Some(std::mem::take(&mut self.tool_calls))
            },
            done: true,
            usage: self.usage.take(),
            ..empty_chunk()
        })
    }
}

fn empty_chunk() -> LLMStreamChunk {
    LLMStreamChunk {
        delta: None,
        thinking_delta: None,
        thinking_signature: None,
        tool_calls: None,
        done: false,
        usage: None,
    }
}
