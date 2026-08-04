//! Application layout and widget rendering.

use crate::app::Tui;
use crate::bottom_pane::status_line_setup::StatusLineItem;
use crate::bottom_pane::status_line_style::status_line_from_segments;
use crate::custom_terminal::Frame;
use crate::history_cell::format_directory_display;
use ratatui::text::Line;
use ratatui::widgets::Clear;

impl Tui {
    pub(crate) fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        // Configuration screens and the pager own the complete frame. Do not
        // paint the chat beneath their sparse layouts.
        if let Some(screen) = self.overlay.as_mut() {
            f.render_widget(Clear, area);
            screen.draw(f);
            return;
        }
        if let Some(pager) = self.pager.as_mut() {
            f.render_widget(Clear, area);
            let lines = if self.raw_output {
                crate::history_cell::render_raw_history_lines_for_agent(&self.history, self.focused)
            } else {
                crate::history_cell::render_history_lines_for_agent(
                    &self.history,
                    self.focused,
                    area.width.max(1),
                )
            };
            pager.replace_lines(lines);
            pager.draw(f);
            return;
        }
        self.composer.set_status_line(self.composer_status_line());
        // The transcript, status row, composer, and any active `/` or `@`
        // completion popup are all in-layout children of `render_codex_chat`.
        // The popup occupies the slot below the composer (Codex's footer
        // slot), so opening it never wipes transcript rows and needs no
        // floating overlay here.
        self.render_codex_chat(area, f.buffer_mut());
        if let Some((x, y)) = self.codex_chat_cursor_pos(area) {
            f.set_cursor_position((x, y));
        }
    }

    fn composer_status_line(&self) -> Option<Line<'static>> {
        let model = match self.focused {
            autoreport_core::types::AgentType::Main => self.main_model.clone(),
            _ => self.sub_model.clone(),
        };
        let directory = format_directory_display(&self.workspace, None);
        status_line_from_segments(
            [
                (StatusLineItem::ModelName, model),
                (StatusLineItem::CurrentDir, directory),
            ],
            true,
        )
    }
}
