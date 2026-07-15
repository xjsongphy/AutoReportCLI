//! Application layout and widget rendering.

use crate::app::Tui;
use crate::app_state::{Cell, SysKind};
use crate::chatwidget::*;
use crate::markdown_render;
use autoreport_core::types::{AgentStatus, AgentType};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const MENTION_LIMIT: usize = 8;
const SLASH_LIMIT: usize = 8;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

impl Tui {
    pub(crate) fn draw(&mut self, f: &mut Frame<'_>) {
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

        if self.slash.is_some() {
            self.draw_slash_popup(f, chunks[2]);
        } else if self.mention.is_some() {
            self.draw_mention_popup(f, chunks[2]);
        }

        if !self.pending_approvals.is_empty() {
            self.draw_approval_popup(f);
        }

        if let Some(screen) = self.overlay.as_mut() {
            screen.draw(f);
        }
    }

    /// Approval modal — ported in spirit from codex's `ApprovalOverlay::draw`
    /// (`tui/src/bottom_pane/approval_overlay.rs`). Shows which agent is asking
    /// (codex's `thread_label`), the command summary, the raw command, and the
    /// decision keys (mirrors `handle_approval_key`: y/enter, a, n/esc). Draws
    /// a queue-depth hint when more than one agent is waiting.
    fn draw_approval_popup(&self, f: &mut Frame<'_>) {
        let Some(req) = self.pending_approvals.front() else {
            return;
        };
        let area = f.area();
        let width = 64u16.min(area.width.saturating_sub(4));
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(" {} agent wants to run a command ", req.agent.label()),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::raw(""));
        for p in &req.summary {
            lines.push(Line::from(Span::styled(
                format!("  {}", p.summary()),
                Style::default().fg(Color::Cyan),
            )));
        }
        if let Some(r) = &req.reason {
            lines.push(Line::from(Span::styled(
                format!("  reason: {r}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::raw(""));
        // `$ <command>` like codex's transcript exec cell.
        let cmd = if req.command.is_empty() {
            "(no command)".to_string()
        } else {
            req.command.clone()
        };
        for (i, line) in cmd.lines().enumerate() {
            let prefix = if i == 0 { "$ " } else { "  " };
            lines.push(Line::from(Span::styled(
                format!("{prefix}{line}"),
                Style::default().fg(Color::Magenta),
            )));
        }
        if let Some(cwd) = &req.cwd {
            lines.push(Line::from(Span::styled(
                format!("  cwd: {cwd}"),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("[y]", Style::default().fg(Color::Green)),
            Span::raw(" approve   "),
            Span::styled("[a]", Style::default().fg(Color::Green)),
            Span::raw(" this session   "),
            Span::styled("[p]", Style::default().fg(Color::Yellow)),
            Span::raw(" save rule   "),
            Span::styled("[n]/Esc", Style::default().fg(Color::Red)),
            Span::raw(" deny"),
        ]));
        if self.pending_approvals.len() > 1 {
            lines.push(Line::from(Span::styled(
                format!("  +{} more pending", self.pending_approvals.len() - 1),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        let popup_area = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 3,
            width,
            height,
        };
        f.render_widget(Clear, popup_area);
        let title = if self.pending_approvals.len() > 1 {
            format!(" approval required (1/{}) ", self.pending_approvals.len())
        } else {
            " approval required ".to_string()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Yellow)));
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, popup_area);
    }

    fn draw_status(&self, f: &mut Frame<'_>, area: Rect) {
        let mut spans = vec![
            Span::styled(
                " AutoReportCLI ",
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ),
            Span::raw(" "),
            Span::styled(
                self.workspace_display.clone(),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw("  "),
            Span::styled(self.provider_id.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("  focused: "),
            Span::styled(
                self.focused.label().to_string(),
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Green),
            ),
        ];
        if self.ide_enabled {
            spans.push(Span::raw("  "));
            spans.push(Span::styled("IDE●", Style::default().fg(Color::Magenta)));
        }
        for a in AgentType::ALL {
            let st = self.statuses.get(&a).copied().unwrap_or(AgentStatus::Idle);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{} {} {}", status_mark(st), a.label(), status_text(st)),
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
            total.saturating_sub(visible.saturating_add(self.scroll))
        } else {
            0
        };
        let start = start.min(total);
        let end = (start + visible).min(total);
        let shown: Vec<Line> = lines
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
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
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(Color::Blue),
                    )]));
                    for line in render_user_text(text) {
                        out.push(line);
                    }
                    out.push(Line::from(""));
                }
                Cell::Assistant {
                    agent,
                    text,
                    streaming,
                } => {
                    let label = Span::styled(
                        format!("{} ", agent.label()),
                        Style::default()
                            .add_modifier(Modifier::BOLD)
                            .fg(agent_color(*agent)),
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
                            last.spans
                                .push(Span::styled("▍", Style::default().fg(Color::Yellow)));
                        }
                    }
                    out.extend(md);
                    out.push(Line::from(""));
                }
                Cell::Reasoning {
                    agent,
                    text,
                    streaming,
                } => {
                    out.extend(render_reasoning_lines(*agent, text, *streaming));
                    out.push(Line::from(""));
                }
                Cell::ToolGroup { agent, items } => {
                    let title = format!(
                        "  ⚒ {} · {} tool{}",
                        agent.label(),
                        items.len(),
                        if items.len() == 1 { "" } else { "s" }
                    );
                    out.push(Line::from(Span::styled(
                        title,
                        Style::default().fg(Color::Yellow),
                    )));
                    for item in items {
                        out.push(Line::from(Span::styled(
                            format!(
                                "    {} {}({})",
                                tool_status_glyph(item),
                                item.name,
                                truncate(&tool_arg_summary(&item.name, &item.args), 72)
                            ),
                            Style::default().fg(tool_status_color(item)),
                        )));
                        out.extend(render_tool_result_lines(
                            &item.name,
                            &item.args,
                            item.result.as_ref(),
                            item.error.as_deref(),
                        ));
                    }
                    out.push(Line::from(""));
                }
                Cell::System { text, kind } => {
                    let color = match kind {
                        SysKind::Info => Color::DarkGray,
                        SysKind::Error => Color::Red,
                    };
                    for l in text.lines() {
                        out.push(Line::from(Span::styled(
                            format!("  {l}"),
                            Style::default().fg(color),
                        )));
                    }
                    out.push(Line::from(""));
                }
            }
        }
        out
    }

    fn draw_input(&self, f: &mut Frame<'_>, area: Rect) {
        let title = format!(" message to {} ", self.focused.label());
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            title,
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Green),
        ));
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
        let hint =
            " Tab: switch agent   Enter: send   ↑/↓: scroll   @: mention   /help   Ctrl+C: quit";
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
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
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

    fn draw_slash_popup(&self, f: &mut Frame<'_>, anchor: Rect) {
        let Some(s) = self.slash.as_ref() else {
            return;
        };
        let count = s.matches.len().min(SLASH_LIMIT).clamp(1, SLASH_LIMIT);
        let height = (count + 2) as u16;
        let width = 68u16.min(anchor.width);
        let popup_area = Rect {
            x: anchor.x,
            y: anchor.y.saturating_sub(height),
            width,
            height,
        };
        f.render_widget(Clear, popup_area);
        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            " / commands ",
            Style::default().fg(Color::Cyan),
        ));
        let mut lines: Vec<Line> = Vec::new();
        if s.matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no matching commands",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, cmd) in s.matches.iter().enumerate().take(SLASH_LIMIT) {
                let style = if i == s.selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                let mark = if i == s.selected { "▶ " } else { "  " };
                lines.push(Line::from(vec![
                    Span::styled(format!("{mark}/{}", cmd.name), style),
                    Span::raw("  "),
                    Span::styled(cmd.description, Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, popup_area);
    }
}
