//! Codex-compatible `request_user_input` tool.

use crate::registry::{Tool, ToolOutput};
use async_trait::async_trait;
use autoreport_core::bus::Bus;
use autoreport_core::request_user_input::RequestUserInputArgs;
use autoreport_core::types::{AgentType, BusMessage};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

/// Minimum auto-resolution window in milliseconds. Matches codex's
/// `MIN_AUTO_RESOLUTION_MS` in `core/src/tools/handlers/request_user_input_spec.rs`.
///
/// Without clamping, a model passing `autoResolutionMs: 1` would auto-resolve
/// the question before the user can see or answer it.
const MIN_AUTO_RESOLUTION_MS: u64 = 60_000;
/// Maximum auto-resolution window in milliseconds. Matches codex's
/// `MAX_AUTO_RESOLUTION_MS` in the same spec file.
const MAX_AUTO_RESOLUTION_MS: u64 = 240_000;

/// Clamp an optional `autoResolutionMs` to the codex-supported range
/// `[MIN_AUTO_RESOLUTION_MS, MAX_AUTO_RESOLUTION_MS]`. `None` passes through
/// unchanged. Extracted as a pure helper so the clamp is unit-testable
/// independently of the bus publish path.
fn clamp_auto_resolution(ms: Option<u64>) -> Option<u64> {
    ms.map(|v| v.clamp(MIN_AUTO_RESOLUTION_MS, MAX_AUTO_RESOLUTION_MS))
}

pub struct RequestUserInputTool {
    bus: Bus,
    agent: AgentType,
}

impl RequestUserInputTool {
    pub fn new(bus: Bus, agent: AgentType) -> Self {
        Self { bus, agent }
    }
}

#[async_trait]
impl Tool for RequestUserInputTool {
    fn name(&self) -> &str {
        "request_user_input"
    }

    fn description(&self) -> &str {
        "Ask the user one or more focused questions and wait for their answers. Use this when the task cannot proceed without user input."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 3,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string"},
                            "header": {"type": "string"},
                            "question": {"type": "string"},
                            "isOther": {"type": "boolean"},
                            "isSecret": {"type": "boolean"},
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": {"type": "string"},
                                        "description": {"type": "string"}
                                    },
                                    "required": ["label", "description"]
                                }
                            }
                        },
                        "required": ["id", "header", "question"]
                    }
                },
                "autoResolutionMs": {"type": "integer", "minimum": 1}
            },
            "required": ["questions"]
        })
    }

    async fn call(&self, args: &Value) -> ToolOutput {
        let mut parsed: RequestUserInputArgs = match serde_json::from_value(args.clone()) {
            Ok(value) => value,
            Err(err) => {
                return ToolOutput::err(format!("invalid request_user_input arguments: {err}"));
            }
        };
        if parsed.questions.is_empty() || parsed.questions.len() > 3 {
            return ToolOutput::err("questions must contain between 1 and 3 items");
        }
        for question in &parsed.questions {
            if question.id.trim().is_empty()
                || question.header.trim().is_empty()
                || question.question.trim().is_empty()
            {
                return ToolOutput::err(
                    "each question requires non-empty id, header, and question",
                );
            }
            // Codex requires non-empty options for every question; reject both
            // `options: None` and empty option lists. A `None` previously slipped
            // through silently because the old check only inspected the `Some`
            // branch.
            if question.options.as_ref().is_none_or(Vec::is_empty) {
                return ToolOutput::err("each question requires non-empty options");
            }
        }

        // Codex marks every question with `is_other = true` so the client can
        // append its free-form "Other" option automatically.
        for question in &mut parsed.questions {
            question.is_other = true;
        }

        // Clamp `autoResolutionMs` to the supported range so a model cannot force
        // an instant auto-resolve (e.g. `autoResolutionMs: 1`) that would yield
        // an empty answer before the user can respond. Mirrors codex's
        // `normalize_request_user_input_args`.
        let original_auto_resolution_ms = parsed.auto_resolution_ms;
        parsed.auto_resolution_ms = clamp_auto_resolution(original_auto_resolution_ms);
        if parsed.auto_resolution_ms != original_auto_resolution_ms {
            log::warn!(
                "clamped request_user_input autoResolutionMs to supported range ({}ms -> {}ms)",
                original_auto_resolution_ms.unwrap_or(0),
                parsed.auto_resolution_ms.unwrap_or(0)
            );
        }

        let call_id = format!("user-input-{}", Uuid::new_v4());
        let receiver = self.bus.register_user_input(&call_id).await;
        self.bus.publish(BusMessage::UserInputRequest {
            agent_type: self.agent,
            call_id,
            questions: parsed.questions,
            auto_resolution_ms: parsed.auto_resolution_ms,
        });
        match receiver.await {
            Ok(response) => match serde_json::to_value(response) {
                Ok(value) => ToolOutput::ok(value),
                Err(err) => ToolOutput::err(format!("failed to encode user input: {err}")),
            },
            Err(_) => ToolOutput::err("user input request was cancelled"),
        }
    }
}

pub fn make(bus: Bus, agent: AgentType) -> Arc<dyn Tool> {
    Arc::new(RequestUserInputTool::new(bus, agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{Duration, timeout};

    fn args() -> Value {
        json!({"questions": [{
            "id": "choice", "header": "Choice", "question": "Pick one",
            "options": [{"label": "A", "description": "first"}]
        }]})
    }

    #[tokio::test]
    async fn publishes_codex_shape_and_returns_answers() {
        let bus = Bus::new();
        let mut events = bus.subscribe();
        let tool = RequestUserInputTool::new(bus.clone(), AgentType::Main);
        let task = tokio::spawn(async move { tool.call(&args()).await });
        let event = timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        let BusMessage::UserInputRequest {
            call_id, questions, ..
        } = event
        else {
            panic!("unexpected event")
        };
        assert_eq!(questions[0].id, "choice");
        let mut answers = std::collections::HashMap::new();
        answers.insert(
            "choice".to_string(),
            autoreport_core::request_user_input::RequestUserInputAnswer {
                answers: vec!["A".into()],
            },
        );
        assert!(
            bus.resolve_user_input(
                &call_id,
                autoreport_core::request_user_input::RequestUserInputResponse { answers },
            )
            .await
        );
        let output = task.await.unwrap();
        assert_eq!(output.error, None);
        assert_eq!(output.result["answers"]["choice"]["answers"][0], "A");
    }

    #[test]
    fn clamp_auto_resolution_aligns_to_codex_range() {
        assert_eq!(clamp_auto_resolution(None), None);
        assert_eq!(clamp_auto_resolution(Some(0)), Some(60_000));
        assert_eq!(clamp_auto_resolution(Some(1)), Some(60_000));
        assert_eq!(clamp_auto_resolution(Some(59_999)), Some(60_000));
        assert_eq!(clamp_auto_resolution(Some(60_000)), Some(60_000));
        assert_eq!(clamp_auto_resolution(Some(100_000)), Some(100_000));
        assert_eq!(clamp_auto_resolution(Some(240_000)), Some(240_000));
        assert_eq!(clamp_auto_resolution(Some(240_001)), Some(240_000));
        assert_eq!(clamp_auto_resolution(Some(999_999)), Some(240_000));
    }

    /// Publish `args()` with `autoResolutionMs: ms` and return the value the
    /// bus actually observed (after clamping). Resolves the request so the
    /// spawned task completes cleanly without hanging.
    async fn observed_auto_resolution(ms: u64) -> Option<u64> {
        let bus = Bus::new();
        let mut events = bus.subscribe();
        let tool = RequestUserInputTool::new(bus.clone(), AgentType::Main);
        let mut a = args();
        a["autoResolutionMs"] = json!(ms);
        let task = tokio::spawn(async move { tool.call(&a).await });
        let event = timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        let BusMessage::UserInputRequest {
            auto_resolution_ms,
            call_id,
            ..
        } = event
        else {
            panic!("unexpected event")
        };
        let mut answers = std::collections::HashMap::new();
        answers.insert(
            "choice".to_string(),
            autoreport_core::request_user_input::RequestUserInputAnswer {
                answers: vec!["A".into()],
            },
        );
        let _ = bus
            .resolve_user_input(
                &call_id,
                autoreport_core::request_user_input::RequestUserInputResponse { answers },
            )
            .await;
        let _ = task.await;
        auto_resolution_ms
    }

    #[tokio::test]
    async fn call_clamps_low_auto_resolution_ms_up_to_minimum() {
        assert_eq!(observed_auto_resolution(1).await, Some(60_000));
    }

    #[tokio::test]
    async fn call_clamps_high_auto_resolution_ms_down_to_maximum() {
        assert_eq!(observed_auto_resolution(999_999).await, Some(240_000));
    }

    #[tokio::test]
    async fn call_passes_through_auto_resolution_ms_within_range() {
        assert_eq!(observed_auto_resolution(100_000).await, Some(100_000));
    }

    #[tokio::test]
    async fn rejects_question_with_no_options() {
        let bus = Bus::new();
        let tool = RequestUserInputTool::new(bus.clone(), AgentType::Main);
        let args = json!({"questions": [{
            "id": "choice", "header": "Choice", "question": "Pick one"
        }]});
        let output = tool.call(&args).await;
        let err = output.error.expect("expected an error for missing options");
        assert!(
            err.contains("non-empty options"),
            "unexpected error message: {err}"
        );
    }

    #[tokio::test]
    async fn rejects_question_with_empty_options() {
        let bus = Bus::new();
        let tool = RequestUserInputTool::new(bus.clone(), AgentType::Main);
        let args = json!({"questions": [{
            "id": "choice", "header": "Choice", "question": "Pick one",
            "options": []
        }]});
        let output = tool.call(&args).await;
        let err = output.error.expect("expected an error for empty options");
        assert!(
            err.contains("non-empty options"),
            "unexpected error message: {err}"
        );
    }
}
