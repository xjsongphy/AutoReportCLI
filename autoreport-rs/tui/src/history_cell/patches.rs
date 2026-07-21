//! Apply-patch history-cell rendering adapted from Codex's dedicated
//! `patches.rs`.

use super::*;
use crate::wrapping::adaptive_wrap_line;

pub(crate) fn display(
    _agent: &str,
    item: &crate::app_state::ToolEntry,
    width: usize,
) -> Vec<Line<'static>> {
    let (bullet, title) = match (&item.result, &item.error) {
        (_, Some(_)) => ("•".red().bold(), "Patch failed"),
        (Some(_), None) => ("•".green().bold(), "Applied patch"),
        (None, None) => (super::base::pending_tool_bullet(item), "Applying patch"),
    };
    let mut lines = vec![vec![bullet, " ".into(), title.bold()].into()];
    let details = render_tool_result_lines(
        &item.name,
        &item.args,
        item.result.as_ref(),
        item.error.as_deref(),
    );
    if !details.is_empty() {
        let mut wrapped = Vec::new();
        for detail in details {
            let line = adaptive_wrap_line(
                &detail,
                RtOptions::new(width.saturating_sub(4).max(1))
                    .initial_indent("".into())
                    .subsequent_indent("    ".into()),
            );
            push_owned_lines(&line, &mut wrapped);
        }
        lines.extend(prefix_lines(wrapped, "  └ ".dim(), "    ".into()));
    }
    lines
}
