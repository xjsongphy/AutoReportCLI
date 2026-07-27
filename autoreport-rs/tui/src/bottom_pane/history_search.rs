//! Incremental input-history search, adapted from Codex's
//! `bottom_pane/chat_composer/history_search.rs` +
//! `chat_composer_history.rs`.
//!
//! Codex's reverse-i-search (`Ctrl+R`) / forward-i-search (`Ctrl+S`) is a
//! *session*: opening it snapshots the current draft, then every typed character
//! extends the query (not the draft) and re-runs a case-insensitive substring
//! search through prior inputs. `Ctrl+R`/`Up` move to the next older match,
//! `Ctrl+S`/`Down` to the next newer one, `Enter` accepts the current match,
//! and `Esc`/`Ctrl+C` cancels and restores the original draft.
//!
//! This is a local-only port: Codex also keeps a persistent cross-session log
//! fetched on demand via its history service; AutoReport has no such daemon, so
//! the search runs only over the in-session `history: Vec<String>` that Up/Down
//! recall already uses. The session lifecycle, query accumulation, status
//! (`Searching`/`Match`/`NoMatch`), draft snapshot/restore, and case-insensitive
//! matching are ported verbatim from Codex.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Mirrors Codex's `has_ctrl_or_alt`: a key carries a composing modifier that
/// means it is NOT plain text input (so it must not be appended to the query).
fn has_ctrl_or_alt(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// Live status of a search session, mirroring Codex's `HistorySearchStatus`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistorySearchStatus {
    Idle,
    Searching,
    Match,
    NoMatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Older,
    Newer,
}

/// Outcome of dispatching one key to an open search session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchKeyOutcome {
    /// Stay in the session (query updated or navigation happened).
    Continue,
    /// User accepted the current match (`Enter` with a match). The composer's
    /// draft already holds the matched entry; the caller should submit it.
    Accept,
    /// User cancelled (`Esc` / `Ctrl+C`). The original draft was restored.
    Cancel,
    /// Key is not part of the search contract and was ignored.
    Ignored,
}

/// A composer-owned reverse/forward-i-search session.
#[derive(Clone, Debug)]
pub(crate) struct HistorySearchSession {
    /// Draft (text + cursor) captured when the session opened, restored on
    /// cancel and on a no-match-with-empty-query retreat.
    original_draft: String,
    original_cursor: usize,
    query: String,
    status: HistorySearchStatus,
    /// Index into the history vector of the current match, if any.
    match_index: Option<usize>,
}

impl HistorySearchSession {
    pub(crate) fn new(original_draft: String, original_cursor: usize) -> Self {
        Self {
            original_draft,
            original_cursor,
            query: String::new(),
            status: HistorySearchStatus::Idle,
            match_index: None,
        }
    }

    pub(crate) fn status(&self) -> HistorySearchStatus {
        self.status
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    /// The draft captured when the session opened (restored on cancel).
    pub(crate) fn original_draft(&self) -> &str {
        &self.original_draft
    }

    /// The cursor captured when the session opened (restored on cancel).
    pub(crate) fn original_cursor(&self) -> usize {
        self.original_cursor
    }
}

/// Case-insensitive substring search in `direction`, starting just before/after
/// `from` (exclusive of `from`). Mirrors Codex's match predicate.
fn find_match(history: &[String], query: &str, from: usize, direction: Direction) -> Option<usize> {
    if query.is_empty() {
        return None;
    }
    let needle = query.to_lowercase();
    match direction {
        Direction::Older => (0..from).rev().find(|&i| {
            history
                .get(i)
                .is_some_and(|h| h.to_lowercase().contains(&needle))
        }),
        Direction::Newer => (from..history.len()).find(|&i| {
            history
                .get(i)
                .is_some_and(|h| h.to_lowercase().contains(&needle))
        }),
    }
}

/// Run the search for the current query, starting from the latest entry (used
/// when the query changes). Sets `status` and `match_index` accordingly.
fn search_from_latest(
    history: &[String],
    session: &mut HistorySearchSession,
    text: &mut String,
    cursor: &mut usize,
) {
    if session.query.is_empty() {
        session.status = HistorySearchStatus::Idle;
        session.match_index = None;
        return;
    }
    session.status = HistorySearchStatus::Searching;
    match find_match(history, &session.query, history.len(), Direction::Older) {
        Some(index) => {
            session.match_index = Some(index);
            session.status = HistorySearchStatus::Match;
            *text = history[index].clone();
            *cursor = text.len();
        }
        None => {
            session.match_index = None;
            session.status = HistorySearchStatus::NoMatch;
            // No match for the extended query: restore the draft captured when
            // the session opened (NOT the previously matched entry), mirroring
            // Codex's `apply_history_search_result` NotFound arm. The query is
            // retained so the user can keep editing.
            *text = session.original_draft.clone();
            *cursor = session.original_cursor;
        }
    }
}

/// Advance to the next match in `direction` from the current match. On a
/// boundary (matches exist but none further in this direction) the status
/// stays `Match` and the current preview is left intact, mirroring Codex's
/// `AtBoundary` result. The "failing"/NoMatch state is reserved for a query
/// that matches nothing at all.
fn advance(
    history: &[String],
    session: &mut HistorySearchSession,
    text: &mut String,
    cursor: &mut usize,
    direction: Direction,
) {
    if session.query.is_empty() {
        // Empty query matches nothing to advance to; Codex early-returns Idle.
        session.status = HistorySearchStatus::Idle;
        session.match_index = None;
        return;
    }
    let start = match (direction, session.match_index) {
        (Direction::Older, Some(i)) => i,
        (Direction::Newer, Some(i)) => i.saturating_add(1),
        _ => match direction {
            Direction::Older => history.len(),
            Direction::Newer => 0,
        },
    };
    match find_match(history, &session.query, start, direction) {
        Some(index) => {
            session.match_index = Some(index);
            session.status = HistorySearchStatus::Match;
            *text = history[index].clone();
            *cursor = text.len();
        }
        None => {
            // AtBoundary: we had a match but there is no further one in this
            // direction. Keep `Match` and leave the current preview intact
            // (Codex `AtBoundary`), instead of flipping to a "failing" state.
            if session.match_index.is_some() {
                session.status = HistorySearchStatus::Match;
            }
        }
    }
}

/// Dispatch one key to an open session. `text` / `cursor` are the composer's
/// live draft; the session mutates them to show the current match (or the
/// restored original draft on cancel).
pub(crate) fn handle_search_key(
    history: &[String],
    session: &mut HistorySearchSession,
    text: &mut String,
    cursor: &mut usize,
    key: KeyEvent,
) -> SearchKeyOutcome {
    // Some terminals emit both Press and Release for each physical keypress;
    // ignore Release so a single keystroke is not processed twice (Codex's
    // first line in `handle_history_search_key`).
    if key.kind == KeyEventKind::Release {
        return SearchKeyOutcome::Ignored;
    }
    // Ctrl+R / Ctrl+S navigate regardless of the query.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('r') => {
                advance(history, session, text, cursor, Direction::Older);
                return SearchKeyOutcome::Continue;
            }
            KeyCode::Char('s') => {
                advance(history, session, text, cursor, Direction::Newer);
                return SearchKeyOutcome::Continue;
            }
            KeyCode::Char('c') => {
                cancel(session, text, cursor);
                return SearchKeyOutcome::Cancel;
            }
            KeyCode::Char('u') => {
                session.query.clear();
                search_from_latest(history, session, text, cursor);
                return SearchKeyOutcome::Continue;
            }
            KeyCode::Char('h') => {
                pop_query(history, session, text, cursor);
                return SearchKeyOutcome::Continue;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Up => {
            advance(history, session, text, cursor, Direction::Older);
            SearchKeyOutcome::Continue
        }
        KeyCode::Down => {
            advance(history, session, text, cursor, Direction::Newer);
            SearchKeyOutcome::Continue
        }
        KeyCode::Esc => {
            cancel(session, text, cursor);
            SearchKeyOutcome::Cancel
        }
        KeyCode::Enter => {
            if session.status == HistorySearchStatus::Match {
                SearchKeyOutcome::Accept
            } else {
                SearchKeyOutcome::Continue
            }
        }
        KeyCode::Backspace => {
            pop_query(history, session, text, cursor);
            SearchKeyOutcome::Continue
        }
        KeyCode::Char(ch) if !has_ctrl_or_alt(key.modifiers) => {
            session.query.push(ch);
            search_from_latest(history, session, text, cursor);
            SearchKeyOutcome::Continue
        }
        _ => SearchKeyOutcome::Ignored,
    }
}

fn pop_query(
    history: &[String],
    session: &mut HistorySearchSession,
    text: &mut String,
    cursor: &mut usize,
) {
    session.query.pop();
    search_from_latest(history, session, text, cursor);
}

fn cancel(session: &mut HistorySearchSession, text: &mut String, cursor: &mut usize) {
    *text = session.original_draft.clone();
    *cursor = session.original_cursor;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn history() -> Vec<String> {
        vec![
            "cargo test".to_string(),
            "cargo build".to_string(),
            "git status".to_string(),
        ]
    }

    #[test]
    fn typing_query_finds_latest_match_case_insensitively() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        // 'g' → matches "git status" (index 2, the latest g-entry)
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert_eq!(text, "git status");
        assert_eq!(session.status(), HistorySearchStatus::Match);
        assert_eq!(session.match_index, Some(2));
    }

    #[test]
    fn ctrl_r_advances_to_older_match() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        assert_eq!(text, "cargo build");
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(text, "cargo test");
        // No older "c" match → AtBoundary: stay `Match`, preview retained
        // (mirrors codex; "failing"/NoMatch is reserved for a query that
        // matches nothing at all).
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(session.status(), HistorySearchStatus::Match);
        assert_eq!(text, "cargo test");
    }

    #[test]
    fn ctrl_s_advances_to_newer_match() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        assert_eq!(text, "cargo test");
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
        );
        assert_eq!(text, "cargo build");
        assert_eq!(session.status(), HistorySearchStatus::Match);
    }

    #[test]
    fn enter_accepts_only_on_match_else_continues() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        // No query yet → Idle → Enter does not accept.
        let out = handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(out, SearchKeyOutcome::Continue);
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        let out = handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert_eq!(out, SearchKeyOutcome::Accept);
    }

    #[test]
    fn esc_restores_original_draft() {
        let history = history();
        let mut session = HistorySearchSession::new("draft text".to_string(), 10);
        let mut text = String::new();
        let mut cursor = 0;
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('g'), KeyModifiers::NONE),
        );
        assert_eq!(text, "git status");
        let out = handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert_eq!(out, SearchKeyOutcome::Cancel);
        assert_eq!(text, "draft text");
        assert_eq!(cursor, 10);
    }

    #[test]
    fn backspace_pops_query_and_re_searches() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        // "ca" still matches "cargo build" (latest)
        assert_eq!(text, "cargo build");
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Backspace, KeyModifiers::NONE),
        );
        assert_eq!(session.query(), "c");
    }

    /// I-3: extending a matched query to one with no match restores the
    /// original draft (not the previously matched entry). Mirrors codex's
    /// `history_search_no_match_restores_preview_but_keeps_search_open`.
    #[test]
    fn no_match_restores_original_draft() {
        let history = history();
        let mut session = HistorySearchSession::new("my draft".to_string(), 8);
        let mut text = "my draft".to_string();
        let mut cursor = 8;
        // 'c' matches "cargo build"; then 'x' → "cx" matches nothing.
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('c'), KeyModifiers::NONE),
        );
        assert_eq!(text, "cargo build");
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert_eq!(session.status(), HistorySearchStatus::NoMatch);
        assert_eq!(text, "my draft", "no-match must restore original draft");
    }

    /// I-1: Alt-modified chars must NOT be appended to the query (word-motion
    /// keys like Alt+b/Alt+f would otherwise corrupt the search).
    #[test]
    fn alt_modified_char_does_not_corrupt_query() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        handle_search_key(
            &history,
            &mut session,
            &mut text,
            &mut cursor,
            key(KeyCode::Char('b'), KeyModifiers::ALT),
        );
        assert!(
            session.query().is_empty(),
            "Alt+char must not enter the query"
        );
        assert_eq!(session.status(), HistorySearchStatus::Idle);
    }

    /// I-2: Release events are ignored so a keypress is not double-processed on
    /// terminals that emit both Press and Release.
    #[test]
    fn release_event_is_ignored() {
        let history = history();
        let mut session = HistorySearchSession::new(String::new(), 0);
        let mut text = String::new();
        let mut cursor = 0;
        let mut evt = key(KeyCode::Char('g'), KeyModifiers::NONE);
        evt.kind = crossterm::event::KeyEventKind::Release;
        let out = handle_search_key(&history, &mut session, &mut text, &mut cursor, evt);
        assert_eq!(out, SearchKeyOutcome::Ignored);
        assert!(
            session.query().is_empty(),
            "Release must not append to query"
        );
    }
}
