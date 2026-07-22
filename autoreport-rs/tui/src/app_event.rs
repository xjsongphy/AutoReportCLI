//! Bus event reduction for the terminal application.

use crate::app::Tui;
use crate::app_state::{Cell, Overlay, PendingApproval, PendingUserInput, SysKind, ToolEntry};
use crate::config_update::Outcome;
use autoreport_core::config::save_settings;
use autoreport_core::types::{BusMessage, MessageSource};
use autoreport_core::types::ApprovalRequestPayload;
use serde_json::Value;
use std::collections::HashMap;

impl Tui {
    /// Push one approval request onto the shared display queue.
    pub(crate) fn push_pending_approval(&mut self, payload: ApprovalRequestPayload) {
        self.pending_approvals.push_back(PendingApproval {
            agent: payload.agent_type,
            call_id: payload.call_id,
            command: payload.command,
            cwd: payload.cwd,
            summary: payload.summary,
            reason: payload.reason,
        });
    }

    /// Rebuild the display queue from the bus's non-lossy source of truth.
    /// Called at startup and after any broadcast `Lagged(_)` so a request
    /// dropped by a lagging (or late-subscribing) receiver is recovered instead
    /// of deadlocking the awaiting agent. Already-queued items keep their
    /// position; resolved-elsewhere items are dropped; newly-seen ones append.
    pub(crate) async fn reconcile_approvals(&mut self) {
        let snapshot = self.bus.pending_approvals().await;
        let pending_ids: std::collections::HashSet<&str> =
            snapshot.iter().map(|p| p.call_id.as_str()).collect();
        self.pending_approvals
            .retain(|p| pending_ids.contains(p.call_id.as_str()));
        for payload in snapshot {
            if !self
                .pending_approvals
                .iter()
                .any(|p| p.call_id == payload.call_id)
            {
                self.push_pending_approval(payload);
            }
        }
    }

    fn set_status_from_bus(
        &mut self,
        agent_type: autoreport_core::types::AgentType,
        status: autoreport_core::types::AgentStatus,
    ) {
        let active = matches!(
            status,
            autoreport_core::types::AgentStatus::Thinking
                | autoreport_core::types::AgentStatus::RunningTool
                | autoreport_core::types::AgentStatus::Queued
                | autoreport_core::types::AgentStatus::DebugMode
        );
        let was_active = self.status_since.contains_key(&agent_type);
        if active && !was_active {
            self.status_since
                .insert(agent_type, std::time::Instant::now());
        } else if !active {
            self.status_since.remove(&agent_type);
        }
        self.statuses.insert(agent_type, status);
        if !active && was_active {
            self.finish_pending_submission(agent_type);
            self.suppress_until_idle.remove(&agent_type);
            self.drain_queued_input(agent_type);
        }
    }

    pub(crate) fn apply_bus(&mut self, msg: BusMessage) {
        // A retracted pre-tool turn may still have one late provider chunk in
        // flight. Consume it at the reducer boundary until Idle, just as
        // Codex clears its active stream tail on interrupt.
        if let Some(agent) = msg.agent_type()
            && self.suppress_until_idle.contains(&agent)
            && !matches!(&msg, BusMessage::StatusChange { .. })
        {
            return;
        }
        match msg {
            BusMessage::UserMessage {
                content,
                agent_type,
                source,
                ..
            } => {
                // Main↔sub-agent prompts are rendered with Codex's
                // `multi_agents::interaction_end` shape. Direct user input is
                // already inserted by the composer and must not be duplicated.
                if !matches!(source, MessageSource::User | MessageSource::System) {
                    let (title, details) =
                        crate::multi_agents::interaction_message(source, agent_type, &content);
                    self.history.push(Cell::Collab {
                        agent: agent_type,
                        title,
                        details,
                    });
                }
            }
            BusMessage::AgentResponse {
                agent_type,
                content,
                streaming,
            } => {
                if !content.is_empty() {
                    self.set_status_from_bus(
                        agent_type,
                        autoreport_core::types::AgentStatus::Thinking,
                    );
                    if streaming {
                        // Codex keeps one mutable AgentMessageCell for the
                        // active stream. Merge deltas in place so wrapping,
                        // markdown fences and continuation indentation are
                        // computed from the complete source rather than from
                        // arbitrary provider chunk boundaries.
                        if let Some(Cell::AgentMessage {
                            agent: owner,
                            text: current,
                            ..
                        }) = self.history.last_mut()
                            && *owner == agent_type
                        {
                            current.push_str(&content);
                        } else {
                            self.history.push(Cell::AgentMessage {
                                agent: agent_type,
                                text: content,
                                is_first_line: true,
                            });
                        }
                    } else {
                        self.history.push(Cell::AgentMarkdown {
                            agent: agent_type,
                            text: content,
                        });
                    }
                }
                if !streaming {
                    // Codex emits `FinalMessageSeparator` after a completed
                    // turn. The local bus does not carry protocol duration,
                    // so use the same monotonic turn clock already powering
                    // the live status row.
                    let elapsed_seconds = self
                        .status_since
                        .get(&agent_type)
                        .map(|started| started.elapsed().as_secs());
                    self.consolidate_agent_message(agent_type);
                    // Consolidate before transitioning to Idle: the Idle
                    // transition drains Codex-style queued follow-ups and
                    // appends their user row, which would otherwise hide the
                    // active AgentMessage from the contiguous-stream merge.
                    if elapsed_seconds.is_some() {
                        self.history.push(Cell::TurnSeparator {
                            agent: agent_type,
                            elapsed_seconds,
                            // Filled retroactively when the Idle StatusChange
                            // arrives with per-turn runtime_metrics.
                            runtime_metrics: None,
                        });
                    }
                    self.set_status_from_bus(agent_type, autoreport_core::types::AgentStatus::Idle);
                }
            }
            BusMessage::AgentReasoning {
                agent_type,
                streaming,
                ..
            } => {
                if streaming {
                    self.set_status_from_bus(
                        agent_type,
                        autoreport_core::types::AgentStatus::Thinking,
                    );
                }
            }
            BusMessage::ToolCall {
                agent_type,
                tool_name,
                arguments,
                call_id,
            } => {
                self.mark_pending_tool_started(agent_type);
                // Delegation has its own Codex collaborator row, emitted from
                // UserMessage/Report. Do not also expose the plumbing tool.
                // Codex renders update_plan as its dedicated PlanUpdateCell,
                // and delegation has its own collaborator row. Neither
                // plumbing call should leak into the generic tool history.
                if matches!(
                    tool_name.as_str(),
                    "send_to_agent" | "respond" | "update_plan" | "request_user_input"
                ) {
                    return;
                }
                self.set_status_from_bus(
                    agent_type,
                    autoreport_core::types::AgentStatus::RunningTool,
                );
                let item = ToolEntry {
                    name: tool_name,
                    args: arguments,
                    result: None,
                    error: None,
                    call_id: Some(call_id),
                    started_at: Some(std::time::Instant::now()),
                };
                // Codex keeps a contiguous batch of tool calls in one history
                // cell (parallel Responses calls therefore render as one
                // grouped interaction). Results still correlate by call_id,
                // so completion order can differ from invocation order.
                if let Some(Cell::ToolGroup { agent, items }) = self.history.last_mut()
                    && *agent == agent_type
                {
                    items.push(item);
                } else {
                    self.history.push(Cell::ToolGroup {
                        agent: agent_type,
                        items: vec![item],
                    });
                }
            }
            BusMessage::ToolResult {
                agent_type,
                tool_name,
                result,
                error,
                call_id,
            } => {
                if matches!(
                    tool_name.as_str(),
                    "send_to_agent" | "respond" | "update_plan"
                ) {
                    if let Some(error) = error {
                        let (title, details) =
                            crate::multi_agents::communication_failed(agent_type, &error);
                        self.history.push(Cell::Collab {
                            agent: agent_type,
                            title,
                            details,
                        });
                    }
                    return;
                }
                if tool_name == "request_user_input" {
                    self.set_status_from_bus(
                        agent_type,
                        autoreport_core::types::AgentStatus::Thinking,
                    );
                    if let Some((owner, questions)) = self.user_input_requests.remove(&call_id) {
                        let answers = result
                            .get("answers")
                            .cloned()
                            .and_then(|value| serde_json::from_value(value).ok())
                            .unwrap_or_else(HashMap::new);
                        self.history.push(Cell::UserInputResult {
                            agent: owner,
                            questions,
                            answers,
                            interrupted: error.is_some(),
                        });
                    }
                    return;
                }
                self.set_status_from_bus(agent_type, autoreport_core::types::AgentStatus::Thinking);
                // Correlate by provider call_id first (the same tool can be
                // invoked more than once in flight; matching by name alone would
                // attach a result to the wrong call when results arrive out of
                // order). Fall back to name + "no result yet" for older entries
                // that lack a call_id (e.g. replayed history).
                let mut matched = false;
                for cell in self.history.iter_mut().rev() {
                    if let Cell::ToolGroup { agent, items } = cell {
                        if *agent == agent_type {
                            if let Some(item) = items.iter_mut().rev().find(|item| {
                                item.result.is_none()
                                    && item
                                        .call_id
                                        .as_deref()
                                        .map_or(item.name == tool_name, |id| id == call_id)
                            }) {
                                item.result = Some(result.clone());
                                item.error = error.clone();
                                matched = true;
                                break;
                            }
                        }
                    }
                }
                if !matched {
                    self.history.push(Cell::ToolGroup {
                        agent: agent_type,
                        items: vec![ToolEntry {
                            name: tool_name,
                            args: Value::Null,
                            result: Some(result),
                            error,
                            call_id: Some(call_id),
                            started_at: None,
                        }],
                    });
                }
            }
            BusMessage::StatusChange {
                agent_type,
                status,
                runtime_metrics,
            } => {
                if !matches!(
                    status,
                    autoreport_core::types::AgentStatus::Thinking
                        | autoreport_core::types::AgentStatus::RunningTool
                        | autoreport_core::types::AgentStatus::Queued
                        | autoreport_core::types::AgentStatus::DebugMode
                ) {
                    self.suppress_until_idle.remove(&agent_type);
                }
                self.set_status_from_bus(agent_type, status);
                // The TurnSeparator is emitted on the final AgentResponse; the
                // per-turn metrics arrive on this Idle transition, so attach
                // them to that agent's most recent separator retroactively.
                if matches!(status, autoreport_core::types::AgentStatus::Idle)
                    && let Some(metrics) = runtime_metrics
                {
                    for cell in self.history.iter_mut().rev() {
                        if let Cell::TurnSeparator {
                            agent,
                            runtime_metrics: slot,
                            ..
                        } = cell
                            && *agent == agent_type
                            && slot.is_none()
                        {
                            *slot = Some(metrics);
                            break;
                        }
                    }
                }
            }
            BusMessage::TaskUpdate { .. } => {
                // Codex's interaction history is emitted from the actual
                // collaborator tool/user event. Task board transitions are
                // bookkeeping and should not duplicate that row.
            }
            BusMessage::PlanUpdate {
                agent_type,
                explanation,
                steps,
            } => {
                self.history.push(Cell::PlanUpdate {
                    agent: agent_type,
                    explanation,
                    steps,
                });
            }
            BusMessage::Report {
                agent_type,
                report_type,
                summary,
                content,
                ..
            } => {
                // Codex renders a collaborator's terminal status (Completed /
                // Interrupted / Errored) inline. A `reply` is the clean
                // `Received report from` row; `missing_data` / `quality` are
                // non-terminal blocks rendered with codex's Interrupted tone.
                let (title, details) = if report_type == "reply" {
                    crate::multi_agents::report_end(agent_type, &summary, &content)
                } else {
                    crate::multi_agents::report_blocked(
                        agent_type,
                        &report_type,
                        &summary,
                        &content,
                    )
                };
                self.history.push(Cell::Collab {
                    agent: agent_type,
                    title,
                    details,
                });
            }
            BusMessage::Waiting { target_agent, .. } => {
                let (title, details) = crate::multi_agents::waiting_begin(target_agent);
                self.history.push(Cell::Collab {
                    agent: target_agent,
                    title,
                    details,
                });
            }
            BusMessage::SystemNotice {
                agent_type,
                content,
            } => {
                let label = agent_type
                    .map(|a| a.label().to_string())
                    .unwrap_or_else(|| "system".into());
                self.system(&format!("{label}: {content}"), SysKind::Info);
            }
            BusMessage::Error { message, .. } => {
                self.system(&format!("error: {message}"), SysKind::Error);
            }
            BusMessage::ApprovalRequest { payload } => {
                // Single shared approval queue: any agent's request lands here
                // regardless of which agent is focused. Ported from codex's
                // `ApprovalOverlay::enqueue_request`. (If the broadcast
                // receiver lagged and dropped this, `reconcile_approvals`
                // re-surfaces it from the bus's non-lossy source of truth.)
                self.push_pending_approval(payload);
            }
            BusMessage::UserInputRequest {
                agent_type,
                call_id,
                questions,
                auto_resolution_ms,
            } => {
                self.user_input_requests
                    .insert(call_id.clone(), (agent_type, questions.clone()));
                self.pending_user_inputs.push_back(PendingUserInput::new(
                    agent_type,
                    call_id,
                    questions,
                    auto_resolution_ms,
                ));
                if let (Some(ms), Some(requester)) =
                    (auto_resolution_ms, self.frame_requester.as_ref())
                {
                    requester.schedule_frame_in(std::time::Duration::from_millis(ms));
                }
            }
        }
    }

    /// Replace the contiguous streamed `AgentMessage` run with a single
    /// source-backed markdown cell, matching Codex's
    /// `AgentMessageCell -> AgentMarkdownCell` consolidation step.
    fn consolidate_agent_message(&mut self, agent: autoreport_core::types::AgentType) {
        let end = self.history.len();
        let mut start = end;
        while start > 0
            && matches!(
                self.history.get(start - 1),
                Some(Cell::AgentMessage { agent: owner, .. }) if *owner == agent
            )
        {
            start -= 1;
        }
        if start == end {
            return;
        }
        let text = self.history[start..end]
            .iter()
            .filter_map(|cell| match cell {
                Cell::AgentMessage { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        self.history
            .splice(start..end, [Cell::AgentMarkdown { agent, text }]);
    }
}

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use std::time::Instant;

impl Tui {
    pub(crate) fn handle_event(&mut self, ev: Event) -> bool {
        if matches!(ev, Event::FocusGained) {
            // Codex refreshes OSC 10/11 after terminal focus changes so the
            // shared light/dark and accent decisions follow the active theme.
            crate::terminal_palette::requery_default_colors();
            return true;
        }
        if let Event::Mouse(mouse) = ev {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    if let Some(pager) = self.pager.as_mut() {
                        pager.scroll_by(-3);
                    } else {
                        self.scroll = self.scroll.saturating_add(3);
                    }
                }
                MouseEventKind::ScrollDown => {
                    if let Some(pager) = self.pager.as_mut() {
                        pager.scroll_by(3);
                    } else {
                        self.scroll = self.scroll.saturating_sub(3);
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Event::Paste(text) = ev {
            // Many terminals convert newlines to `\r` when pasting (e.g. iTerm2),
            // but the composer expects `\n`. Normalize CR to LF. (codex app.rs)
            let text = text.replace("\r", "\n");
            if !self.pending_user_inputs.is_empty() {
                self.insert_user_input_text(&text);
            } else if self.pager.is_none()
                && self.pending_approvals.is_empty()
                && self.overlay.is_none()
            {
                self.flush_paste_burst_before_modified_input();
                self.composer.insert_text(&text);
                self.paste_burst.clear_after_explicit_paste();
                self.recompute_slash();
                self.recompute_mention();
            }
            return true;
        }
        let Event::Key(key) = ev else {
            return true;
        };
        // On terminals that advertise `REPORT_EVENT_TYPES` (kitty/iTerm2/foot),
        // crossterm emits both `Press` and `Release` for a single physical tap.
        // Ignore `Release` so actions don't fire twice. (codex chat_composer.rs)
        if matches!(key.kind, KeyEventKind::Release) {
            return true;
        }

        let now = Instant::now();
        self.flush_paste_burst_if_due(now);
        let is_plain_char = matches!(
            key.code,
            KeyCode::Char(_) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT
        );
        let is_newline_key = matches!(
            key.code,
            KeyCode::Enter
                if key.modifiers.is_empty()
                    || key.modifiers == KeyModifiers::SHIFT
                    || key.modifiers == KeyModifiers::ALT
        ) || matches!(
            key.code,
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL)
        ) || matches!(
            key.code,
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL)
        );
        if !is_plain_char && !is_newline_key {
            self.flush_paste_burst_before_modified_input();
        }

        if self.pager.is_some()
            && key.code == KeyCode::Char('o')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            // Codex's global copy binding remains available while the
            // transcript pager is open.
            self.copy_last_response();
            return true;
        }
        if let Some(pager) = self.pager.as_mut() {
            if !pager.handle_key(key) {
                self.pager = None;
            }
            return true;
        }

        // Codex's reverse/forward-i-search owns all keys while open: typed
        // chars extend the query, Ctrl+R/S + Up/Down navigate, Enter accepts,
        // Esc/Ctrl+C cancels. Delegate the whole key to the session and never
        // fall through to submit or edit.
        if self.composer.history_search_active() {
            let _ = self.composer.handle_history_search_key(key);
            return true;
        }

        // While an approval popup is open, it owns all keys (codex semantics:
        // the modal must emit an explicit decision before anything else runs).
        if !self.pending_approvals.is_empty() {
            self.handle_approval_key(key);
            return true;
        }

        if !self.pending_user_inputs.is_empty() {
            self.handle_user_input_key(key);
            return true;
        }

        // While a configuration overlay is open, route all keys to it.
        if let Some(screen) = self.overlay.as_mut() {
            let is_environment = matches!(screen, Overlay::Environment(_));
            if let Some(outcome) = screen.handle_key(key) {
                match outcome {
                    Outcome::Saved => {
                        if is_environment {
                            self.system("environment saved to environment.toml", SysKind::Info);
                        } else if let Err(e) =
                            save_settings(&self.autoreport_home, screen.settings())
                        {
                            self.system(&format!("config save failed: {e}"), SysKind::Error);
                        } else {
                            self.system(
                                "configuration saved to config.toml — restart to apply",
                                SysKind::Info,
                            );
                        }
                    }
                    Outcome::Cancelled => {
                        self.system(
                            if is_environment {
                                "environment unchanged"
                            } else {
                                "config unchanged"
                            },
                            SysKind::Info,
                        );
                    }
                }
                self.overlay = None;
            }
            return true;
        }

        // While a completion popup is open, intercept navigation keys.
        if self.slash.is_some() {
            match key.code {
                KeyCode::Down => {
                    self.move_slash(1);
                    return true;
                }
                KeyCode::Up => {
                    self.move_slash(-1);
                    return true;
                }
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.move_slash(1);
                    return true;
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.move_slash(-1);
                    return true;
                }
                KeyCode::Tab if key.modifiers.is_empty() => {
                    self.accept_slash();
                    return true;
                }
                KeyCode::Enter if key.modifiers.is_empty() => {
                    if self.run_selected_slash() {
                        return true;
                    }
                    self.submit();
                    return true;
                }
                KeyCode::Esc => {
                    let input = self.composer.text();
                    let cursor = self.composer.cursor().min(input.len());
                    self.dismissed_slash = input
                        .strip_prefix('/')
                        .map(|text| text[..cursor.saturating_sub(1).min(text.len())].to_string());
                    self.slash = None;
                    return true;
                }
                _ => {}
            }
        }
        if self.mention.is_some() {
            match key.code {
                KeyCode::Down => {
                    self.move_mention(1);
                    return true;
                }
                KeyCode::Up => {
                    self.move_mention(-1);
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter if key.modifiers.is_empty() => {
                    self.accept_mention();
                    return true;
                }
                KeyCode::Esc => {
                    if let Some(mention) = self.mention.as_ref() {
                        let input = self.composer.text();
                        let cursor = mention.cursor.min(input.len());
                        let query = input.get(mention.start + 1..cursor).unwrap_or_default();
                        self.dismissed_mention =
                            Some(format!("{}:{}:{}", mention.start, mention.cursor, query));
                    }
                    self.mention = None;
                    return true;
                }
                _ => {}
            }
        }

        // Codex's `/agent` selection popup owns navigation while open: it
        // mirrors the `ListSelectionView` keymap (Up/Down/j/k, Enter accept,
        // Esc cancel, 1-9 quick-select). Tab/BackTab also step the selection
        // since that is the global agent-switch key here.
        if self.agent_picker.is_some() {
            #[derive(Clone, Copy)]
            enum PickerAction {
                Close,
                Shift(i32),
                Accept(usize),
                Swallow,
            }
            let roster = autoreport_core::types::AgentType::ALL;
            let len = roster.len();
            let shift = |idx: usize, delta: i32| {
                (idx as i32 + delta).rem_euclid(len as i32) as usize
            };
            let action = match key.code {
                KeyCode::Esc => Some(PickerAction::Close),
                KeyCode::Enter => Some(PickerAction::Accept(
                    self.agent_picker.as_ref().expect("picker open").selected,
                )),
                KeyCode::Tab | KeyCode::Down => Some(PickerAction::Shift(1)),
                KeyCode::BackTab | KeyCode::Up => Some(PickerAction::Shift(-1)),
                KeyCode::Char('j') if key.modifiers.is_empty() => Some(PickerAction::Shift(1)),
                KeyCode::Char('k') if key.modifiers.is_empty() => Some(PickerAction::Shift(-1)),
                KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(PickerAction::Shift(1))
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    Some(PickerAction::Shift(-1))
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty()
                        && c.is_ascii_digit()
                        && c.to_digit(10).filter(|d| *d as usize <= len).is_some() =>
                {
                    c.to_digit(10).map(|d| PickerAction::Accept((d - 1) as usize))
                }
                _ => Some(PickerAction::Swallow),
            };
            if let Some(action) = action {
                match action {
                    PickerAction::Close => self.agent_picker = None,
                    PickerAction::Shift(delta) => {
                        if let Some(p) = self.agent_picker.as_mut() {
                            p.selected = shift(p.selected, delta);
                        }
                    }
                    PickerAction::Accept(idx) => {
                        let agent = roster[idx.min(len - 1)];
                        self.focused = agent;
                        self.composer.set_focused_agent(agent.label());
                        self.refresh_pending_input_preview();
                        self.agent_picker = None;
                    }
                    PickerAction::Swallow => {}
                }
                return true;
            }
        }

        // Codex's composer shortcut overlay is only bound while the draft is
        // empty. This preserves literal '?' input in a non-empty message.
        if matches!(key.code, KeyCode::Char('?'))
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
            && self.composer.text().is_empty()
        {
            self.composer.toggle_shortcuts();
            return true;
        }

        match key.code {
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                let _ = self.edit_last_queued_input();
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if !self.paste_burst.append_newline_if_active(now) {
                    self.composer.insert_newline();
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                if !self.paste_burst.append_newline_if_active(now) {
                    self.composer.insert_newline();
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                if self
                    .paste_burst
                    .newline_should_insert_instead_of_submit(now)
                {
                    if !self.paste_burst.append_newline_if_active(now) {
                        self.composer.insert_newline();
                        self.paste_burst.extend_window(now);
                    }
                } else {
                    self.submit();
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.paste_burst.append_newline_if_active(now) {
                    self.composer.insert_newline();
                }
            }
            KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.paste_burst.append_newline_if_active(now) {
                    self.composer.insert_newline();
                }
            }
            KeyCode::Tab => self.cycle_agent(),
            KeyCode::BackTab => self.cycle_agent_back(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.composer.clear_for_ctrl_c().is_some() {
                    // Keep the TUI alive and discard only the current draft,
                    // matching Codex's editor-level Ctrl-C behavior.
                } else if self.retract_pending_submission(self.focused) {
                    self.manager.interrupt_and_retract(self.focused);
                } else if matches!(
                    self.statuses.get(&self.focused),
                    Some(
                        autoreport_core::types::AgentStatus::Thinking
                            | autoreport_core::types::AgentStatus::RunningTool
                            | autoreport_core::types::AgentStatus::DebugMode
                    )
                ) {
                    self.restore_queued_inputs_after_interrupt(self.focused);
                    self.manager.interrupt(self.focused);
                } else {
                    return false;
                }
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.composer.text().is_empty() {
                    return false;
                }
                self.composer.delete_next();
            }
            // Codex's Ctrl+O binding copies the latest assistant response.
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.copy_last_response()
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_pager()
            }
            // Codex global clear-terminal binding: clear the visible UI but
            // preserve the underlying conversation/session context.
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear_terminal_ui()
            }
            // Codex toggles raw, copy-friendly transcript output with Alt-R.
            // The normal rich renderer remains the default.
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.raw_output = !self.raw_output
            }
            KeyCode::Backspace
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.composer.delete_word_previous()
            }
            KeyCode::Delete
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.composer.delete_word_next()
            }
            // Codex's multi-agent navigation shortcuts are available while
            // the draft is empty; with text present Option/Alt+arrows retain
            // their word-motion editor semantics.
            KeyCode::Left
                if key.modifiers.contains(KeyModifiers::ALT) && self.composer.text().is_empty() =>
            {
                self.cycle_agent_back()
            }
            KeyCode::Right
                if key.modifiers.contains(KeyModifiers::ALT) && self.composer.text().is_empty() =>
            {
                self.cycle_agent()
            }
            KeyCode::Backspace => self.composer.delete_previous(),
            KeyCode::Delete => self.composer.delete_next(),
            KeyCode::Left
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.composer.move_word_left()
            }
            KeyCode::Right
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.composer.move_word_right()
            }
            KeyCode::Left => self.composer.move_left(),
            KeyCode::Right => self.composer.move_right(),
            KeyCode::Home => self.composer.move_home(),
            KeyCode::End => self.composer.move_end(),
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.move_left()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.move_right()
            }
            KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.delete_previous()
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.move_home()
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.move_end()
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.delete_word_previous()
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.delete_word_next()
            }
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.move_word_left()
            }
            KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.composer.move_word_right()
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.delete_to_home()
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.delete_to_end()
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.composer.move_up() {
                    let _ = self.composer.history_previous();
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if !self.composer.move_down() {
                    let _ = self.composer.history_next();
                }
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // Open a Codex-style reverse-i-search session. Subsequent keys
                // (query chars, Ctrl+R/S, Enter, Esc) route through the
                // search-active handler at the top of this function.
                self.composer.begin_history_search();
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.begin_history_search();
            }
            KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.composer.yank()
            }
            KeyCode::Up => {
                if !self.composer.move_up() && !self.composer.history_previous() {
                    self.scroll = self.scroll.saturating_add(1);
                }
            }
            KeyCode::Down => {
                if self.composer.move_down() {
                    // Multiline editor navigation owns this key.
                } else if !self.composer.text().is_empty() && self.composer.history_next() {
                    // History navigation owns this key while a recalled draft is active.
                } else {
                    self.scroll = self.scroll.saturating_sub(1);
                }
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Esc => {
                // ESC: close completion popups if open, otherwise interrupt
                // the focused agent's active turn (codex semantics).
                if self.slash.is_some() {
                    self.slash = None;
                } else if self.mention.is_some() {
                    self.mention = None;
                } else {
                    self.restore_queued_inputs_after_interrupt(self.focused);
                    self.manager.interrupt(self.focused);
                }
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.insert_plain_char(c, now);
            }
            _ => {}
        }

        // Any input that wasn't intercepted resets manual scroll.
        if !matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        ) {
            self.scroll = 0;
        }

        // Recompute completion popups based on the token under the cursor.
        self.recompute_slash();
        self.recompute_mention();
        if self.exit_requested {
            return false;
        }
        true
    }
}
