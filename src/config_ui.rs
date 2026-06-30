//! Codex-login-page-style full-screen config screen.
//!
//! Two lifecycle modes share one render + key-handling implementation:
//! `run_fullscreen` (first-run wizard, standalone loop) and the `/config`
//! overlay (driven by `tui.rs`).

use crate::config::schema::{ProviderConfig, Settings};
use crate::config::resolve_api_key;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use std::path::PathBuf;

/// Result of a completed config screen session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Saved,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Select,
    Edit,
    Preview,
}

/// Editable form field. `Save`/`Cancel` are pseudo-fields rendered as actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Model,
    ApiBase,
    ApiKey,
    Active,
    Save,
    Cancel,
}

impl Field {
    pub const ALL: [Field; 6] = [
        Field::Model,
        Field::ApiBase,
        Field::ApiKey,
        Field::Active,
        Field::Save,
        Field::Cancel,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Field::Model => "model",
            Field::ApiBase => "api_base",
            Field::ApiKey => "api_key",
            Field::Active => "set as active",
            Field::Save => "► Save",
            Field::Cancel => "✕ Cancel",
        }
    }

    /// Loose validation for a field's string value.
    pub fn validate(self, value: &str) -> Result<(), String> {
        let trimmed = value.trim();
        match self {
            Field::Model => {
                if trimmed.is_empty() {
                    Err("model must not be empty".into())
                } else {
                    Ok(())
                }
            }
            Field::ApiBase => {
                if trimmed.is_empty() {
                    Ok(())
                } else {
                    url::Url::parse(trimmed)
                        .map(|_| ())
                        .map_err(|_| "api_base must be a valid URL".to_string())
                }
            }
            Field::ApiKey => Ok(()), // may be empty (env-only)
            _ => Ok(()),
        }
    }
}

/// Ordered provider keys for the Select list.
fn provider_keys(settings: &Settings) -> Vec<String> {
    settings.providers.keys().cloned().collect()
}

pub struct ConfigScreen {
    pub settings: Settings,
    pub keys: Vec<String>,
    pub selected: usize,
    pub step: Step,
    pub field: Field,
    /// True while typing into `field` (the input buffer is live).
    pub editing: bool,
    pub input: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub workspace: PathBuf,
}

impl ConfigScreen {
    pub fn new(settings: Settings, workspace: PathBuf) -> Self {
        let keys = provider_keys(&settings);
        let selected = settings
            .active_provider
            .as_ref()
            .and_then(|a| keys.iter().position(|k| k == a))
            .unwrap_or(0);
        Self {
            settings,
            keys,
            selected,
            step: Step::Select,
            field: Field::Model,
            editing: false,
            input: String::new(),
            cursor: 0,
            error: None,
            workspace,
        }
    }

    pub fn selected_key(&self) -> Option<&str> {
        self.keys.get(self.selected).map(|s| s.as_str())
    }

    pub fn selected_provider(&self) -> Option<&ProviderConfig> {
        self.selected_key().and_then(|k| self.settings.providers.get(k))
    }

    pub fn selected_provider_mut(&mut self) -> Option<&mut ProviderConfig> {
        let key = self.selected_key().map(String::from)?;
        self.settings.providers.get_mut(&key)
    }

    /// Whether a real API key resolves for the selected provider.
    pub fn key_resolvable(&self) -> bool {
        self.selected_provider()
            .map(|p| resolve_api_key(p).is_ok())
            .unwrap_or(false)
    }

    /// Toggle the selected provider as the active one.
    pub fn toggle_active(&mut self) {
        if let Some(k) = self.selected_key() {
            self.settings.active_provider = Some(k.to_string());
        }
    }

    /// Validate and write a field's value into the selected provider. Returns
    /// Err(message) (also stored in `self.error`) on validation failure.
    pub fn commit(&mut self, field: Field, value: String) -> Result<(), String> {
        if let Err(e) = field.validate(&value) {
            self.error = Some(e.clone());
            return Err(e);
        }
        let value = value.trim().to_string();
        let provider = match self.selected_provider_mut() {
            Some(p) => p,
            None => {
                let e = "no provider selected".to_string();
                self.error = Some(e.clone());
                return Err(e);
            }
        };
        match field {
            Field::Model => provider.model = value,
            Field::ApiBase => provider.api_base = if value.is_empty() { None } else { Some(value) },
            Field::ApiKey => provider.api_key = if value.is_empty() { None } else { Some(value) },
            _ => {}
        }
        self.error = None;
        Ok(())
    }
}

impl ConfigScreen {
    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        // Clear the background (full-screen overlay like codex's centered dialog).
        f.render_widget(Clear, area);

        let title = " AutoReportCLI · provider setup ";
        let block = Block::default()
            .borders(Borders::ALL)
            .title(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan),
            ));
        f.render_widget(block, area);

        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(2)])
            .margin(1)
            .split(area);
        let body = inner[0];
        let footer = inner[1];

        match self.step {
            Step::Select => self.draw_select(f, body),
            Step::Edit => self.draw_edit(f, body),
            Step::Preview => self.draw_preview(f, body),
        }

        self.draw_footer(f, footer);
    }

    fn draw_select(&mut self, f: &mut Frame<'_>, area: Rect) {
        let items: Vec<ListItem> = self
            .keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let active = self.settings.active_provider.as_deref() == Some(k.as_str());
                let ok = self
                    .settings
                    .providers
                    .get(k)
                    .map(|p| resolve_api_key(p).is_ok())
                    .unwrap_or(false);
                let _ = i;
                let mark = if active { "●" } else { "○" };
                let key_icon = if ok { "✔" } else { "✘" };
                let style = if ok {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let line = Line::from(vec![
                    Span::styled(format!("{mark} "), Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{k:<16}"), Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(format!(" kind={} ", self.settings.providers[k].kind)),
                    Span::styled(format!("{key_icon} key"), style),
                ]);
                ListItem::new(line)
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(self.selected));
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(Span::styled(" Providers ", Style::default().fg(Color::Cyan))),
            );
        f.render_stateful_widget(list, area, &mut state);
    }

    fn draw_edit(&mut self, f: &mut Frame<'_>, area: Rect) {
        let provider = match self.selected_provider() {
            Some(p) => p,
            None => return,
        };
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!("Editing provider: {}", self.selected_key().unwrap_or("?")),
            Style::default().add_modifier(Modifier::BOLD).fg(Color::Yellow),
        )));
        lines.push(Line::raw(""));

        for field in Field::ALL {
            let value: String = match field {
                Field::Model => provider.model.clone(),
                Field::ApiBase => provider.api_base.clone().unwrap_or_default(),
                Field::ApiKey => provider
                    .api_key
                    .clone()
                    .map(|k| format!("{}••••", &k[..k.len().min(4)]))
                    .unwrap_or_else(|| "(env)".into()),
                Field::Active => {
                    if self.settings.active_provider.as_deref() == self.selected_key() {
                        "[X]".into()
                    } else {
                        "[ ]".into()
                    }
                }
                Field::Save | Field::Cancel => String::new(),
            };

            let focused = self.field == field;
            let marker = if focused { "▶" } else { " " };
            let mut spans = vec![Span::styled(
                format!("{marker} "),
                Style::default().fg(Color::Green),
            )];
            spans.push(Span::styled(
                format!("{:<14}", field.label()),
                Style::default().add_modifier(Modifier::BOLD),
            ));

            if focused && self.editing {
                // Show the live input buffer + a block cursor.
                let before: String = self.input[..self.cursor].chars().collect();
                let cur: String = self.input[self.cursor..].chars().take(1).collect();
                let after: String = self.input[self.cursor + cur.len()..].chars().collect();
                spans.push(Span::raw(before));
                spans.push(Span::styled(
                    if cur.is_empty() { " ".into() } else { cur },
                    Style::default().bg(Color::DarkGray),
                ));
                spans.push(Span::raw(after));
            } else {
                spans.push(Span::raw(value));
            }
            lines.push(Line::from(spans));
        }

        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        f.render_widget(para, area);
    }

    fn draw_preview(&self, f: &mut Frame<'_>, area: Rect) {
        let yaml = serde_yaml::to_string(&self.settings).unwrap_or_default();
        let para = Paragraph::new(yaml)
            .style(Style::default().fg(Color::Gray))
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(Span::styled(
                        " Preview (Enter=save) ",
                        Style::default().fg(Color::Cyan),
                    )),
            );
        f.render_widget(para, area);
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = match (self.step, self.editing) {
            (_, true) => " Enter: confirm field   Esc: cancel edit".to_string(),
            (Step::Select, _) => " ↑/↓: choose   Enter: edit   Esc: cancel".to_string(),
            (Step::Edit, _) => " ↑/↓: field   Enter: edit/toggle   Esc: back".to_string(),
            (Step::Preview, _) => " Enter: save & finish   Esc: back to edit".to_string(),
        };
        let mut text = hint;
        if let Some(err) = &self.error {
            text = format!("{text}   ⚠ {err}");
        }
        let para = Paragraph::new(text)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Left);
        f.render_widget(para, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{needs_config, save_settings};

    fn settings_with(provider: &str, cfg: ProviderConfig) -> Settings {
        let mut s = Settings::default();
        s.providers.insert(provider.into(), cfg);
        s.active_provider = Some(provider.into());
        s
    }

    fn provider(model: &str) -> ProviderConfig {
        ProviderConfig {
            kind: "anthropic".into(),
            model: model.into(),
            api_key: Some("sk-test".into()),
            api_base: None,
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 8192,
        }
    }

    #[test]
    fn validate_rejects_empty_model() {
        assert!(Field::Model.validate("").is_err());
        assert!(Field::Model.validate("claude-x").is_ok());
    }

    #[test]
    fn validate_allows_empty_api_key() {
        assert!(Field::ApiKey.validate("").is_ok());
    }

    #[test]
    fn validate_api_base_must_be_url() {
        assert!(Field::ApiBase.validate("not a url").is_err());
        assert!(Field::ApiBase.validate("https://api.x.com/v1").is_ok());
        assert!(Field::ApiBase.validate("").is_ok()); // optional
    }

    #[test]
    fn set_active_mutates_settings() {
        let mut s = settings_with("a", provider("m-a"));
        s.providers.insert("b".into(), provider("m-b"));
        let mut screen = ConfigScreen::new(s, PathBuf::from("/tmp/ws"));
        assert_eq!(screen.settings.active_provider.as_deref(), Some("a"));
        screen.selected = 1; // "b"
        screen.toggle_active();
        assert_eq!(screen.settings.active_provider.as_deref(), Some("b"));
    }

    #[test]
    fn commit_field_writes_into_selected_provider() {
        let s = settings_with("a", provider("old"));
        let mut screen = ConfigScreen::new(s, PathBuf::from("/tmp/ws"));
        screen.commit(Field::Model, "new-model".into()).unwrap();
        assert_eq!(screen.settings.providers["a"].model, "new-model");
    }

    #[test]
    fn cancel_does_not_save() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::default();
        let mut screen = ConfigScreen::new(s, dir.path().to_path_buf());
        // Simulate cancel: no save_settings call happens, only in-memory edits.
        screen.commit(Field::Model, "ignored".into()).ok();
        assert!(needs_config(dir.path(), &screen.settings));
        // save_settings was never invoked, so the file is absent:
        assert!(!dir.path().join("autoreport.config.yaml").exists());
        // (sanity: an explicit save would flip it)
        save_settings(dir.path(), &screen.settings).unwrap();
        assert!(dir.path().join("autoreport.config.yaml").exists());
    }
}
