//! Codex-compatible request_user_input wire types.
//!
//! These are intentionally kept separate from the TUI.  The tool, runtime bus,
//! and any future app-server transport can all use the exact same serde shape.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestUserInputQuestionOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestUserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(rename = "isOther", default)]
    pub is_other: bool,
    #[serde(rename = "isSecret", default)]
    pub is_secret: bool,
    pub options: Option<Vec<RequestUserInputQuestionOption>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestUserInputArgs {
    pub questions: Vec<RequestUserInputQuestion>,
    #[serde(rename = "autoResolutionMs", skip_serializing_if = "Option::is_none")]
    pub auto_resolution_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestUserInputAnswer {
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestUserInputResponse {
    pub answers: HashMap<String, RequestUserInputAnswer>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_codex_camel_case_fields() {
        let args = RequestUserInputArgs {
            questions: vec![RequestUserInputQuestion {
                id: "mode".into(),
                header: "Mode".into(),
                question: "Choose".into(),
                is_other: true,
                is_secret: false,
                options: None,
            }],
            auto_resolution_ms: Some(2500),
        };
        let value = serde_json::to_value(args).unwrap();
        assert_eq!(value["autoResolutionMs"], 2500);
        assert_eq!(value["questions"][0]["isOther"], true);
        assert_eq!(value["questions"][0]["isSecret"], false);
    }
}
