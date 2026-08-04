//! Exec history-cell rendering adapted from Codex's dedicated `exec.rs`.

use super::*;
use crate::wrapping::adaptive_wrap_line;
use serde_json::Value;

pub(crate) fn display(
    _agent: &str,
    item: &crate::app_state::ToolEntry,
    width: usize,
) -> Vec<Line<'static>> {
    let command = item
        .args
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| tool_arg_summary(&item.name, &item.args));
    let status = match (&item.result, &item.error) {
        (_, Some(_)) => Some(false),
        (Some(result), None) => Some(
            result
                .get("returncode")
                .and_then(Value::as_i64)
                .is_none_or(|code| code == 0),
        ),
        (None, None) => None,
    };
    let bullet = match status {
        Some(true) => "•".green().bold(),
        Some(false) => "•".red().bold(),
        None => super::base::pending_tool_bullet(item),
    };
    let heading = if status.is_some() { "Ran" } else { "Running" };

    // Codex highlights the command with bash syntax coloring rather than
    // dimming it. The first highlighted line folds into the header row; any
    // wrapped remainder gets the `│` continuation gutter (Codex's
    // `EXEC_DISPLAY_LAYOUT.command_continuation`).
    let highlighted = crate::highlight::highlight_bash_to_lines(&command);
    let mut header_line = Line::from(vec![bullet, " ".into(), heading.bold(), " ".into()]);
    let header_prefix_width = header_line.width();

    let mut continuation_lines: Vec<Line<'static>> = Vec::new();
    if let Some((first, rest)) = highlighted.split_first() {
        let available_first_width = width.saturating_sub(header_prefix_width).max(1);
        let mut first_wrapped: Vec<Line<'static>> = Vec::new();
        push_owned_lines(
            &adaptive_wrap_line(first, RtOptions::new(available_first_width)),
            &mut first_wrapped,
        );
        let mut first_wrapped_iter = first_wrapped.into_iter();
        if let Some(first_segment) = first_wrapped_iter.next() {
            for span in first_segment.spans {
                header_line.push_span(span);
            }
        }
        continuation_lines.extend(first_wrapped_iter);
        for line in rest {
            push_owned_lines(
                &adaptive_wrap_line(line, RtOptions::new(width.max(1))),
                &mut continuation_lines,
            );
        }
    }

    let mut lines: Vec<Line<'static>> = vec![header_line];
    if !continuation_lines.is_empty() {
        lines.extend(prefix_lines(
            continuation_lines,
            Span::from("  │ ").dim(),
            Span::from("  │ ").dim(),
        ));
    }

    // Output block. Codex dims stdout AND stderr uniformly (it does not color
    // stderr red) and shows a "(no output)" placeholder for a finished call
    // with no captured output.
    let mut details = Vec::new();
    if let Some(error) = item.error.as_deref() {
        details.extend(error.lines().map(|line| Line::from(line.dim())));
    } else if let Some(result) = item.result.as_ref() {
        if let Some(stdout) = result.get("stdout").and_then(Value::as_str) {
            details.extend(stdout.lines().map(|line| Line::from(line.dim())));
        }
        if let Some(stderr) = result.get("stderr").and_then(Value::as_str) {
            details.extend(stderr.lines().map(|line| Line::from(line.dim())));
        }
    }
    if details.is_empty() && status.is_some() {
        details.push(Line::from("(no output)".dim()));
    }
    if !details.is_empty() {
        let mut wrapped_details = Vec::new();
        for line in details {
            let wrapped = adaptive_wrap_line(&line, RtOptions::new(width.saturating_sub(4).max(1)));
            push_owned_lines(&wrapped, &mut wrapped_details);
        }
        lines.extend(prefix_lines(wrapped_details, "  └ ".dim(), "    ".into()));
    }
    lines
}
