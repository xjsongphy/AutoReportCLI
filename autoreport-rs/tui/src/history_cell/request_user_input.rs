//! Completed request_user_input transcript rendering, adapted from Codex.

use super::*;
use crate::wrapping::{RtOptions, adaptive_wrap_line};
use autoreport_core::request_user_input::{RequestUserInputAnswer, RequestUserInputQuestion};
use ratatui::style::{Color, Style};
use std::collections::HashMap;

pub(crate) fn display(
    questions: &[RequestUserInputQuestion],
    answers: &HashMap<String, RequestUserInputAnswer>,
    interrupted: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let total = questions.len();
    let answered = questions
        .iter()
        .filter(|question| {
            answers
                .get(&question.id)
                .is_some_and(|a| !a.answers.is_empty())
        })
        .count();
    let mut lines = vec![Line::from(vec![
        "•".dim(),
        " ".into(),
        "Questions".bold(),
        format!(" {answered}/{total} answered").dim(),
        if interrupted {
            " (interrupted)".cyan()
        } else {
            "".into()
        },
    ])];
    for question in questions {
        let answer = answers.get(&question.id).filter(|a| !a.answers.is_empty());
        let mut q = wrap(&question.question, width, "  • ", "    ", Style::default());
        if answer.is_none() {
            if let Some(last) = q.last_mut() {
                last.spans.push(" (unanswered)".dim());
            }
        }
        lines.extend(q);
        let Some(answer) = answer else { continue };
        if question.is_secret {
            lines.extend(wrap(
                "••••••",
                width,
                "    answer: ",
                "            ",
                Style::default().fg(Color::Cyan),
            ));
        } else {
            for value in &answer.answers {
                let (prefix, continuation) = if value.starts_with("user_note: ") {
                    ("    note: ", "          ")
                } else {
                    ("    answer: ", "            ")
                };
                let value = value.strip_prefix("user_note: ").unwrap_or(value);
                lines.extend(wrap(
                    value,
                    width,
                    prefix,
                    continuation,
                    Style::default().fg(Color::Cyan),
                ));
            }
        }
    }
    lines
}

pub(crate) fn raw_lines(
    questions: &[RequestUserInputQuestion],
    answers: &HashMap<String, RequestUserInputAnswer>,
    interrupted: bool,
) -> Vec<Line<'static>> {
    let total = questions.len();
    let answered = questions
        .iter()
        .filter(|question| {
            answers
                .get(&question.id)
                .is_some_and(|a| !a.answers.is_empty())
        })
        .count();
    let mut lines = vec![Line::from(format!("Questions {answered}/{total} answered"))];
    if interrupted {
        lines.push(Line::from("(interrupted)"));
    }
    for question in questions {
        lines.push(Line::from(question.question.clone()));
        match answers.get(&question.id).filter(|a| !a.answers.is_empty()) {
            Some(_answer) if question.is_secret => lines.push(Line::from("answer: ******")),
            Some(answer) => lines.extend(answer.answers.iter().map(|value| {
                if let Some(note) = value.strip_prefix("user_note: ") {
                    Line::from(format!("note: {note}"))
                } else {
                    Line::from(format!("answer: {value}"))
                }
            })),
            None => lines.push(Line::from("(unanswered)")),
        }
    }
    lines
}

fn wrap(
    text: &str,
    width: u16,
    initial: &str,
    continuation: &str,
    style: Style,
) -> Vec<Line<'static>> {
    let line = Line::from(Span::styled(text.to_string(), style));
    let wrapped = adaptive_wrap_line(
        &line,
        RtOptions::new(usize::from(width.max(1)))
            .initial_indent(initial.into())
            .subsequent_indent(continuation.into()),
    );
    let mut out = Vec::new();
    push_owned_lines(&wrapped, &mut out);
    out
}
