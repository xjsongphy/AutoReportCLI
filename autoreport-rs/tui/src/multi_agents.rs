//! Codex collaborator transcript helpers, adapted from codex-rs/tui/src/multi_agents.rs.

use crate::style::accent_style;
use autoreport_core::types::{AgentType, MessageSource};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

pub(crate) type CollabEvent = (Line<'static>, Vec<Line<'static>>);

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
