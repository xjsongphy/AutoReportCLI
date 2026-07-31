//! Approval overlay adapted from Codex's bottom-pane approval surface.

use crate::app_state::PendingApproval;
use crate::custom_terminal::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::collections::VecDeque;

pub(crate) struct ApprovalOverlay;

impl ApprovalOverlay {
    pub(crate) fn draw(f: &mut Frame<'_>, pending_approvals: &VecDeque<PendingApproval>) {
        let Some(req) = pending_approvals.front() else {
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
        // Keymap mirrors codex's default ApprovalKeymap (keymap.rs):
        // y approve · a approve-for-session · p persist-prefix · d deny ·
        // c cancel · Esc/n decline.
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
        if pending_approvals.len() > 1 {
            lines.push(Line::from(Span::styled(
                format!("  +{} more pending", pending_approvals.len() - 1),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        let popup_area = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        f.render_widget(Clear, popup_area);
        let title = if pending_approvals.len() > 1 {
            format!(" approval required (1/{}) ", pending_approvals.len())
        } else {
            " approval required ".to_string()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(title, Style::default().fg(Color::Yellow)));
        let para = Paragraph::new(lines).block(block);
        f.render_widget(para, popup_area);
    }
}
