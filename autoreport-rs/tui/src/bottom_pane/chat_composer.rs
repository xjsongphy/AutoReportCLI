//! Chat composer migrated from Codex's `bottom_pane/chat_composer.rs`.
//!
//! AutoReport's command and mention catalog remains runtime-specific, while the editable draft,
//! cursor, and bottom-pane rendering live in the same component boundary as Codex.
//!
//! Non-bracketed terminal paste classification lives in [`super::paste_burst`]; the event router
//! feeds it plain characters and applies the resulting typed/paste decision here. Enter remains
//! submit by default, but becomes a newline while the burst suppression window is active.

use super::popup_state::PopupState;
use crate::app_state::Mention;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCompletion;
use crate::style::{accent_style, user_message_style};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, WidgetRef};
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;

// Same separator vocabulary as Codex's textarea word-motion helpers. Word
// motion treats punctuation runs separately from identifier runs instead of
// collapsing `foo.bar` into one word.
const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";
const FOOTER_HEIGHT: u16 = 1;

fn is_word_separator(ch: char) -> bool {
    WORD_SEPARATORS.contains(ch)
}

pub(crate) struct ChatComposer {
    text: String,
    cursor: usize,
    focused_agent: String,
    status_line: Option<Line<'static>>,
    show_agent_picker: bool,
    shortcuts_visible: bool,
    history: Vec<String>,
    history_index: Option<usize>,
    draft_before_history: Option<String>,
    /// Codex-style reverse/forward-i-search session. `None` outside a search.
    history_search: Option<super::history_search::HistorySearchSession>,
    killed_text: Option<String>,
    /// Codex keeps completion lifecycle with the textarea/popup owner. The
    /// catalog itself remains AutoReport-specific, but the single active
    /// popup and dismissal markers no longer live on the top-level app.
    popups: PopupState,
    input_width: Cell<usize>,
    preferred_column: Option<usize>,
}

impl ChatComposer {
    pub(crate) fn new(focused_agent: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            focused_agent: focused_agent.into(),
            status_line: None,
            show_agent_picker: true,
            shortcuts_visible: false,
            history: Vec::new(),
            history_index: None,
            draft_before_history: None,
            history_search: None,
            killed_text: None,
            popups: PopupState::default(),
            input_width: Cell::new(80),
            preferred_column: None,
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn record_history(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.trim().is_empty() && self.history.last() != Some(&text) {
            self.history.push(text);
        }
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn set_text_and_cursor(&mut self, text: String, cursor: usize) {
        self.text = text;
        self.cursor = self.clamp_cursor(cursor.min(self.text.len()));
        self.history_search = None;
        self.preferred_column = None;
    }

    /// Remove a UTF-8-safe range immediately before the cursor. This is the
    /// retro-capture operation used by Codex's non-bracketed paste detector.
    pub(crate) fn remove_range_before_cursor(&mut self, start: usize) -> Option<String> {
        if start > self.cursor || !self.text.is_char_boundary(start) {
            return None;
        }
        let removed = self.text[start..self.cursor].to_string();
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.preferred_column = None;
        Some(removed)
    }

    pub(crate) fn take_text(&mut self) -> String {
        self.cursor = 0;
        self.preferred_column = None;
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        let text = std::mem::take(&mut self.text);
        if !text.trim().is_empty() {
            self.history.push(text.clone());
        }
        text
    }

    /// Codex's Ctrl-C editor behavior clears a non-empty draft without adding
    /// it to command history. The kill buffer is intentionally preserved so a
    /// subsequent Ctrl-Y still has the same semantics as the upstream editor.
    pub(crate) fn clear_for_ctrl_c(&mut self) -> Option<String> {
        if self.text.is_empty() {
            return None;
        }
        let cleared = std::mem::take(&mut self.text);
        self.cursor = 0;
        self.preferred_column = None;
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        Some(cleared)
    }

    pub(crate) fn insert(&mut self, ch: char) {
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.preferred_column = None;
    }

    pub(crate) fn insert_text(&mut self, text: &str) {
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.preferred_column = None;
    }

    /// Codex treats Shift+Enter as an editor newline while plain Enter submits.
    pub(crate) fn insert_newline(&mut self) {
        self.insert('\n');
    }

    pub(crate) fn delete_previous(&mut self) {
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .chars()
            .next_back()
            .expect("cursor is after a character");
        self.killed_text = Some(previous.to_string());
        self.cursor -= previous.len_utf8();
        self.text.remove(self.cursor);
        self.preferred_column = None;
    }

    pub(crate) fn delete_next(&mut self) {
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .chars()
            .next()
            .expect("cursor is before a character");
        self.killed_text = Some(next.to_string());
        self.text.drain(self.cursor..self.cursor + next.len_utf8());
        self.preferred_column = None;
    }

    pub(crate) fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= self.text[..self.cursor]
                .chars()
                .next_back()
                .expect("cursor is after a character")
                .len_utf8();
        }
        self.preferred_column = None;
    }

    pub(crate) fn history_previous(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        if self.history_index.is_none() {
            self.draft_before_history = Some(self.text.clone());
            self.history_index = Some(self.history.len());
        }
        let index = self.history_index.unwrap_or(0).saturating_sub(1);
        self.history_index = Some(index);
        self.text = self.history[index].clone();
        self.cursor = self.text.len();
        self.history_search = None;
        self.preferred_column = None;
        true
    }

    pub(crate) fn history_next(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.text = self.draft_before_history.take().unwrap_or_default();
        } else {
            self.history_index = Some(index + 1);
            self.text = self.history[index + 1].clone();
        }
        self.cursor = self.text.len();
        self.history_search = None;
        self.preferred_column = None;
        true
    }

    /// Open a Codex-style reverse-i-search session: snapshot the current draft
    /// (restored on cancel) and begin accumulating a query. The first `Ctrl+R`
    /// from normal mode opens the session; subsequent keys go to
    /// [`Self::handle_history_search_key`].
    pub(crate) fn begin_history_search(&mut self) {
        if self.history_search.is_some() {
            return;
        }
        // Drop any in-progress Up/Down history navigation so a later Down arrow
        // (after accepting a match) does not resume from a stale `history_index`
        // and recall the wrong entry. Mirrors Codex's `history.reset_search()`
        // at the top of `begin_history_search`.
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = Some(super::history_search::HistorySearchSession::new(
            self.text.clone(),
            self.cursor,
        ));
    }

    /// Dispatch one key to an open search session. The session mutates the
    /// composer's draft to show the current match (or restores the original
    /// draft on cancel). On `Accept`/`Cancel` the session is closed here so
    /// callers only need to inspect the outcome.
    pub(crate) fn handle_history_search_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> super::history_search::SearchKeyOutcome {
        let Some(session) = self.history_search.as_mut() else {
            return super::history_search::SearchKeyOutcome::Ignored;
        };
        let outcome = super::history_search::handle_search_key(
            &self.history,
            session,
            &mut self.text,
            &mut self.cursor,
            key,
        );
        match outcome {
            super::history_search::SearchKeyOutcome::Accept
            | super::history_search::SearchKeyOutcome::Cancel => {
                self.history_search = None;
            }
            _ => {}
        }
        outcome
    }

    pub(crate) fn history_search_active(&self) -> bool {
        self.history_search.is_some()
    }

    pub(crate) fn history_search_query(&self) -> &str {
        self.history_search
            .as_ref()
            .map(|s| s.query())
            .unwrap_or_default()
    }

    pub(crate) fn history_search_status(
        &self,
    ) -> Option<super::history_search::HistorySearchStatus> {
        self.history_search.as_ref().map(|s| s.status())
    }

    /// Cancel the session and restore the original draft.
    #[allow(dead_code)]
    pub(crate) fn cancel_history_search(&mut self) {
        if let Some(session) = self.history_search.take() {
            self.text = session.original_draft().to_string();
            self.cursor = session.original_cursor();
            self.preferred_column = None;
        }
    }

    pub(crate) fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += self.text[self.cursor..]
                .chars()
                .next()
                .expect("cursor is before a character")
                .len_utf8();
        }
        self.preferred_column = None;
    }

    pub(crate) fn move_up(&mut self) -> bool {
        let lines = visual_line_ranges(&self.text, self.input_width.get());
        let Some(index) = current_visual_line(&lines, self.cursor) else {
            return false;
        };
        if index == 0 {
            return false;
        }
        let (start, _) = lines[index];
        let column = self
            .preferred_column
            .unwrap_or_else(|| UnicodeWidthStr::width(&self.text[start..self.cursor]));
        self.preferred_column = Some(column);
        let (previous_start, previous_end) = lines[index - 1];
        self.cursor = visual_column_cursor(&self.text, previous_start, previous_end, column);
        true
    }

    pub(crate) fn move_down(&mut self) -> bool {
        let lines = visual_line_ranges(&self.text, self.input_width.get());
        let Some(index) = current_visual_line(&lines, self.cursor) else {
            return false;
        };
        if index + 1 >= lines.len() {
            return false;
        }
        let (start, _) = lines[index];
        let column = self
            .preferred_column
            .unwrap_or_else(|| UnicodeWidthStr::width(&self.text[start..self.cursor]));
        self.preferred_column = Some(column);
        let (next_start, next_end) = lines[index + 1];
        self.cursor = visual_column_cursor(&self.text, next_start, next_end, column);
        true
    }

    pub(crate) fn move_word_left(&mut self) {
        self.cursor = beginning_of_previous_word(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(crate) fn move_word_right(&mut self) {
        self.cursor = end_of_next_word(&self.text, self.cursor);
        self.preferred_column = None;
    }

    pub(crate) fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.preferred_column = None;
    }

    pub(crate) fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.text.len());
        self.preferred_column = None;
    }

    pub(crate) fn delete_word_previous(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let boundary = beginning_of_previous_word(&self.text, self.cursor);
        let killed = self.text[boundary..self.cursor].to_string();
        self.text.replace_range(boundary..self.cursor, "");
        self.killed_text = Some(killed);
        self.cursor = boundary;
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
    }

    pub(crate) fn delete_word_next(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let start = self.cursor;
        let boundary = end_of_next_word(&self.text, start);
        self.killed_text = Some(self.text[start..boundary].to_string());
        self.text.replace_range(start..boundary, "");
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
    }

    pub(crate) fn delete_to_home(&mut self) {
        let home = self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.killed_text = Some(self.text[home..self.cursor].to_string());
        self.text.replace_range(home..self.cursor, "");
        self.cursor = home;
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
    }

    pub(crate) fn delete_to_end(&mut self) {
        let end = self.text[self.cursor..]
            .find('\n')
            .map(|index| self.cursor + index)
            .unwrap_or(self.text.len());
        self.killed_text = Some(self.text[self.cursor..end].to_string());
        self.text.replace_range(self.cursor..end, "");
        self.history_index = None;
        self.draft_before_history = None;
        self.history_search = None;
    }

    pub(crate) fn yank(&mut self) {
        if let Some(text) = self.killed_text.clone() {
            self.insert_text(&text);
        }
    }

    pub(crate) fn set_focused_agent(&mut self, agent: impl Into<String>) {
        self.focused_agent = agent.into();
    }

    pub(crate) fn set_status_line(&mut self, status_line: Option<Line<'static>>) {
        self.status_line = status_line;
    }

    pub(crate) fn slash_popup(&self) -> Option<&SlashCompletion> {
        self.popups.slash()
    }

    pub(crate) fn slash_popup_mut(&mut self) -> Option<&mut SlashCompletion> {
        self.popups.slash_mut()
    }

    pub(crate) fn take_slash_popup(&mut self) -> Option<SlashCompletion> {
        self.popups.take_slash()
    }

    pub(crate) fn set_slash_popup(&mut self, popup: Option<SlashCompletion>) {
        self.popups.set_slash(popup);
    }

    pub(crate) fn mention_popup(&self) -> Option<&Mention> {
        self.popups.mention()
    }

    pub(crate) fn mention_popup_mut(&mut self) -> Option<&mut Mention> {
        self.popups.mention_mut()
    }

    pub(crate) fn take_mention_popup(&mut self) -> Option<Mention> {
        self.popups.take_mention()
    }

    pub(crate) fn set_mention_popup(&mut self, popup: Option<Mention>) {
        self.popups.set_mention(popup);
    }

    pub(crate) fn dismissed_slash(&self) -> Option<&str> {
        self.popups.dismissed_slash()
    }

    pub(crate) fn set_dismissed_slash(&mut self, value: Option<String>) {
        self.popups.set_dismissed_slash(value);
    }

    pub(crate) fn dismissed_mention(&self) -> Option<&str> {
        self.popups.dismissed_mention()
    }

    pub(crate) fn set_dismissed_mention(&mut self, value: Option<String>) {
        self.popups.set_dismissed_mention(value);
    }

    pub(crate) fn clear_popups(&mut self) {
        self.popups.clear();
    }

    pub(crate) fn toggle_shortcuts(&mut self) {
        self.shortcuts_visible = !self.shortcuts_visible;
    }

    fn clamp_cursor(&self, cursor: usize) -> usize {
        if self.text.is_char_boundary(cursor) {
            cursor
        } else {
            self.text
                .char_indices()
                .map(|(index, _)| index)
                .take_while(|index| *index < cursor)
                .last()
                .unwrap_or(0)
        }
    }
}

impl Renderable for ChatComposer {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_footer(area, buf, true);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.input_desired_height(width)
            .saturating_add(FOOTER_HEIGHT)
    }

    fn cursor_pos(&self, area: Rect) -> Option<(u16, u16)> {
        let [composer_rect, _footer_rect] = self.layout_areas(area);
        self.cursor_pos_in(composer_rect)
    }
}

impl ChatComposer {
    /// Render only the input surface. Codex gives an active completion popup
    /// ownership of the footer slot, so the normal footer must be omitted
    /// while the popup is shown.
    pub(crate) fn render_input_only(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_footer(area, buf, false);
    }

    pub(crate) fn input_desired_height(&self, width: u16) -> u16 {
        let width = usize::from(width.saturating_sub(4).max(1));
        let lines = self.input_line_count(width);
        u16::try_from(lines).unwrap_or(u16::MAX).saturating_add(2)
    }

    fn input_line_count(&self, width: usize) -> usize {
        if self.text.is_empty() {
            1
        } else {
            self.text
                .split('\n')
                .map(|line| wrap_input_line(line, width).len())
                .sum()
        }
        .max(1)
        .min(8)
    }

    fn render_with_footer(&self, area: Rect, buf: &mut Buffer, render_footer: bool) {
        if area.is_empty() {
            return;
        }

        let [composer_rect, footer_rect] = if render_footer {
            self.layout_areas(area)
        } else {
            [area, Rect::default()]
        };

        Block::default()
            .style(user_message_style())
            .render_ref(composer_rect, buf);

        // This is Codex's multiline composer geometry: the prompt is shown on
        // the first row and continuation rows use the same two-column indent.
        let prompt = Span::from("›").bold();
        let input_width = usize::from(composer_rect.width.saturating_sub(4).max(1));
        self.input_width.set(input_width);
        let input_lines = if self.text.is_empty() {
            vec!["Implement {feature}".to_string()]
        } else {
            self.text
                .split('\n')
                .flat_map(|line| wrap_input_line(line, input_width))
                .collect()
        };
        let max_input_lines = usize::from(composer_rect.height.saturating_sub(2)).max(1);
        let cursor_line = cursor_visual_line(&self.text, self.cursor, input_width);
        let input_start = cursor_line
            .saturating_add(1)
            .saturating_sub(max_input_lines);
        let visible_input_lines = input_lines.iter().skip(input_start).take(max_input_lines);
        for (row, text) in visible_input_lines.enumerate() {
            let line = if row == 0 {
                let span = if self.text.is_empty() {
                    Span::from(text.clone()).dim()
                } else {
                    Span::from(text.clone())
                };
                Line::from(vec![prompt.clone(), Span::raw(" "), span])
            } else {
                Line::from(vec![Span::raw("  "), Span::raw(text.clone())])
            };
            line.render_ref(
                Rect::new(
                    composer_rect.x + 1,
                    composer_rect.y + 1 + u16::try_from(row).unwrap_or(u16::MAX),
                    composer_rect.width.saturating_sub(2),
                    1,
                ),
                buf,
            );
        }

        if !render_footer || footer_rect.is_empty() {
            return;
        }

        let hint = if self.history_search_active() {
            // Codex renders `reverse-i-search: <query>` with a status suffix
            // (`(searching)` / `enter accept · esc cancel` / `no match`). The
            // prefix flips to `failing reverse-i-search` on a boundary miss.
            let status = self.history_search_status();
            let prefix = matches!(
                status,
                Some(super::history_search::HistorySearchStatus::NoMatch)
            )
            .then_some("  failing reverse-i-search: ")
            .unwrap_or("  reverse-i-search: ");
            let mut spans = vec![
                Span::raw(prefix).dim(),
                Span::raw(self.history_search_query().to_string()).dim(),
            ];
            match status {
                Some(super::history_search::HistorySearchStatus::Searching) => {
                    spans.push(Span::raw("  (searching)").dim());
                }
                Some(super::history_search::HistorySearchStatus::NoMatch) => {
                    spans.push(Span::raw("  no match").dim());
                }
                _ => {
                    spans.push(Span::raw("  Enter accept · Esc cancel").dim());
                }
            }
            Line::from(spans)
        } else if !self.shortcuts_visible {
            self.status_line
                .clone()
                .unwrap_or_else(|| Line::from(Span::from("  ? for shortcuts").dim()))
        } else if self.show_agent_picker {
            Line::from(vec![
                Span::raw("  ").dim(),
                Span::styled(self.focused_agent.clone(), accent_style()),
                Span::raw("  Tab switch agent   Enter send   @ files   /model").dim(),
            ])
        } else {
            Line::from(Span::from("  Enter send   @ files   /model").dim())
        };
        // Codex truncates footer/status lines before painting them.  In
        // particular this keeps a long provider/model id from running past
        // the terminal edge while preserving the model name's leading text.
        let hint = crate::line_truncation::truncate_line_with_ellipsis_if_overflow(
            hint,
            usize::from(footer_rect.width),
        );
        hint.render_ref(footer_rect, buf);
    }

    pub(crate) fn cursor_pos_in(&self, composer_rect: Rect) -> Option<(u16, u16)> {
        self.input_width
            .set(usize::from(composer_rect.width.saturating_sub(4).max(1)));
        let prefix_width = UnicodeWidthStr::width("› ") as u16;
        let before_cursor = &self.text[..self.cursor];
        let (line_index, line_text) = before_cursor
            .rsplit_once('\n')
            .map(|(prefix, line)| (prefix.matches('\n').count() + 1, line))
            .unwrap_or((0, before_cursor));
        let input_width = usize::from(composer_rect.width.saturating_sub(4).max(1));
        let wrapped_before = wrap_input_line(line_text, input_width);
        let visual_row = line_index + wrapped_before.len().saturating_sub(1);
        let total_lines = self
            .text
            .split('\n')
            .flat_map(|line| wrap_input_line(line, input_width))
            .count()
            .max(1);
        let max_input_lines = usize::from(composer_rect.height.saturating_sub(2)).max(1);
        let input_start = visual_row
            .saturating_add(1)
            .saturating_sub(max_input_lines)
            .min(total_lines.saturating_sub(1));
        let visible_row = visual_row.saturating_sub(input_start);
        let cursor_width = UnicodeWidthStr::width(
            wrapped_before
                .last()
                .map(String::as_str)
                .unwrap_or_default(),
        ) as u16;
        let prefix_width = if visual_row == 0 { prefix_width } else { 2 };
        Some((
            composer_rect
                .x
                .saturating_add(1)
                .saturating_add(prefix_width)
                .saturating_add(cursor_width)
                .min(composer_rect.right().saturating_sub(1)),
            composer_rect
                .y
                .saturating_add(1)
                .saturating_add(u16::try_from(visible_row).unwrap_or(u16::MAX)),
        ))
    }
}

impl ChatComposer {
    /// Match Codex's composer layout: the input surface has one row of padding
    /// above and below the textarea, while the footer is a sibling row below
    /// it. Keeping these rectangles separate is important because the
    /// user-message background belongs only to the input surface.
    fn layout_areas(&self, area: Rect) -> [Rect; 2] {
        let composer_height = area.height.saturating_sub(FOOTER_HEIGHT);
        [
            Rect::new(area.x, area.y, area.width, composer_height),
            Rect::new(
                area.x,
                area.y.saturating_add(composer_height),
                area.width,
                area.height.saturating_sub(composer_height),
            ),
        ]
    }
}

fn cursor_visual_line(text: &str, cursor: usize, width: usize) -> usize {
    let before_cursor = &text[..cursor.min(text.len())];
    let (line_index, line_text) = before_cursor
        .rsplit_once('\n')
        .map(|(prefix, line)| (prefix.matches('\n').count() + 1, line))
        .unwrap_or((0, before_cursor));
    line_index + wrap_input_line(line_text, width).len().saturating_sub(1)
}

fn beginning_of_previous_word(text: &str, cursor: usize) -> usize {
    let prefix = &text[..cursor.min(text.len())];
    let Some((last_idx, last)) = prefix
        .char_indices()
        .rev()
        .find(|&(_, ch)| !ch.is_whitespace())
    else {
        return 0;
    };
    let separator = is_word_separator(last);
    let mut boundary = last_idx;
    let mut remaining_non_ascii = !last.is_ascii();
    while boundary > 0 {
        let (idx, ch) = prefix[..boundary]
            .char_indices()
            .next_back()
            .expect("boundary is after a character");
        if ch.is_whitespace() || is_word_separator(ch) != separator {
            break;
        }
        // Unicode word boundaries in Codex treat adjacent CJK graphemes as
        // separate navigation targets. Keep the same useful behavior without
        // introducing another text segmentation dependency here.
        if remaining_non_ascii && !ch.is_ascii() {
            break;
        }
        remaining_non_ascii = !ch.is_ascii();
        boundary = idx;
    }
    boundary
}

fn end_of_next_word(text: &str, cursor: usize) -> usize {
    let suffix = &text[cursor.min(text.len())..];
    let Some((first_offset, first)) = suffix.char_indices().find(|&(_, ch)| !ch.is_whitespace())
    else {
        return text.len();
    };
    let mut boundary = cursor.min(text.len()) + first_offset + first.len_utf8();
    let separator = is_word_separator(first);
    let mut previous_non_ascii = !first.is_ascii();
    for (offset, ch) in suffix[first_offset + first.len_utf8()..].char_indices() {
        if ch.is_whitespace() || is_word_separator(ch) != separator {
            break;
        }
        if previous_non_ascii && !ch.is_ascii() {
            break;
        }
        previous_non_ascii = !ch.is_ascii();
        boundary =
            cursor.min(text.len()) + first_offset + first.len_utf8() + offset + ch.len_utf8();
    }
    boundary
}

/// Convert a visual column back to a UTF-8 byte cursor on one logical line.
/// Codex's textarea uses display width rather than byte offsets, which keeps
/// vertical motion stable for CJK and wide glyphs.
fn visual_line_ranges(text: &str, width: usize) -> Vec<(usize, usize)> {
    let width = width.max(1);
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut row_width = 0;
    for (offset, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, offset));
            start = offset + ch.len_utf8();
            row_width = 0;
            continue;
        }
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if start != offset && row_width + ch_width > width {
            ranges.push((start, offset));
            start = offset;
            row_width = 0;
        }
        row_width += ch_width;
    }
    ranges.push((start, text.len()));
    ranges
}

fn current_visual_line(lines: &[(usize, usize)], cursor: usize) -> Option<usize> {
    lines.iter().enumerate().position(|(index, &(start, end))| {
        if start == end {
            return cursor == start;
        }
        if cursor < start || cursor > end {
            return false;
        }
        // At a wrap boundary the cursor belongs to the next visual row; at
        // an explicit newline it still belongs to the row before the newline.
        cursor < end || index + 1 >= lines.len() || lines[index + 1].0 != end
    })
}

fn visual_column_cursor(text: &str, line_start: usize, line_end: usize, target: usize) -> usize {
    let line = &text[line_start..line_end];
    let mut width = 0usize;
    for (offset, ch) in line.char_indices() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if width + ch_width > target {
            return line_start + offset;
        }
        width += ch_width;
        if width == target {
            return line_start + offset + ch.len_utf8();
        }
    }
    line_end
}

fn wrap_input_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut row_width = 0usize;
    for ch in line.chars() {
        let ch_width = UnicodeWidthStr::width(ch.to_string().as_str());
        if !row.is_empty() && row_width + ch_width > width {
            rows.push(std::mem::take(&mut row));
            row_width = 0;
        }
        row.push(ch);
        row_width += ch_width;
    }
    rows.push(row);
    rows
}

#[cfg(test)]
mod tests {
    use crate::render::renderable::Renderable;
    use ratatui::style::{Color, Modifier, Stylize};
    use ratatui::text::{Line, Span};

    use super::ChatComposer;

    #[test]
    fn composer_keeps_utf8_cursor_boundaries_during_editing() {
        let mut composer = ChatComposer::new("Main");
        composer.insert('你');
        composer.insert('a');
        assert_eq!(composer.text(), "你a");

        composer.move_left();
        composer.delete_previous();
        assert_eq!(composer.text(), "a");
        assert_eq!(composer.cursor(), 0);

        composer.delete_next();
        assert!(composer.text().is_empty());
    }

    #[test]
    fn taking_text_resets_the_cursor() {
        let mut composer = ChatComposer::new("Main");
        composer.insert('a');
        assert_eq!(composer.take_text(), "a");
        assert_eq!(composer.cursor(), 0);
        assert!(composer.text().is_empty());
    }

    #[test]
    fn ctrl_c_clears_draft_without_recording_history() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("draft");
        assert_eq!(composer.clear_for_ctrl_c().as_deref(), Some("draft"));
        assert!(composer.text().is_empty());
        assert!(!composer.history_previous());
    }

    #[test]
    fn history_navigation_restores_draft_after_last_entry() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("first");
        assert_eq!(composer.take_text(), "first");
        composer.insert_text("second");
        assert_eq!(composer.take_text(), "second");
        composer.insert_text("draft");

        assert!(composer.history_previous());
        assert_eq!(composer.text(), "second");
        assert!(composer.history_previous());
        assert_eq!(composer.text(), "first");
        assert!(composer.history_next());
        assert_eq!(composer.text(), "second");
        assert!(composer.history_next());
        assert_eq!(composer.text(), "draft");
    }

    #[test]
    fn pasted_text_preserves_newlines() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("a\nb");
        assert_eq!(composer.text(), "a\nb");
        assert_eq!(composer.cursor(), 3);
    }

    #[test]
    fn long_lines_wrap_and_keep_cursor_on_visual_tail() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("abcdefghij");
        assert_eq!(composer.desired_height(10), 5);
        let cursor = composer.cursor_pos(ratatui::layout::Rect::new(0, 0, 10, 8));
        assert_eq!(cursor.map(|(_, y)| y), Some(2));
    }

    #[test]
    fn composer_surface_stops_before_the_footer_row() {
        let composer = ChatComposer::new("Main");
        let [surface, footer] = composer.layout_areas(ratatui::layout::Rect::new(0, 10, 40, 4));
        assert_eq!(surface, ratatui::layout::Rect::new(0, 10, 40, 3));
        assert_eq!(footer, ratatui::layout::Rect::new(0, 13, 40, 1));
    }

    #[test]
    fn footer_stays_at_the_bottom_when_input_wraps() {
        let mut composer = ChatComposer::new("Main");
        composer.set_status_line(Some(Line::from("status")));
        composer.insert_text("abcdefghij");
        let area = ratatui::layout::Rect::new(0, 0, 10, composer.desired_height(10));
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        composer.render(area, &mut buffer);
        let rows = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .filter_map(|x| buffer.cell((x, y)).map(|cell| cell.symbol().to_string()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows.last().map(String::as_str), Some("status    "));
    }

    #[test]
    fn multiline_input_scrolls_to_keep_the_cursor_visible() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("one\ntwo\nthree\nfour\nfive\nsix");
        let area = ratatui::layout::Rect::new(0, 0, 40, 7);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        composer.render(area, &mut buffer);
        let rendered = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(!rendered.contains("one"));
        assert!(rendered.contains("six"));
        let cursor = composer.cursor_pos(area).expect("cursor");
        assert_eq!(cursor.1, 4);
    }

    #[test]
    fn vertical_motion_follows_wrapped_visual_lines() {
        let mut composer = ChatComposer::new("Main");
        composer.input_width.set(6);
        composer.insert_text("abcdefghi\njklmnop");
        composer.cursor = "abcdefghi\njklmn".len();

        assert!(composer.move_up());
        assert_eq!(composer.cursor(), 9);
        assert!(composer.move_up());
        assert_eq!(composer.cursor(), 5);
        assert!(composer.move_down());
        assert_eq!(composer.cursor(), 9);
    }

    #[test]
    fn codex_editing_shortcuts_preserve_utf8_and_line_boundaries() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("你好 world\nnext");
        composer.move_home();
        assert_eq!(composer.cursor(), "你好 world\n".len());
        composer.move_left();
        composer.delete_word_previous();
        assert_eq!(composer.text(), "你好 \nnext");
        composer.move_end();
        assert_eq!(composer.cursor(), "你好 ".len());
        composer.move_right();
        composer.move_end();
        composer.move_home();
        composer.delete_to_end();
        assert_eq!(composer.text(), "你好 \n");
        composer.move_left();
        composer.delete_to_home();
        assert_eq!(composer.text(), "\n");
    }

    #[test]
    fn multiline_vertical_motion_precedes_history_recall() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("ab\ncd\nef");
        composer.move_home();
        assert!(composer.move_up());
        assert_eq!(composer.cursor(), 3);
        composer.move_home();
        assert!(composer.move_up());
        assert_eq!(composer.cursor(), 0);
        assert!(composer.move_down());
        assert_eq!(composer.cursor(), 3);
    }

    #[test]
    fn vertical_motion_uses_display_width_for_wide_glyphs() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("你a\nbcde");
        composer.move_home();
        assert!(composer.move_up());
        assert_eq!(composer.cursor(), 0);
        assert!(composer.move_down());
        composer.move_end();
        assert!(composer.move_up());
        assert_eq!(composer.cursor(), "你a".len());
    }

    #[test]
    fn codex_word_shortcuts_and_yank_round_trip() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("hello world");
        composer.delete_word_previous();
        assert_eq!(composer.text(), "hello ");
        composer.yank();
        assert_eq!(composer.text(), "hello world");
        composer.move_word_left();
        assert_eq!(composer.cursor(), 6);
        composer.delete_word_next();
        assert_eq!(composer.text(), "hello ");
    }

    #[test]
    fn word_motion_matches_codex_separator_and_cjk_boundaries() {
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("foo.bar 你好");
        composer.move_home();
        composer.move_end();
        composer.move_word_left();
        assert_eq!(composer.cursor(), "foo.bar 你".len());
        composer.move_word_left();
        assert_eq!(composer.cursor(), "foo.bar ".len());
        composer.move_word_left();
        assert_eq!(composer.cursor(), "foo.".len());
        composer.move_word_left();
        assert_eq!(composer.cursor(), "foo".len());
    }

    #[test]
    fn reverse_history_search_matches_and_advances() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut composer = ChatComposer::new("Main");
        composer.insert_text("cargo test");
        composer.take_text();
        composer.insert_text("cargo build");
        composer.take_text();
        composer.insert_text("cargo");
        // Open a Codex-style session and type the query, then navigate.
        composer.begin_history_search();
        composer.handle_history_search_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(composer.text(), "cargo build");
        composer
            .handle_history_search_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(composer.text(), "cargo test");
        composer
            .handle_history_search_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(composer.text(), "cargo build");
    }

    #[test]
    fn history_search_uses_codex_footer_mode_until_accept() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut composer = ChatComposer::new("Main");
        composer.record_history("cargo test");
        composer.begin_history_search();
        composer.handle_history_search_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(composer.text(), "cargo test");
        let area = ratatui::layout::Rect::new(0, 0, 60, 5);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        composer.render(area, &mut buffer);
        let rendered = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("reverse-i-search: c"));
        composer.history_search = None;
        assert!(!composer.history_search_active());
    }

    #[test]
    fn cancelling_search_restores_the_original_draft() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut composer = ChatComposer::new("Main");
        composer.record_history("cargo test");
        composer.insert_text("my draft");
        composer.begin_history_search();
        composer.handle_history_search_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(composer.text(), "cargo test");
        composer.handle_history_search_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(composer.text(), "my draft");
        assert!(!composer.history_search_active());
    }

    #[test]
    fn focused_agent_footer_is_not_dimmed_with_shortcuts() {
        let mut composer = ChatComposer::new("Main");
        composer.toggle_shortcuts();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 4)).expect("terminal");
        terminal
            .draw(|frame| composer.render(frame.area(), frame.buffer_mut()))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let main_column = (0..80)
            .find(|&x| buffer.cell((x, 3)).is_some_and(|cell| cell.symbol() == "M"))
            .expect("agent label");
        let cell = buffer.cell((main_column, 3)).expect("agent cell");
        assert_eq!(cell.fg, Color::Cyan);
        assert!(!cell.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn empty_composer_renders_codex_status_line_when_shortcuts_are_hidden() {
        let mut composer = ChatComposer::new("Main");
        composer.set_status_line(Some(Line::from(vec![
            Span::styled(
                "gpt-5.6-luna",
                ratatui::style::Style::default().fg(Color::Cyan),
            ),
            Span::from(" · ").dim(),
            Span::styled("~", ratatui::style::Style::default().fg(Color::Green)),
        ])));
        let area = ratatui::layout::Rect::new(0, 0, 80, 4);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        composer.render(area, &mut buffer);
        let rendered = buffer
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(rendered.contains("Implement {feature}"));
        assert!(rendered.contains("gpt-5.6-luna"));
        assert!(rendered.contains("~"));
    }

    #[test]
    fn long_status_line_is_truncated_before_footer_render() {
        let mut composer = ChatComposer::new("Main");
        composer.set_status_line(Some(Line::from(
            "provider/this-is-a-very-long-model-name · ~/project",
        )));
        let area = ratatui::layout::Rect::new(0, 0, 20, 4);
        let mut buffer = ratatui::buffer::Buffer::empty(area);
        composer.render(area, &mut buffer);
        let footer = (0..area.width)
            .map(|x| buffer[(x, 3)].symbol())
            .collect::<String>();
        assert!(footer.contains('…'));
        assert!(!footer.contains("project"));
    }
}
