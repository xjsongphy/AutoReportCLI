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
    let invocation = Line::from(vec![
        bullet,
        " ".into(),
        heading.bold(),
        " ".into(),
        Span::from(command).dim(),
    ]);
    let mut lines = Vec::new();
    let wrapped = adaptive_wrap_line(
        &invocation,
        RtOptions::new(width.max(1))
            .initial_indent("".into())
            .subsequent_indent("    ".into()),
    );
    push_owned_lines(&wrapped, &mut lines);

    let mut details = Vec::new();
    if let Some(error) = item.error.as_deref() {
        details.extend(error.lines().map(|line| Line::from(line.red())));
    } else if let Some(result) = item.result.as_ref() {
        if let Some(stdout) = result.get("stdout").and_then(Value::as_str) {
            details.extend(stdout.lines().map(|line| Line::from(line.dim())));
        }
        if let Some(stderr) = result.get("stderr").and_then(Value::as_str) {
            details.extend(stderr.lines().map(|line| Line::from(line.red())));
        }
    }
    if !details.is_empty() {
        let mut wrapped_details = Vec::new();
        for line in details {
            let wrapped = adaptive_wrap_line(
                &line,
                RtOptions::new(width.saturating_sub(4).max(1))
                    .initial_indent("".into())
                    .subsequent_indent("    ".into()),
            );
            push_owned_lines(&wrapped, &mut wrapped_details);
        }
        lines.extend(prefix_lines(wrapped_details, "  └ ".dim(), "    ".into()));
    }
    lines
}
