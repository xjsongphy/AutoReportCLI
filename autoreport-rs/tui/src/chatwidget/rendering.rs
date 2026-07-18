//! Main chat-surface composition migrated from Codex's `chatwidget/rendering.rs`.
//!
//! AutoReport keeps its own runtime and event model, but the terminal surface follows Codex's
//! render tree: transcript flexes into the available space and the composer is a bottom-pane
//! child with no enclosing message box.

use crate::app::Tui;
use crate::bottom_pane::StatusIndicatorWidget;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableItem;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Clear, Paragraph, WidgetRef, Wrap};

impl Tui {
    pub(crate) fn render_codex_chat(&self, area: Rect, buf: &mut Buffer) {
        self.codex_chat_renderable(area.width).render(area, buf);
    }

    pub(crate) fn codex_chat_cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.codex_chat_renderable(area.width).cursor_pos(area)
    }

    fn codex_chat_renderable(&self, width: u16) -> FlexRenderable<'_> {
        let transcript = TranscriptAreaRenderable {
            lines: self.transcript_lines(width),
            scroll: self.scroll,
        };
        let mut flex = FlexRenderable::new();
        flex.push(/*flex*/ 1, RenderableItem::Owned(Box::new(transcript)));
        let status = self
            .statuses
            .get(&self.focused)
            .copied()
            .unwrap_or(autoreport_core::types::AgentStatus::Idle);
        flex.push(
            /*flex*/ 0,
            RenderableItem::Owned(Box::new(StatusIndicatorWidget::new(status))),
        );
        flex.push(/*flex*/ 0, RenderableItem::Borrowed(&self.composer));
        flex
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        use crate::history_cell::{HistoryCell, SessionHeaderHistoryCell};
        let header =
            SessionHeaderHistoryCell::new(self.provider_id.clone(), self.workspace.clone());
        let mut lines = header.display_lines(width);
        lines.extend(crate::history_cell::render_history_lines(
            &self.history,
            width,
        ));
        lines
    }
}

/// Codex's transcript area reserves one breathing row and scrolls the rendered tail into view.
struct TranscriptAreaRenderable {
    lines: Vec<Line<'static>>,
    scroll: usize,
}

impl Renderable for TranscriptAreaRenderable {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let paragraph = Paragraph::new(Text::from(self.lines.clone())).wrap(Wrap { trim: false });
        let line_count = paragraph.line_count(area.width);
        let overflow = line_count.saturating_sub(usize::from(area.height));
        let scroll = overflow.saturating_sub(self.scroll);
        Clear.render_ref(area, buf);
        paragraph
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let paragraph = Paragraph::new(Text::from(self.lines.clone())).wrap(Wrap { trim: false });
        paragraph.line_count(width).try_into().unwrap_or(u16::MAX)
    }
}
