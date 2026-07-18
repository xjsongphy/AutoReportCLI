//! Active-turn status row migrated from Codex's `status_indicator_widget.rs`.

use autoreport_core::types::AgentStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::WidgetRef;

use crate::render::renderable::Renderable;

pub(crate) struct StatusIndicatorWidget {
    status: AgentStatus,
}

impl StatusIndicatorWidget {
    pub(crate) fn new(status: AgentStatus) -> Self {
        Self { status }
    }

    fn active(&self) -> bool {
        matches!(
            self.status,
            AgentStatus::Thinking | AgentStatus::RunningTool | AgentStatus::DebugMode
        )
    }

    fn status_text(&self) -> &'static str {
        match self.status {
            AgentStatus::Thinking => "Working",
            AgentStatus::RunningTool => "Running tool",
            AgentStatus::DebugMode => "Debugging",
            AgentStatus::Queued => "Queued",
            AgentStatus::Error => "Error",
            AgentStatus::Idle => "",
        }
    }
}

impl Renderable for StatusIndicatorWidget {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.active() || area.is_empty() {
            return;
        }
        Line::from(vec![
            Span::raw("  "),
            Span::styled("⠋ ", Style::default().fg(Color::Yellow)),
            Span::styled(self.status_text(), Style::default().bold()),
            Span::raw("  "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::styled(" to interrupt", Style::default().dim()),
        ])
        .render_ref(area, buf);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        u16::from(self.active())
    }
}
