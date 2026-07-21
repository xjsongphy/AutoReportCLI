//! Codex-style preview for follow-up messages queued while an agent is busy.
//!
//! This is intentionally a small adaptation of Codex's
//! `bottom_pane/pending_input_preview.rs`: the local runtime does not expose
//! steers/rejected steers yet, so only its ordinary queued-input section is
//! enabled. Keeping this as a separate renderable preserves the same bottom
//! pane ownership and wrapping behavior as upstream.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::render::renderable::Renderable;
use crate::wrapping::{RtOptions, adaptive_wrap_lines};

const PREVIEW_LINE_LIMIT: usize = 3;
const EDIT_HINT: &str = if cfg!(target_os = "macos") {
    "    ⌥ + ↑ edit last queued message"
} else {
    "    alt + ↑ edit last queued message"
};

#[derive(Debug, Default, Clone)]
pub(crate) struct PendingInputPreview {
    pub queued_messages: Vec<String>,
}

impl PendingInputPreview {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_queued_messages(&mut self, queued_messages: Vec<String>) {
        self.queued_messages = queued_messages;
    }

    fn lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.queued_messages.is_empty() || width < 4 {
            return Vec::new();
        }

        let mut lines = vec![Line::from("• Queued follow-up inputs".dim())];
        for message in &self.queued_messages {
            let wrapped = adaptive_wrap_lines(
                message.lines().map(|line| Line::from(line.dim().italic())),
                RtOptions::new(width as usize)
                    .initial_indent(Line::from("  ↳ ".dim()))
                    .subsequent_indent(Line::from("    ")),
            );
            let len = wrapped.len();
            lines.extend(wrapped.into_iter().take(PREVIEW_LINE_LIMIT));
            if len > PREVIEW_LINE_LIMIT {
                lines.push(Line::from("    …".dim().italic()));
            }
        }
        lines.push(Line::from(EDIT_HINT.dim()));
        lines
    }
}

impl Renderable for PendingInputPreview {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        Paragraph::new(self.lines(area.width)).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.lines(width).len().try_into().unwrap_or(u16::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn empty_preview_has_no_height() {
        assert_eq!(PendingInputPreview::new().desired_height(40), 0);
    }

    #[test]
    fn queued_message_has_codex_header_and_arrow() {
        let mut preview = PendingInputPreview::new();
        preview.set_queued_messages(vec!["follow up".into()]);
        assert_eq!(preview.desired_height(40), 3);
        let area = Rect::new(0, 0, 40, preview.desired_height(40));
        let mut buffer = Buffer::empty(area);
        preview.render(area, &mut buffer);
        let text = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Queued follow-up inputs"));
        assert!(text.contains("↳"));
    }
}
