//! Stub for codex `terminal_palette`. We don't detect the terminal background,
//! so report "unknown" and let the highlighter pick a default theme.

use ratatui::style::Color;

pub fn default_bg() -> Option<Color> {
    None
}
