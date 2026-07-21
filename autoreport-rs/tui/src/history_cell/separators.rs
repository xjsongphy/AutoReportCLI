//! Turn separators copied from Codex's `history_cell/separators.rs`.

use super::HistoryCell;
use ratatui::style::Stylize;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub(crate) struct FinalMessageSeparator {
    elapsed_seconds: Option<u64>,
}

impl FinalMessageSeparator {
    pub(crate) fn new(elapsed_seconds: Option<u64>) -> Self {
        Self { elapsed_seconds }
    }

    fn label(&self) -> Option<String> {
        self.elapsed_seconds
            .filter(|seconds| *seconds > 60)
            .map(crate::bottom_pane::fmt_elapsed_compact)
            .map(|elapsed| format!("Worked for {elapsed}"))
    }
}

impl HistoryCell for FinalMessageSeparator {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(label) = self.label() else {
            return vec![Line::from("─".repeat(width as usize).dim())];
        };
        let label = format!("─ {label} ─");
        let label_width = UnicodeWidthStr::width(label.as_str());
        let label = if label_width > usize::from(width) {
            crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
                Line::from(label),
                usize::from(width),
            )
            .to_string()
        } else {
            label
        };
        let used_width = UnicodeWidthStr::width(label.as_str());
        vec![Line::from(format!(
            "{label}{}",
            "─".repeat(usize::from(width).saturating_sub(used_width))
        ))]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.label()
            .map(|label| vec![Line::from(label)])
            .unwrap_or_default()
    }
}
