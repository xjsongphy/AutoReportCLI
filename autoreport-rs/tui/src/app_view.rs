//! Application layout and widget rendering.

use crate::app::Tui;
use crate::bottom_pane::ApprovalOverlay;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const MENTION_LIMIT: usize = 8;
const SLASH_LIMIT: usize = 8;
impl Tui {
    pub(crate) fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        self.render_codex_chat(area, f.buffer_mut());
        let composer_top = area.bottom().saturating_sub(4);
        let popup_bounds = Rect::new(
            area.x,
            area.y,
            area.width,
            composer_top.saturating_sub(area.y),
        );

        if self.slash.is_some() {
            self.draw_slash_popup(f, popup_bounds);
        } else if self.mention.is_some() {
            self.draw_mention_popup(f, popup_bounds);
        }

        if !self.pending_approvals.is_empty() {
            ApprovalOverlay::draw(f, &self.pending_approvals);
        }

        if let Some(screen) = self.overlay.as_mut() {
            screen.draw(f);
        } else if self.pending_approvals.is_empty()
            && let Some((x, y)) = self.codex_chat_cursor_pos(area)
        {
            f.set_cursor_position((x, y));
        }
    }

    fn draw_mention_popup(&self, f: &mut Frame<'_>, anchor: Rect) {
        let Some(m) = self.mention.as_ref() else {
            return;
        };
        let count = m.matches.len().min(MENTION_LIMIT).clamp(1, MENTION_LIMIT);
        let width = 60u16.min(anchor.width);
        let height = (count as u16).min(anchor.height);
        let popup_area = Rect {
            x: anchor.x,
            y: anchor.bottom().saturating_sub(height),
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(Span::styled(format!("  {p}"), style)));
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
        let height = (count as u16).min(anchor.height);
        let width = 68u16.min(anchor.width);
        let popup_area = Rect {
            x: anchor.x,
            y: anchor.bottom().saturating_sub(height),
            width,
            height,
        };
        f.render_widget(Clear, popup_area);
        let mut lines: Vec<Line> = Vec::new();
        if s.matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "no matching commands",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, cmd) in s.matches.iter().enumerate().take(SLASH_LIMIT) {
                let style = if i == s.selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  /{}", cmd.name), style),
                    Span::raw("  "),
                    Span::styled(cmd.description, Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        let para = Paragraph::new(lines);
        f.render_widget(para, popup_area);
    }
}
