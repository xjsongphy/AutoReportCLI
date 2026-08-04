//! Bottom-pane render tree adapted from Codex's `bottom_pane/mod.rs`.
//!
//! The application still owns AutoReport's runtime state, but all chat-bottom
//! surfaces now share one renderable boundary. An active view replaces the
//! ordinary status/input pane and therefore receives its own height instead of
//! being painted over the transcript after the main render.

use crate::app::Tui;
use crate::bottom_pane::{ApprovalOverlay, ChatComposer, RequestUserInputOverlay};
use crate::bottom_pane::{CompletionPopup, StatusIndicatorWidget};
use crate::render::renderable::{FlexRenderable, Renderable, RenderableItem};
use crate::style::accent_style;
use autoreport_core::types::{AgentStatus, AgentType};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListState, StatefulWidget, WidgetRef};

/// The single bottom-pane ownership boundary used by the chat render tree.
pub(crate) struct BottomPane<'a> {
    app: &'a Tui,
}

impl<'a> BottomPane<'a> {
    pub(crate) fn new(app: &'a Tui) -> Self {
        Self { app }
    }

    fn active_view_height(&self, width: u16) -> Option<u16> {
        if self.app.agent_picker.is_some() {
            Some(AgentPickerRenderable::new(self.app).desired_height(width))
        } else if !self.app.pending_approvals.is_empty() {
            Some(ApprovalOverlay::new(&self.app.pending_approvals).desired_height(width))
        } else if !self.app.pending_user_inputs.is_empty() {
            Some(RequestUserInputOverlay::new(&self.app.pending_user_inputs).desired_height(width))
        } else {
            None
        }
    }

    fn normal_renderable(&self, width: u16) -> RenderableItem<'a> {
        let status = self
            .app
            .statuses
            .get(&self.app.focused)
            .copied()
            .unwrap_or(AgentStatus::Idle);
        let (details, inline_message) = self.app.active_status_context();
        let status_widget = StatusIndicatorWidget::new(
            status,
            self.app.status_since.get(&self.app.focused).copied(),
        )
        .with_frame_requester(self.app.frame_requester.clone())
        .with_details(details, inline_message);
        let has_pending_input = self.app.pending_input_preview.desired_height(width) > 0;
        let has_status = !matches!(status, AgentStatus::Idle);

        let mut status_and_previews = FlexRenderable::new();
        status_and_previews.push(0, RenderableItem::Owned(Box::new(status_widget)));
        if has_pending_input && has_status {
            status_and_previews.push(0, RenderableItem::Owned("".into()));
        }
        status_and_previews.push(1, RenderableItem::Borrowed(&self.app.pending_input_preview));
        if !has_pending_input && has_status {
            status_and_previews.push(0, RenderableItem::Owned("".into()));
        }

        let mut pane = FlexRenderable::new();
        pane.push(1, RenderableItem::Owned(status_and_previews.into()));
        if let Some(popup) = self.app.completion_popup_build() {
            pane.push(
                0,
                RenderableItem::Owned(Box::new(ComposerPopupRenderable {
                    composer: &self.app.composer,
                    popup,
                })),
            );
        } else {
            pane.push(0, RenderableItem::Borrowed(&self.app.composer));
        }
        RenderableItem::Owned(Box::new(pane))
    }

    pub(crate) fn as_renderable(&self, width: u16) -> RenderableItem<'a> {
        if self.app.agent_picker.is_some() {
            RenderableItem::Owned(Box::new(AgentPickerRenderable::new(self.app)))
        } else if !self.app.pending_approvals.is_empty() {
            RenderableItem::Owned(Box::new(ApprovalOverlay::new(&self.app.pending_approvals)))
        } else if !self.app.pending_user_inputs.is_empty() {
            RenderableItem::Owned(Box::new(RequestUserInputOverlay::new(
                &self.app.pending_user_inputs,
            )))
        } else {
            self.normal_renderable(width)
        }
    }

    fn renderable_for_width(&self, width: u16) -> RenderableItem<'a> {
        self.as_renderable(width)
    }
}

impl Renderable for BottomPane<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.renderable_for_width(area.width).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.active_view_height(width)
            .unwrap_or_else(|| self.normal_renderable(width).desired_height(width))
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        self.renderable_for_width(area.width).cursor_pos(area)
    }

    fn cursor_style(&self, area: Rect) -> crossterm::cursor::SetCursorStyle {
        self.renderable_for_width(area.width).cursor_style(area)
    }
}

/// The active completion menu owns the composer's former footer slot.
pub(crate) struct ComposerPopupRenderable<'a> {
    pub(crate) composer: &'a ChatComposer,
    pub(crate) popup: CompletionPopup,
}

impl Renderable for ComposerPopupRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }
        let popup_height = self.popup.desired_height(area.width).min(area.height);
        let input_height = area.height.saturating_sub(popup_height);
        let input_rect = Rect::new(area.x, area.y, area.width, input_height);
        let popup_rect = Rect::new(
            area.x,
            area.y.saturating_add(input_height),
            area.width,
            popup_height,
        );
        self.composer.render_input_only(input_rect, buf);
        self.popup.render(popup_rect, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.composer
            .input_desired_height(width)
            .saturating_add(self.popup.desired_height(width))
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let popup_height = self.popup.desired_height(area.width).min(area.height);
        let input_height = area.height.saturating_sub(popup_height);
        self.composer
            .cursor_pos_in(Rect::new(area.x, area.y, area.width, input_height))
    }
}

/// Codex-style fixed-roster `/agent` selection view. It is a bottom-pane view,
/// not a full-frame overlay, so transcript rows remain outside its area.
struct AgentPickerRenderable<'a> {
    app: &'a Tui,
}

impl<'a> AgentPickerRenderable<'a> {
    fn new(app: &'a Tui) -> Self {
        Self { app }
    }

    fn roster_height() -> u16 {
        AgentType::ALL.len() as u16 + 4
    }
}

impl Renderable for AgentPickerRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let Some(picker) = self.app.agent_picker.as_ref() else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let height = Self::roster_height().min(area.height);
        let width = 64u16.min(area.width);
        let popup_area = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        Clear.render_ref(popup_area, buf);
        let items: Vec<Line<'static>> = AgentType::ALL
            .iter()
            .enumerate()
            .map(|(index, agent)| {
                let status = self
                    .app
                    .statuses
                    .get(agent)
                    .copied()
                    .unwrap_or(AgentStatus::Idle);
                let active = matches!(
                    status,
                    AgentStatus::Thinking
                        | AgentStatus::RunningTool
                        | AgentStatus::Queued
                        | AgentStatus::DebugMode
                );
                let mut spans = vec![Span::raw(format!("{}. ", index + 1))];
                spans.extend(crate::multi_agents::agent_picker_status_dot_spans(active));
                spans.push(Span::raw(
                    crate::multi_agents::format_agent_picker_item_name(*agent),
                ));
                spans.push(Span::raw(format!("  [{}]", status_label(status))));
                Line::from(spans)
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                " Agents · independent histories ",
                accent_style(),
            ))
            .title_bottom(
                Line::from(format!(" {}", crate::multi_agents::picker_subtitle()))
                    .style(Style::default().add_modifier(Modifier::DIM))
                    .alignment(Alignment::Left),
            );
        let list = List::new(items)
            .block(block)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));
        let mut state = ListState::default();
        state.select(Some(picker.selected));
        list.render(popup_area, buf, &mut state);
    }

    fn desired_height(&self, _width: u16) -> u16 {
        Self::roster_height()
    }
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Thinking => "thinking",
        AgentStatus::RunningTool => "running tool",
        AgentStatus::Queued => "queued",
        AgentStatus::Error => "error",
        AgentStatus::DebugMode => "debug",
    }
}
