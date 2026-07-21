//! Reducer and keyboard handling for Codex's `request_user_input` prompt.

use crate::app::Tui;
use crate::app_state::{PendingUserInput, SysKind};
use autoreport_core::request_user_input::{RequestUserInputAnswer, RequestUserInputResponse};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

impl Tui {
    pub(crate) fn insert_user_input_text(&mut self, text: &str) {
        let Some(req) = self.pending_user_inputs.front_mut() else {
            return;
        };
        let Some(question) = req.question() else {
            return;
        };
        let option_count = question.options.as_ref().map_or(0, Vec::len);
        if option_count > 0 && !(question.is_other && req.selected == option_count) {
            return;
        }
        req.draft.insert_str(req.cursor, text);
        req.cursor += text.len();
    }

    pub(crate) fn handle_user_input_key(&mut self, key: KeyEvent) {
        let Some(req) = self.pending_user_inputs.front_mut() else {
            return;
        };
        let Some(question) = req.question().cloned() else {
            return;
        };
        let option_count = question.options.as_ref().map_or(0, Vec::len);
        let max_selected = if question.is_other {
            option_count
        } else {
            option_count.saturating_sub(1)
        };
        match key.code {
            KeyCode::Up => req.selected = req.selected.saturating_sub(1),
            KeyCode::Down => req.selected = (req.selected + 1).min(max_selected),
            KeyCode::Backspace => {
                if req.cursor > 0 {
                    if let Some(ch) = req.draft[..req.cursor].chars().next_back() {
                        req.cursor -= ch.len_utf8();
                        req.draft.remove(req.cursor);
                    }
                }
            }
            KeyCode::Left => {
                if req.cursor > 0 {
                    req.cursor -= req.draft[..req.cursor]
                        .chars()
                        .next_back()
                        .map_or(0, char::len_utf8);
                }
            }
            KeyCode::Right => {
                if req.cursor < req.draft.len() {
                    req.cursor += req.draft[req.cursor..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                }
            }
            KeyCode::Enter => self.submit_user_input_answer(),
            KeyCode::Esc => self.cancel_user_input(),
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                let editable =
                    option_count == 0 || question.is_other && req.selected == option_count;
                if editable {
                    req.draft.insert(req.cursor, ch);
                    req.cursor += ch.len_utf8();
                }
            }
            _ => {}
        }
    }

    fn submit_user_input_answer(&mut self) {
        let Some(mut req) = self.pending_user_inputs.pop_front() else {
            return;
        };
        let Some(question) = req.question().cloned() else {
            return;
        };
        let answer = question
            .options
            .as_ref()
            .and_then(|options| options.get(req.selected))
            .map(|option| option.label.clone())
            .or_else(|| question.is_other.then(|| req.draft.clone()))
            .unwrap_or(req.draft.clone());
        req.answers.insert(question.id, answer);
        if req.question_index + 1 < req.questions.len() {
            req.question_index += 1;
            req.selected = 0;
            req.draft.clear();
            req.cursor = 0;
            self.pending_user_inputs.push_front(req);
        } else {
            let answers = req
                .answers
                .into_iter()
                .map(|(id, answer)| {
                    (
                        id,
                        RequestUserInputAnswer {
                            answers: vec![answer],
                        },
                    )
                })
                .collect();
            self.resolve_user_input(req.call_id, RequestUserInputResponse { answers });
        }
    }

    pub(crate) fn cancel_user_input(&mut self) {
        let Some(req) = self.pending_user_inputs.pop_front() else {
            return;
        };
        self.resolve_user_input(
            req.call_id,
            RequestUserInputResponse {
                answers: HashMap::new(),
            },
        );
        self.system("user input cancelled", SysKind::Info);
    }

    pub(crate) fn cancel_all_user_inputs(&mut self) {
        while let Some(req) = self.pending_user_inputs.pop_front() {
            self.resolve_user_input(
                req.call_id,
                RequestUserInputResponse {
                    answers: HashMap::new(),
                },
            );
        }
    }

    fn resolve_user_input(&self, call_id: String, response: RequestUserInputResponse) {
        let bus = self.bus.clone();
        tokio::spawn(async move {
            let _ = bus.resolve_user_input(&call_id, response).await;
        });
    }

    pub(crate) fn poll_user_input_deadlines(&mut self) {
        let timed_out = self
            .pending_user_inputs
            .front()
            .is_some_and(PendingUserInput::timed_out);
        if !timed_out {
            return;
        }
        let Some(req) = self.pending_user_inputs.pop_front() else {
            return;
        };
        let mut answers = HashMap::new();
        for (id, answer) in req.answers {
            answers.insert(
                id,
                RequestUserInputAnswer {
                    answers: vec![answer],
                },
            );
        }
        for question in req.questions.iter().skip(req.question_index) {
            let answer = question
                .options
                .as_ref()
                .and_then(|options| options.first())
                .map(|option| option.label.clone())
                .unwrap_or_default();
            answers.insert(
                question.id.clone(),
                RequestUserInputAnswer {
                    answers: vec![answer],
                },
            );
        }
        self.resolve_user_input(req.call_id, RequestUserInputResponse { answers });
        self.system("user input auto-resolved", SysKind::Info);
    }
}
