//! Small transcript pager ported from Codex's `pager_overlay` surface.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, Wrap};

#[derive(Debug)]
pub(crate) struct PagerOverlay {
    title: String,
    lines: Vec<Line<'static>>,
    scroll: usize,
    page_height: usize,
    last_max_scroll: usize,
}

impl PagerOverlay {
    pub(crate) fn new(title: impl Into<String>, lines: Vec<Line<'static>>) -> Self {
        Self {
            title: title.into(),
            lines,
            scroll: 0,
            page_height: 20,
            last_max_scroll: 0,
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc => return false,
            KeyCode::Char('q') if key.modifiers.is_empty() => return false,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => return false,
            KeyCode::Up | KeyCode::Char('k') if key.modifiers.is_empty() => {
                self.scroll = self.scroll.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if key.modifiers.is_empty() => {
                self.scroll = self.scroll.saturating_add(1)
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page_height),
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self.scroll.saturating_sub(self.page_height)
            }
            KeyCode::Char(' ') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll = self.scroll.saturating_sub(self.page_height)
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page_height),
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self.scroll.saturating_add(self.page_height)
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                self.scroll = self.scroll.saturating_add(self.page_height)
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self
                    .scroll
                    .saturating_sub(self.page_height.saturating_add(1) / 2)
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll = self
                    .scroll
                    .saturating_add(self.page_height.saturating_add(1) / 2)
            }
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = usize::MAX,
            KeyCode::Char('g') if key.modifiers.is_empty() => self.scroll = 0,
            KeyCode::Char('G') if key.modifiers.is_empty() => self.scroll = usize::MAX,
            _ => {}
        }
        true
    }

    /// Refresh the render-only transcript tail while the overlay is open.
    /// Codex keeps the pager attached to the live history rather than freezing
    /// the snapshot taken when Ctrl+T was pressed.
    pub(crate) fn replace_lines(&mut self, lines: Vec<Line<'static>>) {
        // Codex keeps a transcript pager pinned to the live tail when the
        // user was already at the bottom, while preserving an intentional
        // scroll-up position for inspection.
        let was_at_bottom = self.scroll == usize::MAX || self.scroll >= self.last_max_scroll;
        self.lines = lines;
        if was_at_bottom {
            self.scroll = usize::MAX;
        }
    }

    pub(crate) fn scroll_by(&mut self, delta: i32) {
        if delta.is_negative() {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs() as usize);
        } else {
            self.scroll = self.scroll.saturating_add(delta as usize);
        }
    }

    pub(crate) fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        // Keep the same pager chrome as Codex's `PagerView`: a dim slash
        // rail, one title row, a scrollable content viewport, and a three-row
        // footer. The local app supplies flattened history lines, so the
        // renderable-per-cell cache is unnecessary here; all viewport math is
        // otherwise kept identical.
        let header = Line::from(vec![
            Span::raw("/ "),
            Span::styled(
                self.title.clone(),
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(Span::raw("/ ".repeat(usize::from(area.width) / 2)).dim()),
            Rect::new(area.x, area.y, area.width, 1),
        );
        frame.render_widget(
            Paragraph::new(header),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let content_area = Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(4),
        );
        self.page_height = usize::from(content_area.height.max(1));
        let mut paragraph =
            Paragraph::new(Text::from(self.lines.clone())).wrap(Wrap { trim: false });
        let content_width = content_area.width.max(1);
        let content_height = content_area.height as usize;
        let total_height = paragraph.line_count(content_width);
        let max_scroll = total_height.saturating_sub(content_height);
        self.last_max_scroll = max_scroll;
        let scroll = if self.scroll == usize::MAX {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = if self.scroll == usize::MAX {
            usize::MAX
        } else {
            scroll
        };
        paragraph = paragraph.scroll((scroll.min(u16::MAX as usize) as u16, 0));
        frame.render_widget(paragraph, content_area);
        let separator_y = area.bottom().saturating_sub(3);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().add_modifier(Modifier::DIM),
            ))),
            Rect::new(area.x, separator_y, area.width, 1),
        );
        let percent = if max_scroll == 0 {
            100
        } else {
            ((scroll as f32 / max_scroll as f32) * 100.0).round() as u8
        };
        let percent_text = format!(" {percent}% ");
        let x = area.right().saturating_sub(percent_text.len() as u16 + 1);
        frame.render_widget(
            Paragraph::new(Span::styled(
                percent_text,
                Style::default().add_modifier(Modifier::DIM),
            )),
            Rect::new(x, separator_y, area.width.saturating_sub(x - area.x), 1),
        );
        let hint_style = Style::default().add_modifier(Modifier::DIM);
        frame.render_widget(
            Paragraph::new(Line::from(" ↑/↓ scroll   PgUp/PgDn page   g/G jump")).style(hint_style),
            Rect::new(area.x, separator_y.saturating_add(1), area.width, 1),
        );
        frame.render_widget(
            Paragraph::new(Line::from(" Esc/q close   Ctrl+T close")).style(hint_style),
            Rect::new(area.x, separator_y.saturating_add(2), area.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::PagerOverlay;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::text::Line;

    #[test]
    fn pager_scrolls_and_closes_with_codex_keys() {
        let mut pager = PagerOverlay::new("Transcript", vec![Line::from("one"), Line::from("two")]);
        assert!(pager.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)));
        assert!(pager.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL,)));
        assert!(!pager.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
    }

    #[test]
    fn replacing_live_transcript_preserves_codex_follow_bottom_behavior() {
        let mut pager = PagerOverlay::new(
            "Transcript",
            (0..100).map(|i| Line::from(format!("line {i}"))).collect(),
        );
        pager.last_max_scroll = 80;
        pager.scroll = usize::MAX;
        pager.replace_lines(vec![Line::from("new tail")]);
        assert_eq!(pager.scroll, usize::MAX);

        pager.scroll = 3;
        pager.last_max_scroll = 80;
        pager.replace_lines(vec![Line::from("another tail")]);
        assert_eq!(pager.scroll, 3);
    }
}
