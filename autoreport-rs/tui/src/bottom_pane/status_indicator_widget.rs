//! Live status row copied from Codex's `status_indicator_widget.rs`.
//!
//! AutoReport's runtime exposes a compact `AgentStatus` rather than Codex's
//! richer task-status object, so the constructor maps that enum to the same
//! header/elapsed/detail rendering contract. The widget owns its next-frame
//! request, matching Codex's bottom-pane responsibility boundary.

use autoreport_core::types::AgentStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Paragraph, WidgetRef};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use crate::frame_requester::FrameRequester;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::motion::{MotionMode, ReducedMotionIndicator, activity_indicator, shimmer_text};
use crate::render::renderable::Renderable;
use crate::wrapping::{RtOptions, word_wrap_lines};

pub(crate) const STATUS_DETAILS_DEFAULT_MAX_LINES: usize = 3;
const DETAILS_PREFIX: &str = "  └ ";

pub(crate) struct StatusIndicatorWidget {
    status: AgentStatus,
    started_at: Option<Instant>,
    header: String,
    details: Option<String>,
    details_max_lines: usize,
    inline_message: Option<String>,
    show_interrupt_hint: bool,
    animations_enabled: bool,
    frame_requester: Option<FrameRequester>,
}

impl StatusIndicatorWidget {
    pub(crate) fn new(status: AgentStatus, started_at: Option<Instant>) -> Self {
        let running = matches!(
            status,
            AgentStatus::Thinking | AgentStatus::RunningTool | AgentStatus::DebugMode
        );
        let (header, show_interrupt_hint) = match status {
            AgentStatus::Queued => ("Queued", false),
            AgentStatus::Error => ("Error", false),
            // Codex `update_header` surfaces the active verb rather than a
            // generic "Working" when the caller knows what is happening; our
            // `AgentStatus` carries that granularity for tool runs and debug.
            AgentStatus::RunningTool => ("Running tool", running),
            AgentStatus::DebugMode => ("Debugging", running),
            _ => ("Working", running),
        };
        Self {
            status,
            started_at,
            header: header.to_string(),
            details: None,
            details_max_lines: STATUS_DETAILS_DEFAULT_MAX_LINES,
            inline_message: None,
            show_interrupt_hint,
            animations_enabled: true,
            frame_requester: None,
        }
    }

    pub(crate) fn with_frame_requester(mut self, frame_requester: Option<FrameRequester>) -> Self {
        self.frame_requester = frame_requester;
        self
    }

    /// Attach the short task details that Codex renders below the animated
    /// Working row. Keeping this as a builder preserves the simple status
    /// constructor for idle/queued/error callers while allowing the chat
    /// surface to pass the active tool context through the same API shape.
    pub(crate) fn with_details(
        mut self,
        details: Option<String>,
        inline_message: Option<String>,
    ) -> Self {
        self.update_details(details, STATUS_DETAILS_DEFAULT_MAX_LINES);
        self.update_inline_message(inline_message);
        self
    }

    fn running(&self) -> bool {
        matches!(
            self.status,
            AgentStatus::Thinking | AgentStatus::RunningTool | AgentStatus::DebugMode
        )
    }

    fn visible(&self) -> bool {
        self.running() || matches!(self.status, AgentStatus::Queued | AgentStatus::Error)
    }

    fn elapsed_seconds(&self) -> u64 {
        self.started_at
            .map(|started| started.elapsed().as_secs())
            .unwrap_or_default()
    }

    /// Keep the same optional detail API as Codex's widget. Runtime-specific
    /// callers can attach a short tool/process description without changing
    /// the layout contract.
    #[allow(dead_code)]
    pub(crate) fn update_details(&mut self, details: Option<String>, max_lines: usize) {
        self.details_max_lines = max_lines.max(1);
        self.details = details
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
    }

    #[allow(dead_code)]
    pub(crate) fn update_inline_message(&mut self, message: Option<String>) {
        self.inline_message = message
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
    }

    fn wrapped_details_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(details) = self.details.as_deref() else {
            return Vec::new();
        };
        if width == 0 {
            return Vec::new();
        }
        let prefix_width = UnicodeWidthStr::width(DETAILS_PREFIX);
        let opts = RtOptions::new(usize::from(width))
            .initial_indent(Line::from(DETAILS_PREFIX.dim()))
            .subsequent_indent(Line::from(" ".repeat(prefix_width).dim()))
            .break_words(true);
        let mut lines = word_wrap_lines(details.lines().map(|line| vec![line.dim()]), opts);
        if lines.len() > self.details_max_lines {
            lines.truncate(self.details_max_lines);
            let max_base_len = usize::from(width)
                .saturating_sub(prefix_width)
                .saturating_sub(1);
            if let Some(last) = lines.last_mut()
                && let Some(span) = last.spans.last_mut()
            {
                let trimmed: String = span.content.as_ref().chars().take(max_base_len).collect();
                *span = format!("{trimmed}…").dim();
            }
        }
        lines
    }
}

pub(crate) fn fmt_elapsed_compact(elapsed_secs: u64) -> String {
    if elapsed_secs < 60 {
        return format!("{elapsed_secs}s");
    }
    if elapsed_secs < 3600 {
        return format!("{}m {:02}s", elapsed_secs / 60, elapsed_secs % 60);
    }
    format!(
        "{}h {:02}m {:02}s",
        elapsed_secs / 3600,
        (elapsed_secs % 3600) / 60,
        elapsed_secs % 60
    )
}

impl Renderable for StatusIndicatorWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.visible() || area.is_empty() {
            return;
        }
        // Codex schedules the next animation frame from the status widget
        // itself. Keep the outer event loop responsible only for dispatching
        // the resulting draw request.
        if self.animations_enabled
            && self.running()
            && let Some(frame_requester) = &self.frame_requester
        {
            frame_requester.schedule_frame_in(std::time::Duration::from_millis(32));
        }
        let motion_mode = MotionMode::from_animations_enabled(self.animations_enabled);
        let mut spans = Vec::with_capacity(8);
        if self.running()
            && let Some(indicator) =
                activity_indicator(self.started_at, motion_mode, ReducedMotionIndicator::Hidden)
        {
            spans.push(indicator);
            spans.push(" ".into());
        }
        spans.extend(shimmer_text(&self.header, motion_mode));
        let pretty_elapsed = fmt_elapsed_compact(self.elapsed_seconds());
        if self.running() {
            spans.push(" ".into());
            if self.show_interrupt_hint {
                spans.extend(vec![
                    format!("({pretty_elapsed} • ").dim(),
                    "esc".into(),
                    " to interrupt)".dim(),
                ]);
            } else {
                spans.push(format!("({pretty_elapsed})").dim());
            }
        }
        if let Some(message) = &self.inline_message {
            spans.push(" · ".dim());
            spans.push(message.clone().dim());
        }
        let mut lines = vec![truncate_line_with_ellipsis_if_overflow(
            Line::from(spans),
            usize::from(area.width),
        )];
        if area.height > 1 {
            lines.extend(
                self.wrapped_details_lines(area.width)
                    .into_iter()
                    .take(usize::from(area.height.saturating_sub(1))),
            );
        }
        Paragraph::new(Text::from(lines)).render_ref(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        if !self.visible() {
            return 0;
        }
        1 + u16::try_from(self.wrapped_details_lines(width).len()).unwrap_or(u16::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    #[test]
    fn renders_codex_working_shape() {
        let widget = StatusIndicatorWidget::new(
            AgentStatus::Thinking,
            Some(Instant::now() - Duration::from_secs(61)),
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).expect("terminal");
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        let line = terminal.backend().buffer().content()[..80]
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(line.contains("Working"));
        assert!(line.contains("1m 01s"));
        assert!(line.contains("esc to interrupt"));
    }

    #[test]
    fn queued_and_error_states_are_visible_without_interrupt_hint() {
        for status in [AgentStatus::Queued, AgentStatus::Error] {
            let widget = StatusIndicatorWidget::new(status, None);
            assert_eq!(widget.desired_height(80), 1);
        }
    }

    #[test]
    fn renders_codex_style_tool_details_below_working_row() {
        let widget = StatusIndicatorWidget::new(AgentStatus::RunningTool, Some(Instant::now()))
            .with_details(Some("exec · uname -a".to_string()), None);
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).expect("terminal");
        terminal
            .draw(|frame| widget.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(content.contains("exec · uname -a"));
        assert!(content.contains("└"));
    }
}
