//! Codex-style user question modal.

use crate::app_state::PendingUserInput;
use crate::custom_terminal::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::collections::VecDeque;

pub(crate) struct RequestUserInputOverlay;

impl RequestUserInputOverlay {
    pub(crate) fn draw(f: &mut Frame<'_>, pending: &VecDeque<PendingUserInput>) {
        let Some(req) = pending.front() else { return };
        let Some(question) = req.question() else {
            return;
        };
        let area = f.area();
        let width = 76u16.min(area.width.saturating_sub(4)).max(1);
        let mut lines = vec![Line::from(Span::styled(
            format!(" {} ", question.header),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))];
        lines.push(Line::raw(format!("  {}", question.question)));
        lines.push(Line::raw(""));
        if let Some(options) = &question.options {
            for (index, option) in options.iter().enumerate() {
                let selected = req.selected == index;
                let marker = if selected { "›" } else { " " };
                let style = if selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {marker} {}", option.label), style),
                    Span::styled(
                        format!(" — {}", option.description),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            if question.is_other {
                let selected = req.selected == options.len();
                let marker = if selected { "›" } else { " " };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {marker} Other: {}",
                        masked(&req.draft, question.is_secret)
                    ),
                    if selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                    },
                )));
            }
        } else {
            lines.push(Line::from(Span::styled(
                format!("  {}", masked(&req.draft, question.is_secret)),
                Style::default().fg(Color::White),
            )));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("  [↑/↓]", Style::default().fg(Color::Cyan)),
            Span::raw(" choose   "),
            Span::styled("[Enter]", Style::default().fg(Color::Green)),
            Span::raw(" submit   "),
            Span::styled("[Esc]", Style::default().fg(Color::Red)),
            Span::raw(" cancel"),
        ]));
        if req.questions.len() > 1 {
            lines.push(Line::from(Span::styled(
                format!(
                    "  question {}/{} · {} agent",
                    req.question_index + 1,
                    req.questions.len(),
                    req.agent.label()
                ),
                Style::default().fg(Color::DarkGray),
            )));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(4));
        let popup = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 3,
            width,
            height,
        };
        f.render_widget(Clear, popup);
        f.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" input required "),
            ),
            popup,
        );
    }
}

fn masked(text: &str, secret: bool) -> String {
    if secret {
        "•".repeat(text.chars().count())
    } else {
        text.to_string()
    }
}
