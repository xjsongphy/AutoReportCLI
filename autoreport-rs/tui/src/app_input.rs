//! Editor input, completion, and message submission for the terminal UI.

use crate::app::Tui;
use crate::app_state::{Cell, Mention, PendingSubmission};
use crate::bottom_pane::paste_burst::{CharDecision, FlushResult};
use crate::chatwidget::{extract_mentions, is_mention_char, read_capped};
use crate::slash_command::{self, SlashCompletion};
use autoreport_core::types::{AgentType, MessageSource};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

const MENTION_LIMIT: usize = 8;

impl Tui {
    pub(crate) fn flush_paste_burst_if_due(&mut self, now: Instant) {
        let result = self.paste_burst.flush_if_due(now);
        self.apply_paste_burst_result(result);
    }

    pub(crate) fn flush_paste_burst_before_modified_input(&mut self) {
        if let Some(text) = self.paste_burst.flush_before_modified_input() {
            self.composer.insert_text(&text);
        }
        self.paste_burst.clear_window_after_non_char();
    }

    fn apply_paste_burst_result(&mut self, result: FlushResult) {
        match result {
            FlushResult::Paste(text) => {
                self.composer.insert_text(&text);
            }
            FlushResult::Typed(ch) => self.composer.insert(ch),
            FlushResult::None => {}
        }
    }

    pub(crate) fn insert_plain_char(&mut self, ch: char, now: Instant) {
        match if ch.is_ascii() {
            Some(self.paste_burst.on_plain_char(ch, now))
        } else {
            self.paste_burst.on_plain_char_no_hold(now)
        } {
            Some(CharDecision::RetainFirstChar) => {}
            Some(CharDecision::BeginBufferFromPending) => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            Some(CharDecision::BufferAppend) => {
                self.paste_burst.append_char_to_buffer(ch, now);
            }
            Some(CharDecision::BeginBuffer { retro_chars }) => {
                let cursor = self.composer.cursor();
                let before = self.composer.text()[..cursor].to_string();
                if let Some(grab) =
                    self.paste_burst
                        .decide_begin_buffer(now, &before, usize::from(retro_chars))
                {
                    let _ = self.composer.remove_range_before_cursor(grab.start_byte);
                    self.paste_burst.append_char_to_buffer(ch, now);
                } else {
                    self.composer.insert(ch);
                }
            }
            None => self.composer.insert(ch),
        }
    }

    pub(crate) fn open_pager(&mut self) {
        let lines =
            crate::history_cell::render_history_lines_for_agent(&self.history, self.focused, 200);
        self.pager = Some(crate::pager_overlay::PagerOverlay::new("Transcript", lines));
    }

    pub(crate) fn copy_last_response(&mut self) {
        let Some(text) = self.history.iter().rev().find_map(|cell| match cell {
            Cell::AgentMarkdown { agent, text } if *agent == self.focused && !text.is_empty() => {
                Some(text.clone())
            }
            Cell::AgentMessage { agent, text, .. }
                if *agent == self.focused && !text.is_empty() =>
            {
                Some(text.clone())
            }
            _ => None,
        }) else {
            self.system(
                "no assistant response to copy",
                crate::app_state::SysKind::Info,
            );
            return;
        };
        match crate::clipboard_copy::copy_to_clipboard(&text) {
            Ok(lease) => {
                self.clipboard_lease = lease;
                self.system("copied last response", crate::app_state::SysKind::Info)
            }
            Err(err) => self.system(
                &format!("copy failed: {err}"),
                crate::app_state::SysKind::Error,
            ),
        }
    }

    pub(crate) fn move_mention(&mut self, dir: i32) {
        if let Some(m) = self.mention.as_mut() {
            if m.matches.is_empty() {
                return;
            }
            let n = m.matches.len() as i32;
            m.selected = (m.selected as i32 + dir).rem_euclid(n) as usize;
        }
    }

    pub(crate) fn accept_mention(&mut self) {
        let Some(m) = self.mention.take() else {
            return;
        };
        let Some(path) = m.matches.get(m.selected).cloned() else {
            return;
        };
        let after_at = m.start + 1;
        let input = self.composer.text();
        let end = m.cursor.min(input.len());
        let mut new_input = String::new();
        new_input.push_str(&input[..after_at]);
        new_input.push_str(&path);
        new_input.push_str(&input[end..]);
        new_input.push(' ');
        self.composer
            .set_text_and_cursor(new_input, after_at + path.len() + 1);
    }

    pub(crate) fn move_slash(&mut self, dir: i32) {
        if let Some(s) = self.slash.as_mut() {
            if s.matches.is_empty() {
                return;
            }
            s.selected = (s.selected as i32 + dir).rem_euclid(s.matches.len() as i32) as usize;
        }
    }

    pub(crate) fn accept_slash(&mut self) {
        let Some(s) = self.slash.take() else {
            return;
        };
        let Some(cmd) = s.matches.get(s.selected).copied() else {
            return;
        };
        let input = format!("/{} ", cmd.name);
        let cursor = input.len();
        self.composer.set_text_and_cursor(input, cursor);
        self.dismissed_slash = None;
    }

    pub(crate) fn run_selected_slash(&mut self) -> bool {
        let Some(slash) = self.slash.take() else {
            return false;
        };
        let Some(command) = slash.matches.get(slash.selected) else {
            return false;
        };
        let text = self.composer.take_text();
        let raw_command = text.strip_prefix('/').map(str::trim).unwrap_or_default();
        let args = if raw_command.split_whitespace().count() <= 1 {
            command.name
        } else {
            raw_command
        };
        self.dismissed_slash = None;
        self.run_command(args);
        true
    }

    pub(crate) fn recompute_slash(&mut self) {
        let input = self.composer.text();
        let cursor = self.composer.cursor();
        if !input.starts_with('/') || cursor == 0 {
            self.slash = None;
            self.dismissed_slash = None;
            return;
        }
        let typed = &input[1..cursor.min(input.len())];
        if typed.chars().any(char::is_whitespace) {
            self.slash = None;
            self.dismissed_slash = None;
            return;
        }
        if self.dismissed_slash.as_deref() == Some(typed) {
            self.slash = None;
            return;
        }
        self.dismissed_slash = None;
        self.slash = Some(SlashCompletion {
            matches: slash_command::matches(typed),
            selected: 0,
        });
    }

    /// Detect an open `@token` under the cursor and rebuild the popup.
    pub(crate) fn recompute_mention(&mut self) {
        if self.slash.is_some() {
            self.mention = None;
            return;
        }
        let input = self.composer.text();
        let cursor = self.composer.cursor();
        let bytes = input.as_bytes();
        if cursor == 0 || cursor > bytes.len() {
            self.mention = None;
            return;
        }
        let mut i = cursor;
        let mut query_len = 0usize;
        while i > 0 {
            let c = input[..i].chars().next_back().expect("non-empty prefix");
            if c == '@' {
                break;
            }
            if !is_mention_char(c) {
                self.mention = None;
                return;
            }
            i -= c.len_utf8();
            query_len += c.len_utf8();
        }
        if i == 0 || bytes[i - 1] != b'@' {
            self.mention = None;
            return;
        }
        let at_idx = i - 1;
        let prev_ok = at_idx == 0
            || input[..at_idx]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
        if !prev_ok {
            self.mention = None;
            return;
        }
        let query = &input[at_idx + 1..at_idx + 1 + query_len];
        let mention_key = format!("{at_idx}:{cursor}:{query}");
        if self.dismissed_mention.as_deref() == Some(&mention_key) {
            self.mention = None;
            return;
        }
        self.dismissed_mention = None;
        self.mention = Some(Mention {
            start: at_idx,
            cursor,
            matches: self.index.search(query, MENTION_LIMIT),
            selected: 0,
        });
    }

    pub(crate) fn cycle_agent(&mut self) {
        self.cycle_agent_by(1);
    }
    pub(crate) fn cycle_agent_back(&mut self) {
        self.cycle_agent_by(-1);
    }

    fn cycle_agent_by(&mut self, direction: i32) {
        let order = AgentType::ALL;
        let index = order
            .iter()
            .position(|agent| *agent == self.focused)
            .unwrap_or(0);
        self.focused = order[(index as i32 + direction).rem_euclid(order.len() as i32) as usize];
        self.composer.set_focused_agent(self.focused.label());
        self.refresh_pending_input_preview();
    }

    pub(crate) fn submit(&mut self) {
        self.mention = None;
        self.slash = None;
        let text = self.composer.take_text().trim().to_string();
        if text.is_empty() {
            return;
        }
        if let Some(command) = text.strip_prefix('/') {
            self.run_command(command.trim());
            return;
        }
        let busy = self
            .pending_submissions
            .iter()
            .any(|pending| pending.agent == self.focused)
            || matches!(
                self.statuses.get(&self.focused),
                Some(
                    autoreport_core::types::AgentStatus::Thinking
                        | autoreport_core::types::AgentStatus::RunningTool
                        | autoreport_core::types::AgentStatus::Queued
                        | autoreport_core::types::AgentStatus::DebugMode
                )
            );
        if busy {
            self.queued_inputs
                .entry(self.focused)
                .or_default()
                .push_back(text);
            self.refresh_pending_input_preview();
            return;
        }
        self.submit_now(text);
    }

    fn submit_now(&mut self, text: String) {
        let cancelled = Arc::new(AtomicBool::new(false));
        let submitted = Arc::new(AtomicBool::new(false));
        let history_index = self.history.len();
        self.history.push(Cell::User {
            _agent: self.focused,
            text: text.clone(),
        });
        self.pending_submissions.push(PendingSubmission {
            agent: self.focused,
            text: text.clone(),
            history_index,
            tool_started: false,
            cancelled: cancelled.clone(),
            submitted: submitted.clone(),
        });
        // @mention file reads and the IDE IPC fetch are blocking (the IDE probe
        // allows up to 5s). Run them on a blocking worker task so the TUI event
        // loop keeps handling keystrokes/redraws; the agent turn is submitted
        // once expansion completes. (`manager` is `Arc`, `submit` takes `&self`.)
        let manager = self.manager.clone();
        let workspace = self.workspace.clone();
        let focused = self.focused;
        let ide_enabled = self.ide_enabled;
        let text_for_fallback = text.clone();
        tokio::spawn(async move {
            let expanded = tokio::task::spawn_blocking(move || {
                expand_mentions(&workspace, &text, ide_enabled)
            })
            .await
            .unwrap_or(text_for_fallback);
            if cancelled.load(Ordering::Relaxed) {
                return;
            }
            submitted.store(true, Ordering::Relaxed);
            manager.submit(focused, expanded, MessageSource::User);
        });
    }

    pub(crate) fn mark_pending_tool_started(&mut self, agent: AgentType) {
        if let Some(pending) = self
            .pending_submissions
            .iter_mut()
            .find(|pending| pending.agent == agent)
        {
            pending.tool_started = true;
        }
    }

    pub(crate) fn finish_pending_submission(&mut self, agent: AgentType) {
        if let Some(index) = self
            .pending_submissions
            .iter()
            .position(|pending| pending.agent == agent)
        {
            self.pending_submissions.remove(index);
        }
    }

    /// Retract the most recent pre-tool submission and restore it, together
    /// with any queued follow-ups, to the composer. Returns whether a row was
    /// retracted; tool-started turns must use ordinary interrupt semantics.
    pub(crate) fn retract_pending_submission(&mut self, agent: AgentType) -> bool {
        let Some(index) = self
            .pending_submissions
            .iter()
            .rposition(|pending| pending.agent == agent && !pending.tool_started)
        else {
            return false;
        };
        let pending = self.pending_submissions.remove(index);
        pending.cancelled.store(true, Ordering::Relaxed);
        let was_submitted = pending.submitted.load(Ordering::Relaxed);

        if let Some(Cell::User { _agent, text }) = self.history.get(pending.history_index)
            && *_agent == agent
            && text == &pending.text
        {
            self.history.remove(pending.history_index);
        }

        let mut restored = pending.text;
        if let Some(queue) = self.queued_inputs.remove(&agent) {
            for text in queue {
                if !restored.is_empty() {
                    restored.push('\n');
                }
                restored.push_str(&text);
            }
        }
        self.composer
            .set_text_and_cursor(restored.clone(), restored.len());
        self.refresh_pending_input_preview();
        if was_submitted {
            self.suppress_until_idle.insert(agent);
        }
        true
    }

    pub(crate) fn restore_queued_inputs_after_interrupt(&mut self, agent: AgentType) {
        let Some(queue) = self.queued_inputs.remove(&agent) else {
            return;
        };
        let mut restored = self.composer.text().to_string();
        for text in queue {
            if !restored.is_empty() {
                restored.push('\n');
            }
            restored.push_str(&text);
        }
        self.composer
            .set_text_and_cursor(restored.clone(), restored.len());
        self.refresh_pending_input_preview();
    }

    pub(crate) fn refresh_pending_input_preview(&mut self) {
        let queued = self
            .queued_inputs
            .get(&self.focused)
            .map(|items| items.iter().cloned().collect())
            .unwrap_or_default();
        self.pending_input_preview.set_queued_messages(queued);
    }

    pub(crate) fn drain_queued_input(&mut self, agent: AgentType) {
        let Some(queue) = self.queued_inputs.get_mut(&agent) else {
            return;
        };
        let Some(text) = queue.pop_front() else {
            self.queued_inputs.remove(&agent);
            self.refresh_pending_input_preview();
            return;
        };
        if queue.is_empty() {
            self.queued_inputs.remove(&agent);
        }
        if agent == self.focused {
            self.refresh_pending_input_preview();
        }
        let previous = self.focused;
        self.focused = agent;
        self.submit_now(text);
        self.focused = previous;
    }

    /// Codex's default pending-input preview binding (Alt+Up) moves the last
    /// queued follow-up back into the composer for editing.
    pub(crate) fn edit_last_queued_input(&mut self) -> bool {
        let Some(queue) = self.queued_inputs.get_mut(&self.focused) else {
            return false;
        };
        let Some(text) = queue.pop_back() else {
            return false;
        };
        if queue.is_empty() {
            self.queued_inputs.remove(&self.focused);
        }
        self.composer.set_text_and_cursor(text.clone(), text.len());
        self.refresh_pending_input_preview();
        true
    }
}

/// Expand `@mentions` into inline file contents and, if IDE context is enabled,
/// prefix the result with the current IDE selection/open tabs. Both steps do
/// blocking filesystem / Unix-socket IO, so this is meant to run on a
/// `spawn_blocking` worker — never on the TUI event loop.
fn expand_mentions(workspace: &std::path::Path, text: &str, ide_enabled: bool) -> String {
    const MENTION_CAP: usize = 16_000;
    let refs = extract_mentions(text);
    let mut out = if refs.is_empty() {
        text.to_string()
    } else {
        let mut out = text.to_string();
        out.push_str("\n\n# Referenced files");
        for rel in refs {
            match read_capped(&workspace.join(&rel), MENTION_CAP) {
                Some(content) => out.push_str(&format!("\n\n## @{rel}\n```\n{content}\n```")),
                None => out.push_str(&format!("\n\n## @{rel}\n(not found)")),
            }
        }
        out
    };
    if ide_enabled {
        // Best-effort: a missing/unresponsive IDE silently skips the prefix
        // rather than blocking or erroring the user's turn.
        if let Ok(context) = crate::ide_context::fetch_ide_context(workspace) {
            if let Some(prefixed) = crate::ide_context::apply_ide_context_to_text(&context, &out) {
                out = prefixed;
            }
        }
    }
    out
}
