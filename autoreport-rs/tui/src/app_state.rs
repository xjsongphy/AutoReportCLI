//! State owned by the terminal application.

use crate::config_update::{ConfigScreen, Outcome};
use crate::environment_setup::EnvironmentScreen;
use crate::model_migration::ModelScreen;
use autoreport_core::request_user_input::{RequestUserInputAnswer, RequestUserInputQuestion};
use autoreport_core::types::{AgentType, TaskStatus};
use crate::custom_terminal::Frame;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;

#[derive(Debug)]
pub(crate) enum Cell {
    User {
        _agent: AgentType,
        text: String,
    },
    /// Transient Codex `AgentMessageCell` equivalent. The active stream is
    /// mutated in place; the final contiguous run is consolidated into the
    /// source-backed `AgentMarkdown` cell when the provider completes.
    AgentMessage {
        agent: AgentType,
        text: String,
        is_first_line: bool,
    },
    /// Source-backed Codex `AgentMarkdownCell` equivalent. The raw markdown is
    /// retained so a resize re-renders from source instead of stale wrapped
    /// terminal lines.
    AgentMarkdown {
        agent: AgentType,
        text: String,
    },
    /// Codex `ReasoningSummaryCell` equivalent: the finalized model reasoning
    /// summary, rendered as dimmed italic markdown. Live reasoning still drives
    /// the thinking spinner; this cell is the transcript scrollback entry.
    Reasoning {
        agent: AgentType,
        text: String,
        transcript_only: bool,
    },
    ToolGroup {
        agent: AgentType,
        items: Vec<ToolEntry>,
    },
    /// Codex-style collaborator history row.
    Collab {
        agent: AgentType,
        title: ratatui::text::Line<'static>,
        details: Vec<ratatui::text::Line<'static>>,
    },
    /// Codex's final-message separator with the completed turn duration.
    TurnSeparator {
        agent: AgentType,
        elapsed_seconds: Option<u64>,
    },
    /// Source-backed Codex-style checkbox plan snapshot.
    PlanUpdate {
        agent: AgentType,
        explanation: Option<String>,
        steps: Vec<(String, TaskStatus)>,
    },
    /// Completed Codex request_user_input exchange.
    UserInputResult {
        agent: AgentType,
        questions: Vec<RequestUserInputQuestion>,
        answers: HashMap<String, RequestUserInputAnswer>,
        interrupted: bool,
    },
    System {
        text: String,
        kind: SysKind,
    },
}

#[derive(Debug)]
pub(crate) struct ToolEntry {
    pub(crate) name: String,
    pub(crate) args: Value,
    pub(crate) result: Option<Value>,
    pub(crate) error: Option<String>,
    /// Start time for Codex-style animated in-flight tool rows. Replayed
    /// history has no live clock and leaves this as `None`.
    pub(crate) started_at: Option<Instant>,
    /// Provider-assigned tool call id, used to correlate a `ToolResult` with the
    /// exact `ToolCall` even when the same tool is invoked more than once in
    /// flight. `None` for replayed history where no live correlation is needed.
    pub(crate) call_id: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum SysKind {
    Info,
    Error,
}

pub(crate) struct Mention {
    pub(crate) start: usize,
    pub(crate) cursor: usize,
    pub(crate) matches: Vec<String>,
    pub(crate) selected: usize,
}

pub(crate) enum Overlay {
    Api(ConfigScreen),
    Models(ModelScreen),
    Environment(EnvironmentScreen),
}

/// `/agent` picker popup state. The roster is the fixed `AgentType::ALL` set,
/// so only the highlighted row needs tracking — mirroring codex's
/// `ListSelectionView` selected index. `selected` is an index into
/// `AgentType::ALL`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AgentPickerState {
    pub(crate) selected: usize,
}

impl Overlay {
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>) {
        match self {
            Self::Api(screen) => screen.draw(frame),
            Self::Models(screen) => screen.draw(frame),
            Self::Environment(screen) => screen.draw(frame),
        }
    }

    pub(crate) fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Outcome> {
        match self {
            Self::Api(screen) => screen.handle_key(key),
            Self::Models(screen) => screen.handle_key(key),
            Self::Environment(screen) => screen.handle_key(key),
        }
    }

    pub(crate) fn settings(&self) -> &autoreport_core::config::Settings {
        match self {
            Self::Api(screen) => &screen.settings,
            Self::Models(screen) => &screen.settings,
            Self::Environment(_) => panic!("environment overlay has no API settings"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PendingApproval {
    pub(crate) agent: AgentType,
    pub(crate) call_id: String,
    pub(crate) command: String,
    pub(crate) cwd: Option<String>,
    pub(crate) summary: Vec<autoreport_core::policy::ParsedCommand>,
    pub(crate) reason: Option<String>,
}

/// A Codex `request_user_input` prompt waiting in the shared TUI queue.
#[derive(Clone, Debug)]
pub(crate) struct PendingUserInput {
    pub(crate) agent: AgentType,
    pub(crate) call_id: String,
    pub(crate) questions: Vec<RequestUserInputQuestion>,
    pub(crate) auto_resolution_ms: Option<u64>,
    pub(crate) question_index: usize,
    pub(crate) selected: usize,
    pub(crate) draft: String,
    pub(crate) cursor: usize,
    pub(crate) answers: HashMap<String, String>,
    pub(crate) started_at: Instant,
}

impl PendingUserInput {
    pub(crate) fn new(
        agent: AgentType,
        call_id: String,
        questions: Vec<RequestUserInputQuestion>,
        auto_resolution_ms: Option<u64>,
    ) -> Self {
        Self {
            agent,
            call_id,
            questions,
            auto_resolution_ms,
            question_index: 0,
            selected: 0,
            draft: String::new(),
            cursor: 0,
            answers: HashMap::new(),
            started_at: Instant::now(),
        }
    }

    pub(crate) fn question(&self) -> Option<&RequestUserInputQuestion> {
        self.questions.get(self.question_index)
    }

    pub(crate) fn timed_out(&self) -> bool {
        self.auto_resolution_ms
            .is_some_and(|ms| self.started_at.elapsed() >= std::time::Duration::from_millis(ms))
    }
}

/// A locally-rendered user row whose runtime turn has not reached a tool call
/// yet. This boundary lets Ctrl-C restore the submitted text and retract the
/// row, separately from clearing an editor draft or interrupting a tool.
pub(crate) struct PendingSubmission {
    pub(crate) agent: AgentType,
    pub(crate) text: String,
    pub(crate) history_index: usize,
    pub(crate) tool_started: bool,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) submitted: Arc<AtomicBool>,
}
