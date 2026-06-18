//! Settings loading, API-key resolution and workspace folder creation.

use crate::config::schema::Settings;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// The fixed directories every AutoReport project owns.
/// Users may only add/remove files *inside* these, never rename them.
pub const REQUIRED_DIRS: &[&str] = &[
    "data",
    "data/processed",
    "references",
    "theory",
    "code",
    "tex",
    "outline",
    ".autoreport",
];

/// Create any missing required directories under `workspace`. Idempotent.
pub fn ensure_workspace(workspace: &Path) -> Result<()> {
    for dir in REQUIRED_DIRS {
        let path = workspace.join(dir);
        if !path.exists() {
            std::fs::create_dir_all(&path)
                .with_context(|| format!("creating directory {}", path.display()))?;
            log::info!("created missing directory {}", path.display());
        }
    }
    Ok(())
}

/// Load settings from `autoreport.config.yaml` in `workspace`, falling back to
/// defaults if absent. Environment variables override individual fields.
pub fn load_settings(workspace: &Path) -> Result<Settings> {
    let path = workspace.join("autoreport.config.yaml");
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut settings: Settings = serde_yaml::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))?;
        apply_env_overrides(&mut settings);
        Ok(settings)
    } else {
        log::info!(
            "no config file at {}; using defaults (set providers via env vars)",
            path.display()
        );
        let mut settings = Settings::default();
        apply_env_overrides(&mut settings);
        Ok(settings)
    }
}

/// Pull provider credentials/URLs from the environment when the YAML omits
/// them, so the tool works without a committed config file.
fn apply_env_overrides(settings: &mut Settings) {
    if settings.providers.is_empty() {
        // Auto-register providers from well-known env vars.
        try_register(settings, "anthropic", "anthropic", "ANTHROPIC_API_KEY", None);
        try_register(
            settings,
            "openai",
            "openai",
            "OPENAI_API_KEY",
            Some("https://api.openai.com/v1"),
        );
        try_register(
            settings,
            "deepseek",
            "deepseek",
            "DEEPSEEK_API_KEY",
            Some("https://api.deepseek.com/v1"),
        );
        try_register(
            settings,
            "openrouter",
            "openrouter",
            "OPENROUTER_API_KEY",
            Some("https://openrouter.ai/api/v1"),
        );
    }
    if settings.active_provider.is_none() {
        settings.active_provider = settings.providers.keys().next().cloned();
    }
}

fn try_register(
    settings: &mut Settings,
    key: &str,
    kind: &str,
    env: &str,
    api_base: Option<&str>,
) {
    if let Ok(_k) = std::env::var(env) {
        settings.providers.insert(
            key.to_string(),
            crate::config::schema::ProviderConfig {
                kind: kind.to_string(),
                model: String::new(), // factory picks a default per kind
                api_key: None,
                api_base: api_base.map(String::from),
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
    }
}

/// Resolve the effective API key for a provider: YAML value → env var.
pub fn resolve_api_key(provider: &crate::config::schema::ProviderConfig) -> Result<String> {
    if let Some(k) = &provider.api_key {
        if !k.is_empty() {
            return Ok(k.clone());
        }
    }
    if let Some(env_name) = provider.env_key() {
        if let Ok(k) = std::env::var(env_name) {
            if !k.is_empty() {
                return Ok(k);
            }
        }
    }
    Err(anyhow!(
        "no API key for provider kind '{}' — set {} in the environment or config",
        provider.kind,
        provider.env_key().unwrap_or("the relevant key env var")
    ))
}

/// Where per-agent manifests / runtime metadata live.
pub fn metadata_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport")
}
