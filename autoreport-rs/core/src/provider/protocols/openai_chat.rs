//! OpenAI Chat Completions streaming protocol.
//!
//! Codex removed the Chat Completions wire API (`model-provider-info` keeps
//! only `WireApi::Responses`), so there is no codex precedent here. This module
//! ports the project's existing Chat Completions parsing (used by
//! deepseek/openrouter/google compat providers) into a [`SseProtocol`] impl.
//!
//! Per-frame: read `choices/0/delta/{content,reasoning_content,tool_calls,
//! refusal}` and top-level `usage`. Tool-call fragments accumulate by `index`
//! across frames; the assembled `Vec<ToolCall>` and captured `usage` are
//! emitted in the terminal `flush()` chunk — matching the prior inline
//! behavior of `openai::run_stream`.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::provider::sse_protocol::FrameOutcome;
use crate::provider::sse_protocol::SseProtocol;
use crate::provider::types::{LLMStreamChunk, ToolCall, Usage};

#[derive(Default)]
struct ToolAccum {
    id: String,
    name: String,
    args: String,
}

/// Stateful Chat Completions parser. Accumulates tool-call fragments and usage
/// across frames; emits a single terminal chunk on `flush`.
#[derive(Default)]
pub(crate) struct OpenAIChatProtocol {
    tool_acc: BTreeMap<u64, ToolAccum>,
    // `build_body` sets `stream_options.include_usage`, so the terminal chunk
    // carries a top-level `usage` (prompt_tokens / completion_tokens). Capture
    // it so streamed turns report real token usage like Anthropic.
    usage: Option<Usage>,
}

impl OpenAIChatProtocol {
    pub(crate) fn new() -> Self {
        Self::default()
    }
}

impl SseProtocol for OpenAIChatProtocol {
    fn parse_frame(&mut self, payload: &str) -> FrameOutcome {
        let Ok(ev) = serde_json::from_str::<Value>(payload) else {
            return FrameOutcome::Ignore;
        };
        if let Some(err) = ev.get("error") {
            let msg = err
                .get("message")
                .and_then(|x| x.as_str())
                .unwrap_or("stream error")
                .to_string();
            return FrameOutcome::Error(anyhow::anyhow!(msg));
        }
        if let Some(u) = ev.get("usage") {
            self.usage = Some(Usage {
                input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                output_tokens: u
                    .get("completion_tokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0),
            });
        }
        if let Some(stop_reason) = ev
            .pointer("/choices/0/finish_reason")
            .and_then(|value| value.as_str())
        {
            log::debug!("openai-compatible stream finish_reason={stop_reason}");
        }
        let Some(delta) = ev.pointer("/choices/0/delta").cloned() else {
            return FrameOutcome::Ignore;
        };

        let mut chunks: Vec<LLMStreamChunk> = Vec::new();
        if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                chunks.push(LLMStreamChunk {
                    delta: Some(content.to_string()),
                    ..empty_chunk()
                });
            }
        }
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|c| c.as_str()) {
            if !reasoning.is_empty() {
                chunks.push(LLMStreamChunk {
                    thinking_delta: Some(reasoning.to_string()),
                    ..empty_chunk()
                });
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
            for c in calls {
                let index = c.get("index").and_then(|x| x.as_u64()).unwrap_or(0);
                let entry = self.tool_acc.entry(index).or_default();
                if let Some(id) = c.get("id").and_then(|x| x.as_str()) {
                    entry.id = id.to_string();
                }
                if let Some(name) = c.pointer("/function/name").and_then(|x| x.as_str()) {
                    entry.name = name.to_string();
                }
                if let Some(args) = c.pointer("/function/arguments").and_then(|x| x.as_str()) {
                    entry.args.push_str(args);
                }
            }
        }
        if let Some(refusal) = delta.get("refusal").and_then(|c| c.as_str()) {
            if !refusal.is_empty() {
                chunks.push(LLMStreamChunk {
                    delta: Some(refusal.to_string()),
                    ..empty_chunk()
                });
            }
        }
        if chunks.is_empty() {
            FrameOutcome::Ignore
        } else {
            FrameOutcome::Chunks(chunks)
        }
    }

    fn flush(&mut self) -> FrameOutcome {
        let tool_calls: Vec<ToolCall> = std::mem::take(&mut self.tool_acc)
            .into_iter()
            .map(|a| ToolCall {
                id: a.1.id,
                name: a.1.name,
                arguments: if a.1.args.trim().is_empty() {
                    Value::Object(Default::default())
                } else {
                    serde_json::from_str(&a.1.args)
                        .unwrap_or_else(|_| Value::Object(Default::default()))
                },
            })
            .collect();
        FrameOutcome::Terminal(LLMStreamChunk {
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
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
