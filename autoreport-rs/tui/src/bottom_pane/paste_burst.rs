//! Codex's non-bracketed paste burst state machine.
//!
//! Some terminals deliver a paste as a rapid sequence of ordinary key events.
//! Keeping this state machine separate from the composer lets the event router
//! decide whether to insert a typed character, buffer a paste, or turn Enter
//! into a newline without duplicating timing heuristics.

use std::time::{Duration, Instant};

const PASTE_BURST_MIN_CHARS: u16 = 3;
const PASTE_ENTER_SUPPRESS_WINDOW: Duration = Duration::from_millis(120);
const PASTE_BURST_CHAR_INTERVAL: Duration = Duration::from_millis(8);
#[cfg(not(windows))]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(8);
#[cfg(windows)]
const PASTE_BURST_ACTIVE_IDLE_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Default)]
pub(crate) struct PasteBurst {
    last_plain_char_time: Option<Instant>,
    consecutive_plain_char_burst: u16,
    burst_window_until: Option<Instant>,
    buffer: String,
    active: bool,
    pending_first_char: Option<(char, Instant)>,
}

pub(crate) enum CharDecision {
    BeginBuffer { retro_chars: u16 },
    BufferAppend,
    RetainFirstChar,
    BeginBufferFromPending,
}

pub(crate) struct RetroGrab {
    pub(crate) start_byte: usize,
    #[allow(dead_code)]
    pub(crate) grabbed: String,
}

pub(crate) enum FlushResult {
    Paste(String),
    Typed(char),
    None,
}

impl PasteBurst {
    pub(crate) fn recommended_flush_delay() -> Duration {
        PASTE_BURST_CHAR_INTERVAL + Duration::from_millis(1)
    }

    pub(crate) fn on_plain_char(&mut self, ch: char, now: Instant) -> CharDecision {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BufferAppend;
        }
        if let Some((held, held_at)) = self.pending_first_char
            && now.duration_since(held_at) <= PASTE_BURST_CHAR_INTERVAL
        {
            self.active = true;
            self.pending_first_char = None;
            self.buffer.push(held);
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return CharDecision::BeginBufferFromPending;
        }
        if self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS {
            return CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            };
        }
        self.pending_first_char = Some((ch, now));
        CharDecision::RetainFirstChar
    }

    pub(crate) fn on_plain_char_no_hold(&mut self, now: Instant) -> Option<CharDecision> {
        self.note_plain_char(now);
        if self.active {
            self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
            return Some(CharDecision::BufferAppend);
        }
        (self.consecutive_plain_char_burst >= PASTE_BURST_MIN_CHARS).then_some(
            CharDecision::BeginBuffer {
                retro_chars: self.consecutive_plain_char_burst.saturating_sub(1),
            },
        )
    }

    fn note_plain_char(&mut self, now: Instant) {
        self.consecutive_plain_char_burst = match self.last_plain_char_time {
            Some(previous) if now.duration_since(previous) <= PASTE_BURST_CHAR_INTERVAL => {
                self.consecutive_plain_char_burst.saturating_add(1)
            }
            _ => 1,
        };
        self.last_plain_char_time = Some(now);
    }

    pub(crate) fn flush_if_due(&mut self, now: Instant) -> FlushResult {
        let timeout = if self.is_active_internal() {
            PASTE_BURST_ACTIVE_IDLE_TIMEOUT
        } else {
            PASTE_BURST_CHAR_INTERVAL
        };
        let timed_out = self
            .last_plain_char_time
            .is_some_and(|time| now.duration_since(time) > timeout);
        if !timed_out {
            return FlushResult::None;
        }
        if self.is_active_internal() {
            self.active = false;
            return FlushResult::Paste(std::mem::take(&mut self.buffer));
        }
        self.pending_first_char
            .take()
            .map_or(FlushResult::None, |(ch, _)| FlushResult::Typed(ch))
    }

    pub(crate) fn append_newline_if_active(&mut self, now: Instant) -> bool {
        if !self.is_active() {
            return false;
        }
        self.buffer.push('\n');
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
        true
    }

    pub(crate) fn newline_should_insert_instead_of_submit(&self, now: Instant) -> bool {
        self.is_active() || self.burst_window_until.is_some_and(|until| now <= until)
    }

    #[allow(dead_code)]
    pub(crate) fn direct_insert_newline_should_insert(&self, now: Instant) -> bool {
        self.newline_should_insert_instead_of_submit(now)
            || self
                .last_plain_char_time
                .is_some_and(|time| now.duration_since(time) <= PASTE_BURST_CHAR_INTERVAL)
    }

    pub(crate) fn extend_window(&mut self, now: Instant) {
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    pub(crate) fn begin_with_retro_grabbed(&mut self, grabbed: String, now: Instant) {
        self.buffer.push_str(&grabbed);
        self.active = true;
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    pub(crate) fn append_char_to_buffer(&mut self, ch: char, now: Instant) {
        self.buffer.push(ch);
        self.burst_window_until = Some(now + PASTE_ENTER_SUPPRESS_WINDOW);
    }

    #[allow(dead_code)]
    pub(crate) fn try_append_char_if_active(&mut self, ch: char, now: Instant) -> bool {
        if self.active || !self.buffer.is_empty() {
            self.append_char_to_buffer(ch, now);
            true
        } else {
            false
        }
    }

    pub(crate) fn decide_begin_buffer(
        &mut self,
        now: Instant,
        before: &str,
        retro_chars: usize,
    ) -> Option<RetroGrab> {
        let start_byte = retro_start_index(before, retro_chars);
        let grabbed = before[start_byte..].to_string();
        if grabbed.chars().any(char::is_whitespace) || grabbed.chars().count() >= 16 {
            self.begin_with_retro_grabbed(grabbed.clone(), now);
            Some(RetroGrab {
                start_byte,
                grabbed,
            })
        } else {
            None
        }
    }

    pub(crate) fn flush_before_modified_input(&mut self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        self.active = false;
        let mut out = std::mem::take(&mut self.buffer);
        if let Some((ch, _)) = self.pending_first_char.take() {
            out.push(ch);
        }
        Some(out)
    }

    pub(crate) fn clear_window_after_non_char(&mut self) {
        self.consecutive_plain_char_burst = 0;
        self.last_plain_char_time = None;
        self.burst_window_until = None;
        self.active = false;
        self.pending_first_char = None;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.is_active_internal() || self.pending_first_char.is_some()
    }

    fn is_active_internal(&self) -> bool {
        self.active || !self.buffer.is_empty()
    }

    pub(crate) fn clear_after_explicit_paste(&mut self) {
        self.last_plain_char_time = None;
        self.consecutive_plain_char_burst = 0;
        self.burst_window_until = None;
        self.active = false;
        self.buffer.clear();
        self.pending_first_char = None;
    }
}

pub(crate) fn retro_start_index(before: &str, retro_chars: usize) -> usize {
    if retro_chars == 0 {
        return before.len();
    }
    before
        .char_indices()
        .rev()
        .nth(retro_chars.saturating_sub(1))
        .map(|(index, _)| index)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_chars_are_flushed_as_one_paste() {
        let mut burst = PasteBurst::default();
        let start = Instant::now();
        assert!(matches!(
            burst.on_plain_char('a', start),
            CharDecision::RetainFirstChar
        ));
        assert!(matches!(
            burst.on_plain_char('b', start + Duration::from_millis(1)),
            CharDecision::BeginBufferFromPending
        ));
        burst.append_char_to_buffer('b', start + Duration::from_millis(1));
        assert!(matches!(
            burst.flush_if_due(start + Duration::from_millis(20)),
            FlushResult::Paste(text) if text == "ab"
        ));
    }

    #[test]
    fn single_fast_char_is_flushed_as_typed() {
        let mut burst = PasteBurst::default();
        let start = Instant::now();
        assert!(matches!(
            burst.on_plain_char('a', start),
            CharDecision::RetainFirstChar
        ));
        assert!(matches!(
            burst.flush_if_due(
                start + PasteBurst::recommended_flush_delay() + Duration::from_millis(1)
            ),
            FlushResult::Typed('a')
        ));
    }
}
