//! Session header history cell migrated from Codex's `history_cell/session.rs`.

use super::HistoryCell;
use crate::style::accent_style;
use ratatui::style::{Style, Stylize};
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
pub(crate) fn with_border(
    lines: Vec<Line<'static>>,
    max_content_width: Option<usize>,
) -> Vec<Line<'static>> {
    let lines = lines
        .into_iter()
        .map(|line| {
            if let Some(max_width) = max_content_width
                && line.width() > max_width
            {
                return Line::from(crate::chatwidget::truncate(
                    &line.to_string(),
                    max_width.saturating_sub(1),
                ));
            }
            line
        })
        .collect::<Vec<_>>();
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
    version: &'static str,
    model: String,
    model_style: Style,
    directory: PathBuf,
}

impl SessionHeaderHistoryCell {
    pub(crate) fn new(model: String, directory: impl Into<PathBuf>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION"),
            model,
            model_style: accent_style(),
            directory: directory.into(),
        }
    }

    // Direct adaptation of Codex's `SessionHeaderHistoryCell::format_directory`.
    // Render paths below the user's home as `~/…`, which is both the Codex
    // convention and materially reduces header overflow on narrow terminals.
    fn format_directory(&self, max_width: Option<usize>) -> String {
        let formatted = dirs::home_dir()
            .and_then(|home| {
                self.directory.strip_prefix(&home).ok().map(|relative| {
                    if relative.as_os_str().is_empty() {
                        "~".to_string()
                    } else {
                        format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
                    }
                })
            })
            .unwrap_or_else(|| self.directory.display().to_string());
        if let Some(max_width) = max_width {
            if max_width == 0 {
                return String::new();
            }
            if UnicodeWidthStr::width(formatted.as_str()) > max_width {
                return crate::chatwidget::truncate(&formatted, max_width.saturating_sub(1));
            }
        }
        formatted
    }
}

impl HistoryCell for SessionHeaderHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(inner_width) = card_inner_width(width, SESSION_HEADER_MAX_INNER_WIDTH) else {
            return Vec::new();
        };
        // Copied from Codex's `SessionHeaderHistoryCell::display_lines`.
        let make_row = |spans: Vec<Span<'static>>| Line::from(spans);
        let title_spans: Vec<Span<'static>> = vec![
            Span::from(">_ ").dim(),
            Span::from("AutoReportCLI").bold(),
            Span::from(" ").dim(),
            Span::from(format!("(v{})", self.version)).dim(),
        ];
        const CHANGE_MODEL_HINT_COMMAND: &str = "/model";
        const CHANGE_MODEL_HINT_EXPLANATION: &str = " to change";
        const DIR_LABEL: &str = "directory:";
        let model_label = "model: ";
        let model_hint_width = UnicodeWidthStr::width("   ")
            + UnicodeWidthStr::width(CHANGE_MODEL_HINT_COMMAND)
            + UnicodeWidthStr::width(CHANGE_MODEL_HINT_EXPLANATION);
        let model_max_width = inner_width
            .saturating_sub(UnicodeWidthStr::width(model_label) + model_hint_width)
            .max(1);
        let model = if UnicodeWidthStr::width(self.model.as_str()) > model_max_width {
            crate::chatwidget::truncate(&self.model, model_max_width.saturating_sub(1).max(1))
        } else {
            self.model.clone()
        };
        let model_spans: Vec<Span<'static>> = vec![
            Span::from(model_label).dim(),
            Span::styled(model, self.model_style),
            "   ".dim(),
            CHANGE_MODEL_HINT_COMMAND.cyan(),
            CHANGE_MODEL_HINT_EXPLANATION.dim(),
        ];
        let dir_prefix = format!("{DIR_LABEL} ");
        let dir_prefix_width = UnicodeWidthStr::width(dir_prefix.as_str());
        let dir_max_width = inner_width.saturating_sub(dir_prefix_width);
        let dir = self.format_directory(Some(dir_max_width));
        let lines = vec![
            make_row(title_spans),
            make_row(Vec::new()),
            make_row(model_spans),
            make_row(vec![Span::from(dir_prefix).dim(), Span::from(dir)]),
        ];
        with_border(lines, Some(inner_width))
    }
}

#[cfg(test)]
mod tests {
    use super::{HistoryCell, SESSION_HEADER_MAX_INNER_WIDTH, SessionHeaderHistoryCell};
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
        assert!(rendered.contains("model: openai"));
        assert!(rendered.contains("directory: /tmp/project"));
        assert!(rendered.contains("╭"));
        assert!(rendered.contains("╰"));
    }

    #[test]
    fn renders_one_active_model_like_codex() {
        let cell = SessionHeaderHistoryCell::new(
            "anthropic/deepseek-v4-pro".into(),
            PathBuf::from("/tmp/project"),
        );
        let rendered = cell
            .display_lines(80)
            .into_iter()
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<String>();

        assert!(rendered.contains("model: anthropic/deepseek-v4-pro"));
        assert!(!rendered.contains("sub:"));
    }

    #[test]
    fn truncates_long_model_to_the_codex_card_width() {
        let cell = SessionHeaderHistoryCell::new(
            "provider/this-is-a-deliberately-very-long-model-name".into(),
            PathBuf::from("/tmp/project"),
        );
        let lines = cell.display_lines(80);
        let max_width = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                    .sum::<usize>()
            })
            .max()
            .unwrap_or_default();
        assert!(max_width <= SESSION_HEADER_MAX_INNER_WIDTH + 4);
        assert!(lines.iter().any(|line| line.to_string().contains('…')));
    }

    #[test]
    fn narrow_cards_never_exceed_terminal_width() {
        let cell = SessionHeaderHistoryCell::new(
            "anthropic/deepseek-v4-pro-with-a-very-long-name".into(),
            PathBuf::from("/Users/example/project-with-a-long-name"),
        );
        let lines = cell.display_lines(32);
        assert!(lines.iter().all(|line| line.width() <= 32));
    }

    #[test]
    fn formats_home_directory_with_codex_tilde_prefix() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let cell = SessionHeaderHistoryCell::new("model".into(), home.join("project"));
        let rendered = cell
            .display_lines(100)
            .into_iter()
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<String>();
        assert!(rendered.contains("directory: ~/project"));
    }
}
