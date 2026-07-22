//! Codex collaborator transcript helpers, adapted from codex-rs/tui/src/multi_agents.rs.
//!
//! Two families live here, mirroring codex's `multi_agents.rs`:
//! 1. Collaborator history rows (`interaction_*`, `report_end`, `waiting_begin`,
//!    `report_blocked`, `communication_failed`) — the `• `-prefixed cells.
//! 2. `/agent` picker presentation contracts (`agent_picker_status_dot_spans`,
//!    `format_agent_picker_item_name`) — the row DTO helpers codex's
//!    `ListSelectionView` consumes.

use crate::style::accent_style;
use autoreport_core::types::{AgentType, MessageSource};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

pub(crate) type CollabEvent = (Line<'static>, Vec<Line<'static>>);

/// Status dot for an `/agent` picker row. Direct adaptation of codex's
/// `agent_picker_status_dot_spans(is_closed)`: codex paints a green `•` for a
/// live thread and a default-foreground `•` for a closed one. AutoReport's
/// fixed roster never closes, so the boolean is repurposed to "actively
/// working" (Thinking / RunningTool / Queued / DebugMode) — green when busy,
/// dim when idle.
pub(crate) fn agent_picker_status_dot_spans(is_active: bool) -> Vec<Span<'static>> {
    let dot = if is_active { "•".green() } else { "•".dim() };
    vec![dot, " ".into()]
}

/// Picker row label, adapted from codex's `format_agent_picker_item_name`.
/// AutoReport's agents are a fixed `AgentType` roster with display labels but
/// no nicknames/roles, so the codex fallbacks collapse to the label itself;
/// `Main` keeps codex's primary marker (`Main [default]`).
pub(crate) fn format_agent_picker_item_name(agent: AgentType) -> String {
    if agent == AgentType::Main {
        return "Main [default]".to_string();
    }
    agent.label().to_string()
}

/// Picker subtitle, adapted from codex's `AgentNavigationState::picker_subtitle`.
pub(crate) fn picker_subtitle() -> &'static str {
    "Select an agent to focus. Tab cycles, Alt+Left/Right when the draft is empty."
}

pub(crate) fn interaction_end(target: AgentType, prompt: &str) -> CollabEvent {
    collab_event(
        title_with_agent("Sent input to", target),
        prompt_line(prompt).into_iter().collect(),
    )
}

/// Render either direction of an agent-to-agent prompt using Codex's
/// collaborator title vocabulary. `MainAgent` is the usual dispatch path;
/// `Agent(_)` keeps the display correct if a sub-agent addresses another
/// agent directly.
pub(crate) fn interaction_message(
    source: MessageSource,
    target: AgentType,
    prompt: &str,
) -> CollabEvent {
    match source {
        MessageSource::Agent(sender) => collab_event(
            title_with_agent("Received input from", sender),
            prompt_line(prompt).into_iter().collect(),
        ),
        MessageSource::MainAgent | MessageSource::User | MessageSource::System => {
            interaction_end(target, prompt)
        }
    }
}

pub(crate) fn report_end(source: AgentType, summary: &str, content: &str) -> CollabEvent {
    let details = [summary, content]
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .map(|text| Line::from(text.to_string()))
        .collect();
    collab_event(title_with_agent("Received report from", source), details)
}

pub(crate) fn communication_failed(target: AgentType, error: &str) -> CollabEvent {
    collab_event(
        title_with_agent("Agent communication failed", target),
        vec![Line::from(error.to_string())],
    )
}

/// Main is blocking on a sub-agent's `respond`. Adapted from codex's
/// `waiting_begin` single-receiver title (`Waiting for <agent>`); the matching
/// completion is the sub's `report_end` / `report_blocked` cell.
pub(crate) fn waiting_begin(target: AgentType) -> CollabEvent {
    collab_event(title_with_agent("Waiting for", target), Vec::new())
}

/// A sub-agent's `respond` came back blocked (`missing_data` / `quality`).
/// Mirrors codex's `status_summary_spans` `Interrupted` styling (yellow) so a
/// blocked report reads like codex's non-terminal agent status.
pub(crate) fn report_blocked(
    source: AgentType,
    report_type: &str,
    summary: &str,
    content: &str,
) -> CollabEvent {
    #[allow(clippy::disallowed_methods)]
    let blocking = Span::from(format!(" ({report_type})")).yellow();
    let title = vec![
        Span::from("• ").dim(),
        Span::from("Blocked by ").bold(),
        Span::styled(source.label(), accent_style()),
        blocking,
    ]
    .into();
    let details = [summary, content]
        .into_iter()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| Line::from(text.to_string()))
        .collect::<Vec<_>>();
    collab_event(title, details)
}

fn collab_event(title: Line<'static>, details: Vec<Line<'static>>) -> CollabEvent {
    (title, details)
}

fn title_with_agent(prefix: &str, agent: AgentType) -> Line<'static> {
    vec![
        Span::from("• ").dim(),
        Span::from(format!("{prefix} ")).bold(),
        Span::styled(agent.label(), accent_style()),
    ]
    .into()
}

fn prompt_line(prompt: &str) -> Option<Line<'static>> {
    let trimmed = prompt.trim();
    (!trimmed.is_empty()).then(|| Line::from(trimmed.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::Cell;
    use autoreport_core::types::AgentType;

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn cell_lines(cell: &Cell, width: u16) -> Vec<String> {
        crate::history_cell::HistoryCell::display_lines(cell, width)
            .into_iter()
            .map(|line| line_text(&line))
            .collect()
    }

    #[test]
    fn picker_status_dot_is_a_bullet_followed_by_space() {
        let active = agent_picker_status_dot_spans(true);
        let idle = agent_picker_status_dot_spans(false);
        assert_eq!(line_text(&Line::from(active)), "• ");
        assert_eq!(line_text(&Line::from(idle)), "• ");
    }

    #[test]
    fn picker_name_keeps_codex_main_default_marker() {
        assert_eq!(format_agent_picker_item_name(AgentType::Main), "Main [default]");
        assert_eq!(
            format_agent_picker_item_name(AgentType::DataAnalysis),
            "Data Analysis"
        );
        assert_eq!(format_agent_picker_item_name(AgentType::Plotting), "Plotting");
    }

    #[test]
    fn waiting_begin_uses_codex_waiting_for_title() {
        let (title, details) = waiting_begin(AgentType::Theory);
        assert_eq!(line_text(&title), "• Waiting for Theory");
        assert!(details.is_empty());
    }

    #[test]
    fn blocked_report_renders_codex_style_title_and_tree() {
        let (title, details) =
            report_blocked(AgentType::Theory, "missing_data", "need dataset", "/Data/Raw");
        assert_eq!(line_text(&title), "• Blocked by Theory (missing_data)");
        assert_eq!(
            details.iter().map(|d| line_text(d)).collect::<Vec<_>>(),
            vec!["need dataset", "/Data/Raw"]
        );

        // The reducer pushes the event through `Cell::Collab`, which applies
        // codex's `  └ ` detail tree prefix.
        let cell = Cell::Collab {
            agent: AgentType::Theory,
            title,
            details,
        };
        let rendered = cell_lines(&cell, 80);
        assert_eq!(rendered[0], "• Blocked by Theory (missing_data)");
        assert_eq!(rendered[1], "  └ need dataset");
        assert_eq!(rendered[2], "    /Data/Raw");
    }

    #[test]
    fn blocked_report_omits_empty_detail_segments() {
        let (title, details) = report_blocked(AgentType::Plotting, "quality", "  ", "");
        assert!(details.is_empty());
        assert_eq!(line_text(&title), "• Blocked by Plotting (quality)");
    }
}
