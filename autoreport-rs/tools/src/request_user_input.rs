//! Codex-compatible `request_user_input` tool.

use crate::registry::{Tool, ToolOutput};
use async_trait::async_trait;
use autoreport_core::bus::Bus;
use autoreport_core::request_user_input::RequestUserInputArgs;
use autoreport_core::types::{AgentType, BusMessage};
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

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
        let parsed: RequestUserInputArgs = match serde_json::from_value(args.clone()) {
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
            if let Some(options) = &question.options {
                if options.is_empty() {
                    return ToolOutput::err("question options cannot be empty");
                }
            }
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
}
