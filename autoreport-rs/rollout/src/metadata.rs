//! Rollout metadata and JSONL envelopes.

use crate::ResponseItem;
use serde::{Deserialize, Serialize};

/// First line of a rollout file (codex `SessionMeta`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMeta {
    /// Codex-compatible stable session identifier.
    #[serde(default)]
    pub session_id: String,
    /// Unique rollout identifier. AutoReport uses the same UUID as the file.
    #[serde(default)]
    pub id: String,
    /// AutoReport's logical conversation label. Codex identifies a thread by
    /// `id`; this extra field is retained for the multi-agent UI and is
    /// ignored by Codex readers.
    #[serde(default)]
    pub conversation_id: String,
    pub cli_version: String,
    pub timestamp: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default = "default_originator")]
    pub originator: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default)]
    pub model_provider: Option<String>,
    /// Standard Codex metadata field used to identify an AutoReport agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_role: Option<String>,
}

fn default_originator() -> String {
    "autoreport-cli".to_string()
}

fn default_source() -> String {
    "cli".to_string()
}

/// Codex on-disk wire envelope. Every rollout line is one of these, never a
/// bare item:
///   {"timestamp":"...","type":"session_meta","payload":{...}}
///   {"timestamp":"...","type":"response_item","payload":{"type":"message",...}}
/// This makes files inspectable with `jq 'select(.type=="response_item")'`
/// and listable by codex tooling (`codex list-threads`), which deserialize
/// `RolloutLine` directly.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub(crate) struct RolloutLine {
    pub(crate) timestamp: String,
    #[serde(flatten)]
    pub(crate) payload: RolloutPayload,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub(crate) enum RolloutPayload {
    SessionMeta(SessionMeta),
    ResponseItem(ResponseItem),
}
