//! Inter-agent coordination tools.
//!
//! - `manage_tasks` — todolist/waitlist inspection and lifecycle updates.
//! - `send_to_agent` — Main dispatches a task to a sub-agent and waits for its
//!   `respond` (blocking) or returns immediately (non-blocking). Faithful port
//!   of AutoReport's `SendToAgentTool` report protocol: resolves on the sub's
//!   `ReportMessage`, with a liveness timeout that counts only target IDLE /
//!   ERROR time (busy pauses the clock).
//! - `respond` — sub-agents report the outcome (reply / blocked) of a
//!   Main-dispatched task. This is the ONLY way to finish such a task and the
//!   single reply channel that resolves Main's blocking wait.

use crate::bus::Bus;
use crate::taskboard::TaskBoard;
use crate::tools::registry::{Tool, ToolOutput, arg_str};
use crate::types::{AgentStatus, AgentType, BusMessage, MessageSource, TaskItem, TaskStatus};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::time::{Duration, Instant};

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

fn task_json(t: &TaskItem) -> Value {
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
                let todo: Vec<Value> = self
                    .board
                    .todolist(self.agent)
                    .iter()
                    .map(task_json)
                    .collect();
                let wait: Vec<Value> = self
                    .board
                    .waitlist(self.agent)
                    .iter()
                    .map(task_json)
                    .collect();
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
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if ids.is_empty() {
                    return ToolOutput::err("task_ids required for status actions");
                }
                let mut updated = Vec::new();
                for id in &ids {
                    let res = match action.as_str() {
                        "start" => self.board.start(id),
                        "complete" => {
                            let reply =
                                args.get("reply").and_then(|v| v.as_str()).map(String::from);
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

/// First non-empty line of `content`, truncated to 80 chars — used as a
/// request summary in dispatch results (mirrors AutoReport `_request_summary`).
fn request_summary(content: &str) -> String {
    let first = content
        .lines()
        .find_map(|l| {
            let t = l.trim();
            (!t.is_empty()).then_some(t)
        })
        .unwrap_or("")
        .to_string();
    if first.len() <= 80 {
        first
    } else {
        format!(
            "{}...",
            &first[..first
                .char_indices()
                .take(77)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(77)]
        )
    }
}

fn is_route_placeholder_summary(text: &str) -> bool {
    let normalized = text
        .trim()
        .to_lowercase()
        .replace('_', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    matches!(
        normalized.as_str(),
        "sub to main"
            | "agent to main"
            | "theory to main"
            | "data analysis to main"
            | "data-analysis to main"
            | "plotting to main"
            | "report to main"
            | "main to sub"
            | "main to theory"
            | "main to data analysis"
            | "main to data-analysis"
            | "main to plotting"
            | "main to report"
    )
}

fn clean_required_text(
    args: &Value,
    primary: &str,
    fallback: Option<&str>,
) -> Result<String, String> {
    let raw = args
        .get(primary)
        .and_then(|value| value.as_str())
        .or_else(|| fallback.and_then(|name| args.get(name).and_then(|value| value.as_str())))
        .ok_or_else(|| format!("missing string argument '{primary}'"))?;
    let text = raw.trim();
    if text.is_empty() {
        return Err(format!("{primary} cannot be empty"));
    }
    if primary == "summary" && is_route_placeholder_summary(text) {
        return Err(
            "summary must describe the task outcome or blocker, not the message route".into(),
        );
    }
    Ok(text.to_string())
}

/// `send_to_agent` — Main only. Dispatches a task to a sub-agent and (blocking
/// mode) waits for the sub's `respond`. Non-blocking returns immediately.
pub struct SendToAgentTool {
    board: TaskBoard,
    bus: Bus,
    /// Wall-clock fallback cap (seconds) for the liveness wait. The idle
    /// budget is fixed at 60s; the wall cap is 4× this timeout.
    timeout_secs: u64,
}

impl SendToAgentTool {
    pub fn new(board: TaskBoard, bus: Bus) -> Self {
        Self {
            board,
            bus,
            timeout_secs: 120,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }
}

#[async_trait]
impl Tool for SendToAgentTool {
    fn name(&self) -> &str {
        "send_to_agent"
    }
    fn description(&self) -> &str {
        "Send a task instruction to a sub-agent. Use this to dispatch work to: \
         theory, data_analysis, plotting, report. Keep the message minimal: \
         summary (short visible task summary) plus content (full task detail: \
         task goal, input file locations, dependency, and explicit user constraints only). \
         Do not include formulas, implementation steps, copied source content, \
         output filenames, or quality rules the sub-agent already owns. \
         summary is required and must be non-empty; content is required and must be non-empty.\n\
         Modes (choose one):\n\
         - blocking=true (default): Wait for the sub-agent's `respond` (reply or blocked).\n\
         - blocking=false: Return immediately; the sub-agent's later `respond` notifies you. \
         Prefer this for independent work so multiple sub-agents can run in parallel without blocking Main.\n\
         task_id: omit on first dispatch; pass an existing task_id to RE-DISPATCH a \
         previously blocked task (resets it to in_progress)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "agent_type": {"type": "string", "enum": ["data_analysis", "plotting", "theory", "report"]},
                "summary": {"type": "string", "description": "Short visible task summary for bubbles and task tracking."},
                "content": {"type": "string", "description": "Task instruction to send to the sub-agent."},
                "brief": {"type": "string", "description": "Short label for waitlist tracking (defaults to first line of content)."},
                "task_items": {
                    "type": "array",
                    "description": "Optional task metadata used when creating a new tracked task.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "brief": {"type": "string"},
                            "task_brief": {"type": "string"},
                            "description": {"type": "string"}
                        }
                    }
                },
                "blocking": {"type": "boolean", "default": true},
                "task_id": {"type": "string", "description": "Existing task_id to re-dispatch (resets BLOCKED/COMPLETED -> in_progress)."}
            },
            "required": ["agent_type", "summary", "content"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let agent_str = match arg_str(args, "agent_type") {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        let agent: AgentType = match agent_str.trim().parse() {
            Ok(a) => a,
            Err(e) => return ToolOutput::err(e),
        };
        if agent == AgentType::Main {
            return ToolOutput::err("cannot delegate to main");
        }
        let summary = match clean_required_text(args, "summary", Some("brief")) {
            Ok(text) => text,
            Err(err) => return ToolOutput::err(err),
        };
        let content = match clean_required_text(args, "content", None) {
            Ok(text) => text,
            Err(err) => return ToolOutput::err(err),
        };
        let blocking = args
            .get("blocking")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let req_summary = summary.clone();

        // --- Create or re-dispatch task ---
        let task_id = match arg_str(args, "task_id").ok() {
            Some(id) if !id.trim().is_empty() => {
                let id = id.trim().to_string();
                match self.board.get_task(&id, Some(agent), false) {
                    Some(existing) => {
                        // Re-dispatch: reset settled chain back to in_progress.
                        if existing.status.is_settled() || existing.status == TaskStatus::Pending {
                            self.board.start(&id);
                        }
                        self.bus.publish(BusMessage::TaskUpdate {
                            task_id: id.clone(),
                            action: "started".into(),
                            source_agent: AgentType::Main,
                            target_agent: agent,
                            brief: existing.brief.clone(),
                        });
                        id
                    }
                    None => {
                        return ToolOutput::err(format!(
                            "task_id {id} not found for {}",
                            agent.as_str()
                        ));
                    }
                }
            }
            _ => {
                let task_brief = args
                    .get("task_items")
                    .and_then(|value| value.as_array())
                    .and_then(|items| items.first())
                    .and_then(|item| {
                        item.get("brief")
                            .or_else(|| item.get("task_brief"))
                            .and_then(|value| value.as_str())
                    })
                    .or_else(|| args.get("brief").and_then(|value| value.as_str()))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| request_summary(&summary).chars().take(30).collect());
                let task =
                    self.board
                        .create(AgentType::Main, agent, task_brief.clone(), blocking, None);
                let _ = self.board.start(&task.task_id);
                self.bus.publish(BusMessage::TaskUpdate {
                    task_id: task.task_id.clone(),
                    action: "started".into(),
                    source_agent: AgentType::Main,
                    target_agent: agent,
                    brief: task_brief,
                });
                task.task_id
            }
        };

        // Non-blocking: dispatch and return immediately.
        if !blocking {
            self.bus.publish(BusMessage::UserMessage {
                content: content.clone(),
                agent_type: agent,
                source: MessageSource::MainAgent,
                message_id: uuid::Uuid::new_v4().to_string(),
            });
            return ToolOutput::ok(json!({
                "status": "delegated",
                "agent_type": agent.as_str(),
                "blocking": false,
                "task_id": task_id,
                "summary": summary,
                "content": content,
                "request_summary": req_summary,
                "message": format!("Task sent to {} (non-blocking). Notified on completion.", agent.as_str()),
            }));
        }

        // --- Blocking: wait for the sub's ReportMessage on this task ---
        // Subscribe BEFORE publishing to avoid losing the report (race).
        let mut rx = self.bus.subscribe();
        let dispatch_id = format!("blocking:{task_id}");
        // Prefix the instruction with the task_id so the sub knows what to
        // `respond` on (it has no other context-injected task pointer).
        let dispatched_content = format!("[task_id: {task_id}]\n\n{content}");
        self.bus.publish(BusMessage::UserMessage {
            content: dispatched_content,
            agent_type: agent,
            source: MessageSource::MainAgent,
            message_id: dispatch_id,
        });

        match wait_for_report(&mut rx, agent, &task_id, self.timeout_secs).await {
            WaitOutcome::Report {
                report_type,
                summary: response_summary,
                content: reply,
            } => {
                if report_type == "reply" {
                    ToolOutput::ok(json!({
                        "status": "success",
                        "agent_type": agent.as_str(),
                        "task_id": task_id,
                        "blocking": true,
                        "summary": summary,
                        "request_summary": req_summary,
                        "response_summary": response_summary,
                        "response": reply,
                    }))
                } else {
                    ToolOutput::ok(json!({
                        "status": "blocked",
                        "agent_type": agent.as_str(),
                        "task_id": task_id,
                        "blocking": true,
                        "block_type": report_type,
                        "summary": summary,
                        "request_summary": req_summary,
                        "response_summary": response_summary,
                        "response": reply,
                        "error": format!("Sub-agent responded {}: {}", report_type, reply),
                    }))
                }
            }
            WaitOutcome::Timeout => ToolOutput::ok(json!({
                "status": "timeout",
                "agent_type": agent.as_str(),
                "task_id": task_id,
                "summary": summary,
                "request_summary": req_summary,
                "error": "Sub-agent did not report within the liveness budget. It may still be processing — try again or read its output.",
            })),
            WaitOutcome::ChannelClosed => {
                ToolOutput::err("message bus closed while waiting for sub-agent report")
            }
        }
    }
}

/// Outcome of waiting for a sub-agent's `ReportMessage`.
enum WaitOutcome {
    Report {
        report_type: String,
        summary: String,
        content: String,
    },
    Timeout,
    ChannelClosed,
}

/// Wait for the target's `ReportMessage` matching `task_id`, counting only
/// target IDLE/ERROR time against the idle budget (busy pauses the clock).
/// A wall-clock cap (4× `timeout_secs`) bounds the total wait so a
/// never-reporting turn still resolves. Mirrors AutoReport's
/// `_await_with_liveness`.
async fn wait_for_report(
    rx: &mut broadcast::Receiver<BusMessage>,
    target: AgentType,
    task_id: &str,
    timeout_secs: u64,
) -> WaitOutcome {
    let idle_budget = Duration::from_secs(60);
    let wall_cap = Duration::from_secs(timeout_secs.saturating_mul(4).max(60));
    let wall_deadline = Instant::now() + wall_cap;
    // Target starts IDLE until it picks up the dispatch — arm the idle clock.
    let mut idle_deadline = Instant::now() + idle_budget;

    loop {
        let now = Instant::now();
        if now >= wall_deadline {
            return WaitOutcome::Timeout;
        }
        let next_deadline = idle_deadline.min(wall_deadline);
        match tokio::time::timeout_at(next_deadline, rx.recv()).await {
            Err(_) => {
                // Elapsed deadline: which one?
                if Instant::now() >= wall_deadline {
                    return WaitOutcome::Timeout;
                }
                // Idle budget exhausted with no progress.
                return WaitOutcome::Timeout;
            }
            Ok(Err(broadcast::error::RecvError::Closed)) => return WaitOutcome::ChannelClosed,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Ok(msg)) => match msg {
                BusMessage::Report {
                    agent_type,
                    task_id: tid,
                    report_type,
                    summary,
                    content,
                } if agent_type == target && tid == task_id => {
                    return WaitOutcome::Report {
                        report_type,
                        summary,
                        content,
                    };
                }
                BusMessage::StatusChange { agent_type, status } if agent_type == target => {
                    match status {
                        AgentStatus::Thinking | AgentStatus::RunningTool => {
                            // Busy: pause the idle clock (push beyond wall cap).
                            idle_deadline = wall_deadline + idle_budget;
                        }
                        _ => {
                            // IDLE / ERROR / etc.: (re)arm the idle clock.
                            idle_deadline = Instant::now() + idle_budget;
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// `respond` — sub-agents report the outcome of a Main-dispatched task. This
/// is the ONLY way to finish such a task; the sub MUST call it before its turn
/// can end (enforced by AgentLoop guards). Replaces the old `report_issue`.
pub struct RespondTool {
    board: TaskBoard,
    bus: Bus,
    agent: AgentType,
}

impl RespondTool {
    pub fn new(board: TaskBoard, bus: Bus, agent: AgentType) -> Self {
        Self { board, bus, agent }
    }
}

const VALID_REPORT_TYPES: &[&str] = &["reply", "missing_data", "quality"];

#[async_trait]
impl Tool for RespondTool {
    fn name(&self) -> &str {
        "respond"
    }
    fn description(&self) -> &str {
        "Respond to Main with the outcome of a task Main dispatched to you. This is the ONLY \
         way to finish such a task — you MUST call it before stopping.\n\
         Types:\n\
         - 'reply': you finished. summary = short visible outcome; content = your final response (results, file paths).\n\
         - 'missing_data': you cannot proceed because an input is missing. summary = short blocker; content = exactly what is missing and where it should be.\n\
         - 'quality': you cannot proceed because a dependency's output is wrong. summary = short blocker; content = what is wrong and where.\n\
         Do NOT use this to ask the user questions — make a reasonable assumption or report missing_data to Main."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "The task you are reporting on (the [task_id: ...] prefix in your current instruction)."},
                "type": {"type": "string", "enum": ["reply", "missing_data", "quality"]},
                "summary": {"type": "string", "description": "Short visible summary of the outcome or blocker."},
                "content": {"type": "string"}
            },
            "required": ["task_id", "type", "summary", "content"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let task_id = match arg_str(args, "task_id") {
            Ok(t) => t.trim().to_string(),
            Err(e) => return ToolOutput::err(e),
        };
        if task_id.is_empty() {
            return ToolOutput::err("task_id is required");
        }
        let report_type = match arg_str(args, "type") {
            Ok(t) => t.trim().to_string(),
            Err(e) => return ToolOutput::err(e),
        };
        if !VALID_REPORT_TYPES.contains(&report_type.as_str()) {
            return ToolOutput::err(format!(
                "Unknown report type '{report_type}'. Valid: {}",
                VALID_REPORT_TYPES.join(", ")
            ));
        }
        let summary = match clean_required_text(args, "summary", None) {
            Ok(text) => text,
            Err(err) => return ToolOutput::err(err),
        };
        let content = match clean_required_text(args, "content", None) {
            Ok(text) => text,
            Err(err) => return ToolOutput::err(err),
        };

        // Verify the task is actually assigned to this agent.
        let Some(task) = self.board.get_task(&task_id, Some(self.agent), false) else {
            return ToolOutput::err(format!(
                "Task {task_id} is not assigned to {}",
                self.agent.as_str()
            ));
        };

        // Update task status: reply -> completed, else -> blocked.
        let action = if report_type == "reply" {
            self.board.complete(&task_id, Some(content.clone()));
            "completed"
        } else {
            self.board.block(&task_id);
            "blocked"
        };
        self.bus.publish(BusMessage::TaskUpdate {
            task_id: task_id.clone(),
            action: action.into(),
            source_agent: task.source_agent,
            target_agent: self.agent,
            brief: task.brief.clone(),
        });

        // Single reply channel: resolves Main's wait + marks this turn reported.
        self.bus.publish(BusMessage::Report {
            agent_type: self.agent,
            task_id: task_id.clone(),
            report_type: report_type.clone(),
            summary: summary.clone(),
            content: content.clone(),
        });

        ToolOutput::ok(json!({
            "status": "ok",
            "task_id": task_id,
            "report_type": report_type,
            "summary": summary,
            "content": content,
            "message": format!("Reported {report_type} for task {task_id}"),
        }))
    }
}

pub fn main_tools(board: TaskBoard, bus: Bus) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ManageTasksTool::new(
            board.clone(),
            AgentType::Main,
            bus.clone(),
        )),
        Arc::new(SendToAgentTool::new(board, bus)),
    ]
}

pub fn sub_tools(board: TaskBoard, bus: Bus, agent: AgentType) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ManageTasksTool::new(board.clone(), agent, bus.clone())),
        Arc::new(RespondTool::new(board, bus, agent)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::Bus;

    #[tokio::test]
    async fn send_to_agent_blocking_resolves_on_report() {
        let bus = Bus::new();
        let board = TaskBoard::new();
        let tool = SendToAgentTool::new(board.clone(), bus.clone());

        // Stand in for the sub-agent: wait for the dispatch, then publish a reply.
        // Subscribe BEFORE the tool publishes (broadcast only delivers to
        // receivers that exist at send time).
        let mut rx = bus.subscribe();
        let bus2 = bus.clone();
        let sim = tokio::spawn(async move {
            while let Ok(msg) = rx.recv().await {
                if let BusMessage::UserMessage {
                    agent_type,
                    message_id,
                    ..
                } = msg
                {
                    if message_id.starts_with("blocking:") && agent_type == AgentType::Theory {
                        let task_id = message_id.trim_start_matches("blocking:").to_string();
                        bus2.publish(BusMessage::Report {
                            agent_type: AgentType::Theory,
                            task_id,
                            report_type: "reply".into(),
                            summary: "Theory complete".into(),
                            content: "done: result".into(),
                        });
                        return;
                    }
                }
            }
        });

        let out = tool
            .call(&json!({"agent_type":"theory","summary":"Derive formula","content":"do X"}))
            .await;
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        assert_eq!(out.result["status"], "success");
        assert_eq!(out.result["response_summary"], "Theory complete");
        assert_eq!(out.result["response"], "done: result");
        sim.await.unwrap();
    }

    #[tokio::test]
    async fn respond_missing_data_marks_task_blocked() {
        let bus = Bus::new();
        let board = TaskBoard::new();
        let task = board.create(
            AgentType::Main,
            AgentType::Theory,
            "do theory".into(),
            true,
            None,
        );
        let tool = RespondTool::new(board.clone(), bus.clone(), AgentType::Theory);
        let out = tool
            .call(&json!({
                "task_id": task.task_id,
                "type": "missing_data",
                "summary": "Need measurement file",
                "content": "need measurement file"
            }))
            .await;
        assert!(out.error.is_none(), "unexpected error: {:?}", out.error);
        let updated = board.get_task(&task.task_id, None, false).unwrap();
        assert_eq!(updated.status, TaskStatus::Blocked);
    }

    #[tokio::test]
    async fn respond_reply_marks_task_completed() {
        let bus = Bus::new();
        let board = TaskBoard::new();
        let task = board.create(
            AgentType::Main,
            AgentType::Report,
            "write report".into(),
            true,
            None,
        );
        let tool = RespondTool::new(board.clone(), bus.clone(), AgentType::Report);
        let out = tool
            .call(&json!({
                "task_id": task.task_id,
                "type": "reply",
                "summary": "Report complete",
                "content": "done at tex/out.pdf"
            }))
            .await;
        assert!(out.error.is_none());
        let updated = board.get_task(&task.task_id, None, false).unwrap();
        assert_eq!(updated.status, TaskStatus::Completed);
        assert_eq!(updated.reply.as_deref(), Some("done at tex/out.pdf"));
    }

    #[tokio::test]
    async fn send_to_agent_non_blocking_marks_task_in_progress() {
        let bus = Bus::new();
        let board = TaskBoard::new();
        let tool = SendToAgentTool::new(board.clone(), bus);

        let out = tool
            .call(&json!({
                "agent_type": "plotting",
                "summary": "Draw scatter plot",
                "content": "plot data/processed/out.csv",
                "blocking": false
            }))
            .await;

        assert!(out.error.is_none());
        assert_eq!(out.result["status"], "delegated");
        let task_id = out.result["task_id"].as_str().unwrap();
        let task = board
            .get_task(task_id, Some(AgentType::Plotting), false)
            .unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);
    }
}
