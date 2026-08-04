//! Session header history cell migrated from Codex's `history_cell/session.rs`.

use super::HistoryCell;
use crate::style::accent_style;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use std::path::PathBuf;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
        format_directory_display(&self.directory, max_width)
    }
}

/// Format a session cwd with the same rules in both the header card and the
/// composer footer. This mirrors Codex's `format_directory_inner`: an absolute
/// path below the user's home is shown as `~/...`, and a bounded surface keeps
/// both the leading project context and the final path components visible.
pub(crate) fn format_directory_display(
    directory: &std::path::Path,
    max_width: Option<usize>,
) -> String {
    let formatted = dirs::home_dir()
        .and_then(|home| {
            directory.strip_prefix(&home).ok().map(|relative| {
                if relative.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~{}{}", std::path::MAIN_SEPARATOR, relative.display())
                }
            })
        })
        .unwrap_or_else(|| directory.display().to_string());

    match max_width {
        Some(0) => String::new(),
        Some(width) if UnicodeWidthStr::width(formatted.as_str()) > width => {
            center_truncate_path(&formatted, width)
        }
        _ => formatted,
    }
}

/// Compact adaptation of Codex's `center_truncate_path`.
///
/// Paths are much easier to identify when the beginning and the final two
/// components survive truncation. A single long component is front-truncated
/// so the filename/project suffix remains useful.
fn center_truncate_path(path: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(path) <= max_width {
        return path.to_string();
    }

    let separator = std::path::MAIN_SEPARATOR;
    let leading_separator = path.starts_with(separator);
    let mut segments: Vec<&str> = path.split(separator).collect();
    if leading_separator && segments.first() == Some(&"") {
        segments.remove(0);
    }
    if segments.is_empty() {
        return separator.to_string();
    }

    let assemble = |left: &[&str], right: &[&str]| {
        let mut parts = Vec::with_capacity(left.len() + right.len() + 1);
        parts.extend(left.iter().copied());
        if !right.is_empty() && left.len() + right.len() < segments.len() {
            parts.push("…");
        }
        parts.extend(right.iter().copied());
        let mut result = parts.join(&separator.to_string());
        if leading_separator {
            result.insert(0, separator);
        }
        result
    };

    let desired_suffix = segments.len().min(2);
    for left_count in (1..=segments.len()).rev() {
        let max_right = segments.len().saturating_sub(left_count);
        let min_right = if max_right == 0 {
            0
        } else {
            desired_suffix.min(max_right)
        };
        for right_count in (min_right..=max_right).rev() {
            let left_end = left_count.min(segments.len().saturating_sub(right_count));
            let left = &segments[..left_end];
            let right_start = segments.len().saturating_sub(right_count);
            let right = &segments[right_start..];
            let candidate = assemble(left, right);
            if UnicodeWidthStr::width(candidate.as_str()) <= max_width {
                return candidate;
            }
        }
    }

    // No complete segment combination fits. Preserve the first component and
    // path suffix, then front-truncate the final component as Codex does for
    // a single long segment.
    let suffix = segments.last().copied().unwrap_or_default();
    let prefix = &segments[..segments.len().saturating_sub(1)];
    let prefix_text = prefix.join(&separator.to_string());
    let separator_width = usize::from(!prefix_text.is_empty());
    let prefix_width = UnicodeWidthStr::width(prefix_text.as_str());
    let allowed = max_width
        .saturating_sub(usize::from(leading_separator))
        .saturating_sub(prefix_width)
        .saturating_sub(separator_width)
        .max(1);
    let mut result = String::new();
    if leading_separator {
        result.push(separator);
    }
    if !prefix_text.is_empty() {
        result.push_str(&prefix_text);
        result.push(separator);
    }
    result.push_str(&front_truncate(suffix, allowed));
    result
}

fn front_truncate(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(value) <= max_width {
        return value.to_string();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut kept = Vec::new();
    let mut used = 1usize;
    for ch in value.chars().rev() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > max_width {
            break;
        }
        used += width;
        kept.push(ch);
    }
    kept.reverse();
    let mut result = String::from("…");
    result.extend(kept);
    result
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
    use super::{
        HistoryCell, SESSION_HEADER_MAX_INNER_WIDTH, SessionHeaderHistoryCell,
        format_directory_display,
    };
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

    #[test]
    fn center_truncates_long_home_paths_like_codex() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let path = home
            .join("hello")
            .join("the")
            .join("fox")
            .join("is")
            .join("very")
            .join("fast");

        assert_eq!(
            format_directory_display(&path, Some(24)),
            format!(
                "~{}hello{}the{}…{}very{}fast",
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR,
                std::path::MAIN_SEPARATOR
            )
        );
    }

    #[test]
    fn front_truncates_a_single_long_path_component_without_overflowing() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let path = home.join("supercalifragilisticexpialidocious");
        let rendered = format_directory_display(&path, Some(18));

        assert_eq!(
            rendered,
            format!("~{}…cexpialidocious", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(unicode_width::UnicodeWidthStr::width(rendered.as_str()), 18);
    }

    #[test]
    fn zero_width_directory_display_is_empty() {
        assert_eq!(
            format_directory_display(PathBuf::from("/tmp/project").as_path(), Some(0)),
            ""
        );
    }
}
