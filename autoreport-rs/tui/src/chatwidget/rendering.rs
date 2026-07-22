//! Main chat-surface composition migrated from Codex's `chatwidget/rendering.rs`.
//!
//! AutoReport keeps its own runtime and event model, but the terminal surface follows Codex's
//! render tree: transcript flexes into the available space and the composer is a bottom-pane
//! child with no enclosing message box.

use crate::app::Tui;
use crate::app_state::Cell;
use crate::bottom_pane::StatusIndicatorWidget;
use crate::chatwidget::tool_arg_summary;
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

    /// Total fixed-height bottom pane used by popups that must stop above the
    /// composer, pending-input preview, and live Working/details rows.  Keep
    /// this derived from the same status widget as the render tree so a
    /// wrapped tool detail cannot be painted over by a slash/mention popup.
    pub(crate) fn codex_chat_bottom_pane_height(&self, width: u16) -> u16 {
        let status = self
            .statuses
            .get(&self.focused)
            .copied()
            .unwrap_or(autoreport_core::types::AgentStatus::Idle);
        let (details, inline_message) = self.active_status_context();
        let status_height =
            StatusIndicatorWidget::new(status, self.status_since.get(&self.focused).copied())
                .with_frame_requester(self.frame_requester.clone())
                .with_details(details, inline_message)
                .desired_height(width);
        let has_pending_input = self.pending_input_preview.desired_height(width) > 0;
        let has_status = !matches!(status, autoreport_core::types::AgentStatus::Idle);
        status_height
            .saturating_add(u16::from(has_pending_input && has_status))
            .saturating_add(self.pending_input_preview.desired_height(width))
            .saturating_add(self.composer.desired_height(width))
    }

    fn codex_chat_renderable(&self, width: u16) -> FlexRenderable<'_> {
        let transcript = TranscriptAreaRenderable {
            lines: self.transcript_lines(width),
            hyperlink_lines: self.transcript_hyperlink_lines(width),
            scroll: self.scroll,
        };
        let mut flex = FlexRenderable::new();
        flex.push(/*flex*/ 1, RenderableItem::Owned(Box::new(transcript)));
        let status = self
            .statuses
            .get(&self.focused)
            .copied()
            .unwrap_or(autoreport_core::types::AgentStatus::Idle);
        let (details, inline_message) = self.active_status_context();
        flex.push(
            /*flex*/ 0,
            RenderableItem::Owned(Box::new(
                StatusIndicatorWidget::new(status, self.status_since.get(&self.focused).copied())
                    .with_frame_requester(self.frame_requester.clone())
                    .with_details(details, inline_message),
            )),
        );
        // Codex owns the pending-input preview as a sibling of the status and
        // composer in BottomPane. Keep that same render-tree boundary rather
        // than teaching ChatComposer about queue state.
        let has_pending_input = self.pending_input_preview.desired_height(width) > 0;
        let has_status = !matches!(status, autoreport_core::types::AgentStatus::Idle);
        if has_pending_input && has_status {
            // BottomPane keeps one breathing row between status/footer and
            // inline previews when both are visible.
            flex.push(/*flex*/ 0, RenderableItem::Owned("".into()));
        }
        flex.push(
            /*flex*/ 0,
            RenderableItem::Borrowed(&self.pending_input_preview),
        );
        flex.push(/*flex*/ 0, RenderableItem::Borrowed(&self.composer));
        flex
    }

    fn transcript_lines(&self, width: u16) -> Vec<Line<'static>> {
        use crate::history_cell::{HistoryCell, SessionHeaderHistoryCell};
        let model = if self.focused == autoreport_core::types::AgentType::Main {
            self.main_model.clone()
        } else {
            self.sub_model.clone()
        };
        let header = SessionHeaderHistoryCell::new(model, self.workspace.clone());
        let mut lines = header.display_lines(width);
        if self.raw_output {
            lines.extend(crate::history_cell::render_raw_history_lines_for_agent(
                &self.history,
                self.focused,
            ));
        } else {
            lines.extend(crate::history_cell::render_history_lines_for_agent(
                &self.history,
                self.focused,
                width,
            ));
        }
        lines
    }

    /// Hyperlink-aware counterpart of [`transcript_lines`] in the same
    /// header + cell order, so `mark_buffer_hyperlinks` can annotate OSC 8
    /// links over the rendered transcript area.
    fn transcript_hyperlink_lines(&self, width: u16) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        use crate::history_cell::{HistoryCell, SessionHeaderHistoryCell};
        let model = if self.focused == autoreport_core::types::AgentType::Main {
            self.main_model.clone()
        } else {
            self.sub_model.clone()
        };
        let header = SessionHeaderHistoryCell::new(model, self.workspace.clone());
        let mut lines = header.display_hyperlink_lines(width);
        // Raw-output mode drops styling/links; otherwise carry the per-cell
        // hyperlink annotations (assistant markdown URLs get annotated).
        if !self.raw_output {
            lines.extend(crate::history_cell::render_history_hyperlink_lines_for_agent(
                &self.history,
                self.focused,
                width,
            ));
        }
        lines
    }

    /// Codex's status row shows the active operation below the animated
    /// header. The local bus already owns the authoritative pending tool
    /// entry, so derive the same short detail text from that entry instead of
    /// inventing a second status channel.
    fn active_status_context(&self) -> (Option<String>, Option<String>) {
        let details = self.history.iter().rev().find_map(|cell| {
            let Cell::ToolGroup { agent, items } = cell else {
                return None;
            };
            if *agent != self.focused {
                return None;
            }
            let item = items
                .iter()
                .rev()
                .find(|item| item.result.is_none() && item.error.is_none())?;
            let summary = tool_arg_summary(&item.name, &item.args);
            let text = if summary.is_empty() {
                item.name.clone()
            } else {
                format!("{} · {}", item.name, summary)
            };
            Some(text)
        });
        (details, None)
    }
}

/// Codex's transcript area reserves one breathing row and scrolls the rendered tail into view.
struct TranscriptAreaRenderable {
    lines: Vec<Line<'static>>,
    /// Parallel to `lines`: each row's hyperlink annotations, used to mark
    /// OSC 8 links over the rendered area after the Paragraph draws.
    hyperlink_lines: Vec<crate::terminal_hyperlinks::HyperlinkLine>,
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
        // Mark web URLs as OSC 8 terminal hyperlinks over the transcript area.
        // `mark_buffer_hyperlinks` re-wraps each line to locate URL cells and
        // honors the same scroll offset used above.
        crate::terminal_hyperlinks::mark_buffer_hyperlinks(
            buf,
            area,
            &self.hyperlink_lines,
            scroll,
        );
    }

    fn desired_height(&self, width: u16) -> u16 {
        let paragraph = Paragraph::new(Text::from(self.lines.clone())).wrap(Wrap { trim: false });
        paragraph.line_count(width).try_into().unwrap_or(u16::MAX)
    }
}
