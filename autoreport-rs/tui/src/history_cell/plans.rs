//! Plan-update history cell adapted directly from Codex's history cell.

use crate::line_utils::{prefix_lines, push_owned_lines};
use crate::wrapping::{RtOptions, adaptive_wrap_line};
use autoreport_core::types::TaskStatus;
use ratatui::style::{Style, Styled, Stylize};
use ratatui::text::Line;

pub(crate) fn display(
    explanation: &Option<String>,
    steps: &[(String, TaskStatus)],
    width: u16,
) -> Vec<Line<'static>> {
    let render_note = |text: &str| -> Vec<Line<'static>> {
        let wrap_width = width.saturating_sub(4).max(1) as usize;
        let note = Line::from(text.to_string().dim().italic());
        let wrapped = adaptive_wrap_line(&note, RtOptions::new(wrap_width));
        let mut out = Vec::new();
        push_owned_lines(&wrapped, &mut out);
        out
    };

    let render_step = |status: &TaskStatus, text: &str| -> Vec<Line<'static>> {
        let (box_str, step_style) = match status {
            TaskStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
            TaskStatus::InProgress => ("□ ", Style::default().cyan().bold()),
            _ => ("□ ", Style::default().dim()),
        };
        let opts = RtOptions::new(width.saturating_sub(4).max(1) as usize)
            .initial_indent(box_str.into())
            .subsequent_indent("  ".into());
        let step = Line::from(text.to_string().set_style(step_style));
        let wrapped = adaptive_wrap_line(&step, opts);
        let mut out = Vec::new();
        push_owned_lines(&wrapped, &mut out);
        out
    };

    let mut body = Vec::new();
    if let Some(explanation) = explanation
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        body.extend(render_note(explanation));
    }
    if steps.is_empty() {
        body.push(Line::from("(no steps provided)".dim().italic()));
    } else {
        for (brief, status) in steps {
            body.extend(render_step(status, brief));
        }
    }

    let mut lines = vec![vec!["• ".dim(), "Updated Plan".bold()].into()];
    lines.extend(prefix_lines(body, "  └ ".dim(), "    ".into()));
    lines
}

pub(crate) fn raw_lines(
    explanation: &Option<String>,
    steps: &[(String, TaskStatus)],
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("Updated Plan")];
    if let Some(explanation) = explanation
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.push(Line::from(explanation.to_string()));
    }
    if steps.is_empty() {
        lines.push(Line::from("(no steps provided)"));
    } else {
        lines.extend(
            steps
                .iter()
                .map(|(brief, status)| Line::from(format!("{}: {}", status.as_str(), brief))),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::display;
    use autoreport_core::types::TaskStatus;

    fn text(lines: Vec<ratatui::text::Line<'static>>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn renders_codex_updated_plan_shape() {
        let lines = text(display(
            &Some("Keep the transcript aligned".into()),
            &[
                ("Done".into(), TaskStatus::Completed),
                ("Current".into(), TaskStatus::InProgress),
                ("Later".into(), TaskStatus::Pending),
            ],
            80,
        ));
        assert_eq!(lines[0], "• Updated Plan");
        assert!(lines.iter().any(|line| line.contains("✔ Done")));
        assert!(lines.iter().any(|line| line.contains("□ Current")));
        assert!(lines.iter().any(|line| line.contains("□ Later")));
    }

    #[test]
    fn long_plan_steps_wrap_with_nested_indent() {
        let lines = display(
            &None,
            &[(
                "A deliberately long step that must wrap at a narrow terminal width".into(),
                TaskStatus::InProgress,
            )],
            24,
        );
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|line| line.width() <= 24));
        assert!(
            lines
                .iter()
                .any(|line| line.to_string().starts_with("    "))
        );
    }
}
