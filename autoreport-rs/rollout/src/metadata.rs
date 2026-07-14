//! Rollout metadata and JSONL envelopes.

use crate::ResponseItem;
use serde::{Deserialize, Serialize};

/// First line of a rollout file (codex `SessionMeta`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMeta {
    pub conversation_id: String,
    pub cli_version: String,
    pub timestamp: String,
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
