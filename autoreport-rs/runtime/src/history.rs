//! Conversation-item conversion and bounded history helpers.

use autoreport_core::provider::types::{Message, ToolCall as ProviderToolCall};
use autoreport_rollout::ResponseItem;
use serde_json::Value;

pub(crate) fn inject_before_last_user(out: &mut Vec<Message>, msg: Message) {
    let pos = out.iter().rposition(|m| m.role == "user");
    match pos {
        Some(i) => out.insert(i, msg),
        None => out.push(msg),
    }
}

/// codex `items → messages` conversion: turn `ResponseItem`s into the provider's
/// chat-message wire format. Consecutive `FunctionCall`s fold into one
/// assistant message's `tool_calls` (matching how the model emitted them). A
/// `Reasoning` item preceding an assistant message is attached to it as the
/// `thinking` field (with its signature), so signed reasoning round-trips
/// across turns (codex `reasoning.encrypted_content`).
pub(crate) fn items_to_messages(items: &[ResponseItem]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    let mut pending: Vec<ProviderToolCall> = Vec::new();
    // (thinking text, signature) carried from a preceding Reasoning item to
    // the next assistant message.
    let mut pending_thinking: Option<(String, Option<String>)> = None;
    // Flush pending tool calls as one assistant message. A preceding `Reasoning`
    // item belongs to *this* assistant turn — its signature must round-trip on
    // the same message that carries the tool_calls (providers reject a thinking
    // signature attached to a later, unrelated message), so consume
    // `pending_thinking` here rather than letting it leak onto the next turn.
    let flush = |pending: &mut Vec<ProviderToolCall>,
                 out: &mut Vec<Message>,
                 pending_thinking: &mut Option<(String, Option<String>)>| {
        if !pending.is_empty() {
            let calls = std::mem::take(pending);
            let (thinking, signature): (Option<String>, Option<String>) = pending_thinking
                .take()
                .map(|(t, s)| (Some(t), s))
                .unwrap_or((None::<String>, None::<String>));
            out.push(Message {
                role: "assistant".into(),
                content: String::new(),
                tool_calls: Some(calls),
                tool_call_id: None,
                thinking,
                thinking_signature: signature,
            });
        }
    };
    for item in items {
        match item {
            ResponseItem::Message { role, content, .. } => {
                flush(&mut pending, &mut out, &mut pending_thinking);
                let text: String = content.iter().map(|c| c.text().to_string()).collect();
                let (thinking, signature) = if role == "assistant" {
                    match pending_thinking.take() {
                        Some((t, s)) => (Some(t), s),
                        None => (None, None),
                    }
                } else {
                    pending_thinking = None;
                    (None, None)
                };
                out.push(Message {
                    role: role.clone(),
                    content: text,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking,
                    thinking_signature: signature,
                });
            }
            ResponseItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
                pending.push(ProviderToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: args,
                });
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                flush(&mut pending, &mut out, &mut pending_thinking);
                out.push(Message::tool_result(call_id, output));
            }
            ResponseItem::Reasoning {
                content,
                encrypted_content,
                ..
            } => {
                // Stash for the next assistant message. Only carry the
                // signature when present — providers reject unsigned thinking.
                let text = content
                    .as_ref()
                    .map(|items| {
                        items
                            .iter()
                            .map(|item| item.text())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let sig = encrypted_content.clone();
                flush(&mut pending, &mut out, &mut pending_thinking);
                if !text.is_empty() || sig.as_deref().is_some_and(|sig| !sig.is_empty()) {
                    pending_thinking = Some((text, sig.filter(|s| !s.is_empty())));
                }
            }
            ResponseItem::Compaction { encrypted_content } => {
                if encrypted_content == "__autoreport_retract_last_user__" {
                    // This is an append-only rollout tombstone. Runtime
                    // history is normalized on resume; tolerate it here too
                    // so a live context never sends the marker to the model.
                    continue;
                }
                // Re-feed the compaction summary to the model as a context note
                // (otherwise compact() would trim history and tell nobody).
                flush(&mut pending, &mut out, &mut pending_thinking);
                pending_thinking = None;
                let text = format!(
                    "[This conversation was compacted. Summary of prior context:]\n\n{}",
                    encrypted_content
                );
                out.push(Message {
                    role: "user".into(),
                    content: text,
                    tool_calls: None,
                    tool_call_id: None,
                    thinking: None,
                    thinking_signature: None,
                });
            }
            ResponseItem::Other => {
                // Unknown / codex-only item (local_shell_call, web_search_call,
                // compaction_trigger, …). Tolerated on resume via
                // `#[serde(other)]`; not part of the request history we send.
            }
        }
    }
    flush(&mut pending, &mut out, &mut pending_thinking);
    // Normalize: every assistant tool_call must have a matching tool result,
    // else providers reject the request with a 400. Inject a synthetic
    // "[aborted]" result for any orphaned call id. (codex:
    // `context_manager::normalize::ensure_call_outputs_present`.)
    let answered: std::collections::HashSet<String> = out
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    let orphan_ids: Vec<String> = out
        .iter()
        .filter_map(|m| m.tool_calls.as_ref())
        .flatten()
        .map(|c| c.id.clone())
        .filter(|id| !answered.contains(id))
        .collect();
    for id in orphan_ids {
        out.push(Message::tool_result(
            id,
            "[aborted: turn interrupted, no tool output]",
        ));
    }
    out
}

/// Forward a bus message into the session Op queue (codex session input).
pub(crate) fn transcript_text(items: &[ResponseItem]) -> String {
    let mut s = String::new();
    for item in items {
        let label = match item {
            ResponseItem::Message { role, .. } => role.as_str(),
            ResponseItem::FunctionCall { name, .. } => name.as_str(),
            ResponseItem::FunctionCallOutput { .. } => "tool",
            ResponseItem::Reasoning { .. } => "reasoning",
            ResponseItem::Compaction { .. } => "compaction",
            ResponseItem::Other => "other",
        };
        if let Some(t) = item.text() {
            s.push_str(&format!("[{label}] {t}\n"));
        }
    }
    s
}

/// Build a Reasoning item, attaching the signed blob when the provider
/// returned one so it can be echoed back on the next turn.
pub(crate) fn make_reasoning(text: String, signature: Option<String>) -> ResponseItem {
    match signature {
        Some(sig) if !sig.is_empty() => ResponseItem::reasoning_signed(text, sig),
        _ => ResponseItem::reasoning(text),
    }
}

/// Bound the size of a tool result before it enters history/rollout. A chatty
/// `exec` or a `cat` of a large file can otherwise inject megabytes that flow
/// straight into the next completion request (and, under a runaway tool loop,
/// starve the compacter). Truncates at a UTF-8 char boundary so it cannot panic
/// on multi-byte content.
pub(crate) fn truncate_for_history(mut s: String) -> String {
    const MAX_TOOL_OUTPUT_BYTES: usize = 32_000;
    if s.len() <= MAX_TOOL_OUTPUT_BYTES {
        return s;
    }
    let mut end = MAX_TOOL_OUTPUT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    s.push_str("\n…(truncated)");
    s
}

/// Best-effort extraction of the written file paths from a tool call, for
/// manifest tracking.
pub(crate) fn extract_paths(tool_name: &str, _args: &Value, result: &Value) -> Vec<String> {
    match tool_name {
        // apply_patch's result is `{"applied": [ {"add": p}, {"delete": p},
        // {"update": p}, {"move": from, "to": to}, ... ]}` — extract every
        // path value the hunk touched.
        "apply_patch" => {
            let mut out = Vec::new();
            if let Some(applied) = result.get("applied").and_then(|v| v.as_array()) {
                for item in applied {
                    for key in ["add", "delete", "update", "move", "to"] {
                        if let Some(path) = item.get(key).and_then(|v| v.as_str()) {
                            out.push(path.to_string());
                        }
                    }
                }
            }
            out
        }
        // exec reports the workspace paths it observed being written.
        "exec" => result
            .get("written_paths")
            .and_then(|v| v.as_array())
            .map(|paths| {
                paths
                    .iter()
                    .filter_map(|path| path.as_str().map(ToString::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}
