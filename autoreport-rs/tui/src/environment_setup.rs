//! Python and local-tool environment setup, shared by startup and `/env`.

use autoreport_core::environment::{
    EnvironmentConfig, PythonCandidate, PythonConfig, config_for_candidate, config_for_custom,
    detect_python_environments, ensure_python_environment, managed_venv_path,
    preferred_package_manager, save_environment, snapshot,
};
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use std::io;
use std::path::PathBuf;

use crate::config_update::Outcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Select,
    Custom,
    Review,
}

pub struct EnvironmentScreen {
    home: PathBuf,
    candidates: Vec<PythonCandidate>,
    selected: usize,
    pending: Option<PythonConfig>,
    step: Step,
    custom_path: String,
    error: Option<String>,
}

impl EnvironmentScreen {
    pub fn new(home: PathBuf, workspace: PathBuf) -> Self {
        let candidates = detect_python_environments(&workspace, &home);
        Self {
            home,
            candidates,
            selected: 0,
            pending: None,
            step: Step::Select,
            custom_path: String::new(),
            error: None,
        }
    }

    fn option_count(&self) -> usize {
        self.candidates.len() + 2
    }

    fn choose(&mut self) {
        if self.selected < self.candidates.len() {
            self.pending = Some(config_for_candidate(&self.candidates[self.selected]));
        } else if self.selected == self.candidates.len() {
            self.custom_path.clear();
            self.step = Step::Custom;
            return;
        } else {
            self.pending = Some(PythonConfig {
                source: "managed".into(),
                executable: managed_venv_path(&self.home),
                package_manager: preferred_package_manager(),
                label: "AutoReport managed venv".into(),
            });
        }
        self.step = Step::Review;
    }

    fn save(&mut self) -> Option<Outcome> {
        let Some(config) = self.pending.clone() else {
            return None;
        };
        match ensure_python_environment(&self.home, config).and_then(|config| {
            save_environment(
                &self.home,
                &EnvironmentConfig {
                    python: Some(config),
                },
            )
        }) {
            Ok(()) => Some(Outcome::Saved),
            Err(err) => {
                self.error = Some(err.to_string());
                None
            }
        }
    }

    pub fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(" Python environment ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let status = snapshot(&self.home);
        let status_line = Line::from(vec![
            Span::styled(
                "Local tools  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if status.latex.ready {
                    "LaTeX ✓"
                } else {
                    "LaTeX ·"
                },
                Style::default().fg(if status.latex.ready {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("  "),
            Span::styled(
                if status.typst.ready {
                    "Typst ✓"
                } else {
                    "Typst ·"
                },
                Style::default().fg(if status.typst.ready {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
            Span::raw("  "),
            Span::styled(
                if status.mineru.ready {
                    "MinerU ✓"
                } else {
                    "MinerU ·"
                },
                Style::default().fg(if status.mineru.ready {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
        ]);
        match self.step {
            Step::Select => {
                let mut items = self
                    .candidates
                    .iter()
                    .enumerate()
                    .map(|(i, candidate)| {
                        ListItem::new(Line::from(vec![
                            Span::styled(
                                format!("{}  ", i + 1),
                                Style::default().fg(Color::DarkGray),
                            ),
                            Span::raw(candidate.label.clone()),
                            Span::styled(
                                format!("  · {}", candidate.version),
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]))
                    })
                    .collect::<Vec<_>>();
                items.push(ListItem::new("Custom Python executable"));
                items.push(ListItem::new("AutoReport managed venv (global)"));
                let list = List::new(items).highlight_symbol("› ").highlight_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
                let mut state = ListState::default();
                state.select(Some(self.selected));
                let intro = Paragraph::new(
                    "Select the Python environment used by AutoReportCLI for this machine and all workspaces.\nDetected environments are reused; the managed option creates ~/.autoreport/venv.\n",
                );
                frame.render_widget(intro, inner);
                let list_area = ratatui::layout::Rect {
                    x: inner.x + 1,
                    y: inner.y + 4,
                    width: inner.width.saturating_sub(2),
                    height: inner.height.saturating_sub(7),
                };
                frame.render_stateful_widget(list, list_area, &mut state);
                frame.render_widget(
                    Paragraph::new(status_line),
                    ratatui::layout::Rect {
                        x: inner.x + 1,
                        y: inner.bottom().saturating_sub(2),
                        width: inner.width.saturating_sub(2),
                        height: 1,
                    },
                );
            }
            Step::Custom => {
                let body = vec![
                    Line::from("Enter an executable path (for example /opt/venv/bin/python):"),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("> "),
                        Span::styled(&self.custom_path, Style::default().fg(Color::Yellow)),
                    ]),
                ];
                frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
            }
            Step::Review => {
                let config = self.pending.as_ref().expect("review has a selection");
                let body = vec![
                    Line::from("Selected Python environment")
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                    Line::from(format!("  {}", config.label)),
                    Line::from(format!("  executable: {}", config.executable.display())),
                    Line::from(format!("  package manager: {}", config.package_manager)),
                    Line::from(""),
                    status_line,
                    Line::from(""),
                    Line::from("Press Enter to save · Esc to go back"),
                    self.error
                        .as_deref()
                        .map(Line::from)
                        .unwrap_or_else(|| Line::from("")),
                ];
                frame.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Cancelled);
        }
        match self.step {
            Step::Select => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.option_count() - 1)
                }
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    let i = c.to_digit(10).unwrap() as usize - 1;
                    if i < self.option_count() {
                        self.selected = i;
                    }
                }
                KeyCode::Enter => self.choose(),
                KeyCode::Esc => return Some(Outcome::Cancelled),
                _ => {}
            },
            Step::Custom => match key.code {
                KeyCode::Esc => self.step = Step::Select,
                KeyCode::Backspace => {
                    self.custom_path.pop();
                }
                KeyCode::Char(c) => self.custom_path.push(c),
                KeyCode::Enter => match config_for_custom(PathBuf::from(self.custom_path.trim())) {
                    Ok(config) => {
                        self.pending = Some(config);
                        self.error = None;
                        self.step = Step::Review;
                    }
                    Err(err) => self.error = Some(err.to_string()),
                },
                _ => {}
            },
            Step::Review => match key.code {
                KeyCode::Esc => {
                    self.step = Step::Select;
                    self.error = None;
                }
                KeyCode::Enter => return self.save(),
                _ => {}
            },
        }
        None
    }

    pub fn run_fullscreen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<Outcome> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            if let event::Event::Key(key) = event::read()?
                && let Some(outcome) = self.handle_key(key)
            {
                return Ok(outcome);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn page_renders_python_choice_and_tool_section() {
        let dir = tempfile::tempdir().unwrap();
        let screen = EnvironmentScreen::new(dir.path().to_path_buf(), dir.path().to_path_buf());
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Python environment"));
        assert!(rendered.contains("Custom Python executable"));
        assert!(rendered.contains("AutoReport managed venv"));
        assert!(rendered.contains("Local tools"));
    }
}
