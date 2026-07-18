//! State owned by the terminal application.

use crate::config_update::{ConfigScreen, Outcome};
use crate::model_migration::ModelScreen;
use autoreport_core::types::AgentType;
use ratatui::Frame;
use serde_json::Value;

#[derive(Debug)]
pub(crate) enum Cell {
    User {
        _agent: AgentType,
        text: String,
    },
    Assistant {
        agent: AgentType,
        text: String,
        streaming: bool,
    },
    Reasoning {
        agent: AgentType,
        text: String,
        streaming: bool,
    },
    ToolGroup {
        agent: AgentType,
        items: Vec<ToolEntry>,
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
}

impl Overlay {
    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>) {
        match self {
            Self::Api(screen) => screen.draw(frame),
            Self::Models(screen) => screen.draw(frame),
        }
    }

    pub(crate) fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<Outcome> {
        match self {
            Self::Api(screen) => screen.handle_key(key),
            Self::Models(screen) => screen.handle_key(key),
        }
    }

    pub(crate) fn settings(&self) -> &autoreport_core::config::Settings {
        match self {
            Self::Api(screen) => &screen.settings,
            Self::Models(screen) => &screen.settings,
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
