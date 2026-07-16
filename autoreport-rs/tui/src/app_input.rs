//! Editor input, completion, and message submission for the terminal UI.

use crate::app::Tui;
use crate::app_state::{Cell, Mention, SysKind};
use crate::chatwidget::{extract_mentions, is_mention_char, read_capped};
use crate::slash_command::{self, SlashCompletion};
use autoreport_core::types::{AgentType, MessageSource};

const MENTION_LIMIT: usize = 8;

impl Tui {
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
        let end = m.cursor.min(self.input.len());
        let mut new_input = String::new();
        new_input.push_str(&self.input[..after_at]);
        new_input.push_str(&path);
        new_input.push_str(&self.input[end..]);
        new_input.push(' ');
        self.cursor = after_at + path.len() + 1;
        self.input = new_input;
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
        self.input = format!("/{} ", cmd.name);
        self.cursor = self.input.len();
    }

    pub(crate) fn recompute_slash(&mut self) {
        if !self.input.starts_with('/') || self.cursor == 0 {
            self.slash = None;
            return;
        }
        let typed = &self.input[1..self.cursor.min(self.input.len())];
        if typed.chars().any(char::is_whitespace) {
            self.slash = None;
            return;
        }
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
        let bytes = self.input.as_bytes();
        if self.cursor == 0 || self.cursor > bytes.len() {
            self.mention = None;
            return;
        }
        let mut i = self.cursor;
        let mut query_len = 0usize;
        while i > 0 {
            let c = self.input[..i]
                .chars()
                .next_back()
                .expect("non-empty prefix");
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
            || self.input[..at_idx]
                .chars()
                .next_back()
                .is_none_or(char::is_whitespace);
        if !prev_ok {
            self.mention = None;
            return;
        }
        let query = &self.input[at_idx + 1..at_idx + 1 + query_len];
        self.mention = Some(Mention {
            start: at_idx,
            cursor: self.cursor,
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
    }

    pub(crate) fn submit(&mut self) {
        self.mention = None;
        self.slash = None;
        let text = std::mem::take(&mut self.input).trim().to_string();
        self.cursor = 0;
        if text.is_empty() {
            return;
        }
        if let Some(command) = text.strip_prefix('/') {
            self.run_command(command.trim());
            return;
        }
        self.history.push(Cell::User {
            agent: self.focused,
            text: text.clone(),
        });
        let mut expanded = self.expand_mentions(&text);
        if self.ide_enabled {
            match crate::ide_context::fetch_ide_context(&self.workspace) {
                Ok(context) => {
                    self.ide_warned = false;
                    if let Some(prefixed) =
                        crate::ide_context::apply_ide_context_to_text(&context, &expanded)
                    {
                        expanded = prefixed;
                    }
                }
                Err(error) if !self.ide_warned => {
                    self.ide_warned = true;
                    self.system(
                        &format!("IDE context skipped: {}", error.prompt_skip_hint()),
                        SysKind::Info,
                    );
                }
                Err(_) => {}
            }
        }
        self.manager
            .submit(self.focused, expanded, MessageSource::User);
    }

    fn expand_mentions(&self, text: &str) -> String {
        const MENTION_CAP: usize = 16_000;
        let refs = extract_mentions(text);
        if refs.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();
        out.push_str("\n\n# Referenced files");
        for rel in refs {
            match read_capped(&self.workspace.join(&rel), MENTION_CAP) {
                Some(content) => out.push_str(&format!("\n\n## @{rel}\n```\n{content}\n```")),
                None => out.push_str(&format!("\n\n## @{rel}\n(not found)")),
            }
        }
        out
    }
}
