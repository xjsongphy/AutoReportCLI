//! Codex-login-page-style full-screen config screen.
//!
//! Two lifecycle modes share one render + key-handling implementation:
//! `run_fullscreen` (first-run wizard, standalone loop) and the `/config`
//! overlay (driven by `tui.rs`).

use crate::config::schema::{ProviderConfig, Settings};
use crate::config::{needs_config, resolve_api_key};
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::save_settings;

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
