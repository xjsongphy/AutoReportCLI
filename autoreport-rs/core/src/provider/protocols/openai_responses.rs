//! OpenAI Responses API streaming protocol.
//!
//! Mirrors codex's `codex-api/src/sse/responses.rs` event taxonomy
//! (`process_responses_event`): the match arms here follow codex's
//! `response.*` event-type strings one-for-one. The project keeps extra
//! gateway-compat accumulation on top (per-call_id `FunctionAccum`, dedup sets
//! for servers that omit deltas, refusal handling) — that is the project's own
//! feature layer; the event-parse structure stays aligned with codex.

use serde_json::{Value, json};
use std::collections::BTreeSet;

use crate::provider::sse_protocol::FrameOutcome;
use crate::provider::sse_protocol::SseProtocol;
use crate::provider::types::{LLMStreamChunk, ToolCall, Usage};

#[derive(Default)]
struct FunctionAccum {
    call_id: String,
    name: String,
    arguments: String,
    custom: bool,
}

/// Keep function calls in the order in which Responses emitted them. A sorted
/// map would make parallel calls appear in lexicographic id order, which is not
/// the provider's output order and can change tool side effects.
fn call_entry<'a>(calls: &'a mut Vec<(String, FunctionAccum)>, key: &str) -> &'a mut FunctionAccum {
    let index = calls
        .iter()
        .position(|(existing, _)| existing == key)
        .unwrap_or_else(|| {
            calls.push((key.to_string(), FunctionAccum::default()));
            calls.len() - 1
        });
    &mut calls[index].1
}

pub(crate) fn parse_usage(value: &Value) -> Usage {
    Usage {
        input_tokens: value
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: value
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn reasoning_event_key(event: &Value) -> String {
    if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
        return format!("item:{item_id}");
    }
    if let Some(summary_index) = event.get("summary_index").and_then(Value::as_u64) {
        return format!("summary:{summary_index}");
    }
    if let Some(content_index) = event.get("content_index").and_then(Value::as_u64) {
        return format!("content:{content_index}");
    }
    "anonymous".to_string()
}

/// Stateful Responses API parser. Accumulates function-call arguments and
/// dedups finalized text/reasoning against observed deltas (gateway compat).
#[derive(Default)]
pub(crate) struct OpenAIResponsesProtocol {
    calls: Vec<(String, FunctionAccum)>,
    text_items_with_delta: BTreeSet<String>,
    reasoning_items_with_delta: BTreeSet<String>,
    /// A few Responses-compatible gateways omit `item_id` on the finalized text
    /// event. Remember that case so the later output-item event does not replay
    /// the same full body a second time.
    anonymous_text_emitted: bool,
    usage: Option<Usage>,
    reasoning_signature: Option<String>,
    completed: bool,
}

impl OpenAIResponsesProtocol {
    pub(crate) fn new() -> Self {
        Self::default()
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

impl SseProtocol for OpenAIResponsesProtocol {
    fn parse_frame(&mut self, payload: &str) -> FrameOutcome {
        let Ok(event): std::result::Result<Value, _> = serde_json::from_str(payload) else {
            return FrameOutcome::Ignore;
        };
        let event_type = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mut chunks: Vec<LLMStreamChunk> = Vec::new();
        match event_type {
            // Codex keeps reasoning in the response protocol but does not render
            // it in the normal transcript. Forward it through the thinking_delta
            // channel so the runtime can detect a reasoning-only turn and ask
            // once for the visible final answer.
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let key = reasoning_event_key(&event);
                self.reasoning_items_with_delta.insert(key);
                if let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && !delta.is_empty()
                {
                    chunks.push(LLMStreamChunk {
                        thinking_delta: Some(delta.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            "response.reasoning_summary_text.done" | "response.reasoning_text.done" => {
                // Some gateways omit the delta events and only send the finalized
                // reasoning text. Preserving it keeps the runtime's reasoning-only
                // recovery path correct.
                let key = reasoning_event_key(&event);
                if !self.reasoning_items_with_delta.contains(&key)
                    && let Some(text) = event.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    chunks.push(LLMStreamChunk {
                        thinking_delta: Some(text.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            "response.output_text.delta" => {
                if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
                    self.text_items_with_delta.insert(item_id.to_string());
                } else {
                    self.anonymous_text_emitted = true;
                }
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    chunks.push(LLMStreamChunk {
                        delta: Some(delta.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            // `response.output_text.done` is the canonical finalized text event.
            // Most servers also send deltas; some compatible gateways only send
            // this event. Emit only when no delta for the same item was seen.
            "response.output_text.done" => {
                let item_key = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .map(|index| format!("output-index:{index}"))
                    });
                if item_key
                    .as_deref()
                    .is_none_or(|key| !self.text_items_with_delta.contains(key))
                    && let Some(text) = event.get("text").and_then(Value::as_str)
                    && !text.is_empty()
                {
                    if let Some(key) = item_key {
                        self.text_items_with_delta.insert(key);
                    } else {
                        self.anonymous_text_emitted = true;
                    }
                    chunks.push(LLMStreamChunk {
                        delta: Some(text.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            // A few gateways finalize the content part before the output item.
            "response.content_part.done" => {
                let item_key = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| {
                        event
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .map(|index| format!("output-index:{index}"))
                    });
                if item_key
                    .as_deref()
                    .is_none_or(|key| !self.text_items_with_delta.contains(key))
                    && let Some(part) = event.get("part")
                {
                    let text = match part.get("type").and_then(Value::as_str) {
                        Some("output_text") => part.get("text").and_then(Value::as_str),
                        Some("refusal") => part.get("refusal").and_then(Value::as_str),
                        _ => None,
                    };
                    if let Some(text) = text.filter(|text| !text.is_empty()) {
                        if let Some(key) = item_key {
                            self.text_items_with_delta.insert(key);
                        } else {
                            self.anonymous_text_emitted = true;
                        }
                        chunks.push(LLMStreamChunk {
                            delta: Some(text.to_string()),
                            ..empty_chunk()
                        });
                    }
                }
            }
            // Refusals use a parallel streaming event family; still visible body
            // text, so forward through the unified delta channel.
            "response.refusal.delta" => {
                if let Some(item_id) = event.get("item_id").and_then(Value::as_str) {
                    self.text_items_with_delta.insert(item_id.to_string());
                } else {
                    self.anonymous_text_emitted = true;
                }
                if let Some(delta) = event.get("delta").and_then(Value::as_str)
                    && !delta.is_empty()
                {
                    chunks.push(LLMStreamChunk {
                        delta: Some(delta.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            "response.refusal.done" => {
                let item_key = event.get("item_id").and_then(Value::as_str);
                if item_key.is_some_and(|key| self.text_items_with_delta.contains(key)) {
                    // nothing
                } else if let Some(refusal) = event.get("refusal").and_then(Value::as_str)
                    && !refusal.is_empty()
                {
                    if let Some(key) = item_key {
                        self.text_items_with_delta.insert(key.to_string());
                    } else {
                        self.anonymous_text_emitted = true;
                    }
                    chunks.push(LLMStreamChunk {
                        delta: Some(refusal.to_string()),
                        ..empty_chunk()
                    });
                }
            }
            "response.output_item.added" => {
                if let Some(item) = event.get("item")
                    && matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call" | "custom_tool_call")
                    )
                {
                    let key = event
                        .get("item_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    let entry = call_entry(&mut self.calls, &key);
                    entry.call_id = item
                        .get("call_id")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                    entry.name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    entry.arguments = item
                        .get("arguments")
                        .or_else(|| item.get("input"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    entry.custom =
                        item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
                }
            }
            "response.function_call_arguments.delta" => {
                let key = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("call_id").and_then(Value::as_str))
                    .unwrap_or_default();
                let entry = call_entry(&mut self.calls, key);
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    entry.arguments.push_str(delta);
                }
            }
            "response.custom_tool_call_input.delta" => {
                let key = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("call_id").and_then(Value::as_str))
                    .unwrap_or_default();
                let entry = call_entry(&mut self.calls, key);
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    entry.arguments.push_str(delta);
                }
                if let Some(call_id) = event.get("call_id").and_then(Value::as_str) {
                    entry.call_id = call_id.to_string();
                }
            }
            // The Responses API emits this when argument JSON is finalized. Some
            // gateways omit `response.output_item.done`, so use this as
            // authoritative call metadata too.
            "response.function_call_arguments.done" => {
                let key = event
                    .get("item_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("call_id").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string();
                let entry = call_entry(&mut self.calls, &key);
                entry.call_id = event
                    .get("call_id")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("item_id").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string();
                if let Some(name) = event.get("name").and_then(Value::as_str) {
                    entry.name = name.to_string();
                }
                if let Some(arguments) = event.get("arguments").and_then(Value::as_str) {
                    entry.arguments = arguments.to_string();
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    if item.get("type").and_then(Value::as_str) == Some("reasoning") {
                        self.reasoning_signature = item
                            .get("encrypted_content")
                            .and_then(Value::as_str)
                            .filter(|signature| !signature.is_empty())
                            .map(ToOwned::to_owned);
                    } else if item.get("type").and_then(Value::as_str) == Some("message") {
                        let item_id = event
                            .get("item_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str));
                        if !self.anonymous_text_emitted
                            && item_id.is_none_or(|id| !self.text_items_with_delta.contains(id))
                        {
                            let text = item
                                .get("content")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|part| match part.get("type").and_then(Value::as_str) {
                                    Some("output_text") => part.get("text").and_then(Value::as_str),
                                    Some("refusal") => part.get("refusal").and_then(Value::as_str),
                                    _ => None,
                                })
                                .collect::<String>();
                            if !text.is_empty() {
                                chunks.push(LLMStreamChunk {
                                    delta: Some(text.to_string()),
                                    ..empty_chunk()
                                });
                            }
                        }
                    }
                    if matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("function_call" | "custom_tool_call")
                    ) {
                        let key = event
                            .get("item_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or_default()
                            .to_string();
                        let entry = call_entry(&mut self.calls, &key);
                        entry.custom =
                            item.get("type").and_then(Value::as_str) == Some("custom_tool_call");
                        entry.call_id = item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .or_else(|| item.get("id").and_then(Value::as_str))
                            .unwrap_or_default()
                            .to_string();
                        entry.name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        if let Some(arguments) = item
                            .get("arguments")
                            .or_else(|| item.get("input"))
                            .and_then(Value::as_str)
                        {
                            entry.arguments = arguments.to_string();
                        }
                    }
                }
            }
            "response.completed" => {
                self.completed = true;
                self.usage = event.pointer("/response/usage").map(parse_usage);
            }
            "response.incomplete" => {
                let reason = event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return FrameOutcome::Error(anyhow::anyhow!(format!(
                    "Responses API returned an incomplete response: {reason}"
                )));
            }
            "response.failed" | "error" => {
                let message = event
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| event.pointer("/error/message").and_then(Value::as_str))
                    .unwrap_or("Responses API stream failed");
                return FrameOutcome::Error(anyhow::anyhow!(message.to_string()));
            }
            _ => {}
        }
        if chunks.is_empty() {
            FrameOutcome::Ignore
        } else {
            FrameOutcome::Chunks(chunks)
        }
    }

    fn flush(&mut self) -> FrameOutcome {
        if !self.completed {
            return FrameOutcome::Error(anyhow::anyhow!(
                "Responses API stream closed before response.completed"
            ));
        }
        let tool_calls = std::mem::take(&mut self.calls)
            .into_iter()
            .map(|(_, call)| ToolCall {
                id: call.call_id,
                name: call.name,
                arguments: serde_json::from_str(&call.arguments).unwrap_or_else(|_| {
                    if call.custom {
                        Value::String(call.arguments)
                    } else {
                        json!({})
                    }
                }),
            })
            .collect::<Vec<_>>();
        FrameOutcome::Terminal(LLMStreamChunk {
            thinking_signature: self.reasoning_signature.take(),
            tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
            done: true,
            usage: self.usage.take(),
            ..empty_chunk()
        })
    }
}
