//! Unified-diff rendering for file-mutating tool calls, modelled on codex's
//! `diff_render.rs`: each changed line shown with a `+` / `-` / ` ` gutter
//! sign and GitHub-ish green/red coloring, with codex's theme-aware (dark/light)
//! tinted line backgrounds when the terminal advertises enough color depth.
//! Used by the TUI to render `apply_patch` / `edit_file` / `write_file` results
//! instead of a raw JSON blob.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

// -- Diff background palette (ported verbatim from codex `diff_render.rs`) -----
//
// Dark-theme tints are subtle enough to avoid clashing with syntax colors.
// Light-theme values match GitHub's diff colors for familiarity.
const DARK_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (33, 58, 43); // #213A2B
const DARK_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (74, 34, 29); // #4A221D
const LIGHT_TC_ADD_LINE_BG_RGB: (u8, u8, u8) = (218, 251, 225); // #dafbe1
const LIGHT_TC_DEL_LINE_BG_RGB: (u8, u8, u8) = (255, 235, 233); // #ffebe9

/// Diff theme, derived from the terminal's queried background color (codex
/// `DiffTheme`). Defaults to dark when the background cannot be determined.
fn diff_theme() -> bool {
    // true => light theme. Mirrors codex's `diff_theme` / `is_light`.
    crate::terminal_palette::default_bg()
        .map(crate::color::is_light)
        .unwrap_or(false)
}

/// Resolve the add/delete line background color for the current theme, mapped
/// to the terminal's advertised color depth via `terminal_palette::best_color`
/// (codex resolves the same RGBs through `RichDiffColorLevel`). Returns `None`
/// when color depth is too low (ANSI-16) for a tinted background — callers then
/// fall back to foreground-only styling, exactly as codex does.
fn line_bg(theme_light: bool, add: bool) -> Option<Color> {
    let rgb = if theme_light {
        if add {
            LIGHT_TC_ADD_LINE_BG_RGB
        } else {
            LIGHT_TC_DEL_LINE_BG_RGB
        }
    } else if add {
        DARK_TC_ADD_LINE_BG_RGB
    } else {
        DARK_TC_DEL_LINE_BG_RGB
    };
    // best_color downgrades to the terminal's real depth (truecolor → 256 → 16).
    // On ANSI-16 the result is a saturated palette entry that overpowers text,
    // so skip the background there (codex's `RichDiffColorLevel` does the same).
    match crate::terminal_palette::stdout_color_level() {
        crate::terminal_palette::StdoutColorLevel::TrueColor
        | crate::terminal_palette::StdoutColorLevel::Ansi256 => {
            Some(crate::terminal_palette::best_color(rgb))
        }
        crate::terminal_palette::StdoutColorLevel::Ansi16
        | crate::terminal_palette::StdoutColorLevel::Unknown => None,
    }
}

/// Compute a unified diff between `old` and `new` (None ⇒ file did not exist),
/// returned as a string of `+` / `-` / ` ` gutter-prefixed lines. Empty when
/// nothing changed.
pub fn unified_diff(old: Option<&str>, new: &str) -> String {
    let old_text = old.unwrap_or("");
    if old_text == new {
        return String::new();
    }
    let patch = diffy::create_patch(old_text, new);
    diffy::PatchFormatter::new()
        .missing_newline_message(false)
        .fmt_patch(&patch)
        .to_string()
}

/// Render a unified-diff string into styled ratatui lines (codex-style gutter
/// with theme-aware tinted backgrounds).
pub fn render(diff: &str) -> Vec<Line<'static>> {
    use ratatui::style::Modifier;
    let theme_light = diff_theme();
    let mut out = Vec::new();
    for raw in diff.lines() {
        // File/section headers (unified-diff `--- `/`+++ `) render dim, not red.
        if raw.starts_with("--- ") || raw.starts_with("+++ ") {
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }
        // Hunk headers (`@@ ... @@`) render dim like codex.
        if raw.starts_with("@@") {
            out.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )));
            continue;
        }
        let (sign, body) = match raw.chars().next() {
            Some('+') => ('+', &raw[1..]),
            Some('-') => ('-', &raw[1..]),
            Some(' ') => (' ', &raw[1..]),
            _ => (' ', raw),
        };
        let (glyph, fg) = match sign {
            '+' => ("+ ", Color::Green),
            '-' => ("- ", Color::Red),
            _ => ("  ", Color::Gray),
        };
        // Theme-aware tinted background for add/delete lines (codex fidelity).
        let bg = match sign {
            '+' => line_bg(theme_light, true),
            '-' => line_bg(theme_light, false),
            _ => None,
        };
        let mut style = Style::default().fg(fg);
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        out.push(Line::from(vec![
            Span::styled(glyph.to_string(), style),
            Span::styled(body.to_string(), style),
        ]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_shows_insert_and_delete() {
        let d = unified_diff(Some("alpha\nbeta\n"), "alpha\nBETA\n");
        let lines = render(&d);
        let has_plus = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content == "+ ")
                && l.spans.iter().any(|s| s.content.contains("BETA"))
        });
        let has_minus = lines.iter().any(|l| {
            l.spans.iter().any(|s| s.content == "- ")
                && l.spans.iter().any(|s| s.content.contains("beta"))
        });
        assert!(has_plus, "expected a +BETA line: {lines:?}");
        assert!(has_minus, "expected a -beta line: {lines:?}");
    }

    #[test]
    fn no_change_is_empty() {
        assert_eq!(unified_diff(Some("x\n"), "x\n"), "");
        assert_eq!(unified_diff(None, ""), "");
    }
}
