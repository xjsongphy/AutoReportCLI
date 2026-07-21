//! MCP history-cell rendering adapted from Codex's dedicated `mcp.rs`.

use super::*;

pub(crate) fn display(
    agent: &str,
    item: &crate::app_state::ToolEntry,
    width: usize,
) -> Vec<Line<'static>> {
    let mut parts = item.name.splitn(4, "__");
    let _ = parts.next();
    let server = parts.next().unwrap_or("mcp");
    let tool = parts.next().unwrap_or(item.name.as_str());
    let invocation = format!(
        "{server}/{tool}({})",
        tool_arg_summary(&item.name, &item.args)
    );
    let cloned = crate::app_state::ToolEntry {
        name: invocation,
        args: item.args.clone(),
        result: item.result.clone(),
        error: item.error.clone(),
        call_id: item.call_id.clone(),
        started_at: item.started_at,
    };
    let mut lines = super::base::display_generic_tool_call(agent, &cloned, width);
    if let Some(first) = lines.first_mut() {
        first.spans.insert(0, Span::raw("MCP ").dim());
    }
    lines
}
