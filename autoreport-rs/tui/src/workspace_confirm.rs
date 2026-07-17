//! First-run workspace confirmation page.
//!
//! The workspace is deliberately confirmed before the CLI creates the report
//! layout or project-scoped state. This follows Codex's trust prompt pattern:
//! show the directory plainly, explain what will happen, and require an
//! explicit choice before continuing.

use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceOutcome {
    Confirmed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Selection {
    Continue,
    Quit,
}

/// Full-screen confirmation shown after API and model configuration.
pub struct WorkspaceScreen {
    workspace: PathBuf,
    highlighted: Selection,
}

impl WorkspaceScreen {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            highlighted: Selection::Continue,
        }
    }

    pub fn draw(&self, f: &mut Frame<'_>) {
        let area = f.area();
        f.render_widget(Clear, area);

        let dialog = centered_rect(area, 84, 68);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                " AutoReportCLI · workspace access ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(dialog);
        f.render_widget(block, dialog);

        let columns = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(4),
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);

        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Cyan)),
                Span::styled("You are in ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    self.workspace.display().to_string(),
                    Style::default().fg(Color::Yellow),
                ),
            ])),
            columns[0],
        );

        f.render_widget(
            Paragraph::new(
                "Do you want AutoReportCLI to use this workspace? After you continue, it will create the standard report folders and project-scoped session state."
                    .to_string(),
            )
            .wrap(Wrap { trim: true }),
            columns[1],
        );

        f.render_widget(
            Paragraph::new("Choose an option:".to_string())
                .style(Style::default().fg(Color::DarkGray)),
            columns[3],
        );

        let options = [
            ("Yes, continue", Selection::Continue),
            ("No, quit", Selection::Quit),
        ];
        let option_lines = options
            .iter()
            .enumerate()
            .map(|(index, (label, selection))| {
                let selected = self.highlighted == *selection;
                let marker = if selected { ">" } else { " " };
                let style = if selected {
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Line::from(vec![
                    Span::styled(format!(" {marker} "), style),
                    Span::styled(format!("{}  {label}", index + 1), style),
                ])
            })
            .collect::<Vec<_>>();
        f.render_widget(Paragraph::new(option_lines), columns[5]);

        f.render_widget(
            Paragraph::new("↑/↓ or j/k: select   Enter: confirm   Esc: quit")
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Left),
            columns[7],
        );
    }

    /// Drive one key event. Returns an outcome once the user has made a choice.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<WorkspaceOutcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(WorkspaceOutcome::Cancelled);
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.highlighted = Selection::Continue,
            KeyCode::Down | KeyCode::Char('j') => self.highlighted = Selection::Quit,
            KeyCode::Char('1') => self.highlighted = Selection::Continue,
            KeyCode::Char('2') => self.highlighted = Selection::Quit,
            KeyCode::Enter => {
                return Some(match self.highlighted {
                    Selection::Continue => WorkspaceOutcome::Confirmed,
                    Selection::Quit => WorkspaceOutcome::Cancelled,
                });
            }
            KeyCode::Esc => return Some(WorkspaceOutcome::Cancelled),
            _ => {}
        }
        None
    }

    /// Blocking full-screen loop for the startup confirmation page.
    pub fn run_fullscreen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<WorkspaceOutcome> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            if let event::Event::Key(key) = event::read()?
                && let Some(outcome) = self.handle_key(key)
            {
                return Ok(outcome);
            }
        }
    }
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn continues_by_default() {
        let mut screen = WorkspaceScreen::new(PathBuf::from("/tmp/project"));
        assert_eq!(
            screen.handle_key(key(KeyCode::Enter)),
            Some(WorkspaceOutcome::Confirmed)
        );
    }

    #[test]
    fn can_quit_before_workspace_initialization() {
        let mut screen = WorkspaceScreen::new(PathBuf::from("/tmp/project"));
        screen.handle_key(key(KeyCode::Down));
        assert_eq!(
            screen.handle_key(key(KeyCode::Enter)),
            Some(WorkspaceOutcome::Cancelled)
        );
    }

    #[test]
    fn escape_cancels() {
        let mut screen = WorkspaceScreen::new(PathBuf::from("/tmp/project"));
        assert_eq!(
            screen.handle_key(key(KeyCode::Esc)),
            Some(WorkspaceOutcome::Cancelled)
        );
    }
}
