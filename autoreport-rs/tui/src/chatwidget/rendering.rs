//! Main chat-surface composition migrated from Codex's `chatwidget/rendering.rs`.
//!
//! AutoReport keeps its own runtime and event model, but the terminal surface follows Codex's
//! render tree: transcript flexes into the available space and the composer is a bottom-pane
//! child with no enclosing message box.

use crate::app::Tui;
use crate::app_state::Cell;
use crate::bottom_pane::BottomPane;
use crate::chatwidget::tool_arg_summary;
use crate::history_cell::{HistoryCell, SessionHeaderHistoryCell};
use crate::render::Insets;
use crate::render::renderable::FlexRenderable;
use crate::render::renderable::Renderable;
use crate::render::renderable::RenderableExt;
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

    /// Height needed by the active transcript tail and bottom pane. Finalized history is written
    /// to terminal scrollback and therefore does not consume this viewport.
    pub(crate) fn codex_chat_viewport_height(&self, width: u16) -> u16 {
        let active = TranscriptAreaRenderable {
            child: self.transcript_history_cell(self.committed_history_len()),
        }
        .desired_height(width);
        // The completion popup now lives inside the bottom pane (see
        // `codex_chat_renderable`), so its height is part of the bottom-pane
        // height rather than a separate reservation here.
        active
            .saturating_add(self.codex_chat_bottom_pane_height(width))
            .max(1)
    }

    /// Total chat-bottom height, including Codex's one-row breathing inset.
    /// The value is delegated to the same `BottomPane` renderable that paints
    /// the frame, so viewport sizing and drawing cannot drift apart.
    pub(crate) fn codex_chat_bottom_pane_height(&self, width: u16) -> u16 {
        1u16.saturating_add(BottomPane::new(self).desired_height(width))
    }

    fn codex_chat_renderable(&self, width: u16) -> RenderableItem<'_> {
        let transcript = TranscriptAreaRenderable {
            child: self.transcript_history_cell(self.history_inserted_cells),
        };
        let mut flex = FlexRenderable::new();
        flex.push(/*flex*/ 1, RenderableItem::Owned(Box::new(transcript)));
        flex.push(
            /*flex*/ 0,
            BottomPane::new(self)
                .as_renderable(width)
                .inset(Insets::tlbr(
                    /*top*/ 1, /*left*/ 0, /*bottom*/ 0, /*right*/ 0,
                )),
        );
        RenderableItem::Owned(Box::new(flex))
    }

    fn transcript_history_cell(&self, start: usize) -> TranscriptHistoryCell<'_> {
        let model = if self.focused == autoreport_core::types::AgentType::Main {
            self.main_model.clone()
        } else {
            self.sub_model.clone()
        };
        let start = start.min(self.history.len());
        TranscriptHistoryCell {
            cells: &self.history[start..],
            focused: self.focused,
            model,
            workspace: self.workspace.clone(),
            raw_output: self.raw_output,
            include_header: start == 0,
        }
    }

    /// Codex's status row shows the active operation below the animated
    /// header. The local bus already owns the authoritative pending tool
    /// entry, so derive the same short detail text from that entry instead of
    /// inventing a second status channel.
    pub(crate) fn active_status_context(&self) -> (Option<String>, Option<String>) {
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

/// The local runtime stores protocol events in `Cell`, but the chat render
/// tree consumes the tail through the same dynamic `HistoryCell` contract as
/// Codex. This adapter keeps filtering/header policy outside the paragraph
/// renderer while leaving each `Cell` responsible for its own styled lines.
#[derive(Debug)]
struct TranscriptHistoryCell<'a> {
    cells: &'a [Cell],
    focused: autoreport_core::types::AgentType,
    model: String,
    workspace: std::path::PathBuf,
    raw_output: bool,
    include_header: bool,
}

impl HistoryCell for TranscriptHistoryCell<'_> {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        let mut lines = if self.include_header {
            SessionHeaderHistoryCell::new(self.model.clone(), self.workspace.clone())
                .display_lines(width)
        } else {
            Vec::new()
        };
        if self.raw_output {
            lines.extend(crate::history_cell::render_raw_history_lines_for_agent(
                self.cells,
                self.focused,
            ));
        } else {
            lines.extend(crate::history_cell::render_history_lines_for_agent(
                self.cells,
                self.focused,
                width,
            ));
        }
        lines
    }

    fn display_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        let mut lines = if self.include_header {
            SessionHeaderHistoryCell::new(self.model.clone(), self.workspace.clone())
                .display_hyperlink_lines(width)
        } else {
            Vec::new()
        };
        if !self.raw_output {
            lines.extend(
                crate::history_cell::render_history_hyperlink_lines_for_agent(
                    self.cells,
                    self.focused,
                    width,
                ),
            );
        } else {
            lines = crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width));
        }
        lines
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }
}

/// Codex's transcript area reserves one breathing row and scrolls the rendered tail into view.
struct TranscriptAreaRenderable<'a> {
    child: TranscriptHistoryCell<'a>,
}

impl Renderable for TranscriptAreaRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let lines = self.child.display_lines(area.width);
        let hyperlink_lines = self.child.display_hyperlink_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let line_count = paragraph.line_count(area.width);
        let overflow = line_count.saturating_sub(usize::from(area.height));
        Clear.render_ref(area, buf);
        paragraph
            .scroll((u16::try_from(overflow).unwrap_or(u16::MAX), 0))
            .render(area, buf);
        // Mark web URLs as OSC 8 terminal hyperlinks over the transcript area.
        // `mark_buffer_hyperlinks` re-wraps each line to locate URL cells and
        // honors the same scroll offset used above.
        crate::terminal_hyperlinks::mark_buffer_hyperlinks(buf, area, &hyperlink_lines, overflow);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let paragraph =
            Paragraph::new(Text::from(self.child.display_lines(width))).wrap(Wrap { trim: false });
        paragraph.line_count(width).try_into().unwrap_or(u16::MAX)
    }
}
