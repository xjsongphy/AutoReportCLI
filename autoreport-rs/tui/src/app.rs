//! Codex-style terminal UI.
//!
//! Layout: Codex-style scrolling transcript, bottom-pane composer, and an `@` file-mention popup.
//! Built on ratatui + crossterm. Agent loops run in background tasks; the TUI subscribes to the
//! bus to render their streamed output.

use crate::app_state::{
    AgentPickerState, Cell, Overlay, PendingApproval, PendingSubmission, PendingUserInput, SysKind,
    ToolEntry,
};
use crate::bottom_pane::paste_burst::PasteBurst;
use crate::bottom_pane::{ChatComposer, PendingInputPreview};
use crate::clipboard_copy::ClipboardLease;
use crate::config_update::ConfigScreen;
use crate::configuration_flow::ConfigurationFlow;
use crate::custom_terminal::Terminal;
use crate::environment_setup::EnvironmentScreen;
use crate::file_search::FileIndex;
use crate::frame_requester::FrameRequester;
use crate::pager_overlay::PagerOverlay;
use autoreport_core::bus::Bus;
use autoreport_core::config::load_settings;
use autoreport_core::request_user_input::RequestUserInputQuestion;
use autoreport_core::types::{AgentStatus, AgentType, BusMessage};
use autoreport_rollout::ResponseItem;
use autoreport_runtime::LoopManager;
use crossterm::Command;
use crossterm::event::EventStream;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Rect, Size};
use ratatui::text::Line;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_stream::StreamExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EnableAlternateScroll;

impl Command for EnableAlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[?1007h")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "alternate scroll requires ANSI terminal support",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DisableAlternateScroll;

impl Command for DisableAlternateScroll {
    fn write_ansi(&self, f: &mut impl std::fmt::Write) -> std::fmt::Result {
        write!(f, "\x1b[?1007l")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Err(io::Error::other(
            "alternate scroll requires ANSI terminal support",
        ))
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

pub struct Tui {
    pub(crate) manager: Arc<LoopManager>,
    pub(crate) bus: Bus,
    pub(crate) autoreport_home: PathBuf,
    pub(crate) workspace: PathBuf,
    pub(crate) main_model: String,
    pub(crate) sub_model: String,
    pub(crate) history: Vec<Cell>,
    /// Number of finalized history cells already emitted to terminal scrollback.
    pub(crate) history_inserted_cells: usize,
    pub(crate) scrollback_needs_clear: bool,
    /// Whether the last line written to terminal scrollback was blank. Used to
    /// avoid stacking a separator blank onto a prior cell's trailing blank when
    /// flushing the next incremental batch of history.
    pub(crate) scrollback_tail_blank: bool,
    pub(crate) statuses: HashMap<AgentType, AgentStatus>,
    pub(crate) status_since: HashMap<AgentType, Instant>,
    pub(crate) focused: AgentType,
    pub(crate) composer: ChatComposer,
    pub(crate) pending_input_preview: PendingInputPreview,
    /// Follow-up inputs are held here while the focused agent is in a turn,
    /// matching Codex's input_queue plus PendingInputPreview.
    pub(crate) queued_inputs: HashMap<AgentType, VecDeque<String>>,
    pub(crate) pending_submissions: Vec<PendingSubmission>,
    pub(crate) suppress_until_idle: HashSet<AgentType>,
    pub(crate) paste_burst: PasteBurst,
    rx: tokio::sync::broadcast::Receiver<BusMessage>,
    pub(crate) index: FileIndex,
    pub(crate) overlay: Option<Overlay>,
    pub(crate) pager: Option<PagerOverlay>,
    /// `/agent` picker popup (codex `ListSelectionView` equivalent). `None`
    /// means closed; `Some` holds the highlighted row index.
    pub(crate) agent_picker: Option<AgentPickerState>,
    /// Codex's copy-friendly raw transcript mode (Alt+R).
    pub(crate) raw_output: bool,
    pub(crate) want_config: bool,
    pub(crate) want_models: bool,
    pub(crate) want_models_after_config: bool,
    pub(crate) want_environment: bool,
    // `/ide` toggle state — mirrors codex's IdeContextState. When enabled, each
    // outgoing user turn is prefixed with IDE context fetched over the codex
    // IPC socket (`\\.\pipe\codex-ipc` / `$TMPDIR/codex-ipc/ipc-<uid>.sock`).
    pub(crate) ide_enabled: bool,
    pub(crate) ide_warned: bool,
    /// Pending human-approval requests from any agent (single shared channel —
    /// ported from codex's `ApprovalOverlay` queue). Front = currently shown.
    pub(crate) pending_approvals: VecDeque<PendingApproval>,
    /// Shared queue for Codex-compatible user-input prompts.
    pub(crate) pending_user_inputs: VecDeque<PendingUserInput>,
    pub(crate) user_input_requests: HashMap<String, (AgentType, Vec<RequestUserInputQuestion>)>,
    pub(crate) exit_requested: bool,
    pub(crate) clipboard_lease: Option<ClipboardLease>,
    /// Codex's status widget owns its next-frame requests. The requester is
    /// installed when the terminal loop starts, because it needs the draw
    /// broadcast channel created by `run`.
    pub(crate) frame_requester: Option<FrameRequester>,
}

// Codex's first-session surface includes a command primer below the session
// header, which gives its inline viewport a 19-row natural height at the
// standard 120-column terminal size. AutoReport has a shorter ready notice, so
// preserve the same initial viewport footprint instead of pinning the whole
// first-session surface to the bottom edge.
const INITIAL_CHAT_VIEWPORT_MIN_HEIGHT: u16 = 19;

/// The surface that owns every cell in the terminal frame. Approval and
/// request-input dialogs are intentionally absent: they remain over-chat
/// modals, matching Codex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FullFrameSurface {
    Chat,
    Pager,
    ApiConfiguration,
    ConfigurationFlow,
    EnvironmentConfiguration,
}

fn select_full_frame_surface(
    overlay: Option<FullFrameSurface>,
    pager_is_open: bool,
) -> FullFrameSurface {
    overlay.unwrap_or_else(|| {
        if pager_is_open {
            FullFrameSurface::Pager
        } else {
            FullFrameSurface::Chat
        }
    })
}

fn chat_viewport_height(screen_height: u16, desired_height: u16, is_initial_surface: bool) -> u16 {
    let height = if is_initial_surface {
        desired_height.max(INITIAL_CHAT_VIEWPORT_MIN_HEIGHT)
    } else {
        desired_height
    };
    height.min(screen_height).max(1)
}

fn chat_viewport_y(
    screen: Size,
    previous: Rect,
    height: u16,
    full_screen: bool,
    anchor_top: bool,
) -> u16 {
    if full_screen {
        0
    } else if anchor_top || previous.is_empty() {
        if anchor_top {
            0
        } else {
            screen.height.saturating_sub(height)
        }
    } else if previous.width != screen.width || previous.height != height {
        previous
            .bottom()
            .saturating_sub(height)
            .min(screen.height.saturating_sub(height))
    } else {
        previous.y.min(screen.height.saturating_sub(height))
    }
}

impl Tui {
    pub(crate) fn full_frame_surface(&self) -> FullFrameSurface {
        // Prefer the configuration page defensively. Normal transitions make
        // it exclusive with the pager, but this prevents a stale pager from
        // ever becoming visible beneath a page.
        let overlay = match self.overlay.as_ref() {
            Some(Overlay::Api(_)) => Some(FullFrameSurface::ApiConfiguration),
            Some(Overlay::Configuration(_)) => Some(FullFrameSurface::ConfigurationFlow),
            Some(Overlay::Environment(_)) => Some(FullFrameSurface::EnvironmentConfiguration),
            None => None,
        };
        select_full_frame_surface(overlay, self.pager.is_some())
    }

    fn activate_full_frame_overlay(&mut self, overlay: Overlay) {
        self.pager = None;
        self.overlay = Some(overlay);
    }

    fn prepare_chat_viewport(
        &self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
        anchor_top: bool,
    ) -> io::Result<()> {
        let size = terminal.size()?;
        let full_screen = self.full_frame_surface() != FullFrameSurface::Chat;
        let height = if full_screen {
            size.height
        } else {
            let is_initial_surface = self.history.iter().all(|cell| {
                matches!(
                    cell,
                    Cell::System {
                        kind: SysKind::Info,
                        ..
                    }
                )
            });
            chat_viewport_height(
                size.height,
                self.codex_chat_viewport_height(size.width),
                is_initial_surface,
            )
        };
        let previous = terminal.viewport_area;
        // A clear/new session starts at the top. On an ordinary draw this
        // function preserves the existing origin; only a size change reflows
        // the bottom edge.
        let y = chat_viewport_y(size, previous, height, full_screen, anchor_top);
        let area = Rect::new(0, y, size.width, height);
        if area != terminal.viewport_area {
            terminal.set_viewport_area(area);
            terminal.invalidate_viewport();
        }
        Ok(())
    }

    fn flush_history_to_scrollback(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<()> {
        let committed = self.committed_history_len();
        if committed <= self.history_inserted_cells {
            return Ok(());
        }

        let width = terminal.viewport_area.width.max(1);
        let start = self.history_inserted_cells.min(committed);
        let mut lines = Vec::new();
        if start == 0 {
            use crate::history_cell::{HistoryCell, SessionHeaderHistoryCell};
            let model = if self.focused == AgentType::Main {
                self.main_model.clone()
            } else {
                self.sub_model.clone()
            };
            lines.extend(
                SessionHeaderHistoryCell::new(model, self.workspace.clone()).display_lines(width),
            );
        }
        let history = &self.history[start..committed];
        if self.raw_output {
            lines.extend(crate::history_cell::render_raw_history_lines_for_agent(
                history,
                self.focused,
            ));
        } else {
            lines.extend(crate::history_cell::render_history_lines_for_agent(
                history,
                self.focused,
                width,
            ));
        }
        // Separate the first cell of this batch from the prior scrollback tail
        // with one blank row, matching Codex's per-cell breathing row — but
        // skip it when either side already supplies a blank edge (e.g. a
        // message cell's trailing blank) so scrollback is never double-spaced.
        let first_cell_has_leading_blank = history
            .iter()
            .find(|cell| crate::history_cell::belongs_to_agent(cell, self.focused))
            .is_some_and(crate::history_cell::cell_has_leading_blank);
        if start > 0 && !self.scrollback_tail_blank && !first_cell_has_leading_blank {
            lines.insert(0, Line::from(""));
        }
        crate::insert_history::insert_history_lines(terminal, &lines)?;
        if let Some(last_cell) = history
            .iter()
            .rev()
            .find(|cell| crate::history_cell::belongs_to_agent(cell, self.focused))
        {
            self.scrollback_tail_blank = crate::history_cell::cell_has_trailing_blank(last_cell);
        }
        self.history_inserted_cells = committed;
        Ok(())
    }

    pub(crate) fn committed_history_len(&self) -> usize {
        let mut end = self.history.len();
        loop {
            let Some(cell) = self.history.get(end.saturating_sub(1)) else {
                break;
            };
            let active = match cell {
                Cell::AgentMessage { .. } => true,
                Cell::ToolGroup { items, .. } => items
                    .iter()
                    .any(|item| item.result.is_none() && item.error.is_none()),
                _ => false,
            };
            if !active {
                break;
            }
            end = end.saturating_sub(1);
        }
        end
    }

    pub fn new(
        manager: Arc<LoopManager>,
        bus: Bus,
        autoreport_home: PathBuf,
        workspace: PathBuf,
        main_model: String,
        sub_model: String,
    ) -> Self {
        let index = FileIndex::new(&workspace);
        index.refresh();
        let rx = bus.subscribe();
        Self {
            manager,
            bus,
            autoreport_home,
            rx,
            workspace,
            main_model,
            sub_model,
            history: Vec::new(),
            history_inserted_cells: 0,
            scrollback_needs_clear: false,
            scrollback_tail_blank: false,
            statuses: HashMap::new(),
            status_since: HashMap::new(),
            focused: AgentType::Main,
            composer: ChatComposer::new(AgentType::Main.label()),
            pending_input_preview: PendingInputPreview::new(),
            queued_inputs: HashMap::new(),
            pending_submissions: Vec::new(),
            suppress_until_idle: HashSet::new(),
            paste_burst: PasteBurst::default(),
            index,
            overlay: None,
            pager: None,
            agent_picker: None,
            raw_output: false,
            want_config: false,
            want_models: false,
            want_models_after_config: false,
            want_environment: false,
            ide_enabled: false,
            ide_warned: false,
            pending_approvals: VecDeque::new(),
            pending_user_inputs: VecDeque::new(),
            user_input_requests: HashMap::new(),
            exit_requested: false,
            clipboard_lease: None,
            frame_requester: None,
        }
    }

    pub async fn run(mut self) -> io::Result<()> {
        enable_raw_mode()?;
        // The main chat runs inline on the primary screen so the terminal
        // emulator owns wheel scrolling and real scrollback (Codex's default).
        // Full-screen overlays enter the alternate screen per-surface in the
        // loop below, so their content is discarded on close.
        let backend = CrosstermBackend::new(io::stdout());

        // Codex batches OSC cursor-position, OSC 10/11 palette, and a keyboard-
        // enhancement probe into one bounded startup query before crossterm's
        // event stream owns stdin. Keep that ordering so replies cannot be
        // mistaken for input, and let the shared palette choose the correct
        // RGB/ANSI representation. (tui.rs init(), vendored verbatim in shape.)
        #[cfg(unix)]
        let startup_probe = {
            use crate::terminal_probe::StartupKeyboardEnhancementProbe;

            let started_at = std::time::Instant::now();
            let keyboard_probe = StartupKeyboardEnhancementProbe::Query;
            match crate::terminal_probe::startup(
                crate::terminal_probe::DEFAULT_TIMEOUT,
                keyboard_probe,
            ) {
                Ok(probe) => {
                    tracing::info!(
                        duration_ms = %started_at.elapsed().as_millis(),
                        cursor_position = probe.cursor_position.is_some(),
                        default_colors = probe.default_colors.is_some(),
                        keyboard_enhancement_supported = ?probe.keyboard_enhancement_supported,
                        "terminal startup probes completed"
                    );
                    probe
                }
                Err(err) => {
                    tracing::warn!(
                        duration_ms = %started_at.elapsed().as_millis(),
                        "terminal startup probes failed: {err}"
                    );
                    crate::terminal_probe::StartupProbe {
                        cursor_position: None,
                        default_colors: None,
                        keyboard_enhancement_supported: None,
                    }
                }
            }
        };

        #[cfg(unix)]
        crate::terminal_palette::set_default_colors_from_startup_probe(
            startup_probe.default_colors,
        );

        #[cfg(unix)]
        let cursor_pos = startup_probe.cursor_position.unwrap_or_else(|| {
            tracing::warn!("initial cursor position probe timed out; defaulting to origin");
            ratatui::layout::Position { x: 0, y: 0 }
        });

        #[cfg(not(unix))]
        let cursor_pos = ratatui::layout::Position { x: 0, y: 0 };

        let mut terminal = Terminal::with_options_and_cursor_position(backend, cursor_pos)?;

        // Codex inserts session notices before replayed transcript cells, so a
        // resumed conversation starts below the header/help surface.
        self.system(
            "Tip: use /agent to switch report roles, or @ to add a data or reference file.",
            SysKind::Info,
        );
        self.system(
            "Five specialised agents are ready. Tab switches focus; every agent keeps its own history.",
            SysKind::Info,
        );
        self.restore_history().await;

        // Recover any approval request registered before we subscribed (a
        // broadcast publish with no receiver would otherwise be lost). Re-run
        // on every later `Lagged` event inside the loop.
        self.reconcile_approvals().await;

        let mut events = EventStream::new();
        // Codex uses a coalescing frame scheduler instead of a fixed polling
        // sleep. The same requester drives the Working animation and elapsed
        // timer without redrawing idle sessions.
        let (draw_tx, mut draw_rx) = tokio::sync::broadcast::channel(8);
        let frame_requester = FrameRequester::new(draw_tx);
        self.frame_requester = Some(frame_requester.clone());
        let mut last_full_frame_surface = self.full_frame_surface();
        loop {
            if self.want_models {
                self.want_models = false;
                let settings = load_settings(&self.autoreport_home).unwrap_or_default();
                let presets = autoreport_core::sync::load_presets(&self.autoreport_home);
                self.activate_full_frame_overlay(Overlay::Configuration(ConfigurationFlow::new(
                    settings,
                    self.autoreport_home.clone(),
                    presets,
                )));
            } else if self.want_config {
                self.want_config = false;
                let settings = load_settings(&self.autoreport_home).unwrap_or_default();
                let presets = autoreport_core::sync::load_presets(&self.autoreport_home);
                self.activate_full_frame_overlay(Overlay::Api(ConfigScreen::new_with_presets(
                    settings,
                    self.autoreport_home.clone(),
                    presets,
                )));
            }
            if self.want_environment {
                self.want_environment = false;
                self.activate_full_frame_overlay(Overlay::Environment(EnvironmentScreen::new(
                    self.autoreport_home.clone(),
                    self.workspace.clone(),
                )));
            }
            let full_frame_surface = self.full_frame_surface();
            if full_frame_surface != last_full_frame_surface {
                let entering_overlay = full_frame_surface != FullFrameSurface::Chat;
                if entering_overlay {
                    // Full-screen overlays (config/pager/environment) run in the
                    // alternate screen so their content is discarded on close
                    // and the main chat's real scrollback is preserved
                    // underneath — Codex's `enter_alt_screen`. Enabling
                    // alternate scroll lets terminals translate the wheel to
                    // arrow keys inside the overlay only.
                    execute!(io::stdout(), EnterAlternateScreen, EnableAlternateScroll)?;
                    terminal.clear_visible_screen()?;
                } else {
                    // Leaving the alternate screen restores the main buffer
                    // exactly as the inline chat left it; just force a full
                    // redraw of the viewport — Codex's `leave_alt_screen`.
                    execute!(io::stdout(), DisableAlternateScroll, LeaveAlternateScreen)?;
                    terminal.invalidate_viewport();
                }
                last_full_frame_surface = full_frame_surface;
            }
            let anchor_viewport_top = self.scrollback_needs_clear;
            if self.scrollback_needs_clear {
                terminal.clear_scrollback_and_visible_screen_ansi()?;
                // Codex's clear path moves the inline viewport to row zero
                // before replaying the fresh header. Otherwise the old
                // bottom-aligned viewport can leave the terminal scrollbar
                // anchored to the previous conversation tail.
                let mut area = terminal.viewport_area;
                area.y = 0;
                terminal.set_viewport_area(area);
                self.scrollback_needs_clear = false;
            }
            self.prepare_chat_viewport(&mut terminal, anchor_viewport_top)?;
            self.flush_history_to_scrollback(&mut terminal)?;
            terminal.draw(|f| self.draw(f))?;

            tokio::select! {
                maybe_ev = events.next() => {
                    let Some(Ok(ev)) = maybe_ev else { break; };
                    if !self.handle_event(ev) { break; }
                    if self.paste_burst.is_active() {
                        frame_requester.schedule_frame_in(
                            PasteBurst::recommended_flush_delay(),
                        );
                    }
                }
                msg = self.rx.recv() => {
                    match msg {
                        Ok(m) => {
                            self.apply_bus(m);
                            // State changes should redraw immediately; the
                            // status widget owns subsequent animation ticks.
                            frame_requester.schedule_frame();
                        },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // A lagging receiver skips messages; recover any
                            // approval/user-input request that may have been
                            // dropped so it cannot deadlock the awaiting agent.
                            self.reconcile_approvals().await;
                            continue;
                        }
                        Err(_) => break,
                    }
                }
                _ = draw_rx.recv() => {
                    self.flush_paste_burst_if_due(Instant::now());
                    self.poll_user_input_deadlines();
                    self.recompute_slash();
                    self.recompute_mention();
                }
            }
        }

        self.manager.shutdown().await;
        disable_raw_mode()?;
        execute!(io::stdout(), DisableAlternateScroll, LeaveAlternateScreen)?;
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
                            "user" => {
                                self.composer.record_history(text.clone());
                                self.history.push(Cell::User {
                                    _agent: agent,
                                    text,
                                });
                            }
                            "assistant" => self.history.push(Cell::AgentMarkdown { agent, text }),
                            _ => {}
                        }
                    }
                    // Codex renders finalized reasoning summaries as a dimmed
                    // transcript cell (`ReasoningSummaryCell`); raw encrypted
                    // reasoning stays out. We follow codex's default: summary
                    // text only, content (raw thinking) is opt-in and dropped.
                    // The live spinner is driven separately by the
                    // `BusMessage::AgentReasoning` streaming path.
                    ResponseItem::Reasoning { summary, .. } => {
                        let parts: Vec<String> =
                            summary.iter().map(|s| s.text().to_string()).collect();
                        let (_header, body) =
                            crate::history_cell::split_reasoning_summary_parts(&parts);
                        // Skip reasoning whose body is empty (e.g. a `<!-- -->`
                        // placeholder). `Cell::Reasoning::transcript_only` is
                        // always false here: we render the summary in the main
                        // transcript, matching codex's default.
                        let body_is_empty = body.trim().is_empty();
                        if !body_is_empty {
                            self.history.push(Cell::Reasoning {
                                agent,
                                text: body,
                                transcript_only: false,
                            });
                        }
                    }
                    ResponseItem::FunctionCall {
                        call_id,
                        name,
                        arguments,
                        ..
                    } => {
                        if name == "request_user_input"
                            && let Ok(args) = serde_json::from_str::<
                                autoreport_core::request_user_input::RequestUserInputArgs,
                            >(&arguments)
                        {
                            self.user_input_requests
                                .insert(call_id, (agent, args.questions));
                            continue;
                        }
                        let item = ToolEntry {
                            name,
                            args: match serde_json::from_str(&arguments) {
                                Ok(value) => value,
                                Err(_) => serde_json::Value::String(arguments),
                            },
                            result: None,
                            error: None,
                            call_id: Some(call_id),
                            started_at: None,
                        };
                        if let Some(Cell::ToolGroup {
                            agent: owner,
                            items,
                        }) = self.history.last_mut()
                            && *owner == agent
                        {
                            items.push(item);
                        } else {
                            self.history.push(Cell::ToolGroup {
                                agent,
                                items: vec![item],
                            });
                        }
                    }
                    ResponseItem::FunctionCallOutput { call_id, output } => {
                        if let Some((owner, questions)) = self.user_input_requests.remove(&call_id)
                        {
                            let answers = serde_json::from_str::<serde_json::Value>(&output)
                                .ok()
                                .and_then(|value| value.get("answers").cloned())
                                .and_then(|value| serde_json::from_value(value).ok())
                                .unwrap_or_default();
                            self.history.push(Cell::UserInputResult {
                                agent: owner,
                                questions,
                                answers,
                                interrupted: false,
                            });
                            continue;
                        }
                        // Rollout replay can contain several parallel calls;
                        // correlate by the persisted call id rather than
                        // assuming the last call finished first.
                        for cell in self.history.iter_mut().rev() {
                            let Cell::ToolGroup {
                                agent: owner,
                                items,
                            } = cell
                            else {
                                continue;
                            };
                            if *owner != agent {
                                continue;
                            }
                            if let Some(entry) = items
                                .iter_mut()
                                .rev()
                                .find(|entry| entry.call_id.as_deref() == Some(call_id.as_str()))
                            {
                                entry.result = Some(serde_json::Value::String(output));
                                break;
                            }
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

    /// Clear only the visible transcript, mirroring Codex's global Ctrl-L.
    /// The agent/session context is deliberately untouched; the next draw
    /// still renders the session header and new replies continue the same
    /// conversation.
    pub(crate) fn clear_terminal_ui(&mut self) {
        self.history.clear();
        self.history_inserted_cells = 0;
        self.scrollback_needs_clear = true;
        self.scrollback_tail_blank = false;
        self.composer.clear_popups();
        self.agent_picker = None;
        self.overlay = None;
        self.cancel_all_user_inputs();
        self.user_input_requests.clear();
        self.queued_inputs.clear();
        self.pending_submissions.clear();
        self.suppress_until_idle.clear();
        self.pending_input_preview.set_queued_messages(Vec::new());
        self.pager = None;
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
            // Probe connectivity off the event loop: the IDE IPC fetch can block
            // up to 5s, which would freeze the TUI. Toggle on optimistically and
            // report the probe outcome asynchronously via a system notice.
            self.system("IDE context is on.", SysKind::Info);
            let bus = self.bus.clone();
            let workspace = self.workspace.clone();
            tokio::spawn(async move {
                let probe = tokio::task::spawn_blocking(move || {
                    crate::ide_context::fetch_ide_context(&workspace)
                })
                .await;
                let notice = match probe {
                    Ok(Ok(context)) => {
                        if crate::ide_context::has_prompt_context(&context) {
                            "IDE context is on. Future messages will include your current IDE selection and open tabs.".to_string()
                        } else {
                            "IDE context is on. Connected to your IDE.".to_string()
                        }
                    }
                    Ok(Err(err)) => {
                        format!(
                            "IDE context could not be enabled: {}",
                            err.user_facing_hint()
                        )
                    }
                    Err(_) => "IDE context probe failed.".to_string(),
                };
                bus.publish(autoreport_core::types::BusMessage::SystemNotice {
                    agent_type: None,
                    content: notice,
                });
            });
        } else {
            self.system("IDE context is off.", SysKind::Info);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FullFrameSurface, chat_viewport_height, chat_viewport_y, select_full_frame_surface,
    };
    use crate::chatwidget::{extract_mentions, render_tool_result_lines};
    use crate::slash_command;
    use ratatui::layout::{Rect, Size};

    #[test]
    fn initial_chat_uses_codex_sized_viewport_at_120_by_30() {
        assert_eq!(chat_viewport_height(30, 11, true), 19);
    }

    #[test]
    fn active_chat_keeps_content_driven_viewport_height() {
        assert_eq!(chat_viewport_height(30, 11, false), 11);
    }

    #[test]
    fn initial_chat_viewport_is_clamped_to_small_terminals() {
        assert_eq!(chat_viewport_height(12, 8, true), 12);
    }

    #[test]
    fn ordinary_chat_draw_preserves_viewport_origin() {
        let previous = Rect::new(0, 3, 120, 19);
        assert_eq!(
            chat_viewport_y(Size::new(120, 30), previous, 19, false, false),
            3
        );
    }

    #[test]
    fn clear_and_resize_are_the_only_viewport_reanchors() {
        let previous = Rect::new(0, 3, 120, 19);
        assert_eq!(
            chat_viewport_y(Size::new(120, 30), previous, 19, false, true),
            0
        );
        assert_eq!(
            chat_viewport_y(Size::new(120, 30), previous, 15, false, false),
            7
        );
    }

    #[test]
    fn full_frame_surface_uses_chat_or_pager_without_configuration() {
        assert_eq!(
            select_full_frame_surface(None, false),
            FullFrameSurface::Chat
        );
        assert_eq!(
            select_full_frame_surface(None, true),
            FullFrameSurface::Pager
        );
    }

    #[test]
    fn configuration_surface_wins_over_a_stale_pager() {
        for configuration in [
            FullFrameSurface::ApiConfiguration,
            FullFrameSurface::ConfigurationFlow,
            FullFrameSurface::EnvironmentConfiguration,
        ] {
            assert_eq!(
                select_full_frame_surface(Some(configuration), true),
                configuration
            );
        }
    }

    #[test]
    fn extracts_mentions_skipping_emails() {
        let m =
            extract_mentions("see @Data/Raw.csv and contact me@example.com and @Report/main.tex");
        assert_eq!(
            m,
            vec!["Data/Raw.csv".to_string(), "Report/main.tex".to_string()]
        );
    }

    #[test]
    fn extracts_quoted_mentions_with_spaces() {
        let m = extract_mentions("compare @\"Data/Raw Files/trial one.csv\" with @Report/main.tex");
        assert_eq!(
            m,
            vec![
                "Data/Raw Files/trial one.csv".to_string(),
                "Report/main.tex".to_string()
            ]
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
    fn slash_command_catalog_removes_only_models_alias() {
        assert!(
            slash_command::matches("agent")
                .iter()
                .any(|m| m.name == "agent")
        );
        assert!(
            slash_command::matches("model")
                .iter()
                .any(|m| m.name == "model")
        );
        assert!(slash_command::matches("models").is_empty());
    }
}
