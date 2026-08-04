//! Shared two-page configuration flow used at first start and by `/model`.

use crate::config_update::{ConfigScreen, Outcome};
use crate::custom_terminal::{Frame, Terminal};
use crate::model_migration::ModelScreen;
use autoreport_core::config::schema::Settings;
use autoreport_core::sync::PresetProvider;
use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Providers,
    Models,
}

/// Keeps both editor pages alive so moving Back never discards the provider
/// draft. The caller persists `settings()` once the model page is complete.
pub struct ConfigurationFlow {
    providers: ConfigScreen,
    models: Option<ModelScreen>,
    page: Page,
    home: PathBuf,
}

impl ConfigurationFlow {
    pub fn new(settings: Settings, home: PathBuf, presets: Vec<PresetProvider>) -> Self {
        Self {
            providers: ConfigScreen::new_with_presets(settings, home.clone(), presets),
            models: None,
            page: Page::Providers,
            home,
        }
    }

    pub fn settings(&self) -> &Settings {
        match self.page {
            Page::Providers => &self.providers.settings,
            Page::Models => {
                &self
                    .models
                    .as_ref()
                    .expect("model page initialized")
                    .settings
            }
        }
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        match self.page {
            Page::Providers => self.providers.draw(frame),
            Page::Models => self
                .models
                .as_mut()
                .expect("model page initialized")
                .draw(frame),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Cancelled);
        }
        match self.page {
            Page::Providers => match self.providers.handle_key(key) {
                Some(Outcome::Continue | Outcome::Saved) => {
                    self.models = Some(ModelScreen::new(
                        self.providers.settings.clone(),
                        self.home.clone(),
                    ));
                    self.page = Page::Models;
                    None
                }
                outcome => outcome,
            },
            Page::Models => match self
                .models
                .as_mut()
                .expect("model page initialized")
                .handle_key(key)
            {
                Some(Outcome::Cancelled) => {
                    let settings = self
                        .models
                        .as_ref()
                        .expect("model page initialized")
                        .settings
                        .clone();
                    self.providers.replace_settings(settings);
                    self.page = Page::Providers;
                    None
                }
                outcome => outcome,
            },
        }
    }

    pub fn run_fullscreen(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> io::Result<Outcome> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            match event::read()? {
                event::Event::Resize(width, height) => {
                    terminal.resize(ratatui::layout::Size::new(width, height))?
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
    use autoreport_core::config::schema::ProviderConfig;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn provider() -> ProviderConfig {
        ProviderConfig {
            kind: "openai".into(),
            alias: Some("Work API".into()),
            api_key: Some("test".into()),
            api_base: None,
            api_key_env: None,
            temperature: 0.1,
            max_tokens: 8192,
        }
    }

    #[test]
    fn continue_opens_models_without_discarding_provider_draft() {
        let mut settings = Settings::default();
        settings.providers.insert("work".into(), provider());
        let mut flow = ConfigurationFlow::new(settings, PathBuf::from("/tmp/ws"), vec![]);
        flow.providers
            .commit(
                crate::config_update::Field::ApiBase,
                "https://example.test/v1".into(),
            )
            .unwrap();
        assert_eq!(flow.handle_key(key(KeyCode::Char('c'))), None);
        assert_eq!(flow.page, Page::Models);
        assert_eq!(
            flow.settings().providers["work"].api_base.as_deref(),
            Some("https://example.test/v1")
        );
    }

    #[test]
    fn back_from_models_restores_the_provider_page_and_draft() {
        let mut settings = Settings::default();
        settings.providers.insert("work".into(), provider());
        let mut flow = ConfigurationFlow::new(settings, PathBuf::from("/tmp/ws"), vec![]);
        flow.providers.selected_in_group = 0;
        flow.handle_key(key(KeyCode::Char('c')));
        flow.models.as_mut().unwrap().settings.models.main.model = "gpt-test".into();
        assert_eq!(flow.handle_key(key(KeyCode::Esc)), None);
        assert_eq!(flow.page, Page::Providers);
        assert_eq!(flow.providers.settings.models.main.model, "gpt-test");
        assert_eq!(flow.providers.selected_key(), Some("work"));
    }

    #[test]
    fn q_exits_the_whole_flow_instead_of_returning_to_providers() {
        let mut settings = Settings::default();
        settings.providers.insert("work".into(), provider());
        let mut flow = ConfigurationFlow::new(settings, PathBuf::from("/tmp/ws"), vec![]);

        assert_eq!(flow.handle_key(key(KeyCode::Char('c'))), None);
        assert_eq!(flow.page, Page::Models);
        assert_eq!(
            flow.handle_key(key(KeyCode::Char('q'))),
            Some(Outcome::Quit)
        );
        assert_eq!(flow.page, Page::Models);
    }

    #[test]
    fn completing_both_model_assignments_returns_saved() {
        let mut settings = Settings::default();
        settings.providers.insert("work".into(), provider());
        let mut flow = ConfigurationFlow::new(settings, PathBuf::from("/tmp/ws"), vec![]);

        // Provider picker: `c` advances to the model page. Bind the sole
        // usable provider and a model name to Main, then repeat for Sub.
        assert_eq!(flow.handle_key(key(KeyCode::Char('c'))), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Char('m'))), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Down)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);
        assert_eq!(flow.handle_key(key(KeyCode::Char('s'))), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), None);

        assert_eq!(flow.handle_key(key(KeyCode::Char('s'))), None);
        assert_eq!(flow.handle_key(key(KeyCode::Enter)), Some(Outcome::Saved));
    }
}
