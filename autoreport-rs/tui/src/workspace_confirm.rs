//! First-run workspace confirmation page.
//!
//! The workspace is deliberately confirmed before the CLI creates the report
//! layout or project-scoped state. This follows Codex's trust prompt pattern:
//! show the directory plainly and require an explicit choice before
//! continuing.

use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
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

        // Codex renders this onboarding step as a left-aligned full-screen
        // column. Keeping the directory and choices in the normal transcript
        // flow makes the confirmation feel like a gate, not a modal dialog.
        let columns = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(area);

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
            Paragraph::new("Do you want AutoReportCLI to use this workspace?".to_string())
                .wrap(Wrap { trim: true }),
            inset_left(columns[1], 2),
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
                let marker = if selected { "›" } else { " " };
                let style = if selected {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                };
                Line::from(vec![
                    Span::styled(format!("{marker} {}. ", index + 1), style),
                    Span::styled(*label, style),
                ])
            })
            .collect::<Vec<_>>();
        f.render_widget(Paragraph::new(option_lines), columns[3]);

        f.render_widget(
            Paragraph::new("Press enter to continue").style(Style::default().fg(Color::DarkGray)),
            inset_left(columns[5], 2),
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

fn inset_left(area: Rect, amount: u16) -> Rect {
    Rect {
        x: area.x.saturating_add(amount),
        width: area.width.saturating_sub(amount),
        ..area
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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

    #[test]
    fn renders_a_codex_style_workspace_gate() {
        let screen = WorkspaceScreen::new(PathBuf::from("/tmp/project"));
        let backend = TestBackend::new(80, 14);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| screen.draw(frame)).expect("draw");

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("> You are in /tmp/project"));
        assert!(rendered.contains("› 1. Yes, continue"));
        assert!(rendered.contains("  2. No, quit"));
        assert!(rendered.contains("Press enter to continue"));
        assert!(!rendered.contains("After you continue"));
    }
}
