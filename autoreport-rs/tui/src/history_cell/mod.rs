//! Transcript/history cells migrated from Codex's `tui/src/history_cell`.
//!
//! AutoReport keeps its protocol-specific `Cell` enum, but the chat surface consumes it through
//! the same render contract as Codex: cells own width-aware line generation and report their
//! wrapped height to the parent render tree.

use crate::app_state::{Cell, SysKind};
use crate::chatwidget::{
    render_tool_result_lines, render_user_text, tool_arg_summary, tool_status_color,
    tool_status_glyph, truncate,
};
use crate::line_utils::prefix_lines;
use crate::markdown_render;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use crate::wrapping::{RtOptions, adaptive_wrap_lines};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, WidgetRef, Wrap};

mod session;
pub(crate) use session::SessionHeaderHistoryCell;

/// Width-aware history cell contract from Codex's transcript renderer.
pub(crate) trait HistoryCell: std::fmt::Debug + Send + Sync {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(Text::from(self.display_lines(width)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }
}

impl Renderable for Box<dyn HistoryCell> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let scroll = paragraph
            .line_count(area.width)
            .saturating_sub(usize::from(area.height));
        Clear.render_ref(area, buf);
        paragraph
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self.as_ref(), width)
    }
}

impl HistoryCell for Cell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        render_cell_lines(self, width)
    }
}

pub(crate) fn render_history_lines(cells: &[Cell], width: u16) -> Vec<Line<'static>> {
    cells
        .iter()
        .flat_map(|cell| cell.display_lines(width))
        .collect()
}

fn render_cell_lines(cell: &Cell, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut out = Vec::new();
    match cell {
        Cell::User { text, .. } => {
            let style = user_message_style();
            let message_lines = render_user_text(text)
                .into_iter()
                .map(|line| line.style(style))
                .collect::<Vec<_>>();
            if message_lines.is_empty() {
                return out;
            }
            let wrapped = adaptive_wrap_lines(
                message_lines,
                RtOptions::new(width.saturating_sub(3).max(1)),
            );
            out.push(Line::from("").style(style));
            out.extend(prefix_lines(
                wrapped,
                Span::from("› ").bold().dim(),
                Span::from("  "),
            ));
            out.push(Line::from("").style(style));
        }
        Cell::Assistant { text, .. } => {
            if text.is_empty() {
                return out;
            }
            let markdown = markdown_render::render_markdown_text_with_width(
                text,
                Some(width.saturating_sub(2).max(1)),
            )
            .lines;
            out.extend(prefix_lines(
                markdown,
                Span::from("• ").dim(),
                Span::from("  "),
            ));
        }
        Cell::Reasoning { text, .. } => {
            let style = Style::default().dim().italic();
            let lines = text
                .lines()
                .map(|line| Line::from(Span::styled(line.to_string(), style)))
                .collect::<Vec<_>>();
            out.extend(prefix_lines(
                lines,
                Span::from("• ").dim(),
                Span::from("  "),
            ));
        }
        Cell::ToolGroup { agent, items } => {
            out.push(Line::from(Span::styled(
                format!(
                    "  ⚒ {} · {} tool{}",
                    agent.label(),
                    items.len(),
                    if items.len() == 1 { "" } else { "s" }
                ),
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
                    tool_status_color(item),
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
            out.extend(text.lines().map(|line| {
                Line::from(Span::styled(
                    format!("  {line}"),
                    Style::default().fg(color),
                ))
            }));
            out.push(Line::from(""));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Cell, HistoryCell};
    use autoreport_core::types::AgentType;

    fn plain_lines(cell: &Cell) -> Vec<String> {
        cell.display_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn user_message_uses_codex_prompt_prefix() {
        let lines = plain_lines(&Cell::User {
            _agent: AgentType::Main,
            text: "hi".into(),
        });
        assert!(lines.iter().any(|line| line == "› hi"));
        assert!(!lines.iter().any(|line| line.contains("Main")));
    }

    #[test]
    fn assistant_message_uses_codex_bullet_prefix() {
        let lines = plain_lines(&Cell::Assistant {
            agent: AgentType::Main,
            text: "hello".into(),
            streaming: false,
        });
        assert!(lines.iter().any(|line| line == "• hello"));
        assert!(!lines.iter().any(|line| line.contains("Main")));
    }

    #[test]
    fn reasoning_message_uses_codex_bullet_prefix() {
        let lines = plain_lines(&Cell::Reasoning {
            agent: AgentType::Main,
            text: "checking context".into(),
            streaming: false,
        });
        assert!(lines.iter().any(|line| line == "• checking context"));
        assert!(!lines.iter().any(|line| line.contains("thinking")));
    }
}
