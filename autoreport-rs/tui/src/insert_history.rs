//! Minimal Codex-style insertion of finalized transcript rows into terminal scrollback.
//!
//! The chat surface owns only the active tail. Finalized rows are written above the viewport so
//! the terminal emulator, rather than a Ratatui paragraph, owns wheel scrolling.

use crate::custom_terminal::Terminal;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{Attribute, Print, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::backend::Backend;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use std::io::{self, Write};
use unicode_width::UnicodeWidthStr;

pub(crate) fn insert_history_lines<B: Backend + Write>(
    terminal: &mut Terminal<B>,
    lines: &[Line<'static>],
) -> io::Result<()> {
    if lines.is_empty() || terminal.viewport_area.is_empty() {
        return Ok(());
    }

    let screen_size = terminal.size()?;
    let mut area = terminal.viewport_area;
    let cursor = terminal.last_known_cursor_pos;
    let width = usize::from(area.width.max(1));
    let wrapped_lines: Vec<Line<'static>> = lines
        .iter()
        .flat_map(|line| wrap_line(line, width))
        .collect();
    let wrapped_rows = wrapped_lines.len() as u16;
    let mut writer = terminal.backend_mut();

    // When the viewport is not at the physical bottom, first move it down to
    // make room for the newly finalized rows. This is the important Codex
    // behavior that also works when the viewport currently starts at row zero.
    let scroll_amount = wrapped_rows.min(screen_size.height.saturating_sub(area.bottom()));
    if scroll_amount > 0 {
        write!(
            writer,
            "\x1b[{};{}r",
            area.top().saturating_add(1),
            screen_size.height
        )?;
        queue!(writer, MoveTo(0, area.top()))?;
        for _ in 0..scroll_amount {
            write!(writer, "\x1bM")?;
        }
        write!(writer, "\x1b[r")?;
        area.y = area.y.saturating_add(scroll_amount);
    }

    // Restrict scrolling to the history rows above the viewport. Start one
    // row above the viewport and emit a CRLF before every wrapped line, just
    // like Codex's `insert_history_hyperlink_lines...` implementation.
    write!(writer, "\x1b[1;{}r", area.top())?;
    queue!(writer, MoveTo(0, area.top().saturating_sub(1)))?;
    for line in &wrapped_lines {
        queue!(writer, Print("\r\n"))?;
        write_line(&mut writer, line)?;
    }
    write!(writer, "\x1b[r")?;
    queue!(writer, MoveTo(cursor.x, cursor.y))?;
    std::io::Write::flush(&mut writer)?;
    if area != terminal.viewport_area {
        terminal.set_viewport_area(area);
    }
    terminal.note_history_rows_inserted(wrapped_rows);
    Ok(())
}

fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    if line.spans.is_empty() {
        return vec![Line::from(Span::styled(String::new(), line.style))];
    }

    let width = width.max(1);
    let mut rows = Vec::new();
    let mut row_spans: Vec<Span<'static>> = Vec::new();
    let mut row_width = 0;
    for span in &line.spans {
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
            if !row_spans.is_empty() && row_width + ch_width > width {
                rows.push(Line::from(std::mem::take(&mut row_spans)).style(line.style));
                row_width = 0;
            }
            if let Some(last) = row_spans.last_mut()
                && last.style == span.style
            {
                last.content.to_mut().push(ch);
            } else {
                row_spans.push(Span::styled(ch.to_string(), span.style));
            }
            row_width += ch_width;
        }
    }
    if !row_spans.is_empty() {
        rows.push(Line::from(row_spans).style(line.style));
    }
    rows
}

fn write_line<W: Write>(writer: &mut W, line: &Line<'_>) -> io::Result<()> {
    queue!(writer, SetAttribute(Attribute::Reset))?;
    for span in &line.spans {
        let style = span.style.patch(line.style);
        queue!(
            writer,
            SetForegroundColor(
                style
                    .fg
                    .map_or(crossterm::style::Color::Reset, to_crossterm_color,)
            ),
            SetBackgroundColor(
                style
                    .bg
                    .map_or(crossterm::style::Color::Reset, to_crossterm_color,)
            ),
        )?;
        for modifier in [
            Modifier::BOLD,
            Modifier::DIM,
            Modifier::ITALIC,
            Modifier::UNDERLINED,
            Modifier::REVERSED,
        ] {
            if style.add_modifier.contains(modifier) {
                queue!(writer, SetAttribute(to_crossterm_modifier(modifier)))?;
            }
        }
        write!(writer, "{}", span.content)?;
    }
    queue!(writer, Clear(ClearType::UntilNewLine))
}

fn to_crossterm_color(color: Color) -> crossterm::style::Color {
    match color {
        Color::Reset => crossterm::style::Color::Reset,
        Color::Black => crossterm::style::Color::Black,
        Color::Red => crossterm::style::Color::DarkRed,
        Color::Green => crossterm::style::Color::DarkGreen,
        Color::Yellow => crossterm::style::Color::DarkYellow,
        Color::Blue => crossterm::style::Color::DarkBlue,
        Color::Magenta => crossterm::style::Color::DarkMagenta,
        Color::Cyan => crossterm::style::Color::DarkCyan,
        Color::Gray => crossterm::style::Color::Grey,
        Color::DarkGray => crossterm::style::Color::DarkGrey,
        Color::LightRed => crossterm::style::Color::Red,
        Color::LightGreen => crossterm::style::Color::Green,
        Color::LightYellow => crossterm::style::Color::Yellow,
        Color::LightBlue => crossterm::style::Color::Blue,
        Color::LightMagenta => crossterm::style::Color::Magenta,
        Color::LightCyan => crossterm::style::Color::Cyan,
        Color::White => crossterm::style::Color::White,
        Color::Rgb(r, g, b) => crossterm::style::Color::Rgb { r, g, b },
        Color::Indexed(index) => crossterm::style::Color::AnsiValue(index),
    }
}

fn to_crossterm_modifier(modifier: Modifier) -> crossterm::style::Attribute {
    match modifier {
        Modifier::BOLD => crossterm::style::Attribute::Bold,
        Modifier::DIM => crossterm::style::Attribute::Dim,
        Modifier::ITALIC => crossterm::style::Attribute::Italic,
        Modifier::UNDERLINED => crossterm::style::Attribute::Underlined,
        Modifier::REVERSED => crossterm::style::Attribute::Reverse,
        _ => crossterm::style::Attribute::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::custom_terminal::Terminal;
    use crate::test_support::WritableTestBackend;
    use ratatui::layout::Rect;

    #[test]
    fn insertion_from_top_moves_viewport_and_counts_wrapped_rows() {
        let mut terminal =
            Terminal::with_options(WritableTestBackend::new(10, 10)).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 0, 10, 6));

        insert_history_lines(&mut terminal, &[Line::from("abcdefghijk")])
            .expect("history insertion");

        assert_eq!(terminal.viewport_area, Rect::new(0, 2, 10, 6));
        assert_eq!(terminal.visible_history_rows(), 2);
    }

    #[test]
    fn insertion_at_screen_bottom_preserves_viewport_origin() {
        let mut terminal =
            Terminal::with_options(WritableTestBackend::new(10, 10)).expect("terminal");
        terminal.set_viewport_area(Rect::new(0, 4, 10, 6));

        insert_history_lines(&mut terminal, &[Line::from("one")]).expect("history insertion");

        assert_eq!(terminal.viewport_area, Rect::new(0, 4, 10, 6));
        assert_eq!(terminal.visible_history_rows(), 1);
    }

    #[test]
    fn wrapping_keeps_span_colors_in_scrollback_rows() {
        let red = Color::Red;
        let blue = Color::Blue;
        let line = Line::from(vec![
            Span::styled("red", ratatui::style::Style::default().fg(red)),
            Span::styled("blue", ratatui::style::Style::default().fg(blue)),
        ]);

        let rows = wrap_line(&line, 5);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].spans[0].content, "red");
        assert_eq!(rows[0].spans[0].style.fg, Some(red));
        assert_eq!(rows[0].spans[1].content, "bl");
        assert_eq!(rows[0].spans[1].style.fg, Some(blue));
        assert_eq!(rows[1].spans[0].content, "ue");
        assert_eq!(rows[1].spans[0].style.fg, Some(blue));
    }
}
