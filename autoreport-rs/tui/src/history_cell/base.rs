//! Shared history-cell rendering helpers adapted from Codex's `base.rs`.

use super::*;
use crate::motion::{MotionMode, ReducedMotionIndicator, activity_indicator};
use crate::wrapping::{adaptive_wrap_line, adaptive_wrap_lines, RtOptions};
use ratatui::text::Text;

/// Plain, un-annotated lines. Ported verbatim from Codex's `base.rs`.
#[derive(Debug)]
#[allow(dead_code)] // constructed once R5 builds cells from transcript items
pub(crate) struct PlainHistoryCell {
    pub(super) lines: Vec<Line<'static>>,
}

impl PlainHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for PlainHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.clone())
    }
}

/// Like `PlainHistoryCell` but web URLs in the lines are annotated as OSC 8
/// terminal hyperlinks (clickable in capable terminals). Ported from Codex.
#[derive(Debug)]
#[allow(dead_code)] // constructed once R5 wires assistant-message cells with URLs
pub(crate) struct WebHyperlinkHistoryCell {
    lines: Vec<Line<'static>>,
}

impl WebHyperlinkHistoryCell {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl HistoryCell for WebHyperlinkHistoryCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines.clone()
    }

    fn display_hyperlink_lines(&self, _width: u16) -> Vec<HyperlinkLine> {
        crate::terminal_hyperlinks::annotate_web_urls(self.lines.clone())
    }

    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.lines.clone())
    }
}

/// Wraps a `Text` block with an initial and subsequent prefix per wrapped row.
/// Ported verbatim from Codex's `base.rs`.
#[derive(Debug)]
#[allow(dead_code)] // constructed once R5 wires prefixed transcript blocks
pub(crate) struct PrefixedWrappedHistoryCell {
    text: Text<'static>,
    initial_prefix: Line<'static>,
    subsequent_prefix: Line<'static>,
}

impl PrefixedWrappedHistoryCell {
    pub(crate) fn new(
        text: impl Into<Text<'static>>,
        initial_prefix: impl Into<Line<'static>>,
        subsequent_prefix: impl Into<Line<'static>>,
    ) -> Self {
        Self {
            text: text.into(),
            initial_prefix: initial_prefix.into(),
            subsequent_prefix: subsequent_prefix.into(),
        }
    }
}

impl HistoryCell for PrefixedWrappedHistoryCell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let opts = RtOptions::new(width.max(1) as usize)
            .initial_indent(self.initial_prefix.clone())
            .subsequent_indent(self.subsequent_prefix.clone());
        adaptive_wrap_lines(&self.text, opts)
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        plain_lines(self.text.clone().lines)
    }
}

pub(crate) fn pending_tool_bullet(item: &crate::app_state::ToolEntry) -> Span<'static> {
    activity_indicator(
        item.started_at,
        MotionMode::from_animations_enabled(true),
        ReducedMotionIndicator::StaticBullet,
    )
    .unwrap_or_else(|| "•".dim())
}

pub(crate) fn display_generic_tool_call(
    _agent: &str,
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
        None => pending_tool_bullet(item),
    };
    let header_text = if status.is_some() {
        "Called"
    } else {
        "Calling"
    };
    let invocation_line = Line::from(format!(
        "{}({})",
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
