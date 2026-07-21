//! User and assistant message cells adapted directly from Codex's
//! `history_cell/messages.rs`.

use super::{HistoryCell, sanitize_user_text};
use crate::line_utils::prefix_lines;
use crate::markdown_render;
use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use crate::wrapping::{RtOptions, adaptive_wrap_lines};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, WidgetRef, Wrap};

#[derive(Debug)]
pub(crate) struct UserHistoryCell {
    pub(crate) text: String,
}

impl HistoryCell for UserHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let style = user_message_style();
        let sanitized = sanitize_user_text(&self.text);
        let message = crate::chatwidget::render_user_text(sanitized.trim_end_matches(['\r', '\n']))
            .into_iter()
            .map(|line| line.style(style))
            .collect::<Vec<_>>();
        if message.is_empty() {
            return Vec::new();
        }
        let wrapped = adaptive_wrap_lines(
            message,
            RtOptions::new(usize::from(width.max(1)).saturating_sub(3).max(1))
                .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit),
        );
        let mut lines = vec![Line::from("").style(style)];
        lines.extend(prefix_lines(
            wrapped,
            Span::from("› ").bold().dim(),
            Span::from("  "),
        ));
        lines.push(Line::from("").style(style));
        // The upstream transcript paints the complete user row with
        // `user_message_style`, including trailing cells after the glyphs.
        // Preserve that full-width band instead of styling only the spans.
        for line in &mut lines {
            let padding = usize::from(width).saturating_sub(line.width());
            if padding > 0 {
                line.spans.push(Span::styled(" ".repeat(padding), style));
            }
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        let text = sanitize_user_text(&self.text);
        if text.is_empty() {
            Vec::new()
        } else {
            text.trim_end_matches(['\r', '\n'])
                .split('\n')
                .map(|line| Line::from(line.to_string()))
                .collect()
        }
    }
}

#[derive(Debug)]
pub(crate) struct AgentMessageCell {
    pub(crate) text: String,
    pub(crate) is_first_line: bool,
}

impl HistoryCell for AgentMessageCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.text.is_empty() {
            return Vec::new();
        }
        let raw_lines = self
            .text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect::<Vec<_>>();
        adaptive_wrap_lines(
            raw_lines,
            RtOptions::new(usize::from(width.max(1)))
                .initial_indent(if self.is_first_line {
                    "• ".dim().into()
                } else {
                    "  ".into()
                })
                .subsequent_indent("  ".into()),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect()
    }
}

/// Source-backed finalized assistant cell. Keeping the markdown source rather
/// than wrapped lines is the key Codex resize/reflow invariant.
#[derive(Debug)]
pub(crate) struct AgentMarkdownCell {
    pub(crate) text: String,
}

impl HistoryCell for AgentMarkdownCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.text.is_empty() {
            return Vec::new();
        }
        let markdown = markdown_render::render_markdown_text_with_width(
            &self.text,
            Some(usize::from(width.max(1)).saturating_sub(2).max(1)),
        )
        .lines;
        adaptive_wrap_lines(
            markdown,
            RtOptions::new(usize::from(width.max(1)))
                .initial_indent("• ".dim().into())
                .subsequent_indent("  ".into()),
        )
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.text
            .split('\n')
            .map(|line| Line::from(line.to_string()))
            .collect()
    }
}

impl Renderable for AgentMarkdownCell {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        Clear.render_ref(area, buf);
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .render_ref(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self, width)
    }
}
