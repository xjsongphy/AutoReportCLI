//! `manage_tasks` tool — todolist/waitlist inspection and lifecycle updates.

use crate::bus::Bus;
use crate::taskboard::TaskBoard;
use crate::tools::registry::{arg_str, Tool, ToolOutput};
use crate::types::{AgentType, BusMessage, MessageSource, TaskStatus};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ManageTasksTool {
    board: TaskBoard,
    agent: AgentType,
    bus: Bus,
}

impl ManageTasksTool {
    pub fn new(board: TaskBoard, agent: AgentType, bus: Bus) -> Self {
        Self { board, agent, bus }
    }
}

fn task_json(t: &crate::types::TaskItem) -> Value {
    json!({
        "task_id": t.task_id,
        "brief": t.brief,
        "source": t.source_agent.as_str(),
        "target": t.target_agent.as_str(),
        "status": t.status.as_str(),
        "blocking": t.blocking,
        "reply": t.reply,
    })
}

#[async_trait]
impl Tool for ManageTasksTool {
    fn name(&self) -> &str {
        "manage_tasks"
    }
    fn description(&self) -> &str {
        "Coordinate work. `action: list` shows your todolist (tasks for you) and waitlist (tasks you delegated). `add` creates a local task; `start`/`complete`/`cancel`/`fail` change status by `task_ids`. `complete` accepts `reply` for delegated tasks."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "add", "start", "complete", "cancel", "fail"]},
                "brief": {"type": "string", "description": "Used with `add`."},
                "task_ids": {"type": "array", "items": {"type": "string"}},
                "reply": {"type": "string", "description": "Completion reply for delegated tasks."}
            },
            "required": ["action"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let action = match arg_str(args, "action") {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        match action.as_str() {
            "list" => {
                let todo: Vec<Value> = self.board.todolist(self.agent).iter().map(task_json).collect();
                let wait: Vec<Value> = self.board.waitlist(self.agent).iter().map(task_json).collect();
                ToolOutput::ok(json!({"todolist": todo, "waitlist": wait}))
            }
            "add" => {
                let brief = match arg_str(args, "brief") {
                    Ok(b) => b,
                    Err(e) => return ToolOutput::err(e),
                };
                let t = self.board.add_local(self.agent, brief.clone());
                ToolOutput::ok(json!({"created": task_json(&t)}))
            }
            "start" | "complete" | "cancel" | "fail" => {
                let ids: Vec<String> = args
                    .get("task_ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if ids.is_empty() {
                    return ToolOutput::err("task_ids required for status actions");
                }
                let mut updated = Vec::new();
                for id in &ids {
                    let res = match action.as_str() {
                        "start" => self.board.start(id),
                        "complete" => {
                            let reply = args.get("reply").and_then(|v| v.as_str()).map(String::from);
                            self.board.complete(id, reply)
                        }
                        "cancel" => self.board.cancel(id),
                        "fail" => self.board.fail(id),
                        _ => None,
                    };
                    if let Some(t) = res {
                        // Notify the source agent when a delegated task completes.
                        if matches!(action.as_str(), "complete" | "cancel" | "fail")
                            && t.source_agent != self.agent
                        {
                            let status = match t.status {
                                TaskStatus::Completed => "completed",
                                TaskStatus::Failed => "failed",
                                TaskStatus::Cancelled => "cancelled",
                                _ => "updated",
                            };
                            self.bus.publish(BusMessage::TaskUpdate {
                                task_id: t.task_id.clone(),
                                action: status.to_string(),
                                source_agent: self.agent,
                                target_agent: t.source_agent,
                                brief: t.brief.clone(),
                            });
                        }
                        updated.push(task_json(&t));
                    }
                }
                ToolOutput::ok(json!({"updated": updated}))
            }
            other => ToolOutput::err(format!("unknown action '{other}'")),
        }
    }
}

/// `send_to_agent` — Main only. Delegates a task to a sub-agent over the bus.
pub struct SendToAgentTool {
    board: TaskBoard,
    bus: Bus,
}

impl SendToAgentTool {
    pub fn new(board: TaskBoard, bus: Bus) -> Self {
        Self { board, bus }
    }
}

#[async_trait]
impl Tool for SendToAgentTool {
    fn name(&self) -> &str {
        "send_to_agent"
    }
    fn description(&self) -> &str {
        "Delegate a task to a sub-agent. `agent_type` is one of data_analysis, plotting, theory, report."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_type": {"type": "string", "enum": ["data_analysis", "plotting", "theory", "report"]},
                "task_description": {"type": "string"},
                "brief": {"type": "string"}
            },
            "required": ["agent_type", "task_description"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let agent_str = match arg_str(args, "agent_type") {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        let agent: AgentType = match agent_str.parse() {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        if agent == AgentType::Main {
            return ToolOutput::err("cannot delegate to main");
        }
        let desc = match arg_str(args, "task_description") {
            Ok(d) => d,
            Err(e) => return ToolOutput::err(e),
        };
        let brief = args.get("brief").and_then(|v| v.as_str()).unwrap_or(&desc).to_string();
        let task = self.board.create(
            AgentType::Main,
            agent,
            brief.clone(),
            true,
            None,
        );
        self.bus.publish(BusMessage::UserMessage {
            content: desc,
            agent_type: agent,
            source: MessageSource::MainAgent,
            message_id: uuid::Uuid::new_v4().to_string(),
        });
        ToolOutput::ok(json!({
            "task_id": task.task_id,
            "delegated_to": agent.as_str(),
            "note": "the sub-agent will process this asynchronously and report back via task update",
        }))
    }
}

/// `report_issue` — sub-agents report back to Main for intervention.
pub struct ReportIssueTool {
    bus: Bus,
    agent: AgentType,
}

impl ReportIssueTool {
    pub fn new(bus: Bus, agent: AgentType) -> Self {
        Self { bus, agent }
    }
}

#[async_trait]
impl Tool for ReportIssueTool {
    fn name(&self) -> &str {
        "report_issue"
    }
    fn description(&self) -> &str {
        "Report a problem (missing data, ambiguity, quality concern) that needs the Main agent's intervention."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "feedback_type": {"type": "string", "enum": ["missing_data", "quality", "query"]}
            },
            "required": ["content"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let content = match arg_str(args, "content") {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(e),
        };
        let feedback_type = args
            .get("feedback_type")
            .and_then(|v| v.as_str())
            .unwrap_or("query")
            .to_string();
        self.bus.publish(BusMessage::AgentFeedback {
            agent_type: self.agent,
            content,
            feedback_type,
        });
        ToolOutput::ok(json!({"status": "reported"}))
    }
}

pub fn main_tools(board: TaskBoard, bus: Bus) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ManageTasksTool::new(board.clone(), AgentType::Main, bus.clone())),
        Arc::new(SendToAgentTool::new(board, bus)),
    ]
}

pub fn sub_tools(board: TaskBoard, bus: Bus, agent: AgentType) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ManageTasksTool::new(board, agent, bus.clone())),
        Arc::new(ReportIssueTool::new(bus, agent)),
    ]
}
