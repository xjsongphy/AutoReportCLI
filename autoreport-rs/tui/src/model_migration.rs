//! Dedicated TUI page for binding models to configured APIs.
//!
//! API credentials and base URLs live in `/config`; this page owns only the
//! per-agent API selection and model identifier.

use crate::config_update::Outcome;
use autoreport_core::config::schema::{ModelConfig, Settings};
use autoreport_core::config::{resolve_api_key, save_settings};
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use crate::custom_terminal::{Frame, Terminal};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Target,
    Api,
    Model,
    Preview,
}

const TARGETS: [(&str, &str); 2] = [("main", "Main"), ("sub", "Sub agents (all 4)")];

/// A two-stage model binding editor: select API, then enter model name.
pub struct ModelScreen {
    pub settings: Settings,
    pub home: PathBuf,
    step: Step,
    target_selected: usize,
    api_selected: usize,
    api_scroll_offset: usize,
    input: String,
    cursor: usize,
    error: Option<String>,
}

impl ModelScreen {
    pub fn new(settings: Settings, home: PathBuf) -> Self {
        let mut screen = Self {
            settings,
            home,
            step: Step::Target,
            target_selected: 0,
            api_selected: 0,
            api_scroll_offset: 0,
            input: String::new(),
            cursor: 0,
            error: None,
        };
        screen.sync_api_selection();
        screen
    }

    fn target(&self) -> &ModelConfig {
        if self.target_selected == 0 {
            &self.settings.models.main
        } else {
            &self.settings.models.sub
        }
    }

    fn target_mut(&mut self) -> &mut ModelConfig {
        if self.target_selected == 0 {
            &mut self.settings.models.main
        } else {
            &mut self.settings.models.sub
        }
    }

    fn api_keys(&self) -> Vec<String> {
        self.settings
            .providers
            .iter()
            .filter(|(_, provider)| resolve_api_key(provider).is_ok())
            .map(|(key, _)| key.clone())
            .collect()
    }

    fn api_label(&self, key: &str) -> String {
        self.settings
            .providers
            .get(key)
            .and_then(|provider| provider.alias.as_deref())
            .filter(|alias| !alias.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| key.to_string())
    }

    fn sync_api_selection(&mut self) {
        let keys = self.api_keys();
        self.api_selected = keys
            .iter()
            .position(|key| key == &self.target().provider)
            .unwrap_or(0)
            .min(keys.len().saturating_sub(1));
        self.api_scroll_offset = 0;
    }

    fn selected_api(&self) -> Option<String> {
        self.api_keys().get(self.api_selected).cloned()
    }

    fn target_label(&self) -> &'static str {
        TARGETS[self.target_selected].1
    }

    fn complete(&self) -> bool {
        [&self.settings.models.main, &self.settings.models.sub]
            .iter()
            .all(|model| {
                !model.provider.trim().is_empty()
                    && !model.model.trim().is_empty()
                    && self
                        .settings
                        .providers
                        .get(&model.provider)
                        .is_some_and(|api| resolve_api_key(api).is_ok())
            })
    }

    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        f.render_widget(Clear, area);
        let dialog = centered_rect(area, 82, 70);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    " AutoReportCLI · model configuration ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
            dialog,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .margin(1)
            .split(dialog);
        self.draw_header(f, chunks[0]);
        match self.step {
            Step::Target => self.draw_targets(f, chunks[1]),
            Step::Api => self.draw_apis(f, chunks[1]),
            Step::Model => self.draw_model(f, chunks[1]),
            Step::Preview => self.draw_preview(f, chunks[1]),
        }
        self.draw_footer(f, chunks[2]);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let selected = self.target();
        let selected_provider_label = if selected.provider.is_empty() {
            "-".to_string()
        } else {
            self.api_label(&selected.provider)
        };
        let text = vec![
            Line::from(vec![
                Span::styled("target  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    self.target_label(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("    "),
                Span::styled("API  ", Style::default().fg(Color::DarkGray)),
                Span::styled(selected_provider_label, Style::default().fg(Color::Cyan)),
                Span::raw("    "),
                Span::styled("model  ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    if selected.model.is_empty() {
                        "-"
                    } else {
                        &selected.model
                    },
                    Style::default().fg(Color::LightGreen),
                ),
            ]),
            Line::styled(
                "Each sub-agent currently shares the single ‘sub’ choice.",
                Style::default().fg(Color::DarkGray),
            ),
        ];
        f.render_widget(Paragraph::new(text), area);
    }

    fn draw_targets(&self, f: &mut Frame<'_>, area: Rect) {
        let items = TARGETS
            .iter()
            .enumerate()
            .map(|(index, (_, label))| {
                let model = if index == 0 {
                    &self.settings.models.main
                } else {
                    &self.settings.models.sub
                };
                let value = if model.provider.is_empty() || model.model.is_empty() {
                    "not configured".to_string()
                } else {
                    format!("{} · {}", self.api_label(&model.provider), model.model)
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{label:<22}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value, Style::default().fg(Color::Gray)),
                ]))
            })
            .chain(std::iter::once(ListItem::new(Line::raw(""))))
            .chain(std::iter::once(ListItem::new(Line::styled(
                "Press s to save after both choices are configured.",
                Style::default().fg(Color::DarkGray),
            ))))
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(self.target_selected));
        f.render_stateful_widget(
            List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .title(" Choose target "),
                ),
            area,
            &mut state,
        );
    }

    fn draw_apis(&mut self, f: &mut Frame<'_>, area: Rect) {
        let keys = self.api_keys();
        let mut items = keys
            .iter()
            .map(|key| {
                let api = self.settings.providers.get(key);
                let kind = api.map(|p| p.kind.as_str()).unwrap_or("?");
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<22}", self.api_label(key)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{kind:<14}"), Style::default().fg(Color::Gray)),
                ]))
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            items.push(ListItem::new(Line::styled(
                "No configured APIs — add one in /config",
                Style::default().fg(Color::Yellow),
            )));
        }
        let mut state = ListState::default().with_offset(self.api_scroll_offset);
        state.select((!keys.is_empty()).then_some(self.api_selected));
        f.render_stateful_widget(
            List::new(items)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .block(Block::default().borders(Borders::TOP).title(format!(
                    " 1/2 — select configured API for {} ",
                    self.target_label()
                ))),
            area,
            &mut state,
        );
        self.api_scroll_offset = state.offset();
    }

    fn draw_model(&self, f: &mut Frame<'_>, area: Rect) {
        let api = self.target().provider.as_str();
        let cursor = self.cursor.min(self.input.len());
        let before = &self.input[..cursor];
        let current_len = self.input[cursor..]
            .chars()
            .next()
            .map(|ch| ch.len_utf8())
            .unwrap_or(0);
        let current = if current_len == 0 {
            " ".to_string()
        } else {
            self.input[cursor..cursor + current_len].to_string()
        };
        let after = &self.input[cursor + current_len..];
        let lines = vec![
            Line::raw(format!("2/2 — model name for API: {api}")),
            Line::raw(""),
            Line::from(vec![
                Span::styled("model  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(before),
                Span::styled(current, Style::default().bg(Color::DarkGray)),
                Span::raw(after),
            ]),
        ];
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(" Enter model "),
            ),
            area,
        );
    }

    fn draw_preview(&self, f: &mut Frame<'_>, area: Rect) {
        let yaml = serde_yaml::to_string(&self.settings).unwrap_or_default();
        f.render_widget(
            Paragraph::new(yaml)
                .style(Style::default().fg(Color::Gray))
                .block(
                    Block::default()
                        .borders(Borders::TOP)
                        .title(" Preview (Enter = save) "),
                ),
            area,
        );
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = match self.step {
            Step::Target => "↑/↓: target   Enter: choose API   s: save   Esc: cancel",
            Step::Api => "↑/↓: API   Enter: select API, then enter model   Esc: back",
            Step::Model => "Enter: confirm model   Esc: back to API",
            Step::Preview => "Enter: save & finish   Esc: back",
        };
        let text = match &self.error {
            Some(error) => format!("{hint}   ⚠ {error}"),
            None => hint.to_string(),
        };
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(Color::DarkGray))
                .alignment(Alignment::Left),
            area,
        );
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Cancelled);
        }
        match self.step {
            Step::Target => self.handle_target(key),
            Step::Api => self.handle_api(key),
            Step::Model => self.handle_model(key),
            Step::Preview => self.handle_preview(key),
        }
    }

    fn handle_target(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.target_selected = self.target_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.target_selected = (self.target_selected + 1).min(TARGETS.len() - 1);
            }
            KeyCode::Enter => {
                self.sync_api_selection();
                self.step = Step::Api;
            }
            KeyCode::Char('s') => {
                if self.complete() {
                    self.step = Step::Preview;
                } else {
                    self.error = Some("configure both Main and Sub agents before saving".into());
                }
            }
            KeyCode::Esc => return Some(Outcome::Cancelled),
            _ => {}
        }
        None
    }

    fn handle_api(&mut self, key: KeyEvent) -> Option<Outcome> {
        let len = self.api_keys().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.api_selected > 0 => self.api_selected -= 1,
            KeyCode::Down | KeyCode::Char('j') if self.api_selected + 1 < len => {
                self.api_selected += 1
            }
            KeyCode::Enter => {
                let Some(api) = self.selected_api() else {
                    self.error = Some("no API configured; open /config first".into());
                    return None;
                };
                self.target_mut().provider = api;
                self.input = self.target().model.clone();
                self.cursor = self.input.len();
                self.error = None;
                self.step = Step::Model;
            }
            KeyCode::Esc => self.step = Step::Target,
            _ => {}
        }
        None
    }

    fn handle_model(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Enter => {
                let value = self.input.trim().to_string();
                if value.is_empty() {
                    self.error = Some("model must not be empty".into());
                } else {
                    self.target_mut().model = value;
                    self.error = None;
                    self.step = Step::Target;
                }
            }
            KeyCode::Esc => self.step = Step::Api,
            KeyCode::Backspace if self.cursor > 0 => {
                let prev = self.input[..self.cursor].chars().last().unwrap();
                self.cursor -= prev.len_utf8();
                self.input.remove(self.cursor);
            }
            KeyCode::Left if self.cursor > 0 => {
                let prev = self.input[..self.cursor].chars().last().unwrap();
                self.cursor -= prev.len_utf8();
            }
            KeyCode::Right if self.cursor < self.input.len() => {
                self.cursor += self.input[self.cursor..].chars().next().unwrap().len_utf8();
            }
            KeyCode::Char(ch) => {
                self.input.insert(self.cursor, ch);
                self.cursor += ch.len_utf8();
            }
            _ => {}
        }
        None
    }

    fn handle_preview(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Enter => match save_settings(&self.home, &self.settings) {
                Ok(()) => Some(Outcome::Saved),
                Err(error) => {
                    self.error = Some(format!("save failed: {error}"));
                    None
                }
            },
            KeyCode::Esc => {
                self.step = Step::Target;
                None
            }
            _ => None,
        }
    }

    pub fn run_fullscreen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<Outcome> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            if let event::Event::Key(key) = event::read()? {
                if let Some(outcome) = self.handle_key(key) {
                    return Ok(outcome);
                }
            }
        }
    }
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Percentage(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Percentage(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreport_core::config::schema::ProviderConfig;

    fn api() -> ProviderConfig {
        ProviderConfig {
            kind: "openai".into(),
            alias: None,
            api_key: Some("test".into()),
            api_base: None,
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 8192,
        }
    }

    #[test]
    fn model_entry_requires_api_then_nonempty_model() {
        let mut settings = Settings::default();
        settings.providers.insert("one".into(), api());
        let mut screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(screen.target().provider, "one");
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(screen.error.is_some());
        screen.input = "gpt-test".into();
        screen.cursor = screen.input.len();
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(screen.target().model, "gpt-test");
    }

    #[test]
    fn model_api_list_only_contains_entries_with_resolvable_keys() {
        let mut settings = Settings::default();
        settings.providers.insert("configured".into(), api());
        settings.providers.insert(
            "empty".into(),
            ProviderConfig {
                api_key: None,
                ..api()
            },
        );
        let screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));
        assert_eq!(screen.api_keys(), vec!["configured"]);
    }
}
