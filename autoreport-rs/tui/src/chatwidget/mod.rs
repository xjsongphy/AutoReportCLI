//! Pure application helpers shared by the app event and chat widgets.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;
use std::path::Path;

pub(crate) fn render_user_text(text: &str) -> Vec<Line<'static>> {
    // Render the user's text with @mentions highlighted.
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let chars: Vec<char> = paragraph.chars().collect();
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut buf = String::new();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '@' && i + 1 < chars.len() && is_mention_char(chars[i + 1]) {
                if !buf.is_empty() {
                    spans.push(Span::raw(std::mem::take(&mut buf)));
                }
                let mut run = String::from("@");
                i += 1;
                while i < chars.len() && is_mention_char(chars[i]) {
                    run.push(chars[i]);
                    i += 1;
                }
                spans.push(Span::styled(run, Style::default().fg(Color::Cyan)));
            } else {
                buf.push(chars[i]);
                i += 1;
            }
        }
        if !buf.is_empty() {
            spans.push(Span::raw(buf));
        }
        out.push(Line::from(spans));
    }
    out
}

pub(crate) fn is_mention_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-')
}

/// Read up to `cap` bytes from `path` as a string. Bounded so a `@mention` of
/// a huge file never loads it whole (and never blocks the TUI event loop on a
/// multi-megabyte read). Uses `from_utf8_lossy` so a truncation that splits a
/// multi-byte char cannot panic (the previous `&content[..cap]` sliced at an
/// arbitrary byte offset and panicked on CJK/emoji straddling the boundary).
pub(crate) fn read_capped(path: &Path, cap: usize) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    // Read at most cap+1 bytes: the +1 lets us detect truncation without a
    // second stat/read call.
    let mut buf = vec![0u8; cap + 1];
    let n = file.read(&mut buf).ok()?;
    let truncated = n > cap;
    buf.truncate(n.min(cap));
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("\n…(truncated)");
    }
    Some(s)
}

/// Pull `@rel/path` tokens out of arbitrary text (for expansion).
///
/// File completions containing whitespace are written as `@"rel/path with
/// spaces"`, matching Codex's convention of quoting whitespace-containing
/// file selections as one unit.
pub(crate) fn extract_mentions(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' && i + 1 < chars.len() && (i == 0 || chars[i - 1].is_whitespace()) {
            if chars[i + 1] == '"' {
                let mut run = String::new();
                let mut j = i + 2;
                while j < chars.len() && chars[j] != '"' {
                    run.push(chars[j]);
                    j += 1;
                }
                if j < chars.len() && !run.is_empty() {
                    out.push(run);
                    i = j + 1;
                    continue;
                }
            }
            if !is_mention_char(chars[i + 1]) {
                i += 1;
                continue;
            }
            let mut run = String::new();
            let mut j = i + 1;
            while j < chars.len() && is_mention_char(chars[j]) {
                run.push(chars[j]);
                j += 1;
            }
            if !run.is_empty() {
                out.push(run);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

pub(crate) fn tool_arg_summary(name: &str, args: &Value) -> String {
    match name {
        "send_to_agent" => {
            let agent = args
                .get("agent_type")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let summary = args
                .get("summary")
                .or_else(|| args.get("brief"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let blocking = args
                .get("blocking")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            format!("{agent}, summary={summary:?}, blocking={blocking}")
        }
        "respond" => {
            let task_id = args.get("task_id").and_then(|v| v.as_str()).unwrap_or("?");
            let kind = args.get("type").and_then(|v| v.as_str()).unwrap_or("?");
            let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            format!("task_id={task_id}, type={kind}, summary={summary:?}")
        }
        "manifest" => {
            let action = args
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or("read");
            let agent = args.get("agent").and_then(|v| v.as_str()).unwrap_or("self");
            format!("action={action}, agent={agent}")
        }
        _ => serde_json::to_string(args).unwrap_or_default(),
    }
}

pub(crate) fn render_tool_result_lines(
    name: &str,
    args: &Value,
    result: Option<&Value>,
    error: Option<&str>,
) -> Vec<Line<'static>> {
    if let Some(err) = error {
        return err
            .lines()
            .map(|l| {
                // Codex dims tool errors (it does not color them red); match
                // that and our own exec cell, which dims stderr.
                Line::from(Span::styled(
                    format!("Error: {l}"),
                    Style::default().add_modifier(Modifier::DIM),
                ))
            })
            .collect();
    }

    if result.is_some() {
        if let Some(lines) = render_file_change_lines(name, args) {
            return lines;
        }
    }

    let Some(res) = result else {
        return Vec::new();
    };
    truncate(&pretty(res), 400)
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ))
        })
        .collect()
}

pub(crate) fn render_file_change_lines(name: &str, args: &Value) -> Option<Vec<Line<'static>>> {
    let raw = match name {
        "apply_patch" => {
            // Codex-style `*** Begin Patch` grammar: keep the +/-/space diff
            // lines and turn file markers into header lines; drop the noise
            // (`@@`, `*** End File`, `*** Move to`, Begin/End Patch) so it
            // renders like codex's diff view instead of leaking raw directives.
            let patch = args.get("patch")?.as_str()?;
            let mut filtered = String::new();
            for line in patch.lines() {
                let t = line.trim_start();
                if t == "*** Begin Patch"
                    || t == "*** End Patch"
                    || t == "*** End File"
                    || t == "@@"
                    || t.starts_with("@@ ")
                    || t.starts_with("*** Move to")
                {
                    continue;
                }
                if let Some(rest) = t
                    .strip_prefix("*** Update File: ")
                    .or_else(|| t.strip_prefix("*** Add File: "))
                    .or_else(|| t.strip_prefix("*** Delete File: "))
                {
                    filtered.push_str(&format!("--- {rest}\n"));
                    continue;
                }
                filtered.push_str(line);
                filtered.push('\n');
            }
            filtered
        }
        "edit_file" => {
            let old = args.get("old_text")?.as_str()?;
            let new = args.get("new_text")?.as_str()?;
            crate::diff_render::unified_diff(Some(old), new)
        }
        "write_file" => {
            let content = args.get("content")?.as_str()?;
            crate::diff_render::unified_diff(None, content)
        }
        _ => return None,
    };
    if raw.trim().is_empty() {
        return None;
    }
    Some(crate::diff_render::render(&raw).into_iter().collect())
}

pub(crate) fn pretty(v: &serde_json::Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

mod rendering;
