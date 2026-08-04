//! Codex-compatible conversation item types.
//!
//! Vendored data model + on-disk format from codex (`codex-protocol::ResponseItem`
//! and `codex-rollout`): every conversation item is a `ResponseItem` tagged
//! `{"type": ...}` (snake_case), serialized one-per-line as append-only JSONL
//! under `$AUTOREPORT_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<id>.jsonl`, preceded by a
//! `SessionMeta` header line — the same shape codex writes, so files are
//! inspectable/replayable with the same tools (e.g. `jq`).
//!
//! We keep the variants our direct-API provider layer actually produces:
//! `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, and a
//! `Compaction` marker. codex's richer variants (local shell, web search, etc.)
//! round-trip through `Other` on read.

use serde::{Deserialize, Serialize};

/// One content piece of a message. codex uses `input_text` / `output_text` /
/// `text`; we accept all three on read and emit the role-appropriate one.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
    Text { text: String },
}

/// Reasoning content uses structured objects on Codex's wire, unlike the
/// plain strings used by the provider adapter internally.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContent {
    ReasoningText { text: String },
    Text { text: String },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummary {
    SummaryText { text: String },
}

impl ReasoningSummary {
    pub fn text(&self) -> &str {
        match self {
            Self::SummaryText { text } => text,
        }
    }
}

impl ReasoningContent {
    pub fn text(&self) -> &str {
        match self {
            Self::ReasoningText { text } | Self::Text { text } => text,
        }
    }
}

impl ContentItem {
    pub fn text(&self) -> &str {
        match self {
            ContentItem::InputText { text }
            | ContentItem::OutputText { text }
            | ContentItem::Text { text } => text,
        }
    }
    pub fn input(text: impl Into<String>) -> Self {
        ContentItem::InputText { text: text.into() }
    }
    pub fn output(text: impl Into<String>) -> Self {
        ContentItem::OutputText { text: text.into() }
    }
}

/// A single conversation item, codex `ResponseItem` shape (subset).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
    },
    Reasoning {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        #[serde(default)]
        summary: Vec<ReasoningSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        /// Opaque signed reasoning blob to echo back on the next turn (codex
        /// `encrypted_content`; Anthropic thinking `signature`). Absent on
        /// providers that don't sign reasoning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    FunctionCall {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        call_id: String,
        name: String,
        /// JSON-encoded arguments string (codex serializes arguments as a string).
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
        /// Tool error message, persisted separately from `output` so a failed
        /// tool renders as failed on resume (every renderer's red arm keys off
        /// `error`). Absent on older rollouts, where it deserializes to `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// codex emits a Compaction item when the context is summarized.
    Compaction {
        encrypted_content: String,
    },
    /// Catch-all for unknown / codex-only variants (e.g. `local_shell_call`,
    /// `web_search_call`, `compaction_trigger`) encountered when resuming a
    /// rollout written by codex or a future writer. Mirrors codex's `Other`
    /// arm (`protocol/src/models.rs`); `#[serde(other)]` makes reads tolerate
    /// forward-compatible type tags instead of dropping the whole line.
    /// We don't act on these items — they're skipped during history conversion.
    #[serde(other)]
    Other,
}

impl ResponseItem {
    pub fn user_message(text: impl Into<String>) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::input(text)],
        }
    }
    pub fn assistant_message(text: impl Into<String>) -> Self {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::output(text)],
        }
    }
    pub fn function_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments_json: String,
    ) -> Self {
        ResponseItem::FunctionCall {
            id: None,
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments_json,
        }
    }
    pub fn function_call_output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
            error: None,
        }
    }
    /// Like [`function_call_output`](Self::function_call_output) but persists the
    /// tool's `error` so a failed call is reconstructed as failed on resume.
    pub fn function_call_output_with_error(
        call_id: impl Into<String>,
        output: impl Into<String>,
        error: Option<String>,
    ) -> Self {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
            error,
        }
    }
    pub fn reasoning(text: impl Into<String>) -> Self {
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![ReasoningContent::ReasoningText { text: text.into() }]),
            encrypted_content: None,
        }
    }

    /// Reasoning with a signed blob (Anthropic `signature` / codex
    /// `encrypted_content`), so it can be echoed back to continue an
    /// extended-thinking turn.
    pub fn reasoning_signed(text: impl Into<String>, signature: impl Into<String>) -> Self {
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![ReasoningContent::ReasoningText { text: text.into() }]),
            encrypted_content: Some(signature.into()),
        }
    }

    /// The signed reasoning blob, if any (for echo-back on the next turn).
    pub fn reasoning_signature(&self) -> Option<&str> {
        match self {
            ResponseItem::Reasoning {
                encrypted_content: Some(s),
                ..
            } => Some(s),
            _ => None,
        }
    }

    /// Plain-text view for display / transcript summarization.
    pub fn text(&self) -> Option<String> {
        match self {
            ResponseItem::Message { content, .. } => Some(
                content
                    .iter()
                    .map(|c| c.text().to_string())
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            ResponseItem::FunctionCall {
                name, arguments, ..
            } => Some(format!("{}({})", name, arguments)),
            ResponseItem::FunctionCallOutput { output, .. } => Some(output.clone()),
            ResponseItem::Reasoning {
                content, summary, ..
            } => {
                let joined = content
                    .as_ref()
                    .map(|items| {
                        items
                            .iter()
                            .map(ReasoningContent::text)
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let trimmed = joined.trim();
                if !trimmed.is_empty() {
                    Some(trimmed.to_string())
                } else {
                    Some(
                        summary
                            .iter()
                            .map(ReasoningSummary::text)
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                }
            }
            ResponseItem::Compaction { .. } => None,
            ResponseItem::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_call_output_round_trips_error_and_omits_it_when_none() {
        // GAP-2: the error channel must survive disk so a failed tool is
        // reconstructed as failed on resume.
        let with_err = ResponseItem::function_call_output_with_error(
            "call-1",
            r#"{"stdout":"hi\n","returncode":0}"#,
            Some("boom".to_string()),
        );
        let json = serde_json::to_string(&with_err).unwrap();
        assert!(json.contains(r#""error":"boom""#), "error must be persisted: {json}");
        match serde_json::from_str::<ResponseItem>(&json).unwrap() {
            ResponseItem::FunctionCallOutput { call_id, output, error } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(output, r#"{"stdout":"hi\n","returncode":0}"#);
                assert_eq!(error.as_deref(), Some("boom"));
            }
            _ => panic!("deserialized to wrong variant"),
        }

        // When error is None the field is omitted, preserving the pre-change
        // on-disk shape for older readers.
        let no_err = ResponseItem::function_call_output("call-2", "ok");
        let json2 = serde_json::to_string(&no_err).unwrap();
        assert!(!json2.contains(r#""error""#), "error must be omitted when None: {json2}");
        match serde_json::from_str::<ResponseItem>(&json2).unwrap() {
            ResponseItem::FunctionCallOutput { error, .. } => assert!(error.is_none()),
            _ => panic!("deserialized to wrong variant"),
        }
    }

    #[test]
    fn legacy_function_call_output_without_error_field_still_loads() {
        // A rollout line written before the error field existed must still
        // deserialize, with error defaulting to None.
        let legacy = r#"{"type":"function_call_output","call_id":"c3","output":"ok"}"#;
        match serde_json::from_str::<ResponseItem>(legacy).unwrap() {
            ResponseItem::FunctionCallOutput { call_id, output, error } => {
                assert_eq!(call_id, "c3");
                assert_eq!(output, "ok");
                assert!(error.is_none());
            }
            _ => panic!("deserialized to wrong variant"),
        }
    }
}
