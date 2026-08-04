//! Completion popup state rendered as a child of the composer bottom pane.

use crate::app::Tui;
use crate::render::renderable::Renderable;
use crate::style::accent_style;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, WidgetRef};

const MENTION_LIMIT: usize = 8;
const SLASH_LIMIT: usize = 8;
const POPUP_BORDER_ROWS: u16 = 2;
const SLASH_COMMAND_COLUMN_WIDTH: usize = 22;

pub(crate) struct CompletionPopup {
    lines: Vec<Line<'static>>,
    bordered: bool,
}

impl Tui {
    pub(crate) fn completion_popup_build(&self) -> Option<CompletionPopup> {
        if let Some(s) = self.composer.slash_popup() {
            let mut lines: Vec<Line> = Vec::new();
            if s.matches.is_empty() {
                lines.push(Line::from(Span::styled(
                    "no matching commands",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for (i, cmd) in s.matches.iter().enumerate().take(SLASH_LIMIT) {
                    let selected = i == s.selected;
                    let style = if selected {
                        accent_style()
                    } else {
                        Style::default()
                    };
                    let name = format!("/{}", cmd.name);
                    let padding = " ".repeat(SLASH_COMMAND_COLUMN_WIDTH.saturating_sub(name.len()));
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {name}{padding}"), style),
                        Span::styled(
                            cmd.description,
                            if selected {
                                style
                            } else {
                                Style::default().fg(Color::DarkGray)
                            },
                        ),
                    ]));
                }
            }
            Some(CompletionPopup {
                lines,
                bordered: false,
            })
        } else if let Some(m) = self.composer.mention_popup() {
            let mut lines: Vec<Line> = Vec::new();
            if m.matches.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  no matching files",
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                for (i, p) in m.matches.iter().enumerate().take(MENTION_LIMIT) {
                    let style = if i == m.selected {
                        accent_style()
                    } else {
                        Style::default()
                    };
                    lines.push(Line::from(Span::styled(format!("  {p}"), style)));
                }
            }
            Some(CompletionPopup {
                lines,
                bordered: true,
            })
        } else {
            None
        }
    }
}

impl Renderable for CompletionPopup {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        Clear.render_ref(area, buf);
        let paragraph = if self.bordered {
            Paragraph::new(self.lines.clone()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" @ files ", accent_style())),
            )
        } else {
            Paragraph::new(self.lines.clone())
        };
        paragraph.render_ref(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        self.lines.len() as u16 + if self.bordered { POPUP_BORDER_ROWS } else { 0 }
    }
}

#[cfg(test)]
impl CompletionPopup {
    pub(crate) fn new(lines: Vec<Line<'static>>, bordered: bool) -> Self {
        Self { lines, bordered }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bottom_pane::ChatComposer;
    use crate::bottom_pane::rendering::ComposerPopupRenderable;

    #[test]
    fn active_popup_owns_the_composer_footer_slot() {
        let composer = ChatComposer::new("Main");
        let popup = CompletionPopup::new(vec![Line::from("/help")], false);
        let renderable = ComposerPopupRenderable {
            composer: &composer,
            popup,
        };
        assert_eq!(renderable.desired_height(80), 4);

        let area = Rect::new(0, 0, 40, 4);
        let mut buffer = Buffer::empty(area);
        renderable.render(area, &mut buffer);
        let rows = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows[3].contains("/help"));
        assert!(!rows.iter().any(|row| row.contains("? for shortcuts")));
    }

    #[test]
    fn popup_height_includes_file_border_rows() {
        assert_eq!(
            CompletionPopup::new(vec![Line::from("a")], true).desired_height(80),
            1 + POPUP_BORDER_ROWS
        );
    }
}
