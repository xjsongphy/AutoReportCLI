//! Per-project session state for the multi-project TUI (Route A).
//!
//! One `ProjectSession` owns everything tied to a single workspace + its
//! fixed-agent `LoopManager`: the bus receiver, transcript history, per-agent
//! status, queued inputs, and the approval/user-input queues. The `Tui` shell
//! (see `app.rs`) holds a vec of these and switches between them, keeping
//! cross-project UI (composer, overlays, scroll) at the shell level.
//!
//! v1 keeps a single session (behavior identical to the pre-refactor TUI);
//! multi-project open/switch arrives in later steps (see
//! `docs/APP-ROUTE-A-PLAN.md`). Session model stays fixed-agent.

use crate::app_state::{Cell, PendingApproval, PendingSubmission, PendingUserInput};
use crate::file_search::FileIndex;
use autoreport_core::bus::Bus;
use autoreport_core::request_user_input::RequestUserInputQuestion;
use autoreport_core::types::{AgentStatus, AgentType, BusMessage};
use autoreport_runtime::LoopManager;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub(crate) struct ProjectSession {
    pub(crate) manager: Arc<LoopManager>,
    pub(crate) bus: Bus,
    pub(crate) workspace: PathBuf,
    pub(crate) history: Vec<Cell>,
    pub(crate) statuses: HashMap<AgentType, AgentStatus>,
    pub(crate) status_since: HashMap<AgentType, Instant>,
    pub(crate) focused: AgentType,
    /// Follow-up inputs held while the focused agent is in a turn.
    pub(crate) queued_inputs: HashMap<AgentType, VecDeque<String>>,
    pub(crate) pending_submissions: Vec<PendingSubmission>,
    pub(crate) suppress_until_idle: HashSet<AgentType>,
    pub(crate) rx: tokio::sync::broadcast::Receiver<BusMessage>,
    pub(crate) index: FileIndex,
    /// Pending human-approval requests from any agent in this project.
    pub(crate) pending_approvals: VecDeque<PendingApproval>,
    /// Shared queue for Codex-compatible user-input prompts in this project.
    pub(crate) pending_user_inputs: VecDeque<PendingUserInput>,
    pub(crate) user_input_requests: HashMap<String, (AgentType, Vec<RequestUserInputQuestion>)>,
}

impl ProjectSession {
    pub(crate) fn new(manager: Arc<LoopManager>, bus: Bus, workspace: PathBuf) -> Self {
        let index = FileIndex::new(&workspace);
        index.refresh();
        let rx = bus.subscribe();
        Self {
            manager,
            bus,
            rx,
            workspace,
            history: Vec::new(),
            statuses: HashMap::new(),
            status_since: HashMap::new(),
            focused: AgentType::Main,
            queued_inputs: HashMap::new(),
            pending_submissions: Vec::new(),
            suppress_until_idle: HashSet::new(),
            index,
            pending_approvals: VecDeque::new(),
            pending_user_inputs: VecDeque::new(),
            user_input_requests: HashMap::new(),
        }
    }
}
