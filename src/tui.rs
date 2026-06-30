//! Codex-style terminal UI.
//!
//! Layout: top status bar, scrolling markdown-rendered history, bottom input
//! box, and an `@` file-mention popup. Built on ratatui + crossterm. Agent loops
//! run in background tasks; the TUI subscribes to the bus to render their
//! streamed output.

use crate::bus::Bus;
use crate::codex_render::markdown_render;
use crate::config::{load_settings, save_settings};
use crate::config_ui::{ConfigScreen, Outcome};
use crate::file_search::FileIndex;
use crate::runtime::LoopManager;
use crate::types::{AgentStatus, AgentType, BusMessage};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_stream::StreamExt;

/// One rendered history entry.
enum Cell {
    User { agent: AgentType, text: String },
    Assistant { agent: AgentType, text: String, streaming: bool },
    Tool {
        agent: AgentType,
        name: String,
        args: String,
        result: Option<String>,
        error: Option<String>,
    },
    System { text: String, kind: SysKind },
}

#[derive(Clone, Copy)]
enum SysKind {
    Info,
    Error,
}

/// Active `@`-mention completion state.
struct Mention {
    /// Byte index of the `@` in the input.
    start: usize,
    /// Cursor byte index at the time of capture.
    cursor: usize,
    matches: Vec<String>,
    selected: usize,
}

const MENTION_LIMIT: usize = 8;

pub struct Tui {
    manager: Arc<LoopManager>,
    workspace: PathBuf,
    workspace_display: String,
    provider_id: String,
    history: Vec<Cell>,
    statuses: HashMap<AgentType, AgentStatus>,
    focused: AgentType,
    input: String,
    cursor: usize,
    scroll: usize,
    rx: tokio::sync::broadcast::Receiver<BusMessage>,
    index: FileIndex,
    mention: Option<Mention>,
    overlay: Option<ConfigScreen>,
    want_config: bool,
    tick: usize,
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Tui {
    pub fn new(
        manager: Arc<LoopManager>,
        bus: Bus,
        workspace: PathBuf,
        provider_id: String,
    ) -> Self {
        let workspace_display = workspace.display().to_string();
        let index = FileIndex::new(&workspace);
        index.refresh();
        Self {
            manager,
            rx: bus.subscribe(),
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
            overlay: None,
            want_config: false,
            tick: 0,
        }
    }

    pub async fn run(mut self) -> io::Result<()> {
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
                let settings = load_settings(&self.workspace).unwrap_or_default();
                self.overlay = Some(ConfigScreen::new(settings, self.workspace.clone()));
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

        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen)?;
        Ok(())
    }

    fn system(&mut self, text: &str, kind: SysKind) {
        self.history.push(Cell::System {
            text: text.to_string(),
            kind,
        });
    }

    fn apply_bus(&mut self, msg: BusMessage) {
        match msg {
            BusMessage::AgentResponse { agent_type, content, streaming } => {
                if !content.is_empty() && streaming {
                    if let Some(Cell::Assistant { agent, text, streaming: true }) =
                        self.history.last_mut()
                    {
                        if *agent == agent_type {
                            text.push_str(&content);
                            return;
                        }
                    }
                    self.history.push(Cell::Assistant {
                        agent: agent_type,
                        text: content,
                        streaming: true,
                    });
                } else if !streaming {
                    for cell in self.history.iter_mut().rev() {
                        if let Cell::Assistant { agent, streaming, .. } = cell {
                            if *agent == agent_type {
                                *streaming = false;
                                break;
                            }
                        }
                    }
                }
            }
            BusMessage::ToolCall { agent_type, tool_name, arguments } => {
                self.history.push(Cell::Tool {
                    agent: agent_type,
                    name: tool_name,
                    args: serde_json::to_string(&arguments).unwrap_or_default(),
                    result: None,
                    error: None,
                });
            }
            BusMessage::ToolResult { agent_type, tool_name, result, error } => {
                for cell in self.history.iter_mut().rev() {
                    if let Cell::Tool { agent, name, result: r, error: e, .. } = cell {
                        if *agent == agent_type && name == &tool_name && r.is_none() {
                            *r = Some(pretty(&result));
                            *e = error.clone();
                            break;
                        }
                    }
                }
            }
            BusMessage::StatusChange { agent_type, status } => {
                self.statuses.insert(agent_type, status);
            }
            BusMessage::Error { message, .. } => {
                self.system(&format!("error: {message}"), SysKind::Error);
            }
            _ => {}
        }
    }

    fn handle_event(&mut self, ev: Event) -> bool {
        let Event::Key(key) = ev else {
            return true;
        };

        // While the /config overlay is open, route all keys to it.
        if let Some(screen) = self.overlay.as_mut() {
            if let Some(outcome) = screen.handle_key(key) {
                match outcome {
                    Outcome::Saved => {
                        if let Err(e) = save_settings(&self.workspace, &screen.settings) {
                            self.system(&format!("config save failed: {e}"), SysKind::Error);
                        } else {
                            self.system(
                                "config saved to autoreport.config.yaml — restart to apply",
                                SysKind::Info,
                            );
                        }
                    }
                    Outcome::Cancelled => {
                        self.system("config unchanged", SysKind::Info);
                    }
                }
                self.overlay = None;
            }
            return true;
        }

        // While the mention popup is open, intercept navigation keys.
        if self.mention.is_some() {
            match key.code {
                KeyCode::Down => {
                    self.move_mention(1);
                    return true;
                }
                KeyCode::Up => {
                    self.move_mention(-1);
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter => {
                    self.accept_mention();
                    return true;
                }
                KeyCode::Esc => {
                    self.mention = None;
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => self.cycle_agent(),
            KeyCode::BackTab => self.cycle_agent_back(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    // step back one char
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    let next = self.input[self.cursor..].chars().next().unwrap();
                    self.cursor += next.len_utf8();
                }
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc => {
                // ESC: close the mention popup if open, otherwise interrupt the
                // focused agent's active turn (codex semantics).
                if self.mention.is_some() {
                    self.mention = None;
                } else {
                    self.manager.interrupt(self.focused);
                }
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            _ => {}
        }

        // Any input that wasn't intercepted resets manual scroll.
        if !matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) {
            self.scroll = 0;
        }

        // Recompute the mention popup based on the token under the cursor.
        self.recompute_mention();
        true
    }

    fn move_mention(&mut self, dir: i32) {
        if let Some(m) = self.mention.as_mut() {
            if m.matches.is_empty() {
                return;
            }
            let n = m.matches.len() as i32;
            let mut s = m.selected as i32 + dir;
            if s < 0 {
                s = n - 1;
            } else if s >= n {
                s = 0;
            }
            m.selected = s as usize;
        }
    }

    fn accept_mention(&mut self) {
        let Some(m) = self.mention.take() else {
            return;
        };
        let Some(path) = m.matches.get(m.selected).cloned() else {
            return;
        };
        // Replace the query (between @ and cursor) with the chosen path.
        let at = m.start; // index of '@'
        let after_at = at + 1;
        let end = m.cursor.min(self.input.len());
        let mut new_input = String::new();
        new_input.push_str(&self.input[..after_at]);
        new_input.push_str(&path);
        let tail_start = end;
        new_input.push_str(&self.input[tail_start..]);
        new_input.push(' ');
        let new_cursor = after_at + path.len() + 1;
        self.input = new_input;
        self.cursor = new_cursor;
    }

    /// Detect an open `@token` under the cursor and (re)build the popup.
    fn recompute_mention(&mut self) {
        let bytes = self.input.as_bytes();
        if self.cursor == 0 || self.cursor > bytes.len() {
            self.mention = None;
            return;
        }
        // Walk back from cursor collecting mention-name chars until we hit '@'.
        let mut i = self.cursor;
        let mut query_len = 0usize;
        while i > 0 {
            let prev = &bytes[i - 1..i];
            let Ok(s) = std::str::from_utf8(prev) else {
                break;
            };
            let c = s.chars().next().unwrap();
            if c == '@' {
                break;
            } else if is_mention_char(c) {
                i -= c.len_utf8();
                query_len += 1;
                continue;
            } else {
                // whitespace or punctuation closes the token
                self.mention = None;
                return;
            }
        }
        if i == 0 || bytes[i - 1] != b'@' {
            self.mention = None;
            return;
        }
        // The '@' must be at start or preceded by whitespace (avoid emails).
        let at_idx = i - 1;
        let prev_ok = at_idx == 0 || {
            let prev = &bytes[at_idx - 1..at_idx];
            std::str::from_utf8(prev)
                .ok()
                .and_then(|s| s.chars().next())
                .map(|c| c.is_whitespace())
                .unwrap_or(true)
        };
        if !prev_ok {
            self.mention = None;
            return;
        }
        let query = &self.input[at_idx + 1..at_idx + 1 + query_len];
        let matches = self.index.search(query, MENTION_LIMIT);
        let selected = 0;
        self.mention = Some(Mention {
            start: at_idx,
            cursor: self.cursor,
            matches,
            selected,
        });
    }

    fn cycle_agent(&mut self) {
        let order = AgentType::ALL;
        let idx = order.iter().position(|a| *a == self.focused).unwrap_or(0);
        self.focused = order[(idx + 1) % order.len()];
    }
    fn cycle_agent_back(&mut self) {
        let order = AgentType::ALL;
        let idx = order.iter().position(|a| *a == self.focused).unwrap_or(0);
        self.focused = order[(idx + order.len() - 1) % order.len()];
    }

    fn submit(&mut self) {
        self.mention = None;
        let raw = std::mem::take(&mut self.input);
        self.cursor = 0;
        let text = raw.trim().to_string();
        if text.is_empty() {
            return;
        }

        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd.trim());
            return;
        }

        self.history.push(Cell::User {
            agent: self.focused,
            text: text.clone(),
        });
        let expanded = self.expand_mentions(&text);
        self.manager
            .submit(self.focused, expanded, crate::types::MessageSource::User);
    }

    /// Expand `@rel/path` references: the visible text is unchanged, but the
    /// message handed to the agent has each referenced file's contents appended
    /// so the model can see them (codex expands mentions into context).
    fn expand_mentions(&self, text: &str) -> String {
        let refs = extract_mentions(text);
        if refs.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();
        out.push_str("\n\n# Referenced files");
        for rel in refs {
            let path = self.workspace.join(&rel);
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let truncated = if content.len() > 16_000 {
                        format!("{}\n…(truncated)", &content[..16_000])
                    } else {
                        content
                    };
                    out.push_str(&format!("\n\n## @{rel}\n```\n{truncated}\n```"));
                }
                Err(_) => {
                    out.push_str(&format!("\n\n## @{rel}\n(not found)"));
                }
            }
        }
        out
    }

    fn run_command(&mut self, cmd: &str) {
        let mut parts = cmd.split_whitespace();
        let name = parts.next().unwrap_or("");
        let rest: String = parts.collect::<Vec<_>>().join(" ");
        match name {
            "help" | "h" | "?" => self.system(
                "Commands:\n  /agents           list agents + statuses\n  /switch <agent>   focus an agent\n  /config           view & edit provider settings\n  /clear            clear focused agent's context\n  /compact          compact focused agent's context\n  /new              reset focused agent\n  /manifest         show produced files\n  /index            rebuild the @ file index\n  /quit             exit",
                SysKind::Info,
            ),
            "config" => {
                self.want_config = true;
            }
            "agents" => {
                let mut s = String::from("Agents:\n");
                for a in AgentType::ALL {
                    let st = self.statuses.get(&a).copied().unwrap_or(AgentStatus::Idle);
                    let mark = if a == self.focused { "▶" } else { " " };
                    s.push_str(&format!("  {mark} {} [{:?}]\n", a.label(), st));
                }
                self.system(s.trim_end(), SysKind::Info);
            }
            "switch" => {
                if let Ok(a) = rest.parse::<AgentType>() {
                    self.focused = a;
                    self.system(&format!("focused: {}", a.label()), SysKind::Info);
                } else {
                    self.system(
                        "usage: /switch <main|data_analysis|plotting|theory|report>",
                        SysKind::Error,
                    );
                }
            }
            "clear" => {
                self.manager.clear_context(self.focused);
                self.system(&format!("cleared {} context", self.focused.label()), SysKind::Info);
            }
            "compact" => {
                self.manager.compact(self.focused);
                self.system(&format!("compacting {} context…", self.focused.label()), SysKind::Info);
            }
            "new" => {
                self.manager.clear_context(self.focused);
                self.system(&format!("reset {}", self.focused.label()), SysKind::Info);
            }
            "index" => {
                self.index.refresh();
                self.system("@ file index rebuilt", SysKind::Info);
            }
            "manifest" => {
                let snap = self.manager.manifest_snapshot(None);
                self.system(&format!("manifests:\n{}", pretty(&snap)), SysKind::Info);
            }
            "quit" | "exit" => {
                self.system("bye", SysKind::Info);
                std::process::exit(0);
            }
            "" => {}
            other => {
                self.system(&format!("unknown command: /{other}"), SysKind::Error);
            }
        }
    }

    fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        self.draw_status(f, chunks[0]);
        self.draw_history(f, chunks[1]);
        self.draw_input(f, chunks[2]);
        self.draw_hints(f, chunks[3]);

        if self.mention.is_some() {
            self.draw_mention_popup(f, chunks[2]);
        }

        if let Some(screen) = self.overlay.as_mut() {
            screen.draw(f);
        }
    }

    fn draw_status(&self, f: &mut Frame<'_>, area: Rect) {
        let mut spans = vec![
            Span::styled(
                " AutoReportCLI ",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
            ),
            Span::raw(" "),
            Span::styled(self.workspace_display.clone(), Style::default().fg(Color::DarkGray)),
            Span::raw("  "),
            Span::styled(self.provider_id.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("  focused: "),
            Span::styled(
                self.focused.label().to_string(),
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Green),
            ),
        ];
        for a in AgentType::ALL {
            let st = self.statuses.get(&a).copied().unwrap_or(AgentStatus::Idle);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{} {}", status_mark(st), a.label()),
                Style::default().fg(status_color(st)),
            ));
        }
        let block = Block::default().borders(Borders::BOTTOM);
        let para = Paragraph::new(Line::from(spans)).block(block);
        f.render_widget(para, area);
    }

    fn draw_history(&self, f: &mut Frame<'_>, area: Rect) {
        let width = area.width as usize;
        let lines = self.render_history_lines(width);
        let total = lines.len();
        let visible = area.height as usize;
        let start = if total > visible {
            total.saturating_sub(visible + self.scroll)
        } else {
            0
        };
        let start = start.min(total);
        let end = (start + visible).min(total);
        let shown: Vec<Line> = lines.into_iter().skip(start).take(end.saturating_sub(start)).collect();
        let para = Paragraph::new(shown);
        f.render_widget(para, area);
    }

    fn render_history_lines(&self, width: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line<'static>> = Vec::new();
        for cell in &self.history {
            match cell {
                Cell::User { agent, text } => {
                    out.push(Line::from(vec![Span::styled(
                        format!("▸ {}", agent.label()),
                        Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue),
                    )]));
                    for line in render_user_text(text) {
                        out.push(line);
                    }
                    out.push(Line::from(""));
                }
                Cell::Assistant { agent, text, streaming } => {
                    let label = Span::styled(
                        format!("{} ", agent.label()),
                        Style::default().add_modifier(Modifier::BOLD).fg(agent_color(*agent)),
                    );
                    if text.is_empty() {
                        if *streaming {
                            let frame = SPINNER[self.tick % SPINNER.len()];
                            out.push(Line::from(vec![
                                label,
                                Span::styled(frame, Style::default().fg(Color::Yellow)),
                                Span::styled(" thinking…", Style::default().fg(Color::DarkGray)),
                            ]));
                        }
                        out.push(Line::from(""));
                        continue;
                    }
                    let mut md =
                        markdown_render::render_markdown_text_with_width(text, Some(width)).lines;
                    if let Some(first) = md.first_mut() {
                        // Prefix the very first rendered line with the agent label.
                        let mut spans = vec![label];
                        spans.append(&mut first.spans);
                        first.spans = spans;
                    } else {
                        out.push(Line::from(label));
                    }
                    if *streaming {
                        if let Some(last) = md.last_mut() {
                            last.spans.push(Span::styled("▍", Style::default().fg(Color::Yellow)));
                        }
                    }
                    out.extend(md);
                    out.push(Line::from(""));
                }
                Cell::Tool { agent, name, args, result, error } => {
                    let title = format!("  ⚒ {} · {}({})", agent.label(), name, truncate(args, 60));
                    out.push(Line::from(Span::styled(title, Style::default().fg(Color::Yellow))));
                    if let Some(err) = error {
                        for l in err.lines() {
                            out.push(Line::from(Span::styled(
                                format!("    error: {l}"),
                                Style::default().fg(Color::Red),
                            )));
                        }
                    } else if let Some(res) = result {
                        for l in truncate(res, 400).lines() {
                            out.push(Line::from(Span::styled(
                                format!("    {l}"),
                                Style::default().fg(Color::DarkGray),
                            )));
                        }
                    }
                    out.push(Line::from(""));
                }
                Cell::System { text, kind } => {
                    let color = match kind {
                        SysKind::Info => Color::DarkGray,
                        SysKind::Error => Color::Red,
                    };
                    for l in text.lines() {
                        out.push(Line::from(Span::styled(format!("  {l}"), Style::default().fg(color))));
                    }
                    out.push(Line::from(""));
                }
            }
        }
        out
    }

    fn draw_input(&self, f: &mut Frame<'_>, area: Rect) {
        let title = format!(" message to {} ", self.focused.label());
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().add_modifier(Modifier::BOLD).fg(Color::Green)));
        let prompt = if self.input.is_empty() {
            "  /help for commands, @ to mention a file, then describe what you need…".to_string()
        } else {
            format!("  {}", self.input)
        };
        let style = if self.input.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };
        let para = Paragraph::new(prompt).style(style).block(block);
        f.render_widget(para, area);
    }

    fn draw_hints(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = " Tab: switch agent   Enter: send   ↑/↓: scroll   @: mention   /help   Ctrl+C: quit";
        let para = Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Left);
        f.render_widget(para, area);
    }

    fn draw_mention_popup(&self, f: &mut Frame<'_>, anchor: Rect) {
        let Some(m) = self.mention.as_ref() else {
            return;
        };
        let count = m.matches.len().min(MENTION_LIMIT).clamp(1, MENTION_LIMIT);
        let height = (count + 2) as u16;
        let width = 60u16.min(anchor.width);
        let popup_area = Rect {
            x: anchor.x,
            y: anchor.y.saturating_sub(height),
            width,
            height,
        };
        f.render_widget(Clear, popup_area);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(" @ files ", Style::default().fg(Color::Cyan)));
        let mut lines: Vec<Line> = Vec::new();
        if m.matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no matching files",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, p) in m.matches.iter().enumerate() {
                let style = if i == m.selected {
                    Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                let mark = if i == m.selected { "▶ " } else { "  " };
                lines.push(Line::from(Span::styled(format!("{mark}{p}"), style)));
            }
        }
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, popup_area);
    }
}

// --- helpers ---

fn render_user_text(text: &str) -> Vec<Line<'static>> {
    // Render the user's text with @mentions highlighted.
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let chars: Vec<char> = paragraph.chars().collect();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buf = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '@' && i + 1 < chars.len() && is_mention_char(chars[i + 1]) {
                if !buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut buf)));
                }
                let mut run = String::from("@");
                i += 1;
                while i < chars.len() && is_mention_char(chars[i]) {
                    run.push(chars[i]);
                    i += 1;
                }
                spans.push(Span::styled(
                    run,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED),
                ));
            } else {
                buf.push(chars[i]);
                i += 1;
            }
        }
        if !buf.is_empty() {
            spans.push(Span::raw(buf));
        }
        out.push(Line::from(spans));
    }
    out
}

fn is_mention_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')
}

/// Pull `@rel/path` tokens out of arbitrary text (for expansion).
fn extract_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@'
            && i + 1 < chars.len()
            && is_mention_char(chars[i + 1])
            && (i == 0 || chars[i - 1].is_whitespace())
        {
            let mut run = String::new();
            let mut j = i + 1;
            while j < chars.len() && is_mention_char(chars[j]) {
                run.push(chars[j]);
                j += 1;
            }
            if !run.is_empty() {
                out.push(run);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn status_mark(s: AgentStatus) -> &'static str {
    match s {
        AgentStatus::Idle => "○",
        AgentStatus::Thinking => "◐",
        AgentStatus::RunningTool => "◑",
        AgentStatus::Queued => "◔",
        AgentStatus::Error => "✗",
        AgentStatus::DebugMode => "◐",
    }
}

fn status_color(s: AgentStatus) -> Color {
    match s {
        AgentStatus::Idle => Color::DarkGray,
        AgentStatus::Thinking => Color::Yellow,
        AgentStatus::RunningTool => Color::Cyan,
        AgentStatus::Queued => Color::DarkGray,
        AgentStatus::Error => Color::Red,
        AgentStatus::DebugMode => Color::Magenta,
    }
}

fn agent_color(a: AgentType) -> Color {
    match a {
        AgentType::Main => Color::Green,
        AgentType::DataAnalysis => Color::Cyan,
        AgentType::Plotting => Color::Magenta,
        AgentType::Theory => Color::Blue,
        AgentType::Report => Color::Yellow,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

fn pretty(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_mentions_skipping_emails() {
        let m = extract_mentions("see @data/raw.csv and contact me@example.com and @tex/main.tex");
        assert_eq!(m, vec!["data/raw.csv".to_string(), "tex/main.tex".to_string()]);
    }
}
