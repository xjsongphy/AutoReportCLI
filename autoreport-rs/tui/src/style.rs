//! Small subset of Codex's shared TUI surface styles used by the bottom pane.

use crate::color::{blend, is_light};
use crate::terminal_palette::default_bg;
use ratatui::style::{Color, Style};

/// Codex's low-contrast background for the composer and completion surfaces.
pub(crate) fn user_message_style() -> Style {
    let Some(background) = default_bg() else {
        return Style::default();
    };
    let (foreground, alpha) = if is_light(background) {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    let (r, g, b) = blend(foreground, background, alpha);
    Style::default().bg(Color::Rgb(r, g, b))
}
