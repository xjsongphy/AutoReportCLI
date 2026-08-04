//! Transcript/history cells migrated from Codex's `tui/src/history_cell`.
//!
//! AutoReport keeps its protocol-specific `Cell` enum, but the chat surface consumes it through
//! the same render contract as Codex: cells own width-aware line generation and report their
//! wrapped height to the parent render tree.

use crate::app_state::{Cell, SysKind};
use crate::chatwidget::{render_tool_result_lines, tool_arg_summary};
use crate::line_utils::{prefix_lines, push_owned_lines};
use crate::render::renderable::Renderable;
use crate::wrapping::RtOptions;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph, WidgetRef, Wrap};

mod base;
mod exec;
mod mcp;
mod messages;
mod patches;
mod plans;
mod request_user_input;
mod separators;
mod session;
pub(crate) use crate::terminal_hyperlinks::HyperlinkLine;
pub(crate) use messages::split_reasoning_summary_parts;
pub(crate) use session::{SessionHeaderHistoryCell, format_directory_display};

/// Strip styling from lines, keeping only their text content. Ported from
/// Codex's `history_cell::plain_lines`.
#[allow(dead_code)] // used by PlainHistoryCell/WebHyperlinkHistoryCell once R5 constructs them
pub(crate) fn plain_lines(lines: impl IntoIterator<Item = Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| {
            let text = line
                .spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>();
            Line::from(text)
        })
        .collect()
}

/// AutoReport stores all agent events in one vector; Codex displays the
/// currently selected thread. Apply that same boundary when building lines.
pub(crate) fn belongs_to_agent(cell: &Cell, focused: autoreport_core::types::AgentType) -> bool {
    match cell {
        Cell::User { _agent, .. } => *_agent == focused,
        Cell::AgentMessage { agent, .. }
        | Cell::AgentMarkdown { agent, .. }
        | Cell::Reasoning { agent, .. }
        | Cell::ToolGroup { agent, .. }
        | Cell::Collab { agent, .. }
        | Cell::TurnSeparator { agent, .. }
        | Cell::PlanUpdate { agent, .. }
        | Cell::UserInputResult { agent, .. } => *agent == focused,
        Cell::System { .. } => true,
    }
}

/// Width-aware history cell contract from Codex's transcript renderer.
#[allow(dead_code)] // hyperlink methods used once R5 marks the transcript buffer
pub(crate) trait HistoryCell: std::fmt::Debug + Send + Sync {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>>;

    /// Hyperlink-aware lines for terminals that support OSC 8. Cells without
    /// web URLs fall back to plain lines (no link annotations). Ported from
    /// Codex's `HistoryCell::display_hyperlink_lines`.
    fn display_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width))
    }

    /// Hyperlink lines used when writing the transcript to disk/export.
    /// Defaults to the display hyperlinks (Codex parity).
    fn transcript_hyperlink_lines(
        &self,
        width: u16,
    ) -> Vec<crate::terminal_hyperlinks::HyperlinkLine> {
        self.display_hyperlink_lines(width)
    }

    /// Copy-friendly source lines, matching Codex's raw scrollback mode.
    /// Cells that do not have a separate source representation fall back to
    /// an unwrapped rich render at an effectively unlimited width.
    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(u16::MAX)
    }

    fn desired_height(&self, width: u16) -> u16 {
        Paragraph::new(Text::from(self.display_lines(width)))
            .wrap(Wrap { trim: false })
            .line_count(width)
            .try_into()
            .unwrap_or(0)
    }
}

impl Renderable for Box<dyn HistoryCell> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.display_lines(area.width);
        let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
        let scroll = paragraph
            .line_count(area.width)
            .saturating_sub(usize::from(area.height));
        Clear.render_ref(area, buf);
        paragraph
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
            .render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        HistoryCell::desired_height(self.as_ref(), width)
    }
}

impl HistoryCell for Cell {
    fn display_lines(&self, width: u16) -> Vec<Line<'static>> {
        render_cell_lines(self, width)
    }

    /// Dispatch per-variant so finalized assistant markdown is annotated with
    /// OSC 8 web-URL hyperlinks. Mirrors Codex, where each concrete cell type
    /// overrides `display_hyperlink_lines` and the render path dispatches via
    /// the trait; variants without a richer override fall back to the default
    /// plain-lines representation.
    fn display_hyperlink_lines(&self, width: u16) -> Vec<HyperlinkLine> {
        match self {
            Cell::AgentMarkdown { text, .. } => {
                messages::AgentMarkdownCell { text: text.clone() }.display_hyperlink_lines(width)
            }
            _ => crate::terminal_hyperlinks::plain_hyperlink_lines(self.display_lines(width)),
        }
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        match self {
            Cell::User { text, .. } => vec![Line::from(sanitize_user_text(text))],
            Cell::AgentMessage { text, .. } | Cell::AgentMarkdown { text, .. } => text
                .split('\n')
                .map(|line| Line::from(line.to_string()))
                .collect(),
            Cell::Reasoning {
                text,
                transcript_only,
                ..
            } => {
                if *transcript_only {
                    Vec::new()
                } else {
                    text.split('\n')
                        .map(|line| Line::from(line.to_string()))
                        .collect()
                }
            }
            Cell::Collab { title, details, .. } => {
                let mut lines = vec![Line::from(
                    title
                        .spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>(),
                )];
                lines.extend(details.iter().map(|line| {
                    Line::from(
                        line.spans
                            .iter()
                            .map(|span| span.content.as_ref())
                            .collect::<String>(),
                    )
                }));
                lines
            }
            Cell::ToolGroup { .. } | Cell::System { .. } => self.display_lines(u16::MAX),
            Cell::TurnSeparator {
                elapsed_seconds,
                runtime_metrics,
                ..
            } => separators::FinalMessageSeparator::new(*elapsed_seconds, *runtime_metrics)
                .raw_lines(),
            Cell::PlanUpdate {
                explanation, steps, ..
            } => plans::raw_lines(explanation, steps),
            Cell::UserInputResult {
                questions,
                answers,
                interrupted,
                ..
            } => request_user_input::raw_lines(questions, answers, *interrupted),
        }
    }
}

pub(crate) fn render_history_lines_for_agent(
    cells: &[Cell],
    focused: autoreport_core::types::AgentType,
    width: u16,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut previous_has_trailing_blank = false;
    for cell in cells.iter().filter(|cell| belongs_to_agent(cell, focused)) {
        let lines = cell.display_lines(width);
        if lines.is_empty() {
            continue;
        }
        // Codex owns spacing at the history-cell boundary. Do not infer that
        // boundary from rendered text: a tool may legitimately end with an
        // empty output line, which must not swallow the separator before the
        // next tool call.
        if !out.is_empty() && !previous_has_trailing_blank && !cell_has_leading_blank(cell) {
            out.push(Line::from(""));
        }
        out.extend(lines);
        previous_has_trailing_blank = cell_has_trailing_blank(cell);
    }
    out
}

/// Hyperlink-aware counterpart of [`render_history_lines_for_agent`]: same
/// cells/order and the same separator structure, so OSC 8 link annotations stay
/// row-aligned with the rendered transcript.
pub(crate) fn render_history_hyperlink_lines_for_agent(
    cells: &[Cell],
    focused: autoreport_core::types::AgentType,
    width: u16,
) -> Vec<HyperlinkLine> {
    let mut out = Vec::new();
    let mut previous_has_trailing_blank = false;
    for cell in cells.iter().filter(|cell| belongs_to_agent(cell, focused)) {
        let lines = cell.display_hyperlink_lines(width);
        if lines.is_empty() {
            continue;
        }
        if !out.is_empty() && !previous_has_trailing_blank && !cell_has_leading_blank(cell) {
            out.push(HyperlinkLine::from(""));
        }
        out.extend(lines);
        previous_has_trailing_blank = cell_has_trailing_blank(cell);
    }
    out
}

/// User-message cells intentionally paint a leading and trailing surface row,
/// just like Codex's `UserHistoryCell`. Other cells rely on the transcript
/// boundary to insert one separator row. Keep this semantic rather than
/// inspecting rendered text, because tool output may itself contain blanks.
pub(crate) fn cell_has_leading_blank(cell: &Cell) -> bool {
    matches!(cell, Cell::User { .. })
}

pub(crate) fn cell_has_trailing_blank(cell: &Cell) -> bool {
    matches!(cell, Cell::User { .. })
}

pub(crate) fn render_raw_history_lines_for_agent(
    cells: &[Cell],
    focused: autoreport_core::types::AgentType,
) -> Vec<Line<'static>> {
    cells
        .iter()
        .filter(|cell| belongs_to_agent(cell, focused))
        .flat_map(HistoryCell::raw_lines)
        .collect()
}

/// Codex strips terminal CSI/control sequences from user-authored text before
/// placing it in scrollback. This prevents pasted escape sequences from
/// changing terminal state while preserving tabs and explicit newlines.
fn sanitize_user_text(text: &str) -> String {
    let mut sanitized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.next_if_eq(&'[').is_some() {
            let _ = chars.find(|ch| ('@'..='~').contains(ch));
        } else if matches!(ch, '\n' | '\t') || !ch.is_control() {
            sanitized.push(ch);
        }
    }
    sanitized
}

fn render_cell_lines(cell: &Cell, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let width_u16 = width.min(u16::MAX as usize) as u16;
    let mut out = Vec::new();
    match cell {
        Cell::User { text, .. } => {
            out.extend(messages::UserHistoryCell { text: text.clone() }.display_lines(width_u16));
        }
        Cell::AgentMarkdown { text, .. } => {
            out.extend(messages::AgentMarkdownCell { text: text.clone() }.display_lines(width_u16));
        }
        Cell::Reasoning {
            text,
            transcript_only,
            ..
        } => {
            out.extend(
                messages::ReasoningSummaryCell::new(text.clone(), *transcript_only)
                    .display_lines(width_u16),
            );
        }
        Cell::AgentMessage {
            text,
            is_first_line,
            ..
        } => {
            out.extend(
                messages::AgentMessageCell {
                    text: text.clone(),
                    is_first_line: *is_first_line,
                }
                .display_lines(width_u16),
            );
        }
        Cell::Collab { title, details, .. } => {
            out.push(title.clone());
            out.extend(prefix_lines(details.clone(), "  └ ".dim(), "    ".into()));
        }
        Cell::ToolGroup { agent, items } => {
            for (i, item) in items.iter().enumerate() {
                // Codex renders each tool call as its own cell with one blank
                // breathing row before it. Grouped calls get the same separator
                // between items so two adjacent tools are not jammed together.
                if i > 0 {
                    out.push(Line::from(""));
                }
                out.extend(render_tool_call_lines(agent.label(), item, width));
            }
        }
        Cell::System { text, kind } => {
            // Direct adaptation of Codex's `new_info_event` and
            // `new_error_event` in `history_cell/notices.rs`.
            match kind {
                SysKind::Info => out.push(vec!["• ".dim(), text.clone().into()].into()),
                SysKind::Error => out.push(vec![format!("■ {text}").red()].into()),
            }
        }
        Cell::TurnSeparator {
            elapsed_seconds,
            runtime_metrics,
            ..
        } => {
            out.extend(
                separators::FinalMessageSeparator::new(*elapsed_seconds, *runtime_metrics)
                    .display_lines(width_u16),
            );
        }
        Cell::PlanUpdate {
            explanation, steps, ..
        } => out.extend(plans::display(explanation, steps, width_u16)),
        Cell::UserInputResult {
            questions,
            answers,
            interrupted,
            ..
        } => out.extend(request_user_input::display(
            questions,
            answers,
            *interrupted,
            width_u16,
        )),
    }
    out
}

/// Direct adaptation of Codex's `McpToolCallCell::display_lines` for
/// AutoReport's generic tool protocol. The only project-specific part is the
/// invocation text: Codex has an MCP invocation type, while AutoReport has a
/// tool name, JSON arguments, and an agent owner.
fn render_tool_call_lines(
    agent: &str,
    item: &crate::app_state::ToolEntry,
    width: usize,
) -> Vec<Line<'static>> {
    // Codex keeps exec, patch, and MCP calls as distinct history-cell
    // families. AutoReport's bus has one compact tool event, so dispatch to
    // the same visual families at the rendering boundary.
    if item.name == "exec" {
        return exec::display(agent, item, width);
    }
    if item.name == "apply_patch" {
        return patches::display(agent, item, width);
    }
    if item.name.starts_with("mcp__") {
        return mcp::display(agent, item, width);
    }
    base::display_generic_tool_call(agent, item, width)
}

#[cfg(test)]
mod tests {
    use super::{Cell, HistoryCell};
    use autoreport_core::types::AgentType;
    use ratatui::style::{Color, Modifier, Stylize};
    use ratatui::text::Line;

    fn plain_lines(cell: &Cell) -> Vec<String> {
        cell.display_lines(80)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .map(|line: String| line.trim_end().to_string())
            .collect()
    }

    #[test]
    fn user_message_uses_codex_prompt_prefix() {
        let lines = plain_lines(&Cell::User {
            _agent: AgentType::Main,
            text: "hi".into(),
        });
        assert!(lines.iter().any(|line| line == "› hi"));
        assert!(!lines.iter().any(|line| line.contains("Main")));
    }

    #[test]
    fn user_message_rows_fill_the_available_width_like_codex() {
        let cell = Cell::User {
            _agent: AgentType::Main,
            text: "hi".into(),
        };
        assert!(cell.display_lines(40).iter().all(|line| line.width() <= 40));
        assert!(cell.display_lines(40).iter().any(|line| line.width() == 40));
    }

    #[test]
    fn user_message_strips_terminal_control_sequences_like_codex() {
        let lines = plain_lines(&Cell::User {
            _agent: AgentType::Main,
            text: "hello\u{1b}[2Jworld\u{7}".into(),
        });
        assert!(lines.iter().any(|line| line == "› helloworld"));
        assert!(!lines.iter().any(|line| line.contains('\u{1b}')));
    }

    #[test]
    fn assistant_message_uses_codex_bullet_prefix() {
        let lines = plain_lines(&Cell::AgentMarkdown {
            agent: AgentType::Main,
            text: "hello".into(),
        });
        assert!(lines.iter().any(|line| line == "• hello"));
        assert!(!lines.iter().any(|line| line.contains("Main")));
    }

    #[test]
    fn raw_history_lines_keep_source_without_rich_prefixes() {
        let cells = vec![
            Cell::User {
                _agent: AgentType::Main,
                text: "hello".into(),
            },
            Cell::AgentMarkdown {
                agent: AgentType::Main,
                text: "**answer**".into(),
            },
        ];
        let lines = super::render_raw_history_lines_for_agent(&cells, AgentType::Main);
        let text = lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(text, vec!["hello", "**answer**"]);
    }

    #[test]
    fn exec_history_cell_shows_command_output_instead_of_raw_json() {
        let lines = plain_lines(&Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "exec".into(),
                args: serde_json::json!({"command": "uname -a"}),
                result: Some(serde_json::json!({
                    "stdout": "Darwin\n",
                    "stderr": "",
                    "returncode": 0
                })),
                error: None,
                call_id: None,
                started_at: None,
            }],
        });
        assert!(lines.iter().any(|line| line.contains("uname -a")));
        assert!(lines.iter().any(|line| line.contains("Darwin")));
        assert!(!lines.iter().any(|line| line.contains("returncode")));
    }

    #[test]
    fn mcp_history_cell_uses_server_tool_invocation_shape() {
        let lines = plain_lines(&Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "mcp__filesystem__read_file".into(),
                args: serde_json::json!({"path": "README.md"}),
                result: Some(serde_json::json!("ok")),
                error: None,
                call_id: None,
                started_at: None,
            }],
        });
        assert!(lines.iter().any(|line| line.contains("MCP")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("filesystem/read_file"))
        );
    }

    #[test]
    fn tool_rows_wrap_without_exceeding_narrow_width() {
        let cell = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "mcp__filesystem__read_file".into(),
                args: serde_json::json!({"path": "/a/very/long/path/to/a/file.txt"}),
                result: Some(serde_json::json!("result")),
                error: None,
                call_id: None,
                started_at: None,
            }],
        };
        assert!(cell.display_lines(24).iter().all(|line| line.width() <= 24));
    }

    #[test]
    fn collaborator_rows_use_codex_detail_tree() {
        let lines = Cell::Collab {
            agent: AgentType::Main,
            title: vec![
                "• ".dim(),
                "Sent input to ".bold(),
                "Data Analysis".cyan().bold(),
            ]
            .into(),
            details: vec![Line::from("calculate the uncertainty")],
        }
        .display_lines(80);
        let text = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>();
        assert_eq!(text[0], "• Sent input to Data Analysis");
        assert_eq!(text[1], "  └ calculate the uncertainty");
    }

    #[test]
    fn focused_agent_history_hides_other_threads_and_tool_owners() {
        let cells = vec![
            Cell::AgentMarkdown {
                agent: AgentType::Main,
                text: "main reply".into(),
            },
            Cell::ToolGroup {
                agent: AgentType::DataAnalysis,
                items: vec![crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "python analyze.py"}),
                    result: None,
                    error: None,
                    call_id: None,
                    started_at: None,
                }],
            },
        ];
        let text = super::render_history_lines_for_agent(&cells, AgentType::Main, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(text.contains("main reply"));
        assert!(!text.contains("python analyze.py"));
        assert!(!text.contains("Data Analysis"));
    }

    #[test]
    fn final_separator_matches_codex_worked_for_label() {
        let lines = Cell::TurnSeparator {
            agent: AgentType::Main,
            elapsed_seconds: Some(61),
            runtime_metrics: None,
        }
        .display_lines(80);
        let text = lines
            .into_iter()
            .map(|line| line.to_string())
            .collect::<String>();
        assert!(text.contains("Worked for 1m 01s"));
    }

    #[test]
    fn request_user_input_result_masks_secret_answers() {
        let question = autoreport_core::request_user_input::RequestUserInputQuestion {
            id: "token".into(),
            header: "Token".into(),
            question: "Provide token".into(),
            is_other: false,
            is_secret: true,
            options: None,
        };
        let mut answers = std::collections::HashMap::new();
        answers.insert(
            "token".into(),
            autoreport_core::request_user_input::RequestUserInputAnswer {
                answers: vec!["secret".into()],
            },
        );
        let text = Cell::UserInputResult {
            agent: AgentType::Main,
            questions: vec![question],
            answers,
            interrupted: false,
        }
        .display_lines(80)
        .into_iter()
        .map(|line| line.to_string())
        .collect::<String>();
        assert!(text.contains("••••••"));
        assert!(!text.contains("secret"));
    }

    fn plain_lines_at(cell: &Cell, width: u16) -> Vec<String> {
        cell.display_lines(width)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect()
            })
            .map(|line: String| line.trim_end().to_string())
            .collect()
    }

    #[test]
    fn two_tool_calls_in_a_group_are_separated_by_a_blank_line() {
        // Codex renders each tool call with one blank breathing row before it;
        // grouped calls now get the same separator instead of being jammed
        // together.
        let cell = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![
                crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "echo one"}),
                    result: Some(serde_json::json!({
                        "stdout": "one\n",
                        "stderr": "",
                        "returncode": 0
                    })),
                    error: None,
                    call_id: None,
                    started_at: None,
                },
                crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "echo two"}),
                    result: Some(serde_json::json!({
                        "stdout": "two\n",
                        "stderr": "",
                        "returncode": 0
                    })),
                    error: None,
                    call_id: None,
                    started_at: None,
                },
            ],
        };
        let lines = plain_lines_at(&cell, 80);
        let one = lines
            .iter()
            .position(|l| l.contains("echo one"))
            .expect("first command rendered");
        let two = lines
            .iter()
            .rposition(|l| l.contains("echo two"))
            .expect("second command rendered");
        assert!(two > one);
        assert!(
            lines[one..two].iter().any(|l| l.trim().is_empty()),
            "expected a blank separator between adjacent tool calls, got: {lines:?}"
        );
    }

    #[test]
    fn adjacent_tool_cells_get_a_blank_separator() {
        let cells = vec![
            Cell::ToolGroup {
                agent: AgentType::Main,
                items: vec![crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "echo first"}),
                    result: None,
                    error: None,
                    call_id: None,
                    started_at: None,
                }],
            },
            Cell::ToolGroup {
                agent: AgentType::Main,
                items: vec![crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "echo second"}),
                    result: None,
                    error: None,
                    call_id: None,
                    started_at: None,
                }],
            },
        ];
        let lines: Vec<String> = super::render_history_lines_for_agent(&cells, AgentType::Main, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect();
        let first = lines
            .iter()
            .position(|l| l.contains("echo first"))
            .expect("first tool rendered");
        let second = lines
            .iter()
            .rposition(|l| l.contains("echo second"))
            .expect("second tool rendered");
        assert!(second > first);
        assert!(
            lines[first..second].iter().any(|l| l.trim().is_empty()),
            "expected a blank separator between adjacent tool cells, got: {lines:?}"
        );
        assert_eq!(
            lines[first..second]
                .iter()
                .filter(|line| line.trim().is_empty())
                .count(),
            1,
            "Codex uses exactly one breathing row between tool cells: {lines:?}"
        );
    }

    #[test]
    fn empty_tool_output_does_not_swallow_the_next_tool_separator() {
        let cells = vec![
            Cell::ToolGroup {
                agent: AgentType::Main,
                items: vec![crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "printf '\\n'"}),
                    result: Some(serde_json::json!({
                        "stdout": "\n",
                        "stderr": "",
                        "returncode": 0
                    })),
                    error: None,
                    call_id: None,
                    started_at: None,
                }],
            },
            Cell::ToolGroup {
                agent: AgentType::Main,
                items: vec![crate::app_state::ToolEntry {
                    name: "exec".into(),
                    args: serde_json::json!({"command": "echo next"}),
                    result: Some(serde_json::json!({
                        "stdout": "next\n",
                        "stderr": "",
                        "returncode": 0
                    })),
                    error: None,
                    call_id: None,
                    started_at: None,
                }],
            },
        ];
        let lines: Vec<String> = super::render_history_lines_for_agent(&cells, AgentType::Main, 80)
            .into_iter()
            .map(|line| line.to_string())
            .collect();
        let next = lines
            .iter()
            .position(|line| line.contains("echo next"))
            .expect("next tool rendered");
        assert!(
            lines[..next].iter().any(|line| line.trim().is_empty()),
            "the separator must remain present after blank tool output: {lines:?}"
        );
    }

    #[test]
    fn tool_status_colors_survive_history_rendering() {
        let success = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "exec".into(),
                args: serde_json::json!({"command": "true"}),
                result: Some(serde_json::json!({"stdout": "ok\n", "returncode": 0})),
                error: None,
                call_id: None,
                started_at: None,
            }],
        };
        let failure = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "exec".into(),
                args: serde_json::json!({"command": "false"}),
                result: Some(serde_json::json!({
                    "stdout": "",
                    "stderr": "failed\n",
                    "returncode": 1
                })),
                error: None,
                call_id: None,
                started_at: None,
            }],
        };
        let success_line = success
            .display_lines(80)
            .into_iter()
            .next()
            .expect("success row");
        let failure_line = failure
            .display_lines(80)
            .into_iter()
            .next()
            .expect("failure row");
        assert_eq!(success_line.spans[0].style.fg, Some(Color::Green));
        assert!(
            success_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(failure_line.spans[0].style.fg, Some(Color::Red));
        assert!(
            failure_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn wrapped_command_rows_use_the_pipe_gutter() {
        // Codex's EXEC_DISPLAY_LAYOUT prefixes wrapped command rows with `│`.
        let cell = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "exec".into(),
                args: serde_json::json!({"command": "echo a-very-long-command-token-that-must-wrap-now"}),
                result: None,
                error: None,
                call_id: None,
                started_at: None,
            }],
        };
        let lines = plain_lines_at(&cell, 24);
        assert!(
            lines.iter().any(|l| l.contains('│')),
            "expected a `│` continuation gutter for a wrapped command, got: {lines:?}"
        );
    }

    #[test]
    fn finished_call_with_no_output_shows_placeholder() {
        // Codex renders "(no output)" for a finished call that captured nothing.
        let cell = Cell::ToolGroup {
            agent: AgentType::Main,
            items: vec![crate::app_state::ToolEntry {
                name: "exec".into(),
                args: serde_json::json!({"command": "true"}),
                result: Some(serde_json::json!({
                    "stdout": "",
                    "stderr": "",
                    "returncode": 0
                })),
                error: None,
                call_id: None,
                started_at: None,
            }],
        };
        let lines = plain_lines_at(&cell, 80);
        assert!(
            lines.iter().any(|l| l.contains("(no output)")),
            "expected a (no output) placeholder, got: {lines:?}"
        );
    }
}
