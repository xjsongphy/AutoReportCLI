//! Transcript/history cells migrated from Codex's `tui/src/history_cell`.
//!
//! AutoReport keeps its protocol-specific `Cell` enum, but the chat surface consumes it through
//! the same render contract as Codex: cells own width-aware line generation and report their
//! wrapped height to the parent render tree.

use crate::app_state::{Cell, SysKind};
use crate::chatwidget::{render_tool_result_lines, render_user_text, tool_arg_summary};
use crate::line_utils::{prefix_lines, push_owned_lines};
use crate::markdown_render;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use crate::wrapping::{RtOptions, adaptive_wrap_line, adaptive_wrap_lines};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
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
            // Direct adaptation of Codex's `AgentMarkdownCell`: render the
            // source at the reserved content width, then let Codex's wrapper
            // own the bullet and continuation indentation.
            let markdown = markdown_render::render_markdown_text_with_width(
                text,
                Some(width.saturating_sub(2).max(1)),
            )
            .lines;
            out.extend(adaptive_wrap_lines(
                markdown,
                RtOptions::new(width)
                    .initial_indent("• ".dim().into())
                    .subsequent_indent("  ".into()),
            ));
        }
        Cell::Reasoning { text, .. } => {
            // Copied from Codex's `ReasoningSummaryCell::lines`, using the
            // already-migrated markdown renderer in place of `append_markdown`.
            let summary_style = Style::default().dim().italic();
            let summary_lines = markdown_render::render_markdown_text_with_width(
                text,
                Some(width.saturating_sub(2).max(1)),
            )
            .lines
            .into_iter()
            .map(|mut line| {
                line.spans = line
                    .spans
                    .into_iter()
                    .map(|span| span.patch_style(summary_style))
                    .collect();
                line
            })
            .collect::<Vec<_>>();
            out.extend(adaptive_wrap_lines(
                summary_lines,
                RtOptions::new(width)
                    .initial_indent("• ".dim().into())
                    .subsequent_indent("  ".into()),
            ));
        }
        Cell::ToolGroup { agent, items } => {
            for item in items {
                out.extend(render_tool_call_lines(agent.label(), item, width));
            }
        }
        Cell::System { text, kind } => {
            // Direct adaptation of Codex's `new_info_event` and
            // `new_error_event` in `history_cell/notices.rs`.
            match kind {
                SysKind::Info => out.push(vec!["• ".dim(), text.clone().into()].into()),
                SysKind::Error => out.push(vec![format!("■ {text}").red()].into()),
            }
        }
    }
    out
}

/// Direct adaptation of Codex's `McpToolCallCell::display_lines` for
/// AutoReport's generic tool protocol. The only project-specific part is the
/// invocation text: Codex has an MCP invocation type, while AutoReport has a
/// tool name, JSON arguments, and an agent owner.
fn render_tool_call_lines(
    agent: &str,
    item: &crate::app_state::ToolEntry,
    width: usize,
) -> Vec<Line<'static>> {
    let status = match (&item.result, &item.error) {
        (_, Some(_)) => Some(false),
        (Some(_), None) => Some(true),
        (None, None) => None,
    };
    let bullet = match status {
        Some(true) => "•".green().bold(),
        Some(false) => "•".red().bold(),
        None => "•".dim(),
    };
    let header_text = if status.is_some() {
        "Called"
    } else {
        "Calling"
    };
    let invocation_line = Line::from(format!(
        "{agent} · {}({})",
        item.name,
        tool_arg_summary(&item.name, &item.args)
    ));
    let mut compact_spans = vec![bullet.clone(), " ".into(), header_text.bold(), " ".into()];
    let mut compact_header = Line::from(compact_spans.clone());
    let reserved = compact_header.width();
    let inline_invocation = invocation_line.width() <= width.saturating_sub(reserved);

    let mut lines = Vec::new();
    if inline_invocation {
        compact_header.extend(invocation_line.spans);
        lines.push(compact_header);
    } else {
        compact_spans.pop();
        lines.push(Line::from(compact_spans));
        let wrapped = adaptive_wrap_line(
            &invocation_line,
            RtOptions::new(width.saturating_sub(4).max(1))
                .initial_indent("".into())
                .subsequent_indent("    ".into()),
        );
        let mut body_lines = Vec::new();
        push_owned_lines(&wrapped, &mut body_lines);
        lines.extend(prefix_lines(body_lines, "  └ ".dim(), "    ".into()));
    }

    let detail_lines = render_tool_result_lines(
        &item.name,
        &item.args,
        item.result.as_ref(),
        item.error.as_deref(),
    );
    if !detail_lines.is_empty() {
        let detail_width = width.saturating_sub(4).max(1);
        let mut wrapped_details = Vec::new();
        for detail in detail_lines {
            let wrapped = adaptive_wrap_line(
                &detail,
                RtOptions::new(detail_width)
                    .initial_indent("".into())
                    .subsequent_indent("    ".into()),
            );
            push_owned_lines(&wrapped, &mut wrapped_details);
        }
        let initial_prefix = if inline_invocation {
            "  └ ".dim()
        } else {
            "    ".into()
        };
        lines.extend(prefix_lines(wrapped_details, initial_prefix, "    ".into()));
    }
    lines
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
