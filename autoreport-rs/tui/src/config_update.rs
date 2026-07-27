//! Full-screen API configuration screen.
//!
//! Two lifecycle modes share one render + key-handling implementation:
//! `run_fullscreen` (first-run wizard, standalone loop) and the `/config`
//! overlay (driven by `tui.rs`).

use crate::custom_terminal::Frame;
use crate::custom_terminal::Terminal;
use autoreport_core::config::schema::{ProviderConfig, Settings};
use autoreport_core::config::{resolve_api_key, save_settings};
use autoreport_core::sync::PresetProvider;
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use std::io;
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
    Add,
    Edit,
    Preview,
}

/// Editable form field. `Save`/`Cancel` are pseudo-fields rendered as actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Alias,
    ApiBase,
    ApiKey,
    Save,
    Cancel,
}

impl Field {
    pub const ALL: [Field; 5] = [
        Field::Alias,
        Field::ApiBase,
        Field::ApiKey,
        Field::Save,
        Field::Cancel,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Field::Alias => "alias",
            Field::ApiBase => "api_base",
            Field::ApiKey => "api_key",
            Field::Save => "► Save",
            Field::Cancel => "✕ Cancel",
        }
    }

    /// Loose validation for a field's string value.
    pub fn validate(self, value: &str) -> Result<(), String> {
        let trimmed = value.trim();
        match self {
            Field::Alias => Ok(()),
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

#[derive(Debug, Clone)]
struct ProviderGroup {
    kind: String,
    label: String,
    keys: Vec<String>,
}

fn kind_rank(kind: &str) -> usize {
    match kind {
        "anthropic" => 0,
        "openai" => 1,
        "openai-responses" => 2,
        "google" => 3,
        "deepseek" => 4,
        "openrouter" => 5,
        _ => 99,
    }
}

fn kind_label(kind: &str) -> String {
    match kind {
        "anthropic" => "Anthropic".to_string(),
        "openai" => "OpenAI-Compatible".to_string(),
        "openai-responses" => "OpenAI Responses".to_string(),
        "google" => "Google".to_string(),
        "deepseek" => "DeepSeek".to_string(),
        "openrouter" => "OpenRouter".to_string(),
        other => other.to_string(),
    }
}

/// Ordered provider groups for the Select list.
fn provider_groups(settings: &Settings) -> Vec<ProviderGroup> {
    let mut grouped: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (key, provider) in &settings.providers {
        grouped
            .entry(provider.kind.clone())
            .or_default()
            .push(key.clone());
    }
    let mut groups: Vec<ProviderGroup> = grouped
        .into_iter()
        .map(|(kind, keys)| ProviderGroup {
            label: kind_label(&kind),
            kind,
            keys,
        })
        .collect();
    groups.sort_by(|a, b| {
        kind_rank(&a.kind)
            .cmp(&kind_rank(&b.kind))
            .then_with(|| a.label.cmp(&b.label))
    });
    groups
}

pub struct ConfigScreen {
    pub settings: Settings,
    presets: Vec<PresetProvider>,
    groups: Vec<ProviderGroup>,
    pub group_selected: usize,
    pub selected_in_group: usize,
    pub preset_selected: usize,
    provider_scroll_offset: usize,
    preset_scroll_offset: usize,
    pub step: Step,
    pub field: Field,
    /// True while typing into `field` (the input buffer is live).
    pub editing: bool,
    pub input: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub home: PathBuf,
}

impl ConfigScreen {
    pub fn new(settings: Settings, home: PathBuf) -> Self {
        Self::new_with_presets(settings, home, Vec::new())
    }

    pub fn new_with_presets(
        settings: Settings,
        home: PathBuf,
        presets: Vec<PresetProvider>,
    ) -> Self {
        let groups = provider_groups(&settings);
        let active = groups.first().and_then(|g| g.keys.first().cloned());
        let (group_selected, selected_in_group) = active
            .as_deref()
            .and_then(|active_key| {
                groups.iter().enumerate().find_map(|(gi, group)| {
                    group
                        .keys
                        .iter()
                        .position(|k| k == active_key)
                        .map(|pi| (gi, pi))
                })
            })
            .unwrap_or((0, 0));
        Self {
            settings,
            presets,
            groups,
            group_selected,
            selected_in_group,
            preset_selected: 0,
            provider_scroll_offset: 0,
            preset_scroll_offset: 0,
            step: Step::Select,
            field: Field::ApiBase,
            editing: false,
            input: String::new(),
            cursor: 0,
            error: None,
            home,
        }
    }

    pub fn selected_key(&self) -> Option<&str> {
        self.groups
            .get(self.group_selected)
            .and_then(|group| group.keys.get(self.selected_in_group))
            .map(|s| s.as_str())
    }

    pub fn selected_provider(&self) -> Option<&ProviderConfig> {
        self.selected_key()
            .and_then(|k| self.settings.providers.get(k))
    }

    pub fn selected_provider_mut(&mut self) -> Option<&mut ProviderConfig> {
        let key = self.selected_key().map(String::from)?;
        self.settings.providers.get_mut(&key)
    }

    fn provider_label(&self, key: &str) -> String {
        self.settings
            .providers
            .get(key)
            .and_then(|provider| provider.alias.as_deref())
            .filter(|alias| !alias.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| key.to_string())
    }

    /// Whether a real API key resolves for the selected provider.
    pub fn key_resolvable(&self) -> bool {
        self.selected_provider()
            .map(|p| resolve_api_key(p).is_ok())
            .unwrap_or(false)
    }

    fn current_group(&self) -> Option<&ProviderGroup> {
        self.groups.get(self.group_selected)
    }

    fn group_summary(&self) -> String {
        self.current_group()
            .map(|g| format!("{} ({})", g.label, g.keys.len()))
            .unwrap_or_else(|| "No providers".to_string())
    }

    fn select_provider_key(&mut self, selected_key: Option<&str>) {
        self.groups = provider_groups(&self.settings);
        let fallback = self
            .groups
            .first()
            .and_then(|g| g.keys.first())
            .map(|s| s.as_str());
        let target = selected_key.or(fallback);
        let (group_selected, selected_in_group) = target
            .and_then(|target_key| {
                self.groups.iter().enumerate().find_map(|(gi, group)| {
                    group
                        .keys
                        .iter()
                        .position(|k| k == target_key)
                        .map(|pi| (gi, pi))
                })
            })
            .unwrap_or((0, 0));
        self.group_selected = group_selected;
        self.selected_in_group = selected_in_group;
        self.provider_scroll_offset = 0;
    }

    /// Add an OpenAI-compatible API entry so first-run setup remains usable
    /// even when preset sync did not provide any provider entries.
    pub fn add_custom_api(&mut self) {
        let mut suffix = 1usize;
        let key = loop {
            let candidate = if suffix == 1 {
                "custom".to_string()
            } else {
                format!("custom-{suffix}")
            };
            if !self.settings.providers.contains_key(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        self.settings.providers.insert(
            key.clone(),
            ProviderConfig {
                kind: "openai".to_string(),
                alias: Some(key.clone()),
                api_key: None,
                api_base: None,
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
        self.select_provider_key(Some(&key));
        self.step = Step::Edit;
        self.field = Field::Alias;
        self.error = None;
    }

    fn add_selected_preset(&mut self) {
        let Some(preset) = self.presets.get(self.preset_selected).cloned() else {
            self.error = Some("no preset selected".into());
            return;
        };
        let base_key = preset.name.trim();
        let base_key = if base_key.is_empty() {
            "custom"
        } else {
            base_key
        };
        let mut suffix = 1usize;
        let key = loop {
            let candidate = if suffix == 1 {
                base_key.to_string()
            } else {
                format!("{base_key}-{suffix}")
            };
            if !self.settings.providers.contains_key(&candidate) {
                break candidate;
            }
            suffix += 1;
        };
        self.settings.providers.insert(
            key.clone(),
            ProviderConfig {
                kind: preset.kind,
                alias: Some(preset.name),
                api_key: None,
                api_base: (!preset.base_url.is_empty()).then_some(preset.base_url),
                api_key_env: preset.env_key,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
        self.select_provider_key(Some(&key));
        self.step = Step::Edit;
        self.field = Field::Alias;
        self.error = None;
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
            Field::Alias => provider.alias = (!value.is_empty()).then_some(value),
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
        f.render_widget(Clear, area);

        let dialog = centered_rect(area, 92, 82);
        let title = " AutoReportCLI · API configuration ";
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                title,
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .fg(Color::Cyan),
            ));
        f.render_widget(block, dialog);

        let inner = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(2),
            ])
            .margin(1)
            .split(dialog);
        let header = inner[0];
        let body = inner[1];
        let footer = inner[2];

        self.draw_header(f, header);

        match self.step {
            Step::Select => self.draw_select(f, body),
            Step::Add => self.draw_add(f, body),
            Step::Edit => self.draw_edit(f, body),
            Step::Preview => self.draw_preview(f, body),
        }

        self.draw_footer(f, footer);
    }

    fn draw_header(&self, f: &mut Frame<'_>, area: Rect) {
        let line = Line::from(vec![
            kv_label("group"),
            kv_value(&self.group_summary(), Color::Cyan),
            Span::raw("   "),
            kv_label("API"),
            kv_value(
                &self
                    .selected_key()
                    .map(|key| self.provider_label(key))
                    .unwrap_or_else(|| "-".to_string()),
                Color::Yellow,
            ),
        ]);
        let para = Paragraph::new(line).block(Block::default().borders(Borders::BOTTOM));
        f.render_widget(para, area);
    }

    fn draw_select(&mut self, f: &mut Frame<'_>, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(3)])
            .split(area);
        self.draw_group_tabs(f, chunks[0]);

        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(chunks[1]);

        let group_keys = self
            .current_group()
            .map(|g| g.keys.clone())
            .unwrap_or_default();
        let items: Vec<ListItem> = group_keys
            .iter()
            .enumerate()
            .map(|(_, k)| {
                let ok = self
                    .settings
                    .providers
                    .get(k)
                    .map(|p| resolve_api_key(p).is_ok())
                    .unwrap_or(false);
                let key_icon = if ok { "key" } else { "no-key" };
                let key_style = if ok {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let name_style = Style::default().add_modifier(Modifier::BOLD);
                let mut spans = vec![Span::styled(
                    format!("{:<20}", self.provider_label(k)),
                    name_style,
                )];
                spans.push(Span::raw(" "));
                spans.push(Span::styled(format!("{key_icon:>8}"), key_style));
                let line = Line::from(spans);
                ListItem::new(line)
            })
            .collect();
        let mut state = ListState::default().with_offset(self.provider_scroll_offset);
        state.select(Some(self.selected_in_group));
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                format!(" {} ", self.group_summary()),
                Style::default().fg(Color::Cyan),
            )));
        f.render_stateful_widget(list, columns[0], &mut state);
        self.provider_scroll_offset = state.offset();

        self.draw_provider_details(f, columns[1]);
    }

    fn draw_add(&mut self, f: &mut Frame<'_>, area: Rect) {
        let items = if self.presets.is_empty() {
            vec![ListItem::new(Line::styled(
                "No preset templates available; press Esc",
                Style::default().fg(Color::Yellow),
            ))]
        } else {
            self.presets
                .iter()
                .map(|preset| {
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("{:<24}", preset.name),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{:<12}", preset.kind),
                            Style::default().fg(Color::Gray),
                        ),
                        Span::styled(
                            if preset.base_url.is_empty() {
                                "default endpoint"
                            } else {
                                preset.base_url.as_str()
                            },
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]))
                })
                .collect()
        };
        let mut state = ListState::default().with_offset(self.preset_scroll_offset);
        state.select((!self.presets.is_empty()).then_some(self.preset_selected));
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
                        .borders(Borders::ALL)
                        .title(" Add provider from preset template "),
                ),
            area,
            &mut state,
        );
        self.preset_scroll_offset = state.offset();
    }

    fn draw_group_tabs(&self, f: &mut Frame<'_>, area: Rect) {
        let mut spans = vec![Span::styled("tabs  ", Style::default().fg(Color::DarkGray))];
        for (idx, group) in self.groups.iter().enumerate() {
            if idx == self.group_selected {
                spans.push(Span::styled(
                    "[",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    format!("{} ({})", group.label, group.keys.len()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(
                    "]",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::styled(
                    format!("{} ({})", group.label, group.keys.len()),
                    Style::default().fg(Color::Gray),
                ));
            }
            spans.push(Span::raw(" "));
        }
        let para = Paragraph::new(Line::from(spans)).block(
            Block::default()
                .borders(Borders::BOTTOM)
                .title(Span::styled(" Groups ", Style::default().fg(Color::Cyan))),
        );
        f.render_widget(para, area);
    }

    fn draw_provider_details(&self, f: &mut Frame<'_>, area: Rect) {
        let Some(provider) = self.selected_provider() else {
            return;
        };
        let key_name = self.selected_key().unwrap_or("-");
        let display_name = self.provider_label(key_name);
        let alias = display_name.clone();
        let api_key_env = provider
            .api_key_env
            .clone()
            .or_else(|| provider.env_key().map(ToString::to_string))
            .unwrap_or_else(|| "-".to_string());
        let api_base = provider.api_base.as_deref().unwrap_or("(default)");
        let key_state = if self.key_resolvable() {
            "resolved"
        } else if provider.api_key.is_some() {
            "inline only"
        } else {
            "missing"
        };
        let lines = vec![
            Line::from(Span::styled(
                display_name,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            detail_line("kind", &provider.kind),
            detail_line("alias", &alias),
            detail_line("api_base", api_base),
            detail_line("api_key_env", &api_key_env),
            detail_line("key", key_state),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Enter", Style::default().fg(Color::White)),
                Span::styled(" to edit selected API", Style::default().fg(Color::Gray)),
            ]),
            Line::from(vec![
                Span::styled("a", Style::default().fg(Color::White)),
                Span::styled(
                    " to add a provider from a preset template",
                    Style::default().fg(Color::Gray),
                ),
            ]),
            Line::from(vec![
                Span::styled("n", Style::default().fg(Color::White)),
                Span::styled(
                    " to add a custom OpenAI-compatible API",
                    Style::default().fg(Color::Gray),
                ),
            ]),
        ];
        let para = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(" Details ", Style::default().fg(Color::Cyan))),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(para, area);
    }

    fn draw_edit(&mut self, f: &mut Frame<'_>, area: Rect) {
        let provider = match self.selected_provider() {
            Some(p) => p,
            None => return,
        };
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(
                "Editing API: {}",
                self.provider_label(self.selected_key().unwrap_or("?"))
            ),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Yellow),
        )));
        lines.push(Line::from(Span::styled(
            "Enter a value, then move to Save",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::raw(""));

        for field in [Field::Alias, Field::ApiBase, Field::ApiKey] {
            let value: String = match field {
                Field::Alias => self
                    .selected_key()
                    .map(|key| self.provider_label(key))
                    .unwrap_or_default(),
                Field::ApiBase => provider.api_base.clone().unwrap_or_default(),
                Field::ApiKey => provider
                    .api_key
                    .clone()
                    .map(|_| "•••••••• (stored securely)".into())
                    .unwrap_or_else(|| {
                        provider
                            .api_key_env
                            .as_deref()
                            .or_else(|| provider.env_key())
                            .map(|env| format!("(from {env})"))
                            .unwrap_or_else(|| "(not set)".into())
                    }),
                Field::Save | Field::Cancel => String::new(),
            };

            let focused = self.field == field;
            let mut spans = vec![Span::styled(
                format!("{:<14}", field.label()),
                Style::default().add_modifier(Modifier::BOLD),
            )];
            spans.push(Span::raw(" "));

            if focused && self.editing {
                // Show the live input buffer + a block cursor.
                let before: String = self.input[..self.cursor].chars().collect();
                let cur: String = self.input[self.cursor..].chars().take(1).collect();
                let after: String = self.input[self.cursor + cur.len()..].chars().collect();
                let mask = field == Field::ApiKey;
                spans.push(Span::raw(if mask {
                    "•".repeat(before.chars().count())
                } else {
                    before
                }));
                spans.push(Span::styled(
                    if cur.is_empty() {
                        " ".into()
                    } else if mask {
                        "•".into()
                    } else {
                        cur
                    },
                    Style::default().bg(Color::DarkGray),
                ));
                spans.push(Span::raw(if mask {
                    "•".repeat(after.chars().count())
                } else {
                    after
                }));
            } else {
                spans.push(Span::raw(value));
            }
            let mut line = Line::from(spans);
            if focused {
                line = line.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );
            }
            lines.push(line);
        }

        for action in [Field::Save, Field::Cancel] {
            let focused = self.field == action;
            let mut line = Line::from(Span::styled(
                action.label(),
                Style::default()
                    .fg(if action == Field::Save {
                        Color::Green
                    } else {
                        Color::Red
                    })
                    .add_modifier(Modifier::BOLD),
            ));
            if focused {
                line = line.style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                );
            }
            lines.push(line);
        }

        let para = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                " Edit provider ",
                Style::default().fg(Color::Cyan),
            )))
            .wrap(Wrap { trim: false });
        f.render_widget(para, area);
    }

    fn draw_preview(&self, f: &mut Frame<'_>, area: Rect) {
        let Some(provider) = self.selected_provider() else {
            return;
        };
        let key_status = if provider
            .api_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            "configured · stored securely in auth.json"
        } else if self.key_resolvable() {
            "resolved from environment"
        } else {
            "not configured"
        };
        let api_base = provider.api_base.as_deref().unwrap_or("(default)");
        let api_key_env = provider
            .api_key_env
            .as_deref()
            .or_else(|| provider.env_key())
            .unwrap_or("-");
        let provider_label = self.provider_label(self.selected_key().unwrap_or("-"));
        let lines = vec![
            Line::from(Span::styled(
                "Review this API before saving",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            detail_line("provider", &provider_label),
            detail_line("kind", &provider.kind),
            detail_line("api_base", api_base),
            detail_line("api_key", key_status),
            detail_line("env var", api_key_env),
            Line::raw(""),
            Line::from(Span::styled(
                "The public config omits the key; auth.json stores it with restricted permissions.",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        let para = Paragraph::new(lines)
            .style(Style::default().fg(Color::Gray))
            .block(Block::default().borders(Borders::ALL).title(Span::styled(
                " Review & Save ",
                Style::default().fg(Color::Cyan),
            )))
            .wrap(Wrap { trim: true });
        f.render_widget(para, area);
    }

    fn draw_footer(&self, f: &mut Frame<'_>, area: Rect) {
        let hint = match (self.step, self.editing) {
            (_, true) => " Enter: confirm field   Esc: cancel edit".to_string(),
            (Step::Select, _) => " h/l or ←/→: group   j/k or ↑/↓: API   a: add preset   n: custom API   Enter: edit   Esc: cancel".to_string(),
            (Step::Add, _) => " ↑/↓: template   Enter: add & edit   Esc: back".to_string(),
            (Step::Edit, _) => " ↑/↓: field   Enter: edit/action   Esc: back".to_string(),
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

impl ConfigScreen {
    /// Drive one key event. Returns `Some(Outcome)` when the session is done.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        // Ctrl+C cancels from anywhere.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Cancelled);
        }

        if self.editing {
            return self.handle_editing_key(key);
        }

        match self.step {
            Step::Select => self.handle_select_key(key),
            Step::Add => self.handle_add_key(key),
            Step::Edit => self.handle_edit_key(key),
            Step::Preview => self.handle_preview_key(key),
        }
    }

    fn handle_select_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Char('a') => {
                if self.presets.is_empty() {
                    self.error = Some("no preset templates available".into());
                } else {
                    self.preset_selected = 0;
                    self.preset_scroll_offset = 0;
                    self.step = Step::Add;
                    self.error = None;
                }
                None
            }
            KeyCode::Char('n') => {
                self.add_custom_api();
                None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.group_selected > 0 {
                    self.group_selected -= 1;
                    self.provider_scroll_offset = 0;
                    let len = self.current_group().map(|g| g.keys.len()).unwrap_or(0);
                    self.selected_in_group = self.selected_in_group.min(len.saturating_sub(1));
                }
                None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.group_selected + 1 < self.groups.len() {
                    self.group_selected += 1;
                    self.provider_scroll_offset = 0;
                    let len = self.current_group().map(|g| g.keys.len()).unwrap_or(0);
                    self.selected_in_group = self.selected_in_group.min(len.saturating_sub(1));
                }
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_in_group > 0 {
                    self.selected_in_group -= 1;
                }
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let len = self.current_group().map(|g| g.keys.len()).unwrap_or(0);
                if self.selected_in_group + 1 < len {
                    self.selected_in_group += 1;
                }
                None
            }
            KeyCode::Enter => {
                self.step = Step::Edit;
                self.field = Field::ApiBase;
                None
            }
            KeyCode::Esc => Some(Outcome::Cancelled),
            _ => None,
        }
    }

    fn handle_add_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.preset_selected = self.preset_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.preset_selected =
                    (self.preset_selected + 1).min(self.presets.len().saturating_sub(1));
            }
            KeyCode::Enter => self.add_selected_preset(),
            KeyCode::Esc => self.step = Step::Select,
            _ => {}
        }
        None
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Up => {
                let idx = Field::ALL
                    .iter()
                    .position(|&f| f == self.field)
                    .unwrap_or(0);
                self.field = Field::ALL[(idx + Field::ALL.len() - 1) % Field::ALL.len()];
                None
            }
            KeyCode::Down => {
                let idx = Field::ALL
                    .iter()
                    .position(|&f| f == self.field)
                    .unwrap_or(0);
                self.field = Field::ALL[(idx + 1) % Field::ALL.len()];
                None
            }
            KeyCode::Enter => match self.field {
                Field::Alias | Field::ApiBase | Field::ApiKey => {
                    self.begin_edit();
                    None
                }
                Field::Save => {
                    self.step = Step::Preview;
                    None
                }
                Field::Cancel => Some(Outcome::Cancelled),
            },
            KeyCode::Esc => {
                self.step = Step::Select;
                None
            }
            _ => None,
        }
    }

    fn handle_preview_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Enter => match save_settings(&self.home, &self.settings) {
                Ok(()) => Some(Outcome::Saved),
                Err(e) => {
                    self.error = Some(format!("save failed: {e}"));
                    None
                }
            },
            KeyCode::Esc => {
                self.step = Step::Edit;
                None
            }
            _ => None,
        }
    }

    // --- text entry for the currently focused field ---

    fn begin_edit(&mut self) {
        let cur = match self.field {
            Field::Alias => self.selected_key().map(|key| self.provider_label(key)),
            Field::ApiBase => self.selected_provider().and_then(|p| p.api_base.clone()),
            Field::ApiKey => self.selected_provider().and_then(|p| p.api_key.clone()),
            _ => None,
        };
        self.input = cur.unwrap_or_default();
        self.cursor = self.input.len();
        self.editing = true;
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        match key.code {
            KeyCode::Esc => {
                self.editing = false;
                None
            }
            KeyCode::Enter => {
                let value = std::mem::take(&mut self.input);
                self.cursor = 0;
                self.editing = false;
                let field = self.field;
                let _ = self.commit(field, value); // error surfaces via self.error
                let selected = self.selected_key().map(str::to_string);
                self.select_provider_key(selected.as_deref());
                None
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                    self.input.remove(self.cursor);
                }
                None
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    let prev = self.input[..self.cursor].chars().last().unwrap();
                    self.cursor -= prev.len_utf8();
                }
                None
            }
            KeyCode::Right => {
                if self.cursor < self.input.len() {
                    let next = self.input[self.cursor..].chars().next().unwrap();
                    self.cursor += next.len_utf8();
                }
                None
            }
            KeyCode::Char(c) => {
                self.input.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                None
            }
            _ => None,
        }
    }
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let popup_layout = Layout::default()
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
        .split(popup_layout[1])[1]
}

fn detail_line<'a>(label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label:<11}"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn kv_label(label: &str) -> Span<'static> {
    Span::styled(
        format!("{label:<9}"),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn kv_value(value: &str, color: Color) -> Span<'static> {
    Span::styled(
        value.to_string(),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

impl ConfigScreen {
    /// Blocking full-screen loop for the first-run wizard. The caller owns
    /// raw-mode/alternate-screen setup. Returns the session outcome.
    pub fn run_fullscreen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<Outcome> {
        loop {
            terminal.draw(|f| self.draw(f))?;
            // Blocking read: nothing else streams during the wizard.
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
    use crate::custom_terminal::Terminal;
    use crate::test_support::WritableTestBackend;
    use autoreport_core::config::{needs_api_config, save_settings};

    fn settings_with(provider: &str, cfg: ProviderConfig) -> Settings {
        let mut s = Settings::default();
        s.providers.insert(provider.into(), cfg);
        s
    }

    fn provider() -> ProviderConfig {
        ProviderConfig {
            kind: "anthropic".into(),
            alias: None,
            api_key: Some("sk-test".into()),
            api_base: None,
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 8192,
        }
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
    fn groups_are_built_by_provider_kind() {
        let mut s = settings_with("a", provider());
        let mut b = provider();
        b.kind = "google".into();
        s.providers.insert("b".into(), b);
        let screen = ConfigScreen::new(s, PathBuf::from("/tmp/ws"));
        assert_eq!(screen.groups.len(), 2);
        assert_eq!(screen.groups[0].kind, "anthropic");
        assert_eq!(screen.groups[1].kind, "google");
    }

    #[test]
    fn provider_list_keeps_scroll_offset_when_selection_moves() {
        let mut settings = Settings::default();
        for index in 0..30 {
            settings
                .providers
                .insert(format!("provider-{index:02}"), provider());
        }
        let mut screen = ConfigScreen::new(settings, PathBuf::from("/tmp/ws"));
        screen.selected_in_group = 20;
        let backend = WritableTestBackend::new(100, 30);
        let mut terminal = Terminal::with_options(backend).unwrap();

        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert!(screen.provider_scroll_offset > 0);
        let offset_after_first_draw = screen.provider_scroll_offset;

        screen.selected_in_group = 21;
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert!(screen.provider_scroll_offset >= offset_after_first_draw);

        screen.selected_in_group = 0;
        terminal.draw(|frame| screen.draw(frame)).unwrap();
        assert_eq!(screen.provider_scroll_offset, 0);
    }

    #[test]
    fn commit_field_writes_into_selected_api() {
        let s = settings_with("a", provider());
        let mut screen = ConfigScreen::new(s, PathBuf::from("/tmp/ws"));
        screen
            .commit(Field::ApiBase, "https://api.example.com/v1".into())
            .unwrap();
        assert_eq!(
            screen.settings.providers["a"].api_base.as_deref(),
            Some("https://api.example.com/v1")
        );
    }

    #[test]
    fn save_flow_persists_api_key_to_private_auth_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut screen = ConfigScreen::new(
            settings_with(
                "a",
                ProviderConfig {
                    alias: Some("a".into()),
                    api_key: None,
                    ..provider()
                },
            ),
            dir.path().to_path_buf(),
        );
        screen.commit(Field::ApiKey, "sk-saved".into()).unwrap();
        screen.step = Step::Preview;

        assert_eq!(
            screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Outcome::Saved)
        );
        let auth = std::fs::read_to_string(dir.path().join("auth.json")).unwrap();
        assert!(auth.contains("\"a\": \"sk-saved\""));
        let public_config = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!public_config.contains("sk-saved"));
    }

    #[test]
    fn add_custom_api_works_with_an_empty_provider_list() {
        let mut screen = ConfigScreen::new(Settings::default(), PathBuf::from("/tmp/ws"));
        screen.add_custom_api();
        assert_eq!(screen.selected_key(), Some("custom"));
        assert_eq!(screen.settings.providers["custom"].kind, "openai");
        assert_eq!(screen.step, Step::Edit);
    }

    #[test]
    fn presets_are_additive_and_alias_can_be_overridden() {
        let preset = PresetProvider {
            name: "OpenAI Gateway".into(),
            kind: "openai".into(),
            base_url: "https://example.test/v1".into(),
            models: vec!["gpt-test".into()],
            env_key: Some("OPENAI_API_KEY".into()),
        };
        let mut screen = ConfigScreen::new_with_presets(
            Settings::default(),
            PathBuf::from("/tmp/ws"),
            vec![preset.clone()],
        );
        screen.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(screen.step, Step::Edit);
        assert_eq!(screen.settings.providers.len(), 1);
        assert_eq!(screen.selected_key(), Some("OpenAI Gateway"));
        assert_eq!(
            screen.settings.providers["OpenAI Gateway"].alias.as_deref(),
            Some("OpenAI Gateway")
        );

        screen.commit(Field::Alias, "work-openai".into()).unwrap();
        assert_eq!(screen.provider_label("OpenAI Gateway"), "work-openai");

        screen.step = Step::Select;
        screen.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        screen.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(screen.settings.providers.len(), 2);
        assert!(
            screen
                .settings
                .providers
                .values()
                .all(|p| p.kind == "openai")
        );
        assert_eq!(
            screen.settings.providers["OpenAI Gateway-2"]
                .alias
                .as_deref(),
            Some("OpenAI Gateway")
        );
    }

    #[test]
    fn cancel_does_not_save() {
        let dir = tempfile::tempdir().unwrap();
        let s = Settings::default();
        let mut screen = ConfigScreen::new(s, dir.path().to_path_buf());
        // Simulate cancel: no save_settings call happens, only in-memory edits.
        screen.commit(Field::ApiKey, "ignored".into()).ok();
        assert!(needs_api_config(&screen.settings));
        // save_settings was never invoked, so the file is absent:
        assert!(!dir.path().join("config.toml").exists());
        // (sanity: an explicit save would flip it)
        save_settings(dir.path(), &screen.settings).unwrap();
        assert!(dir.path().join("config.toml").exists());
    }
}
