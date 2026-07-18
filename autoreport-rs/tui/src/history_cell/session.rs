//! Session header history cell migrated from Codex's `history_cell/session.rs`.

use super::HistoryCell;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use std::path::PathBuf;
use unicode_width::UnicodeWidthStr;

pub(crate) const SESSION_HEADER_MAX_INNER_WIDTH: usize = 56;

pub(crate) fn card_inner_width(width: u16, max_inner_width: usize) -> Option<usize> {
    if width < 4 {
        return None;
    }
    Some(usize::from(width.saturating_sub(4)).min(max_inner_width))
}

/// Render lines inside the same rounded border used by Codex's session cards.
pub(crate) fn with_border(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let content_width = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0);
    let border_width = content_width + 2;
    let mut out = Vec::with_capacity(lines.len() + 2);
    out.push(Line::from(format!("╭{}╮", "─".repeat(border_width)).dim()));
    for line in lines {
        let used_width = line
            .spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum::<usize>();
        let mut spans = vec![Span::from("│ ").dim()];
        spans.extend(line.spans);
        if used_width < content_width {
            spans.push(Span::from(" ".repeat(content_width - used_width)).dim());
        }
        spans.push(Span::from(" │").dim());
        out.push(Line::from(spans));
    }
    out.push(Line::from(format!("╰{}╯", "─".repeat(border_width)).dim()));
    out
}

#[derive(Debug)]
pub(crate) struct SessionHeaderHistoryCell {
    provider: String,
    directory: PathBuf,
}

impl SessionHeaderHistoryCell {
    pub(crate) fn new(provider: String, directory: impl Into<PathBuf>) -> Self {
        Self {
            provider,
            directory: directory.into(),
        }
    }

    fn format_directory(&self, max_width: usize) -> String {
        let value = self.directory.display().to_string();
        if UnicodeWidthStr::width(value.as_str()) <= max_width {
            value
        } else {
            let mut truncated = value
                .chars()
                .take(max_width.saturating_sub(1))
                .collect::<String>();
            truncated.push('…');
            truncated
        }
    }

    fn format_provider(&self, max_width: usize) -> String {
        if UnicodeWidthStr::width(self.provider.as_str()) <= max_width {
            return self.provider.clone();
        }
        let mut truncated = self
            .provider
            .chars()
            .take(max_width.saturating_sub(1))
            .collect::<String>();
        truncated.push('…');
        truncated
    }
}

impl HistoryCell for SessionHeaderHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(inner_width) = card_inner_width(width, SESSION_HEADER_MAX_INNER_WIDTH) else {
            return Vec::new();
        };
        let lines = vec![
            Line::from(vec![
                Span::from(">_ ").dim(),
                Span::from("AutoReportCLI").bold(),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::from("provider: ").dim(),
                Span::from(self.format_provider(inner_width.saturating_sub(10))),
            ]),
            Line::from(vec![
                Span::from("directory: ").dim(),
                Span::from(self.format_directory(inner_width.saturating_sub(11))),
            ]),
        ];
        with_border(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryCell, SessionHeaderHistoryCell};
    use std::path::PathBuf;

    #[test]
    fn renders_codex_session_card_shape() {
        let cell = SessionHeaderHistoryCell::new("openai".into(), PathBuf::from("/tmp/project"));
        let rendered = cell
            .display_lines(80)
            .into_iter()
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<String>();
        assert!(rendered.contains("AutoReportCLI"));
        assert!(rendered.contains("provider: openai"));
        assert!(rendered.contains("directory: /tmp/project"));
        assert!(rendered.contains("╭"));
        assert!(rendered.contains("╰"));
    }
}
