//! Stub for codex `color::is_light`.

use ratatui::style::Color;

pub fn is_light(c: Color) -> bool {
    match c {
        Color::Rgb(r, g, b) => (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) > 160.0,
        Color::Black => false,
        Color::White | Color::Gray => true,
        _ => false,
    }
}
