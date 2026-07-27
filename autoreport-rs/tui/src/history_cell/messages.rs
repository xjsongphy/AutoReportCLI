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
use ratatui::style::Style;
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

    /// Annotate web URLs in the rendered markdown as OSC 8 terminal hyperlinks
    /// (clickable in capable terminals). Mirrors Codex's `WebHyperlinkHistoryCell`.
    fn display_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        crate::terminal_hyperlinks::annotate_web_urls(HistoryCell::display_lines(self, width))
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

/// Reasoning-summary cell, adapted from Codex's `ReasoningSummaryCell`
/// (`history_cell/messages.rs`). The summary text is rendered as dimmed italic
/// markdown with a `"• "` bullet indent, matching codex's transcript style.
/// (`cwd`-relative file-link rendering is dropped — the project reasons about
/// no local files inside the summary body.)
#[derive(Debug)]
pub(crate) struct ReasoningSummaryCell {
    content: String,
    transcript_only: bool,
}

impl ReasoningSummaryCell {
    pub(crate) fn new(content: String, transcript_only: bool) -> Self {
        Self {
            content,
            transcript_only,
        }
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        let markdown = markdown_render::render_markdown_text_with_width(
            &self.content,
            Some(usize::from(width.max(1)).saturating_sub(2).max(1)),
        )
        .lines;
        // codex paints every span of the rendered summary `dim().italic()`.
        let summary_style = Style::default().dim().italic();
        let summary_lines = markdown
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
        adaptive_wrap_lines(
            summary_lines,
            RtOptions::new(usize::from(width.max(1)))
                .initial_indent("• ".dim().into())
                .subsequent_indent("  ".into()),
        )
    }
}

impl HistoryCell for ReasoningSummaryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.transcript_only {
            Vec::new()
        } else {
            self.lines(width)
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        if self.transcript_only {
            Vec::new()
        } else {
            self.content
                .trim()
                .split('\n')
                .map(|line| Line::from(line.to_string()))
                .collect()
        }
    }
}

/// Split structured reasoning-summary parts into a status header and
/// renderable content. Ported verbatim from codex
/// `history_cell/messages.rs::split_reasoning_summary_parts`: trims each part,
/// drops `<!-- -->` placeholder bodies, strips a leading `**...**` bold header,
/// and returns `(header, content)`. An empty content body makes the cell
/// `transcript_only` (hidden from the transcript).
pub(crate) fn split_reasoning_summary_parts(reasoning_parts: &[String]) -> (String, String) {
    let mut leading_empty_part_header = None;
    let mut content_parts = Vec::with_capacity(reasoning_parts.len());

    for part in reasoning_parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let header_end = part.strip_prefix("**").and_then(|after_open| {
            after_open
                .find("**")
                .and_then(|close| (close > 0).then_some(close + 4))
        });
        let body = header_end.map_or(part, |header_end| &part[header_end..]);
        if body.trim() == "<!-- -->" {
            if content_parts.is_empty()
                && leading_empty_part_header.is_none()
                && let Some(header_end) = header_end
            {
                leading_empty_part_header = Some(part[..header_end].to_string());
            }
            continue;
        }

        content_parts.push(part);
    }

    let content = content_parts.join("\n\n");
    if content.is_empty() {
        return (leading_empty_part_header.unwrap_or_default(), content);
    }

    if let Some(after_open) = content.strip_prefix("**")
        && let Some(close) = after_open.find("**")
    {
        let after_close_idx = 2 + close + 2;
        let after_close = &content[after_close_idx..];
        if after_close.starts_with('\n') || after_close.starts_with('\r') {
            return (
                content[..after_close_idx].to_string(),
                after_close.to_string(),
            );
        }
    }

    (leading_empty_part_header.unwrap_or_default(), content)
}

#[cfg(test)]
mod hyperlink_tests {
    use super::*;
    use crate::app_state::Cell;
    use crate::history_cell::HistoryCell;
    use autoreport_core::types::AgentType;

    #[test]
    fn agent_markdown_annotates_web_urls_as_hyperlinks() {
        let cell = AgentMarkdownCell {
            text: "see https://example.com/x for details".into(),
        };
        let lines = cell.display_hyperlink_lines(80);
        let found = lines
            .iter()
            .flat_map(|l| l.hyperlinks.iter())
            .any(|h| h.destination == "https://example.com/x");
        assert!(found, "URL should be annotated as a hyperlink destination");
    }

    #[test]
    fn agent_markdown_without_urls_has_no_hyperlinks() {
        let cell = AgentMarkdownCell {
            text: "plain text without any link".into(),
        };
        let lines = cell.display_hyperlink_lines(80);
        let any = lines.iter().any(|l| !l.hyperlinks.is_empty());
        assert!(!any, "no URL → no hyperlink annotations");
    }

    /// Regression: the `Cell` enum wrapper (the production render path) must
    /// dispatch `display_hyperlink_lines` to the annotated markdown cell, not
    /// the default plain-lines impl. Previously the wrapper overrode only
    /// `display_lines`/`raw_lines`, leaving OSC 8 marking dead.
    #[test]
    fn cell_agent_markdown_dispatches_to_hyperlink_annotation() {
        let cell = Cell::AgentMarkdown {
            agent: AgentType::Main,
            text: "see https://example.com/x for details".into(),
        };
        let lines = cell.display_hyperlink_lines(80);
        let found = lines
            .iter()
            .flat_map(|l| l.hyperlinks.iter())
            .any(|h| h.destination == "https://example.com/x");
        assert!(
            found,
            "Cell::AgentMarkdown must annotate URLs via display_hyperlink_lines"
        );
    }
}
