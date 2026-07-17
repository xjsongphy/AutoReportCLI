//! Codex-style terminal UI.
//!
//! Layout: top status bar, scrolling markdown-rendered history, bottom input
//! box, and an `@` file-mention popup. Built on ratatui + crossterm. Agent loops
//! run in background tasks; the TUI subscribes to the bus to render their
//! streamed output.

use crate::app_state::{Cell, Mention, Overlay, PendingApproval, SysKind, ToolEntry};
use crate::config_update::ConfigScreen;
use crate::file_search::FileIndex;
use crate::model_migration::ModelScreen;
use crate::slash_command::SlashCompletion;
use autoreport_core::bus::Bus;
use autoreport_core::config::load_settings;
use autoreport_core::types::{AgentStatus, AgentType, BusMessage};
use autoreport_rollout::ResponseItem;
use autoreport_runtime::LoopManager;
use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::StreamExt;

pub struct Tui {
    pub(crate) manager: Arc<LoopManager>,
    pub(crate) bus: Bus,
    pub(crate) autoreport_home: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) workspace_display: String,
    pub(crate) provider_id: String,
    pub(crate) history: Vec<Cell>,
    pub(crate) statuses: HashMap<AgentType, AgentStatus>,
    pub(crate) focused: AgentType,
    pub(crate) input: String,
    pub(crate) cursor: usize,
    pub(crate) scroll: usize,
    rx: tokio::sync::broadcast::Receiver<BusMessage>,
    pub(crate) index: FileIndex,
    pub(crate) mention: Option<Mention>,
    pub(crate) slash: Option<SlashCompletion>,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) want_config: bool,
    pub(crate) want_models: bool,
    pub(crate) tick: usize,
    // `/ide` toggle state — mirrors codex's IdeContextState. When enabled, each
    // outgoing user turn is prefixed with IDE context fetched over the codex
    // IPC socket (`\\.\pipe\codex-ipc` / `$TMPDIR/codex-ipc/ipc-<uid>.sock`).
    pub(crate) ide_enabled: bool,
    pub(crate) ide_warned: bool,
    /// Pending human-approval requests from any agent (single shared channel —
    /// ported from codex's `ApprovalOverlay` queue). Front = currently shown.
    pub(crate) pending_approvals: VecDeque<PendingApproval>,
    pub(crate) exit_requested: bool,
}

impl Tui {
    pub fn new(
        manager: Arc<LoopManager>,
        bus: Bus,
        autoreport_home: PathBuf,
        workspace: PathBuf,
        provider_id: String,
    ) -> Self {
        let workspace_display = workspace.display().to_string();
        let index = FileIndex::new(&workspace);
        index.refresh();
        let rx = bus.subscribe();
        Self {
            manager,
            bus,
            autoreport_home,
            rx,
            workspace,
            workspace_display,
            provider_id,
            history: Vec::new(),
            statuses: HashMap::new(),
            focused: AgentType::Main,
            input: String::new(),
            cursor: 0,
            scroll: 0,
            index,
            mention: None,
            slash: None,
            overlay: None,
            want_config: false,
            want_models: false,
            tick: 0,
            ide_enabled: false,
            ide_warned: false,
            pending_approvals: VecDeque::new(),
            exit_requested: false,
        }
    }

    pub async fn run(mut self) -> io::Result<()> {
        self.restore_history().await;
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        self.system(
            "AutoReportCLI ready. Type @ to mention a file, Tab switches agent, /help for commands.",
            SysKind::Info,
        );

        let mut events = EventStream::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(120));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if self.want_config {
                self.want_config = false;
                let settings = load_settings(&self.autoreport_home).unwrap_or_default();
                self.overlay = Some(Overlay::Api(ConfigScreen::new(
                    settings,
                    self.autoreport_home.clone(),
                )));
            }
            if self.want_models {
                self.want_models = false;
                let settings = load_settings(&self.autoreport_home).unwrap_or_default();
                self.overlay = Some(Overlay::Models(ModelScreen::new(
                    settings,
                    self.autoreport_home.clone(),
                )));
            }
            terminal.draw(|f| self.draw(f))?;

            tokio::select! {
                maybe_ev = events.next() => {
                    let Some(Ok(ev)) = maybe_ev else { break; };
                    if !self.handle_event(ev) { break; }
                }
                msg = self.rx.recv() => {
                    match msg {
                        Ok(m) => self.apply_bus(m),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
                _ = interval.tick() => {
                    self.tick = self.tick.wrapping_add(1);
                }
            }
        }

        self.manager.shutdown().await;
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }

    /// Rebuild the visible transcript from the already-resumed project
    /// sessions. Startup recovery happens before `Tui::run`, so this is a
    /// deterministic snapshot rather than a race with the session processor.
    async fn restore_history(&mut self) {
        for (agent, items) in self.manager.history_snapshot().await {
            for item in items {
                match item {
                    ResponseItem::Message { role, content, .. } => {
                        let text = content
                            .iter()
                            .map(|part| part.text().to_string())
                            .collect::<String>();
                        if text.is_empty() {
                            continue;
                        }
                        match role.as_str() {
                            "user" => self.history.push(Cell::User { agent, text }),
                            "assistant" => self.history.push(Cell::Assistant {
                                agent,
                                text,
                                streaming: false,
                            }),
                            _ => {}
                        }
                    }
                    ResponseItem::Reasoning {
                        content, summary, ..
                    } => {
                        let text = content
                            .as_ref()
                            .map(|parts| {
                                parts
                                    .iter()
                                    .map(|part| part.text())
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            })
                            .filter(|text| !text.trim().is_empty())
                            .or_else(|| {
                                let text = summary
                                    .iter()
                                    .map(|part| part.text())
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                (!text.trim().is_empty()).then_some(text)
                            });
                        if let Some(text) = text {
                            self.history.push(Cell::Reasoning {
                                agent,
                                text,
                                streaming: false,
                            });
                        }
                    }
                    ResponseItem::FunctionCall {
                        name, arguments, ..
                    } => {
                        self.history.push(Cell::ToolGroup {
                            agent,
                            items: vec![ToolEntry {
                                name,
                                args: match serde_json::from_str(&arguments) {
                                    Ok(value) => value,
                                    Err(_) => serde_json::Value::String(arguments),
                                },
                                result: None,
                                error: None,
                            }],
                        });
                    }
                    ResponseItem::FunctionCallOutput { output, .. } => {
                        if let Some(Cell::ToolGroup {
                            agent: owner,
                            items,
                        }) = self.history.last_mut()
                            && *owner == agent
                            && let Some(entry) = items.last_mut()
                        {
                            entry.result = Some(serde_json::Value::String(output));
                        }
                    }
                    ResponseItem::Compaction { .. } | ResponseItem::Other => {}
                }
            }
        }
    }

    pub(crate) fn system(&mut self, text: &str, kind: SysKind) {
        self.history.push(Cell::System {
            text: text.to_string(),
            kind,
        });
    }

    /// `/ide [on|off|status]` — mirrors codex's IdeContextState toggle. Toggling
    /// on probes the IPC socket once so the user gets immediate feedback.
    pub(crate) fn handle_ide_command(&mut self, args: &str) {
        let on = match args.trim().to_ascii_lowercase().as_str() {
            "" => !self.ide_enabled,
            "on" => true,
            "off" => false,
            "status" => {
                if self.ide_enabled {
                    self.system("IDE context is on.", SysKind::Info);
                } else {
                    self.system("IDE context is off.", SysKind::Info);
                }
                return;
            }
            _ => {
                self.system("usage: /ide [on|off|status]", SysKind::Error);
                return;
            }
        };
        self.ide_enabled = on;
        self.ide_warned = false;
        if on {
            match crate::ide_context::fetch_ide_context(&self.workspace) {
                Ok(context) => {
                    if crate::ide_context::has_prompt_context(&context) {
                        self.system(
                            "IDE context is on. Future messages will include your current IDE selection and open tabs.",
                            SysKind::Info,
                        );
                    } else {
                        self.system("IDE context is on. Connected to your IDE.", SysKind::Info);
                    }
                }
                Err(err) => {
                    self.ide_enabled = false;
                    self.system(
                        &format!(
                            "IDE context could not be enabled: {}",
                            err.user_facing_hint()
                        ),
                        SysKind::Error,
                    );
                }
            }
        } else {
            self.system("IDE context is off.", SysKind::Info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatwidget::{
        extract_mentions, render_reasoning_lines, render_tool_result_lines, status_mark,
    };
    use crate::slash_command;

    #[test]
    fn extracts_mentions_skipping_emails() {
        let m = extract_mentions("see @Data/Raw.csv and contact me@example.com and @Tex/main.tex");
        assert_eq!(
            m,
            vec!["Data/Raw.csv".to_string(), "Tex/main.tex".to_string()]
        );
    }

    #[test]
    fn apply_patch_tool_result_renders_patch_not_json() {
        let args = serde_json::json!({
            "patch": "*** Begin Patch\n*** Update File: Plots/Scripts/main.py\n@@\n-old\n+new\n*** End Patch\n"
        });
        let result = serde_json::json!({"applied": [{"update": "/tmp/Plots/Scripts/main.py"}]});

        let lines = render_tool_result_lines("apply_patch", &args, Some(&result), None);

        assert!(lines.iter().any(|line| {
            line.spans.iter().any(|span| span.content == "+ ")
                && line.spans.iter().any(|span| span.content.contains("new"))
        }));
        assert!(lines.iter().any(|line| {
            line.spans.iter().any(|span| span.content == "- ")
                && line.spans.iter().any(|span| span.content.contains("old"))
        }));
        assert!(
            !lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.contains("\"applied\""))
        );
    }

    #[test]
    fn active_statuses_use_codex_dot_indicator() {
        assert_eq!(status_mark(AgentStatus::Thinking), "●");
        assert_eq!(status_mark(AgentStatus::RunningTool), "●");
        assert_eq!(status_mark(AgentStatus::Idle), "○");
    }

    #[test]
    fn slash_command_matches_filter_by_prefix_in_presentation_order() {
        let matches = slash_command::matches("co");
        let names: Vec<&str> = matches.iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["config", "compact"]);
    }

    #[test]
    fn reasoning_lines_show_thinking_label_and_content() {
        let lines = render_reasoning_lines(AgentType::Main, "checking context", false);
        let flattened = lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(flattened.contains("Main thinking"));
        assert!(flattened.contains("checking context"));
    }
}
