//! Shared domain types: agents, statuses, messages.
//!
//! These mirror the AutoReport `interfaces/types.py` definitions but are
//! trimmed to what a codex-style CLI needs (no GUI / MCP / image types).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

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
            AgentType::Report => Some("Tex"),
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
    },
    ToolResult {
        agent_type: AgentType,
        tool_name: String,
        result: serde_json::Value,
        error: Option<String>,
    },
    StatusChange {
        agent_type: AgentType,
        status: AgentStatus,
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
    Error {
        agent_type: Option<AgentType>,
        message: String,
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
            BusMessage::SystemNotice { agent_type, .. } => *agent_type,
            BusMessage::TaskUpdate { target_agent, .. } => Some(*target_agent),
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
