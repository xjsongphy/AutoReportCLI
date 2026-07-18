//! Chat composer migrated from Codex's `bottom_pane/chat_composer.rs`.
//!
//! AutoReport's command and mention catalog remains runtime-specific, while the editable draft,
//! cursor, and bottom-pane rendering live in the same component boundary as Codex.

use crate::render::renderable::Renderable;
use crate::style::user_message_style;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, WidgetRef};
use unicode_width::UnicodeWidthStr;

pub(crate) struct ChatComposer {
    text: String,
    cursor: usize,
    focused_agent: String,
    show_agent_picker: bool,
}

impl ChatComposer {
    pub(crate) fn new(focused_agent: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            focused_agent: focused_agent.into(),
            show_agent_picker: true,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn set_text_and_cursor(&mut self, text: String, cursor: usize) {
        self.text = text;
        self.cursor = self.clamp_cursor(cursor.min(self.text.len()));
    }

    pub(crate) fn take_text(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub(crate) fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub(crate) fn delete_previous(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor is after a character");
        self.cursor -= previous.len_utf8();
        self.text.remove(self.cursor);
    }

    pub(crate) fn delete_next(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor is before a character");
        self.text.drain(self.cursor..self.cursor + next.len_utf8());
    }

    pub(crate) fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= self.text[..self.cursor]
                .chars()
                .next_back()
                .expect("cursor is after a character")
                .len_utf8();
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..]
                .chars()
                .next()
                .expect("cursor is before a character")
                .len_utf8();
        }
    }

    pub(crate) fn set_focused_agent(&mut self, agent: impl Into<String>) {
        self.focused_agent = agent.into();
    }

    fn clamp_cursor(&self, cursor: usize) -> usize {
        if self.text.is_char_boundary(cursor) {
            cursor
        } else {
            self.text
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < cursor)
                .last()
                .unwrap_or(0)
        }
    }
}

impl Renderable for ChatComposer {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        Block::default()
            .style(user_message_style())
            .render_ref(area, buf);

        // This is Codex's composer geometry: no surrounding border, a two-column live prefix,
        // and the footer hint below the editable row.
        let prompt = Span::from("›").bold();
        let draft = if self.text.is_empty() {
            Span::from("Describe a task or ask a question…").dim()
        } else {
            Span::from(self.text.clone())
        };
        Line::from(vec![prompt, Span::raw(" "), draft]).render_ref(
            Rect::new(area.x + 1, area.y + 1, area.width.saturating_sub(2), 1),
            buf,
        );

        let hint = if self.show_agent_picker {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(&self.focused_agent, Style::default().fg(Color::Cyan)),
                Span::raw("  Tab switch agent   Enter send   @ files   / commands"),
            ])
        } else {
            Line::from("  Enter send   @ files   / commands")
        };
        hint.dim().render_ref(
            Rect::new(
                area.x,
                area.y + 2,
                area.width,
                area.height.saturating_sub(2).min(1),
            ),
            buf,
        );
    }

    fn desired_height(&self, _width: u16) -> u16 {
        4
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let prefix_width = UnicodeWidthStr::width("› ") as u16;
        let cursor_width = UnicodeWidthStr::width(&self.text[..self.cursor]) as u16;
        Some((
            area.x
                .saturating_add(1)
                .saturating_add(prefix_width)
                .saturating_add(cursor_width)
                .min(area.right().saturating_sub(1)),
            area.y.saturating_add(1),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::ChatComposer;

    #[test]
    fn composer_keeps_utf8_cursor_boundaries_during_editing() {
        let mut composer = ChatComposer::new("Main");
        composer.insert('你');
        composer.insert('a');
        assert_eq!(composer.text(), "你a");

        composer.move_left();
        composer.delete_previous();
        assert_eq!(composer.text(), "a");
        assert_eq!(composer.cursor(), 0);

        composer.delete_next();
        assert!(composer.text().is_empty());
    }

    #[test]
    fn taking_text_resets_the_cursor() {
        let mut composer = ChatComposer::new("Main");
        composer.insert('a');
        assert_eq!(composer.take_text(), "a");
        assert_eq!(composer.cursor(), 0);
        assert!(composer.text().is_empty());
    }
}
