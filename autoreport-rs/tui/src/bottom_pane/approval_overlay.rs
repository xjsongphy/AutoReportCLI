//! Approval overlay adapted from Codex's bottom-pane approval surface.

use crate::app_state::PendingApproval;
use crate::render::renderable::Renderable;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, WidgetRef};
use std::collections::VecDeque;

pub(crate) struct ApprovalOverlay<'a> {
    pending_approvals: &'a VecDeque<PendingApproval>,
}

impl<'a> ApprovalOverlay<'a> {
    pub(crate) fn new(pending_approvals: &'a VecDeque<PendingApproval>) -> Self {
        Self { pending_approvals }
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let Some(req) = self.pending_approvals.front() else {
            return Vec::new();
        };
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
            Span::raw(" session   "),
            Span::styled("[p]", Style::default().fg(Color::Yellow)),
            Span::raw(" save rule   "),
            Span::styled("[d]", Style::default().fg(Color::Red)),
            Span::raw(" deny   "),
            Span::styled("[c]/[n]/Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" cancel"),
        ]));
        if self.pending_approvals.len() > 1 {
            lines.push(Line::from(Span::styled(
                format!("  +{} more pending", self.pending_approvals.len() - 1),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines
    }

    pub(crate) fn desired_height(&self, _width: u16) -> u16 {
        if self.pending_approvals.is_empty() {
            0
        } else {
            self.lines().len() as u16 + 2
        }
    }
}

impl Renderable for ApprovalOverlay<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.pending_approvals.is_empty() {
            return;
        }
        let lines = self.lines();
        let height = self.desired_height(area.width).min(area.height);
        let width = 64u16.min(area.width.saturating_sub(4)).max(1);
        let popup_area = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        Clear.render_ref(popup_area, buf);
        let title = if self.pending_approvals.len() > 1 {
            format!(" approval required (1/{}) ", self.pending_approvals.len())
        } else {
            " approval required ".to_string()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Yellow)));
        Paragraph::new(lines)
            .block(block)
            .render_ref(popup_area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        if self.pending_approvals.is_empty() {
            0
        } else {
            self.lines().len() as u16 + 2
        }
    }
}
