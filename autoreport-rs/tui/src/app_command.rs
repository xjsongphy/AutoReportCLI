//! Application command dispatch, separated from the terminal event loop.

use crate::app::Tui;
use crate::app_state::{AgentPickerState, SysKind};
use autoreport_core::types::AgentType;

impl Tui {
    pub(crate) fn run_command(&mut self, command: &str) {
        let mut parts = command.split_whitespace();
        let name = parts.next().unwrap_or("");
        let rest = parts.collect::<Vec<_>>().join(" ");
        match name {
            "help" | "h" | "?" => self.system(
                "Commands:\n  /agent            switch the active agent thread (picker)\n  /switch <agent>   focus an agent\n  /sessions         list this project's persisted sessions\n  /config           view & edit API settings\n  /model            assign main/sub APIs and model names\n  /models           alias for /model\n  /env              select Python and inspect local tool readiness\n  /clear            clear the terminal and start a new chat\n  /compact          compact focused agent's context\n  /new              reset focused agent\n  /copy             copy the last assistant response\n  /pager            open the transcript pager\n  /manifest         show produced files\n  /index            rebuild the @ file index\n  /ide [on|off]     toggle IDE context injection (open file + selection)\n  /quit             exit",
                SysKind::Info,
            ),
            "config" => self.want_config = true,
            "model" | "models" => self.want_models = true,
            "env" => self.want_environment = true,
            "agent" | "agents" => {
                // Codex opens a `/agent` selection popup (`ListSelectionView`).
                // We open the equivalent picker rooted on the currently focused
                // agent, matching codex's `initial_selected_idx` behavior.
                let selected = AgentType::ALL
                    .iter()
                    .position(|agent| *agent == self.focused)
                    .unwrap_or(0);
                self.agent_picker = Some(AgentPickerState { selected });
            }
            "sessions" => {
                let sessions = self.manager.session_summaries();
                if sessions.is_empty() {
                    self.system("No persisted sessions for this project.", SysKind::Info);
                } else {
                    let mut output = String::from("Project sessions:\n");
                    for (agent, conversation_id, timestamp) in sessions {
                        output.push_str(&format!("  {agent}: {conversation_id} [{timestamp}]\n"));
                    }
                    self.system(output.trim_end(), SysKind::Info);
                }
            }
            "switch" => match rest.parse::<AgentType>() {
                Ok(agent) => {
                    self.focused = agent;
                    self.composer.set_focused_agent(agent.label());
                    self.refresh_pending_input_preview();
                    self.system(&format!("focused: {}", agent.label()), SysKind::Info);
                }
                Err(_) => self.system(
                    "usage: /switch <main|data_analysis|plotting|theory|report>",
                    SysKind::Error,
                ),
            },
            "clear" => {
                // Codex's `/clear` clears the visible transcript as well as
                // starting a fresh context. Keep our focused-agent scope,
                // but match that user-visible behavior.
                self.clear_terminal_ui();
                self.manager.clear_context(self.focused);
            }
            "compact" => {
                self.manager.compact(self.focused);
                self.system(&format!("compacting {} context…", self.focused.label()), SysKind::Info);
            }
            "copy" => self.copy_last_response(),
            "pager" => self.open_pager(),
            "new" => {
                self.manager.clear_context(self.focused);
                self.system(&format!("reset {}", self.focused.label()), SysKind::Info);
            }
            "index" => {
                self.index.refresh();
                self.system("@ file index rebuilt", SysKind::Info);
            }
            "ide" => self.handle_ide_command(&rest),
            "manifest" => {
                let snapshot = self.manager.manifest_snapshot(None);
                self.system(
                    &format!("manifests:\n{}", serde_json::to_string_pretty(&snapshot).unwrap_or_default()),
                    SysKind::Info,
                );
            }
            "quit" | "exit" => self.exit_requested = true,
            "" => {}
            other => self.system(&format!("unknown command: /{other}"), SysKind::Error),
        }
    }
}
