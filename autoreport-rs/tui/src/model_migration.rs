//! Dedicated TUI page for binding models to configured APIs.
//!
//! API credentials and base URLs live in `/model`; this page owns only the
//! per-agent API selection and model identifier.

use crate::config_update::Outcome;
use crate::custom_terminal::{Frame, Terminal};
use autoreport_core::config::resolve_api_key;
use autoreport_core::config::schema::{ModelConfig, Settings};
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Target,
    Model,
    Preview,
}

const TARGETS: [(&str, &str); 2] = [("main", "Main"), ("sub", "Sub agents (all 4)")];

/// A two-stage model binding editor: choose a role, then enter its model name.
pub struct ModelScreen {
    pub settings: Settings,
    pub home: PathBuf,
    step: Step,
    target_selected: usize,
    input: String,
    cursor: usize,
    error: Option<String>,
}

impl ModelScreen {
    pub fn new(settings: Settings, home: PathBuf) -> Self {
        Self {
            settings,
            home,
            step: Step::Target,
            target_selected: 0,
            input: String::new(),
            cursor: 0,
            error: None,
        }
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

    fn api_label(&self, key: &str) -> String {
        self.settings
            .providers
            .get(key)
            .and_then(|provider| provider.alias.as_deref())
            .filter(|alias| !alias.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| key.to_string())
    }

    fn target_label(&self) -> &'static str {
        TARGETS[self.target_selected].1
    }

    fn complete(&self) -> bool {
        let main = &self.settings.models.main;
        let sub = &self.settings.models.sub;
        !main.provider.trim().is_empty()
            && main.provider == sub.provider
            && !main.model.trim().is_empty()
            && !sub.model.trim().is_empty()
            && self
                .settings
                .providers
                .get(&main.provider)
                .is_some_and(|api| resolve_api_key(api).is_ok())
    }

    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        f.render_widget(Clear, area);
        // Keep this screen deliberately flat: it is already a full-screen
        // flow, so a centered dialog wastes most of the terminal.
        let chrome = Rect::new(
            area.x.saturating_add(2),
            area.y,
            area.width.saturating_sub(4),
            area.height,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(2),
            ])
            .split(chrome);
        f.render_widget(
            Paragraph::new(Line::styled(
                "Configure models · 2/2",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            chunks[0],
        );

        if chunks[2].width >= 78 && chunks[2].height >= 7 {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(58),
                    Constraint::Length(1),
                    Constraint::Min(28),
                ])
                .split(chunks[2]);
            self.draw_surface(f, columns[0]);
            self.draw_details(f, columns[2]);
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(1), Constraint::Length(4)])
                .split(chunks[2]);
            self.draw_surface(f, rows[0]);
            self.draw_details(f, rows[1]);
        }
        self.draw_footer(f, chunks[3]);
    }

    fn draw_surface(&mut self, f: &mut Frame<'_>, area: Rect) {
        match self.step {
            Step::Target => self.draw_targets(f, area),
            Step::Model => self.draw_model(f, area),
            Step::Preview => self.draw_preview(f, area),
        }
    }

    fn draw_details(&self, f: &mut Frame<'_>, area: Rect) {
        let selected = self.target();
        let provider = (!selected.provider.is_empty())
            .then(|| self.api_label(&selected.provider))
            .unwrap_or_else(|| "not selected".to_string());
        let model = (!selected.model.is_empty())
            .then_some(selected.model.as_str())
            .unwrap_or("not set");
        let lines = vec![
            Line::styled("Assignment", Style::default().add_modifier(Modifier::BOLD)),
            Line::from(vec![
                Span::styled("Target  ", Style::default().fg(Color::DarkGray)),
                Span::raw(self.target_label()),
            ]),
            Line::from(vec![
                Span::styled("Provider ", Style::default().fg(Color::DarkGray)),
                Span::styled(provider, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("Model   ", Style::default().fg(Color::DarkGray)),
                Span::styled(model, Style::default().fg(Color::LightGreen)),
            ]),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
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
                        format!("{label:<20}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(value, Style::default().fg(Color::Gray)),
                ]))
            })
            .collect::<Vec<_>>();
        let mut state = ListState::default();
        state.select(Some(self.target_selected));
        f.render_stateful_widget(
            List::new(items)
                .highlight_spacing(HighlightSpacing::Always)
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("› "),
            area,
            &mut state,
        );
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
            Line::styled(
                format!("Model for {api}"),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::raw(""),
            Line::from(vec![
                Span::styled("model  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(before),
                Span::styled(current, Style::default().bg(Color::DarkGray)),
                Span::raw(after),
            ]),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    fn draw_preview(&self, f: &mut Frame<'_>, area: Rect) {
        let yaml = serde_yaml::to_string(&self.settings).unwrap_or_default();
        f.render_widget(
            Paragraph::new(yaml).style(Style::default().fg(Color::Gray)),
            area,
        );
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let text = self.footer_hint(area.width);
        f.render_widget(
            Paragraph::new(text)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn footer_hint(&self, width: u16) -> String {
        if let Some(error) = &self.error {
            return format!("⚠ {error}").chars().take(width as usize).collect();
        }
        let hints = match self.step {
            Step::Target => [
                ("↑/↓", "browse"),
                ("Enter", "configure"),
                ("s", "save"),
                ("Esc", "cancel"),
                ("q", "quit"),
            ]
            .as_slice(),
            Step::Model => [("Enter", "confirm"), ("Esc", "back")].as_slice(),
            Step::Preview => [("Enter", "save"), ("Esc", "back"), ("q", "quit")].as_slice(),
        };
        let wide = hints
            .iter()
            .map(|(key, label)| format!("{key} {label}"))
            .collect::<Vec<_>>()
            .join("   ");
        if wide.len() <= width as usize {
            return wide;
        }
        let compact = hints
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>()
            .join("  ");
        compact.chars().take(width as usize).collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Cancelled);
        }
        // Keep `q` available as a model-name character while editing. On the
        // selection and review pages it is the one-key escape hatch for the
        // entire `/model` flow.
        if self.step != Step::Model && key.modifiers.is_empty() && key.code == KeyCode::Char('q') {
            return Some(Outcome::Quit);
        }
        match self.step {
            Step::Target => self.handle_target(key),
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
                self.input = self.target().model.clone();
                self.cursor = self.input.len();
                self.error = None;
                self.step = Step::Model;
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
            KeyCode::Esc => self.step = Step::Target,
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
            KeyCode::Enter => Some(Outcome::Saved),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::WritableTestBackend;
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
        settings.models.main.provider = "one".into();
        settings.models.sub.provider = "one".into();
        let mut screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));
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
    fn model_page_keeps_main_and_sub_on_the_same_provider() {
        let mut settings = Settings::default();
        settings.providers.insert("configured".into(), api());
        settings.models.main.provider = "configured".into();
        settings.models.sub.provider = "other".into();
        let screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));
        assert!(!screen.complete());
    }

    #[test]
    fn q_quits_selection_but_remains_a_model_name_character() {
        let mut screen = ModelScreen::new(Settings::default(), PathBuf::from("/tmp/ws"));
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Outcome::Quit)
        );

        screen.step = Step::Model;
        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(screen.input, "q");
    }

    fn render(screen: &mut ModelScreen, width: u16, height: u16) -> String {
        let mut terminal = Terminal::with_options(WritableTestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        terminal
            .rendered_buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn picker_is_flat_full_frame_and_keeps_provider_aliases_visible() {
        let mut settings = Settings::default();
        let mut configured = api();
        configured.alias = Some("Team OpenAI".into());
        settings.providers.insert("openai-prod".into(), configured);
        settings.models.main.provider = "openai-prod".into();
        settings.models.main.model = "gpt-5".into();
        let mut screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));

        let rendered = render(&mut screen, 100, 20);
        assert!(rendered.starts_with("  Configure models · 2/2"));
        assert!(rendered.contains("› Main"));
        assert!(rendered.contains("Provider Team OpenAI"));
        assert!(!rendered.contains("╭"));
        assert!(!rendered.contains("┌"));
    }

    #[test]
    fn narrow_picker_stacks_compact_assignment_details_below_the_list() {
        let mut settings = Settings::default();
        settings.providers.insert("one".into(), api());
        let mut screen = ModelScreen::new(settings, PathBuf::from("/tmp/ws"));

        let rendered = render(&mut screen, 50, 12);
        let lines = rendered.lines().collect::<Vec<_>>();
        let target_row = lines
            .iter()
            .position(|line| line.contains("› Main"))
            .unwrap();
        let details_row = lines
            .iter()
            .position(|line| line.contains("Assignment"))
            .unwrap();
        assert!(details_row > target_row);
        assert!(lines.iter().any(|line| line.contains("↑/↓  Enter  s  Esc")));
    }
}
