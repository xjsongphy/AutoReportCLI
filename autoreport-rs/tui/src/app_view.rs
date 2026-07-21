//! Application layout and widget rendering.

use crate::app::Tui;
use crate::bottom_pane::status_line_setup::StatusLineItem;
use crate::bottom_pane::status_line_style::status_line_from_segments;
use crate::bottom_pane::{ApprovalOverlay, RequestUserInputOverlay};
use crate::style::accent_style;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

const MENTION_LIMIT: usize = 8;
const SLASH_LIMIT: usize = 8;
impl Tui {
    pub(crate) fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        self.composer.set_status_line(self.composer_status_line());
        self.render_codex_chat(area, f.buffer_mut());
        let bottom_pane_height = self
            .codex_chat_bottom_pane_height(area.width)
            .min(area.height);
        let composer_top = area.bottom().saturating_sub(bottom_pane_height);
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
        } else if !self.pending_user_inputs.is_empty() {
            RequestUserInputOverlay::draw(f, &self.pending_user_inputs);
        }

        if let Some(pager) = self.pager.as_mut() {
            let lines = if self.raw_output {
                crate::history_cell::render_raw_history_lines_for_agent(&self.history, self.focused)
            } else {
                crate::history_cell::render_history_lines_for_agent(
                    &self.history,
                    self.focused,
                    area.width.max(1),
                )
            };
            pager.replace_lines(lines);
            pager.draw(f);
        }

        if let Some(screen) = self.overlay.as_mut() {
            screen.draw(f);
        } else if self.pager.is_none()
            && self.pending_approvals.is_empty()
            && self.pending_user_inputs.is_empty()
            && let Some((x, y)) = self.codex_chat_cursor_pos(area)
        {
            f.set_cursor_position((x, y));
        }
    }

    fn composer_status_line(&self) -> Option<Line<'static>> {
        let model = match self.focused {
            autoreport_core::types::AgentType::Main => self.main_model.clone(),
            _ => self.sub_model.clone(),
        };
        let directory = dirs::home_dir()
            .and_then(|home| {
                self.workspace.strip_prefix(home).ok().map(|relative| {
                    if relative.as_os_str().is_empty() {
                        "~".to_string()
                    } else {
                        format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
                    }
                })
            })
            .unwrap_or_else(|| self.workspace.display().to_string());
        status_line_from_segments(
            [
                (StatusLineItem::ModelName, model),
                (StatusLineItem::CurrentDir, directory),
            ],
            true,
        )
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
            .title(Span::styled(" @ files ", accent_style()));
        let mut lines: Vec<Line> = Vec::new();
        if m.matches.is_empty() {
            lines.push(Line::from(Span::styled(
                "  no matching files",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (i, p) in m.matches.iter().enumerate() {
                let style = if i == m.selected {
                    accent_style()
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
                    accent_style()
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
