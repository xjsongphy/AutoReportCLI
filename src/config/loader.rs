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
    // Materialize bundled skills + report template (standalone defaults).
    crate::bundled::materialize(workspace);
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
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
    }
}

/// Resolve the effective API key for a provider: YAML value → preset env var →
/// kind default env var.
pub fn resolve_api_key(provider: &crate::config::schema::ProviderConfig) -> Result<String> {
    if let Some(k) = &provider.api_key {
        if !k.is_empty() {
            return Ok(k.clone());
        }
    }
    // Env var named explicitly by a synced cc-switch preset, if any.
    if let Some(env_name) = provider.api_key_env.as_deref() {
        if !env_name.is_empty() {
            if let Ok(k) = std::env::var(env_name) {
                if !k.is_empty() {
                    return Ok(k);
                }
            }
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
        provider
            .api_key_env
            .as_deref()
            .or_else(|| provider.env_key())
            .unwrap_or("the relevant key env var")
    ))
}

/// Where per-agent manifests / runtime metadata live.
pub fn metadata_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport")
}

/// Serialize settings to `autoreport.config.yaml` in `workspace`.
pub fn save_settings(workspace: &Path, settings: &Settings) -> Result<()> {
    let path = workspace.join("autoreport.config.yaml");
    let raw = serde_yaml::to_string(settings)
        .with_context(|| "serializing settings to YAML")?;
    std::fs::write(&path, raw)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// True when there is no config file AND no provider key is resolvable — the
/// first-run wizard trigger.
pub fn needs_config(workspace: &Path, settings: &Settings) -> bool {
    if workspace.join("autoreport.config.yaml").exists() {
        return false;
    }
    if settings.providers.is_empty() {
        return true;
    }
    settings.providers.values().all(|p| resolve_api_key(p).is_err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ProviderConfig;
    use tempfile::tempdir;

    #[test]
    fn save_settings_writes_yaml_and_roundtrips() {
        let dir = tempdir().unwrap();
        let mut settings = Settings::default();
        settings.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                kind: "anthropic".into(),
                model: "claude-x".into(),
                api_key: Some("sk-test".into()),
                api_base: None,
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
        settings.active_provider = Some("anthropic".into());

        save_settings(dir.path(), &settings).unwrap();
        assert!(dir.path().join("autoreport.config.yaml").exists());

        let reloaded = load_settings(dir.path()).unwrap();
        assert_eq!(reloaded.active_provider.as_deref(), Some("anthropic"));
        assert_eq!(reloaded.providers["anthropic"].model, "claude-x");
        assert_eq!(
            reloaded.providers["anthropic"].api_key.as_deref(),
            Some("sk-test")
        );
    }

    #[test]
    fn needs_config_true_when_no_file_and_no_key() {
        let dir = tempdir().unwrap();
        let settings = Settings::default(); // empty providers
        assert!(needs_config(dir.path(), &settings));
    }

    #[test]
    fn needs_config_false_when_file_exists() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("autoreport.config.yaml"), "active_provider: x\n").unwrap();
        let settings = Settings::default();
        assert!(!needs_config(dir.path(), &settings));
    }
}
