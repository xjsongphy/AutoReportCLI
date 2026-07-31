//! Python and local-tool environment setup, shared by startup and `/env`.

use crate::custom_terminal::{Frame, Terminal};
use autoreport_core::environment::{
    EnvironmentConfig, PythonCandidate, PythonConfig, config_for_candidate, config_for_custom,
    detect_python_environments, ensure_python_environment, managed_venv_path,
    preferred_package_manager, save_environment, snapshot,
};
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Wrap};
use std::io;
use std::path::PathBuf;

use crate::config_update::Outcome;
use crate::render::renderable::{ColumnRenderable, Renderable};
use crate::selection_list::{plain_selection_option_row, selection_option_row_indented};
use autoreport_core::project::{
    MaterializePolicy, ProjectConfig, ReportLanguage, infer_report_language, load_project_config,
    plan_report_resources, prepare_report_resources, save_project_config,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Select,
    Custom,
    Review,
    Language,
    FinalReview,
}

pub struct EnvironmentScreen {
    home: PathBuf,
    workspace: PathBuf,
    candidates: Vec<PythonCandidate>,
    selected: usize,
    language_selected: ReportLanguage,
    pending: Option<PythonConfig>,
    step: Step,
    custom_path: String,
    error: Option<String>,
}

impl EnvironmentScreen {
    pub fn new(home: PathBuf, workspace: PathBuf) -> Self {
        let candidates = detect_python_environments(&workspace, &home);
        let initial_step = Step::Select;
        let initial_language = load_project_config(&home, &workspace)
            .ok()
            .flatten()
            .map(|c| c.report_language)
            .unwrap_or_else(|| match infer_report_language(&workspace) {
                autoreport_core::project::ReportLanguageInference::Typst => ReportLanguage::Typst,
                _ => ReportLanguage::Latex,
            });
        Self {
            home,
            workspace,
            candidates,
            selected: if initial_step == Step::Language && initial_language == ReportLanguage::Typst
            {
                1
            } else {
                0
            },
            language_selected: initial_language,
            pending: None,
            step: initial_step,
            custom_path: String::new(),
            error: None,
        }
    }

    /// Startup may skip the global Python stage when it is already valid;
    /// manual `/env` always uses `new` and therefore shows the full flow.
    pub fn language_only(home: PathBuf, workspace: PathBuf) -> Self {
        let mut screen = Self::new(home, workspace);
        screen.step = Step::Language;
        screen
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
            Ok(()) => {
                self.step = Step::Language;
                self.error = None;
                None
            }
            Err(err) => {
                self.error = Some(err.to_string());
                None
            }
        }
    }

    pub fn draw(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        frame.render_widget(Clear, area);
        // Keep the wizard's content as a readable, centered block.  The
        // terminal remains the full drawing area so resizing still works;
        // only the content column is constrained and repositioned.
        let content_width = area.width.min(84);
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
                let mut column = ColumnRenderable::new();
                column.push(Line::from("Python environment").style(Style::default().dim()));
                column.push(
                    Paragraph::new(
                        "Select the Python environment used by AutoReportCLI for this machine and all workspaces.\nDetected environments are reused; the managed option creates ~/.autoreport/venv.",
                    )
                    .wrap(Wrap { trim: true }),
                );
                column.push("");
                let conda_candidates = self
                    .candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| candidate.source == "conda")
                    .collect::<Vec<_>>();
                if !conda_candidates.is_empty() {
                    column.push(Line::from("Conda").style(Style::default().dim()));
                    for (local_index, (candidate_index, candidate)) in
                        conda_candidates.iter().enumerate()
                    {
                        let detail = candidate
                            .label
                            .split_once(" · ")
                            .map(|(_, detail)| detail)
                            .unwrap_or(candidate.label.as_str());
                        column.push(selection_option_row_indented(
                            local_index,
                            format!("{detail} · {}", candidate.version),
                            self.selected == *candidate_index,
                            2,
                        ));
                    }
                }
                for (candidate_index, candidate) in self
                    .candidates
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| candidate.source != "conda")
                {
                    let (title, detail) = candidate
                        .label
                        .split_once(" · ")
                        .map_or((candidate.label.as_str(), ""), |parts| parts);
                    column.push(plain_selection_option_row(
                        title.to_string(),
                        self.selected == candidate_index,
                    ));
                    let detail = if detail.is_empty() {
                        candidate.version.clone()
                    } else {
                        format!("{detail} · {}", candidate.version)
                    };
                    column.push(
                        Line::from(format!("    {detail}"))
                            .style(Style::default().fg(Color::DarkGray)),
                    );
                }
                column.push(plain_selection_option_row(
                    "Custom Python executable".into(),
                    self.selected == self.candidates.len(),
                ));
                column.push(plain_selection_option_row(
                    "AutoReport managed venv (global)".into(),
                    self.selected == self.candidates.len() + 1,
                ));
                column.push("");
                column.push(status_line);
                column.push(
                    Line::from("↑/↓ select · Enter choose · Esc cancel")
                        .style(Style::default().dim()),
                );
                let content_area = centered_content_area(
                    area,
                    content_width,
                    column.desired_height(content_width),
                );
                column.render(content_area, frame.buffer_mut());
            }
            Step::Custom => {
                let body = vec![
                    Line::from("Enter an executable path (for example /opt/venv/bin/python):"),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("› "),
                        Span::styled(&self.custom_path, Style::default().fg(Color::Yellow)),
                    ]),
                ];
                let paragraph = Paragraph::new(body).wrap(Wrap { trim: true });
                let content_area = centered_content_area(
                    area,
                    content_width,
                    paragraph.line_count(content_width) as u16,
                );
                frame.render_widget(paragraph, content_area);
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
                let paragraph = Paragraph::new(body).wrap(Wrap { trim: true });
                let content_area = centered_content_area(
                    area,
                    content_width,
                    paragraph.line_count(content_width) as u16,
                );
                frame.render_widget(paragraph, content_area);
            }
            Step::Language => {
                let mut column = ColumnRenderable::new();
                column.push(
                    Line::from("Report language (this project)").style(Style::default().dim()),
                );
                column.push(plain_selection_option_row(
                    "LaTeX · Report/main.tex".into(),
                    self.language_selected == ReportLanguage::Latex,
                ));
                column.push(plain_selection_option_row(
                    "Typst · Report/main.typ".into(),
                    self.language_selected == ReportLanguage::Typst,
                ));
                column.push("");
                column.push(status_line);
                column.push(
                    Line::from("↑/↓ select · Enter continue · Esc cancel")
                        .style(Style::default().dim()),
                );
                if let Some(error) = self.error.as_deref() {
                    column.push(Line::from(error));
                }
                let content_area = centered_content_area(
                    area,
                    content_width,
                    column.desired_height(content_width),
                );
                column.render(content_area, frame.buffer_mut());
            }
            Step::FinalReview => {
                let language = self.language_selected;
                let (entry, theme, compiler) = match language {
                    ReportLanguage::Latex => {
                        ("Report/main.tex", "Report/mpltx.cls", "XeLaTeX/latexmk")
                    }
                    ReportLanguage::Typst => ("Report/main.typ", "Report/mplts.typ", "typst"),
                };
                let preview = plan_report_resources(&self.workspace, &self.home, language)
                    .unwrap_or_default();
                let names = |paths: &[PathBuf]| {
                    paths
                        .iter()
                        .filter_map(|p| p.file_name())
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                let body = vec![
                    Line::from("Final Review").style(Style::default().add_modifier(Modifier::BOLD)),
                    Line::from(format!(
                        "  Python environment (global): {}",
                        self.pending
                            .as_ref()
                            .map(|p| p.executable.display().to_string())
                            .or_else(
                                || autoreport_core::environment::selected_python_environment(
                                    &self.home
                                )
                                .map(|p| p.executable.display().to_string())
                            )
                            .unwrap_or_else(|| "unchanged".into())
                    )),
                    Line::from(format!(
                        "  Report language (this project): {}",
                        if language == ReportLanguage::Latex {
                            "LaTeX"
                        } else {
                            "Typst"
                        }
                    )),
                    Line::from(format!("  Entry: {entry}")),
                    Line::from(format!("  Theme: {theme}")),
                    Line::from(format!("  Compiler: {compiler}")),
                    Line::from(format!(
                        "  Create: {}",
                        if preview.created.is_empty() {
                            "none".into()
                        } else {
                            names(&preview.created)
                        }
                    )),
                    Line::from(format!(
                        "  Preserve: {}",
                        if preview.preserved.is_empty() {
                            "none".into()
                        } else {
                            names(&preview.preserved)
                        }
                    )),
                    Line::from(""),
                    Line::from("Enter save · Esc go back"),
                    self.error
                        .as_deref()
                        .map(Line::from)
                        .unwrap_or_else(|| Line::from("")),
                ];
                let paragraph = Paragraph::new(body).wrap(Wrap { trim: true });
                let content_area = centered_content_area(
                    area,
                    content_width,
                    paragraph.line_count(content_width) as u16,
                );
                frame.render_widget(paragraph, content_area);
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
                    let local_index = c.to_digit(10).unwrap() as usize - 1;
                    if let Some((candidate_index, _)) = self
                        .candidates
                        .iter()
                        .enumerate()
                        .filter(|(_, candidate)| candidate.source == "conda")
                        .nth(local_index)
                    {
                        self.selected = candidate_index;
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
            Step::Language => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.language_selected = ReportLanguage::Latex,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.language_selected = ReportLanguage::Typst
                }
                KeyCode::Esc => return Some(Outcome::Cancelled),
                KeyCode::Enter => self.step = Step::FinalReview,
                _ => {}
            },
            Step::FinalReview => match key.code {
                KeyCode::Esc => {
                    self.step = Step::Language;
                    self.error = None;
                }
                KeyCode::Enter => {
                    let language = self.language_selected;
                    let result = save_project_config(
                        &self.home,
                        &self.workspace,
                        &ProjectConfig {
                            report_language: language,
                        },
                    )
                    .and_then(|_| {
                        prepare_report_resources(
                            &self.workspace,
                            &self.home,
                            language,
                            MaterializePolicy::CreateMissingOnly,
                        )
                        .and_then(|r| {
                            if r.failed.is_empty() {
                                Ok(())
                            } else {
                                Err(anyhow::anyhow!(r.failed.join("; ")))
                            }
                        })
                    });
                    match result {
                        Ok(()) => return Some(Outcome::Saved),
                        Err(err) => self.error = Some(err.to_string()),
                    }
                }
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
            match event::read()? {
                event::Event::Resize(width, height) => {
                    terminal.resize(ratatui::layout::Size::new(width, height))?;
                }
                event::Event::Key(key) => {
                    if let Some(outcome) = self.handle_key(key) {
                        return Ok(outcome);
                    }
                }
                _ => {}
            }
        }
    }
}

fn centered_content_area(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::WritableTestBackend;

    #[test]
    fn page_renders_python_choice_and_tool_section() {
        let dir = tempfile::tempdir().unwrap();
        let screen = EnvironmentScreen::new(dir.path().to_path_buf(), dir.path().to_path_buf());
        let mut terminal = Terminal::with_options(WritableTestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let rendered = terminal
            .rendered_buffer()
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

    #[test]
    fn final_review_is_read_only() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        autoreport_core::bundled::materialize(home.path());
        let mut screen = EnvironmentScreen::language_only(
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );
        screen.step = Step::FinalReview;
        let mut terminal = Terminal::with_options(WritableTestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert!(!workspace.path().join("Report/main.typ").exists());
        assert!(!workspace.path().join("Report/main.tex").exists());
    }

    #[test]
    fn language_page_uses_the_shared_selection_menu_style() {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let screen = EnvironmentScreen::language_only(
            home.path().to_path_buf(),
            workspace.path().to_path_buf(),
        );
        let mut terminal = Terminal::with_options(WritableTestBackend::new(100, 20)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        let rendered = terminal
            .rendered_buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("› LaTeX · Report/main.tex"));
        assert!(rendered.contains("Typst · Report/main.typ"));
        assert!(rendered.contains("Local tools"));

        let buffer = terminal.rendered_buffer();
        let first_content = buffer
            .content()
            .iter()
            .enumerate()
            .find(|(_, cell)| !cell.symbol().trim().is_empty())
            .map(|(index, _)| (index % 100, index / 100))
            .unwrap();
        assert!(first_content.0 > 0);
        assert!(first_content.1 > 0);
    }
}
