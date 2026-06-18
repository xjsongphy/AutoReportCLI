//! Codex-style terminal UI.
//!
//! Layout: a top status bar (workspace / provider / focused agent / per-agent
//! statuses), a scrolling history of user turns, assistant streams and tool
//! calls, and a bottom input box. Built on ratatui + crossterm. The agent loops
//! run in background tasks; the TUI subscribes to the bus to render their
//! output as it streams.

use crate::bus::Bus;
use crate::runtime::LoopManager;
use crate::types::{AgentStatus, AgentType, BusMessage};
use crossterm::event::{Event, EventStream, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Terminal;
use std::collections::HashMap;
use std::io::{self, Stdout};
use std::sync::Arc;
use tokio_stream::StreamExt;

type Term = Terminal<CrosstermBackend<Stdout>>;

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

pub struct Tui {
    manager: Arc<LoopManager>,
    workspace: String,
    provider_id: String,
    history: Vec<Cell>,
    statuses: HashMap<AgentType, AgentStatus>,
    focused: AgentType,
    input: String,
    cursor: usize,
    scroll: usize, // lines scrolled from the bottom
    rx: tokio::sync::broadcast::Receiver<BusMessage>,
}

impl Tui {
    pub fn new(
        manager: Arc<LoopManager>,
        bus: Bus,
        workspace: String,
        provider_id: String,
    ) -> Self {
        Self {
            manager,
            rx: bus.subscribe(),
            workspace,
            provider_id,
            history: Vec::new(),
            statuses: HashMap::new(),
            focused: AgentType::Main,
            input: String::new(),
            cursor: 0,
            scroll: 0,
        }
    }

    /// Run the UI to completion (until the user quits).
    pub async fn run(mut self) -> io::Result<()> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend)?;

        self.system(
            "AutoReportCLI ready. Type your request; Tab switches agent. /help for commands.",
            SysKind::Info,
        );

        let mut events = EventStream::new();
        loop {
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
                    if let Some(Cell::Assistant { agent, text, streaming: true }) = self.history.last_mut() {
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
                    // finalize the most recent streaming assistant cell of this agent
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
        match key.code {
            KeyCode::Enter => self.submit(),
            KeyCode::Tab => self.cycle_agent(),
            KeyCode::BackTab => self.cycle_agent_back(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    self.cursor += 1;
                }
            }
            KeyCode::Up => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Down => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += 1;
            }
            _ => {}
        }
        // any input resets manual scroll
        if !matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown) {
            self.scroll = 0;
        }
        true
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
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }

        // Slash commands.
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd.trim());
            return;
        }

        self.history.push(Cell::User {
            agent: self.focused,
            text: text.clone(),
        });
        self.manager.submit(self.focused, text, crate::types::MessageSource::User);
    }

    fn run_command(&mut self, cmd: &str) {
        let mut parts = cmd.split_whitespace();
        let name = parts.next().unwrap_or("");
        let rest: String = parts.collect::<Vec<_>>().join(" ");
        match name {
            "help" | "h" | "?" => {
                self.system(
                    "Commands:\n  /agents           list agents + statuses\n  /switch <agent>   focus an agent\n  /clear            clear focused agent's context\n  /compact          compact focused agent's context\n  /new              reset focused agent\n  /manifest         show produced files\n  /quit             exit",
                    SysKind::Info,
                );
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
                    self.system("usage: /switch <main|data_analysis|plotting|theory|report>", SysKind::Error);
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

    fn draw(&self, f: &mut ratatui::Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(5), Constraint::Length(3), Constraint::Length(1)])
            .split(area);

        self.draw_status(f, chunks[0]);
        self.draw_history(f, chunks[1]);
        self.draw_input(f, chunks[2]);
        self.draw_hints(f, chunks[3]);
    }

    fn draw_status(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let mut spans = vec![
            Span::styled(
                " AutoReportCLI ",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
            ),
            Span::raw(" "),
            Span::styled(self.workspace.clone(), Style::default().fg(Color::DarkGray)),
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
            let mark = status_mark(st);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{} {}", mark, a.label()),
                Style::default().fg(status_color(st)),
            ));
        }
        let block = Block::default().borders(Borders::BOTTOM);
        let para = Paragraph::new(Line::from(spans)).block(block);
        f.render_widget(para, area);
    }

    fn draw_history(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let lines = self.render_history_lines(area.width as usize);
        let total = lines.len();
        let visible = area.height as usize;
        // anchor to bottom unless scrolled up
        let start = if total > visible {
            total.saturating_sub(visible + self.scroll)
        } else {
            0
        };
        let start = start.min(total);
        let end = (start + visible).min(total);
        let shown: Vec<Line> = lines.into_iter().skip(start).take(end.saturating_sub(start)).collect();
        let block = Block::default().borders(Borders::empty());
        let para = Paragraph::new(shown).block(block);
        f.render_widget(para, area);
    }

    fn render_history_lines<'a>(&'a self, width: usize) -> Vec<Line<'a>> {
        let mut out: Vec<Line> = Vec::new();
        let w = width.max(20);
        for cell in &self.history {
            match cell {
                Cell::User { agent, text } => {
                    out.push(Line::from(vec![
                        Span::styled(
                            format!("▸ {}", agent.label()),
                            Style::default().add_modifier(Modifier::BOLD).fg(Color::Blue),
                        ),
                    ]));
                    for l in wrap(text, w) {
                        out.push(Line::from(Span::styled(l, Style::default().fg(Color::Gray))));
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
                            out.push(Line::from(vec![
                                label,
                                Span::styled("thinking…", Style::default().fg(Color::DarkGray)),
                            ]));
                        }
                        out.push(Line::from(""));
                        continue;
                    }
                    let lines = wrap(text, w);
                    for (i, l) in lines.iter().enumerate() {
                        if i == 0 {
                            out.push(Line::from(vec![label.clone(), Span::raw(l.clone())]));
                        } else {
                            out.push(Line::from(l.clone()));
                        }
                    }
                    if *streaming {
                        if let Some(last) = out.last_mut() {
                            last.spans
                                .push(Span::styled("▍", Style::default().fg(Color::Yellow)));
                        }
                    }
                    out.push(Line::from(""));
                }
                Cell::Tool { agent, name, args, result, error } => {
                    let title = format!("  ⚒ {} · {}({})", agent.label(), name, truncate(args, 60));
                    out.push(Line::from(Span::styled(title, Style::default().fg(Color::Yellow))));
                    if let Some(err) = error {
                        for l in wrap(&format!("    error: {err}"), w) {
                            out.push(Line::from(Span::styled(l, Style::default().fg(Color::Red))));
                        }
                    } else if let Some(res) = result {
                        for l in wrap(&format!("    {}", truncate(res, 300)), w) {
                            out.push(Line::from(Span::styled(l, Style::default().fg(Color::DarkGray))));
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

    fn draw_input(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let title = format!(" message to {} ", self.focused.label());
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD).fg(Color::Green),
        ));
        let prompt = if self.input.is_empty() {
            "  /help for commands, then describe what you need…".to_string()
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

    fn draw_hints(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let hint = " Tab: switch agent   Enter: send   ↑/↓: scroll   /help   Ctrl+C: quit";
        let para = Paragraph::new(hint)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Left);
        f.render_widget(para, area);
    }
}

// --- helpers ---

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

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split(' ') {
            if line.is_empty() {
                line.push_str(word);
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
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
