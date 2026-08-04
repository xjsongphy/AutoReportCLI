//! Editor input, completion, and message submission for the terminal UI.

use crate::app::Tui;
use crate::app_state::{Cell, Mention, PendingSubmission};
use crate::bottom_pane::paste_burst::{CharDecision, FlushResult};
use crate::chatwidget::{extract_mentions, is_mention_char, read_capped};
use crate::slash_command::{self, SlashCompletion};
use autoreport_core::types::{AgentType, MessageSource};
use std::path::{Component, Path, PathBuf};
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
        // Completion introducers need to become visible in the same frame as
        // the key event. Codex normally holds the first ASCII character for a
        // few milliseconds while distinguishing typing from an unbracketed
        // paste. For `/` at an empty composer and `@` at a token boundary that
        // delay also holds the state which opens the completion popup, leaving
        // a real terminal with an apparently empty composer and no menu.
        //
        // Explicit paste events already bypass this path. Treat these two
        // unambiguous interactive introducers as typed input and reset the
        // burst detector so the draw immediately after the event includes
        // both the character and its popup.
        if is_completion_introducer(ch, self.composer.text(), self.composer.cursor()) {
            self.paste_burst.clear_after_explicit_paste();
            self.composer.insert(ch);
            return;
        }
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
        if self.overlay.is_some() {
            return;
        }
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
        if let Some(m) = self.composer.mention_popup_mut() {
            if m.matches.is_empty() {
                return;
            }
            let n = m.matches.len() as i32;
            m.selected = (m.selected as i32 + dir).rem_euclid(n) as usize;
        }
    }

    pub(crate) fn accept_mention(&mut self) {
        let Some(m) = self.composer.take_mention_popup() else {
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
        if path.chars().any(char::is_whitespace) && !path.contains('"') {
            new_input.push('"');
            new_input.push_str(&path);
            new_input.push('"');
        } else {
            new_input.push_str(&path);
        }
        new_input.push_str(&input[end..]);
        new_input.push(' ');
        let inserted_len = if path.chars().any(char::is_whitespace) && !path.contains('"') {
            path.len() + 2
        } else {
            path.len()
        };
        self.composer
            .set_text_and_cursor(new_input, after_at + inserted_len + 1);
    }

    pub(crate) fn move_slash(&mut self, dir: i32) {
        if let Some(s) = self.composer.slash_popup_mut() {
            if s.matches.is_empty() {
                return;
            }
            s.selected = (s.selected as i32 + dir).rem_euclid(s.matches.len() as i32) as usize;
        }
    }

    pub(crate) fn accept_slash(&mut self) {
        let Some(s) = self.composer.take_slash_popup() else {
            return;
        };
        let Some(cmd) = s.matches.get(s.selected).copied() else {
            return;
        };
        let input = format!("/{} ", cmd.name);
        let cursor = input.len();
        self.composer.set_text_and_cursor(input, cursor);
        self.composer.set_dismissed_slash(None);
    }

    pub(crate) fn run_selected_slash(&mut self) -> bool {
        let Some(slash) = self.composer.take_slash_popup() else {
            return false;
        };
        let Some(command) = slash.matches.get(slash.selected) else {
            return false;
        };
        // A pending paste burst can still hold fast-typed characters outside
        // the composer (`/new` -> `/` in the composer, `new` in the burst
        // buffer). Flush it into the composer first so `take_text` observes the
        // full command and nothing re-injects into the cleared buffer afterward.
        // Mirrors Codex's `handle_submission_with_time`, which folds burst
        // handling into submission before the textarea is read.
        self.flush_paste_burst_before_modified_input();
        let text = self.composer.take_text();
        let raw_command = text.strip_prefix('/').map(str::trim).unwrap_or_default();
        let args = if raw_command.split_whitespace().count() <= 1 {
            command.name
        } else {
            raw_command
        };
        self.composer.set_dismissed_slash(None);
        self.run_command(args);
        true
    }

    pub(crate) fn recompute_slash(&mut self) {
        let input = self.composer.text();
        let cursor = self.composer.cursor();
        if !input.starts_with('/') || cursor == 0 {
            self.composer.set_slash_popup(None);
            self.composer.set_dismissed_slash(None);
            return;
        }
        let typed = input[1..cursor.min(input.len())].to_string();
        if typed.chars().any(char::is_whitespace) {
            self.composer.set_slash_popup(None);
            self.composer.set_dismissed_slash(None);
            return;
        }
        if self.composer.dismissed_slash() == Some(typed.as_str()) {
            self.composer.set_slash_popup(None);
            return;
        }
        self.composer.set_dismissed_slash(None);
        self.composer.set_slash_popup(Some(SlashCompletion {
            matches: slash_command::matches(&typed),
            selected: 0,
        }));
    }

    /// Detect an open `@token` under the cursor and rebuild the popup.
    pub(crate) fn recompute_mention(&mut self) {
        if self.composer.slash_popup().is_some() {
            self.composer.set_mention_popup(None);
            return;
        }
        let input = self.composer.text();
        let cursor = self.composer.cursor();
        let bytes = input.as_bytes();
        if cursor == 0 || cursor > bytes.len() {
            self.composer.set_mention_popup(None);
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
                self.composer.set_mention_popup(None);
                return;
            }
            i -= c.len_utf8();
            query_len += c.len_utf8();
        }
        if i == 0 || bytes[i - 1] != b'@' {
            self.composer.set_mention_popup(None);
            return;
        }
        let at_idx = i - 1;
        let prev_ok = at_idx == 0
            || input[..at_idx]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
        if !prev_ok {
            self.composer.set_mention_popup(None);
            return;
        }
        let query = input[at_idx + 1..at_idx + 1 + query_len].to_string();
        let mention_key = format!("{at_idx}:{cursor}:{query}");
        if self.composer.dismissed_mention() == Some(mention_key.as_str()) {
            self.composer.set_mention_popup(None);
            return;
        }
        self.composer.set_dismissed_mention(None);
        self.composer.set_mention_popup(Some(Mention {
            start: at_idx,
            cursor,
            matches: self.index.search(&query, MENTION_LIMIT),
            selected: 0,
        }));
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
        self.composer.set_mention_popup(None);
        self.composer.set_slash_popup(None);
        // Fold any pending paste burst into the composer before reading the
        // draft, so a fast-typed `/new` is submitted (and cleared) as a unit
        // instead of leaving buffered characters that re-appear after clear.
        self.flush_paste_burst_before_modified_input();
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

fn is_completion_introducer(ch: char, text: &str, cursor: usize) -> bool {
    let cursor = cursor.min(text.len());
    match ch {
        '/' => text.is_empty() && cursor == 0,
        '@' => {
            cursor == 0
                || text[..cursor]
                    .chars()
                    .next_back()
                    .is_some_and(char::is_whitespace)
        }
        _ => false,
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
            let resolved = resolve_workspace_mention(workspace, &rel);
            match resolved.and_then(|path| read_capped(&path, MENTION_CAP)) {
                Some(content) => out.push_str(&format!("\n\n## @{rel}\n```\n{content}\n```")),
                None => out.push_str(&format!("\n\n## @{rel}\n(invalid or not found)")),
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

/// Resolve an `@` file mention without allowing it to escape the workspace.
///
/// Codex file completions originate as workspace-relative paths. Keep that
/// invariant at the filesystem boundary as well: reject absolute paths and
/// lexical parent traversal, then canonicalize both sides to prevent a
/// workspace symlink from reaching an outside file.
fn resolve_workspace_mention(workspace: &Path, mention: &str) -> Option<PathBuf> {
    let relative = Path::new(mention);
    if relative.as_os_str().is_empty() || relative.is_absolute() {
        return None;
    }
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return None;
    }

    let workspace = workspace.canonicalize().ok()?;
    let candidate = workspace.join(relative).canonicalize().ok()?;
    candidate.starts_with(&workspace).then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::{expand_mentions, is_completion_introducer, resolve_workspace_mention};
    use std::fs;

    #[test]
    fn completion_introducers_bypass_the_first_character_hold() {
        assert!(is_completion_introducer('/', "", 0));
        assert!(is_completion_introducer('@', "", 0));
        assert!(is_completion_introducer('@', "inspect ", 8));
        assert!(!is_completion_introducer('/', "text", 4));
        assert!(!is_completion_introducer('@', "email", 5));
    }

    #[test]
    fn mention_resolution_rejects_absolute_and_parent_paths() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside file");

        assert!(
            resolve_workspace_mention(workspace.path(), outside.path().to_str().unwrap()).is_none()
        );
        assert!(resolve_workspace_mention(workspace.path(), "../secret.txt").is_none());
        assert!(resolve_workspace_mention(workspace.path(), "nested/../../secret.txt").is_none());
    }

    #[test]
    fn expansion_preserves_valid_nested_paths_with_spaces() {
        let workspace = tempfile::tempdir().expect("workspace");
        let nested = workspace.path().join("Data/Raw Files");
        fs::create_dir_all(&nested).expect("nested directory");
        fs::write(nested.join("trial one.csv"), "time,value\n0,1\n").expect("fixture");

        let expanded = expand_mentions(
            workspace.path(),
            "inspect @\"Data/Raw Files/trial one.csv\"",
            false,
        );

        assert!(expanded.contains("## @Data/Raw Files/trial one.csv"));
        assert!(expanded.contains("time,value\n0,1"));
    }

    #[test]
    fn expansion_does_not_read_parent_traversal() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        fs::create_dir(&workspace).expect("workspace");
        fs::write(root.path().join("secret.txt"), "DO NOT LEAK").expect("secret");

        let expanded = expand_mentions(&workspace, "inspect @../secret.txt", false);

        assert!(!expanded.contains("DO NOT LEAK"));
        assert!(expanded.contains("(invalid or not found)"));
    }

    #[cfg(unix)]
    #[test]
    fn mention_resolution_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::NamedTempFile::new().expect("outside file");
        symlink(outside.path(), workspace.path().join("linked.txt")).expect("symlink");

        assert!(resolve_workspace_mention(workspace.path(), "linked.txt").is_none());
    }
}
