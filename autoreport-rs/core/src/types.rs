//! Shared domain types: agents, statuses, messages.
//!
//! These mirror the AutoReport `interfaces/types.py` definitions but are
//! trimmed to what a codex-style CLI needs (no GUI / MCP / image types).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Per-category runtime metric totals for one turn (count + wall duration).
/// Mirrors codex's `RuntimeMetricTotals` (also present in the `otel` crate).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMetricTotals {
    pub count: u64,
    pub duration_ms: u64,
}

impl RuntimeMetricTotals {
    pub fn is_empty(self) -> bool {
        self.count == 0 && self.duration_ms == 0
    }

    pub fn record(&mut self, duration_ms: u64) {
        self.count = self.count.saturating_add(1);
        self.duration_ms = self.duration_ms.saturating_add(duration_ms);
    }
}

/// Per-turn runtime metrics shown on the turn-end separator. Faithful to
/// codex's `RuntimeMetricsSummary`; the runtime fills `tool_calls` and
/// `api_calls` (the categories it instruments), the rest stay default/zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeMetricsSummary {
    pub tool_calls: RuntimeMetricTotals,
    pub api_calls: RuntimeMetricTotals,
    pub streaming_events: RuntimeMetricTotals,
    pub websocket_calls: RuntimeMetricTotals,
    pub websocket_events: RuntimeMetricTotals,
}

impl RuntimeMetricsSummary {
    pub fn is_empty(self) -> bool {
        self.tool_calls.is_empty()
            && self.api_calls.is_empty()
            && self.streaming_events.is_empty()
            && self.websocket_calls.is_empty()
            && self.websocket_events.is_empty()
    }
}

/// The agents that participate in a report run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentType {
    Main,
    DataAnalysis,
    Plotting,
    Theory,
    Report,
}

impl AgentType {
    pub const ALL: [AgentType; 5] = [
        AgentType::Main,
        AgentType::DataAnalysis,
        AgentType::Plotting,
        AgentType::Theory,
        AgentType::Report,
    ];

    /// Stable identifier used in config, prompts and tool args.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentType::Main => "main",
            AgentType::DataAnalysis => "data_analysis",
            AgentType::Plotting => "plotting",
            AgentType::Theory => "theory",
            AgentType::Report => "report",
        }
    }

    /// Human-friendly label shown in the TUI.
    pub fn label(&self) -> &'static str {
        match self {
            AgentType::Main => "Main",
            AgentType::DataAnalysis => "Data Analysis",
            AgentType::Plotting => "Plotting",
            AgentType::Theory => "Theory",
            AgentType::Report => "Report",
        }
    }

    /// Directory this agent is allowed to write into (relative to workspace).
    /// `None` for Main — it only writes a single outline file.
    pub fn write_dir(&self) -> Option<&'static str> {
        match self {
            AgentType::Main => None,
            AgentType::DataAnalysis => Some("Data/Processed"),
            AgentType::Plotting => Some("Plots"),
            AgentType::Theory => Some("Theory"),
            AgentType::Report => Some("Report"),
        }
    }
}

impl fmt::Display for AgentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "main" => Ok(AgentType::Main),
            "data_analysis" | "data" => Ok(AgentType::DataAnalysis),
            "plotting" | "plot" => Ok(AgentType::Plotting),
            "theory" => Ok(AgentType::Theory),
            "report" => Ok(AgentType::Report),
            other => Err(format!("unknown agent '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Thinking,
    RunningTool,
    Queued,
    Error,
    DebugMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    /// Sub-agent cannot proceed; needs the dispatcher (source agent) to act.
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Terminal or blocked states — i.e. no longer in-flight.
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            TaskStatus::Blocked
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
        )
    }
}

/// A task tracked on the shared task board, used by `update_plan` /
/// `send_to_agent` to coordinate Main ↔ sub-agent work.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskItem {
    pub task_id: String,
    pub brief: String,
    pub source_agent: AgentType,
    pub target_agent: AgentType,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub blocking: bool,
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_order: Option<u32>,
    /// Free-text reply attached when a delegated task completes.
    pub reply: Option<String>,
}

/// The full, displayable payload of an approval request. Carried by
/// [`BusMessage::ApprovalRequest`] on the broadcast bus *and* retained in
/// [`crate::bus::Bus`]'s pending-approvals map as the non-lossy source of truth
/// (so a broadcast lag, or a request registered before the TUI subscribed,
/// cannot drop or deadlock a request).
#[derive(Debug, Clone)]
pub struct ApprovalRequestPayload {
    pub agent_type: AgentType,
    pub call_id: String,
    /// Display command (e.g. the shell script the agent wants to run).
    pub command: String,
    /// Working directory the command runs in, if known.
    pub cwd: Option<String>,
    /// Pre-classified one-line summary of the command
    /// ([`crate::policy::ParsedCommand`]); rendered like codex's popup.
    pub summary: Vec<crate::policy::ParsedCommand>,
    /// Optional human-readable reason (e.g. "retry without sandbox").
    pub reason: Option<String>,
}

/// Messages flowing over the [`crate::runtime::bus::MessageBus`].
#[derive(Debug, Clone)]
pub enum BusMessage {
    /// A user (or another agent) addressing an agent.
    UserMessage {
        content: String,
        agent_type: AgentType,
        source: MessageSource,
        message_id: String,
    },
    /// Streaming/final text fragment produced by an agent.
    AgentResponse {
        agent_type: AgentType,
        content: String,
        streaming: bool,
    },
    /// Streaming/final reasoning fragment produced by a provider that exposes it.
    AgentReasoning {
        agent_type: AgentType,
        content: String,
        streaming: bool,
    },
    ToolCall {
        agent_type: AgentType,
        tool_name: String,
        arguments: serde_json::Value,
        call_id: String,
    },
    ToolResult {
        agent_type: AgentType,
        tool_name: String,
        result: serde_json::Value,
        error: Option<String>,
        call_id: String,
    },
    StatusChange {
        agent_type: AgentType,
        status: AgentStatus,
        /// Per-turn runtime metrics, attached when a turn ends (status → Idle)
        /// so the TUI can show tool/inference counts+duration on the turn
        /// separator. `None` outside turn-end transitions.
        runtime_metrics: Option<RuntimeMetricsSummary>,
    },
    /// Sub-agent's explicit report on a Main-dispatched task — the single
    /// reply channel. `SendToAgent` (Main side) subscribes to resolve its
    /// blocking wait; the sub-agent's loop observes its own report to mark
    /// the turn "reported".
    Report {
        agent_type: AgentType,
        task_id: String,
        /// "reply" | "missing_data" | "quality"
        report_type: String,
        summary: String,
        content: String,
    },
    /// Main's `send_to_agent(blocking=true)` has dispatched and is now waiting
    /// for the sub's `respond`. Surfaces codex's `Waiting for <agent>`
    /// collaborator row; the matching `Report` ends the wait.
    Waiting {
        target_agent: AgentType,
        task_id: String,
    },
    /// A notice explaining why an agent is waiting/busy (loop guards, etc.).
    /// Rendered as a bubble; the agent loop does NOT treat it as input.
    SystemNotice {
        agent_type: Option<AgentType>,
        content: String,
    },
    TaskUpdate {
        task_id: String,
        action: String,
        source_agent: AgentType,
        target_agent: AgentType,
        brief: String,
    },
    /// A Codex-style `update_plan` snapshot.  Unlike TaskUpdate (which is
    /// delegation bookkeeping), this is a user-visible plan history event.
    PlanUpdate {
        agent_type: AgentType,
        explanation: Option<String>,
        steps: Vec<(String, TaskStatus)>,
    },
    Error {
        agent_type: Option<AgentType>,
        message: String,
    },
    /// An agent requests human approval before running a command. Published on
    /// the bus so the TUI surfaces it regardless of which agent is focused —
    /// the single shared approval channel (no background agent stalls). The
    /// reply is delivered out-of-band via `Bus::resolve_approval(call_id)`.
    ///
    /// The full payload is also retained in `Bus`'s pending-approvals map as
    /// the source of truth, so the TUI can reconcile its display queue after a
    /// broadcast lag (or a request registered before it subscribed) without
    /// dropping or deadlocking a request. See [`crate::bus::Bus::pending_approvals`].
    ApprovalRequest { payload: ApprovalRequestPayload },
    /// Codex `request_user_input` prompt. The answer is delivered through the
    /// broker on [`crate::bus::Bus`], while this broadcast is consumed by the
    /// TUI (or a future app-server client).
    UserInputRequest {
        agent_type: AgentType,
        call_id: String,
        questions: Vec<crate::request_user_input::RequestUserInputQuestion>,
        auto_resolution_ms: Option<u64>,
    },
}

impl BusMessage {
    pub fn agent_type(&self) -> Option<AgentType> {
        match self {
            BusMessage::UserMessage { agent_type, .. }
            | BusMessage::AgentResponse { agent_type, .. }
            | BusMessage::AgentReasoning { agent_type, .. }
            | BusMessage::ToolCall { agent_type, .. }
            | BusMessage::ToolResult { agent_type, .. }
            | BusMessage::StatusChange { agent_type, .. }
            | BusMessage::Report { agent_type, .. } => Some(*agent_type),
            BusMessage::ApprovalRequest { payload } => Some(payload.agent_type),
            BusMessage::Waiting { target_agent, .. } => Some(*target_agent),
            BusMessage::UserInputRequest { agent_type, .. } => Some(*agent_type),
            BusMessage::SystemNotice { agent_type, .. } => *agent_type,
            BusMessage::TaskUpdate { target_agent, .. } => Some(*target_agent),
            BusMessage::PlanUpdate { agent_type, .. } => Some(*agent_type),
            BusMessage::Error { agent_type, .. } => *agent_type,
        }
    }
}

/// Who originated a user-targeted message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageSource {
    User,
    MainAgent,
    /// Originating sub-agent identifier (when reporting back).
    Agent(AgentType),
    System,
}

impl MessageSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageSource::User => "user",
            MessageSource::MainAgent => "main_agent",
            MessageSource::System => "system",
            MessageSource::Agent(a) => a.as_str(),
        }
    }
}
