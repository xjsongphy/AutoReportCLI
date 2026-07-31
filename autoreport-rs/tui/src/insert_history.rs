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
    if lines.is_empty() || terminal.viewport_area.top() == 0 {
        return Ok(());
    }

    let area = terminal.viewport_area;
    let cursor = terminal.last_known_cursor_pos;
    let width = usize::from(area.width.max(1));
    let mut writer = terminal.backend_mut();

    // This is the same scroll-region trick used by Codex's insert_history.rs: the rows above the
    // viewport move upward while the composer viewport remains anchored at the bottom.
    write!(writer, "\x1b[1;{}r", area.top())?;
    queue!(writer, MoveTo(0, area.top().saturating_sub(1)))?;
    let mut first = true;
    for line in lines {
        for visual_line in wrap_line(line, width) {
            if !first {
                queue!(writer, Print("\r\n"))?;
            }
            first = false;
            write_line(&mut writer, &visual_line)?;
        }
    }
    write!(writer, "\x1b[r")?;
    queue!(writer, MoveTo(cursor.x, cursor.y))?;
    std::io::Write::flush(&mut writer)
}

fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    if text.is_empty() {
        return vec![Line::from(Span::styled(String::new(), line.style))];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if !row.is_empty() && row_width + ch_width > width.max(1) {
            rows.push(Line::from(Span::styled(
                std::mem::take(&mut row),
                line.style,
            )));
            row_width = 0;
        }
        row.push(ch);
        row_width += ch_width;
    }
    rows.push(Line::from(Span::styled(row, line.style)));
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
