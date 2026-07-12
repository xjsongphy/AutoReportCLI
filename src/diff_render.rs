//! Unified-diff rendering for file-mutating tool calls, modelled on codex's
//! `diff_render.rs`: each changed line shown with a `+` / `-` / ` ` gutter
//! sign and GitHub-ish green/red coloring. Used by the TUI to render
//! `apply_patch` / `edit_file` / `write_file` results instead of a raw JSON
//! blob.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

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

/// Render a unified-diff string into styled ratatui lines (codex-style gutter).
pub fn render(diff: &str) -> Vec<Line<'static>> {
    use ratatui::style::Modifier;
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
        let (sign, body) = match raw.chars().next() {
            Some('+') => ('+', &raw[1..]),
            Some('-') => ('-', &raw[1..]),
            Some(' ') => (' ', &raw[1..]),
            _ => (' ', raw),
        };
        let (glyph, color) = match sign {
            '+' => ("+ ", Color::Green),
            '-' => ("- ", Color::Red),
            _ => ("  ", Color::Gray),
        };
        out.push(Line::from(vec![
            Span::styled(glyph.to_string(), Style::default().fg(color)),
            Span::styled(body.to_string(), Style::default().fg(color)),
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
