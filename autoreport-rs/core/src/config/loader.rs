//! Settings loading, API-key resolution and workspace folder creation.

use crate::config::schema::{ModelConfig, Settings};
use crate::policy::AskForApproval;
use anyhow::{Context, Result, anyhow};
use std::path::{Path, PathBuf};

/// The fixed directories every AutoReport project owns.
/// Users may only add/remove files *inside* these, never rename them.
pub const REQUIRED_DIRS: &[&str] = &[
    "Data",
    "Data/Processed",
    "References",
    "Theory",
    "Plots",
    "Plots/Fig",
    "Plots/Scripts",
    "Tex",
    "Outline",
    ".autoreport",
];

/// Lowercase -> capitalized directory pairs for migrating projects created
/// before the directory-name capitalization change. Applied once, in
/// `ensure_workspace`, before the create-missing loop: if the capitalized dir
/// does not exist but the legacy lowercase one does, rename it in place.
const LEGACY_DIR_RENAMES: &[(&str, &str)] = &[
    ("data/processed", "Data/Processed"),
    ("data", "Data"),
    ("references", "References"),
    ("theory", "Theory"),
    ("code", "Plots"),
    ("tex", "Tex"),
    ("outline", "Outline"),
];

/// Create any missing required directories under `workspace`. Idempotent.
pub fn ensure_workspace(workspace: &Path) -> Result<()> {
    // One-time migration: rename legacy lowercase dirs to the capitalized
    // layout. Order matters — rename `data/processed` before `data`, etc.
    for (legacy, current) in LEGACY_DIR_RENAMES {
        let new_path = workspace.join(current);
        let old_path = workspace.join(legacy);
        if !new_path.exists() && old_path.exists() {
            if let Some(parent) = new_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::rename(&old_path, &new_path).with_context(|| {
                format!("migrating {} -> {}", old_path.display(), new_path.display())
            })?;
            log::info!("migrated directory {} -> {}", legacy, current);
        }
    }

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
        let mut settings: Settings =
            serde_yaml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        apply_env_overrides(&mut settings);
        normalize(&mut settings);
        Ok(settings)
    } else {
        log::info!(
            "no config file at {}; using defaults (set providers via env vars)",
            path.display()
        );
        let mut settings = Settings::default();
        apply_env_overrides(&mut settings);
        normalize(&mut settings);
        Ok(settings)
    }
}

/// Pull provider credentials/URLs from the environment when the YAML omits
/// them, so the tool works without a committed config file.
fn apply_env_overrides(settings: &mut Settings) {
    if settings.providers.is_empty() {
        // Auto-register providers from well-known env vars.
        try_register(
            settings,
            "anthropic",
            "anthropic",
            "ANTHROPIC_API_KEY",
            None,
        );
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
}

/// Post-parse normalization. Today this only clamps the approval policy: only
/// `AskForApproval::Never` is wired into the agent loop, so any other codex
/// variant the user set is logged and forced to `Never` (rather than silently
/// behaving as if honored). Once interactive approval is implemented, this
/// clamp moves aside.
fn normalize(settings: &mut Settings) {
    if settings.agents.approval_policy != AskForApproval::Never {
        log::warn!(
            "approval_policy: only 'never' is currently supported (got '{}'); treating as 'never'",
            settings.agents.approval_policy
        );
        settings.agents.approval_policy = AskForApproval::Never;
    }
    // Mirror the top-level user-facing `context_window` into the per-agent
    // defaults that runtime code (auto-compaction) actually reads.
    settings.agents.context_window = settings.context_window;
    migrate_legacy_model_settings(settings);
}

/// Populate the new model bindings from an older `active_provider` plus its
/// `model`, but only when a binding has not already been configured.
fn migrate_legacy_model_settings(settings: &mut Settings) {
    let fallback_provider = settings
        .legacy_active_provider
        .clone()
        .filter(|key| settings.providers.contains_key(key))
        .or_else(|| settings.providers.keys().next().cloned());
    let Some(provider) = fallback_provider else {
        return;
    };
    let legacy_model = settings
        .providers
        .get(&provider)
        .and_then(|cfg| cfg.legacy_model.clone())
        .unwrap_or_default();
    for selection in [&mut settings.models.main, &mut settings.models.sub] {
        let inherited_provider = selection.provider.is_empty();
        if selection.provider.is_empty() {
            selection.provider = provider.clone();
        }
        // Never copy a model name from one API onto an explicitly selected
        // different API. A partially migrated config must still open the model
        // page instead of silently sending (for example) a Claude model to an
        // OpenAI endpoint.
        if selection.model.is_empty() && (inherited_provider || selection.provider == provider) {
            selection.model = legacy_model.clone();
        }
    }
}

fn try_register(settings: &mut Settings, key: &str, kind: &str, env: &str, api_base: Option<&str>) {
    if let Ok(_k) = std::env::var(env) {
        settings.providers.insert(
            key.to_string(),
            crate::config::schema::ProviderConfig {
                kind: kind.to_string(),
                legacy_model: None,
                api_key: None,
                api_base: api_base.map(String::from),
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
    }
}

/// True when both runtime model bindings are complete and reference known APIs.
pub fn needs_model_config(settings: &Settings) -> bool {
    [&settings.models.main, &settings.models.sub]
        .iter()
        .any(|model| {
            model.provider.is_empty()
                || model.model.is_empty()
                || settings
                    .providers
                    .get(&model.provider)
                    .is_none_or(|provider| resolve_api_key(provider).is_err())
        })
}

/// True when startup needs the API configuration page. An API is usable only
/// when its key can be resolved from inline config or the environment.
pub fn needs_api_config(settings: &Settings) -> bool {
    settings
        .providers
        .values()
        .all(|provider| resolve_api_key(provider).is_err())
}

/// Resolve one model binding to its API configuration and model identifier.
pub fn resolve_model<'a>(
    settings: &'a Settings,
    model: &'a ModelConfig,
    label: &str,
) -> Result<(&'a crate::config::schema::ProviderConfig, &'a str)> {
    if model.provider.trim().is_empty() || model.model.trim().is_empty() {
        return Err(anyhow!("{label} model is not configured; run /models"));
    }
    let provider = settings
        .providers
        .get(&model.provider)
        .ok_or_else(|| anyhow!("{label} model references unknown API '{}'", model.provider))?;
    Ok((provider, model.model.trim()))
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
    let raw = serde_yaml::to_string(settings).with_context(|| "serializing settings to YAML")?;
    std::fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// True when there is no config file AND no provider key is resolvable — the
/// first-run wizard trigger.
pub fn needs_config(workspace: &Path, settings: &Settings) -> bool {
    let _ = workspace;
    needs_api_config(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ProviderConfig;
    use tempfile::tempdir;

    #[test]
    fn ensure_workspace_migrates_legacy_lowercase_dirs() {
        let dir = tempdir().unwrap();
        let ws = dir.path();
        // Seed legacy lowercase layout with content.
        std::fs::create_dir_all(ws.join("data/processed")).unwrap();
        std::fs::create_dir_all(ws.join("code")).unwrap();
        std::fs::write(ws.join("data/processed/out.csv"), "x").unwrap();
        std::fs::write(ws.join("code/plot.py"), "x").unwrap();

        ensure_workspace(ws).unwrap();

        // Content is reachable at the capitalized paths. On case-sensitive
        // filesystems the lowercase dirs are renamed; on case-insensitive ones
        // (Windows NTFS) the case-only rename is a no-op but the content is
        // accessible via the capitalized name regardless.
        assert!(ws.join("Data/Processed/out.csv").exists());
        assert!(ws.join("Plots/plot.py").exists());
        // New required sub-dirs created.
        assert!(ws.join("Plots/Fig").exists());
        assert!(ws.join("Plots/Scripts").exists());
        assert!(ws.join("Outline").exists());
    }

    #[test]
    fn ensure_workspace_creates_all_required_dirs_when_empty() {
        let dir = tempdir().unwrap();
        ensure_workspace(dir.path()).unwrap();
        for d in REQUIRED_DIRS {
            assert!(dir.path().join(d).exists(), "missing required dir {d}");
        }
    }

    #[test]
    fn save_settings_writes_yaml_and_roundtrips() {
        let dir = tempdir().unwrap();
        let mut settings = Settings::default();
        settings.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                kind: "anthropic".into(),
                legacy_model: None,
                api_key: Some("sk-test".into()),
                api_base: None,
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
        settings.models.main = ModelConfig {
            provider: "anthropic".into(),
            model: "claude-x".into(),
        };
        settings.models.sub = settings.models.main.clone();

        save_settings(dir.path(), &settings).unwrap();
        assert!(dir.path().join("autoreport.config.yaml").exists());

        let reloaded = load_settings(dir.path()).unwrap();
        assert_eq!(reloaded.models.main.provider, "anthropic");
        assert_eq!(reloaded.models.main.model, "claude-x");
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
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "providers:\n  openai:\n    api_key: test\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert!(!needs_config(dir.path(), &settings));
    }

    #[test]
    fn default_approval_policy_is_never() {
        let settings = Settings::default();
        assert_eq!(
            settings.agents.approval_policy,
            crate::policy::AskForApproval::Never
        );
    }

    #[test]
    fn loader_clamps_non_never_approval_to_never() {
        let dir = tempdir().unwrap();
        // Valid codex value, but unsupported here — loader must warn + clamp.
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "agents:\n  approval_policy: on-request\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert_eq!(
            settings.agents.approval_policy,
            crate::policy::AskForApproval::Never
        );
    }

    #[test]
    fn loader_keeps_never_approval() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "agents:\n  approval_policy: never\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert_eq!(
            settings.agents.approval_policy,
            crate::policy::AskForApproval::Never
        );
    }

    #[test]
    fn needs_api_config_when_existing_file_has_no_resolvable_key() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "providers:\n  openai:\n    kind: openai\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert!(needs_config(dir.path(), &settings));
    }

    #[test]
    fn partial_migration_never_crosses_api_and_model() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "active_provider: anthropic\nproviders:\n  anthropic:\n    kind: anthropic\n    model: claude-legacy\n    api_key: test\n  openai:\n    kind: openai\n    api_key: test\nmodels:\n  main:\n    provider: openai\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert_eq!(settings.models.main.provider, "openai");
        assert!(settings.models.main.model.is_empty());
        assert!(needs_model_config(&settings));
    }

    #[test]
    fn legacy_active_provider_and_model_migrate_to_main_and_sub() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("autoreport.config.yaml"),
            "active_provider: legacy\nproviders:\n  legacy:\n    kind: openai\n    model: gpt-legacy\n    api_key: test\n",
        )
        .unwrap();

        let settings = load_settings(dir.path()).unwrap();
        assert_eq!(settings.models.main.provider, "legacy");
        assert_eq!(settings.models.main.model, "gpt-legacy");
        assert_eq!(settings.models.sub.provider, "legacy");
        assert_eq!(settings.models.sub.model, "gpt-legacy");

        save_settings(dir.path(), &settings).unwrap();
        let saved = std::fs::read_to_string(dir.path().join("autoreport.config.yaml")).unwrap();
        assert!(!saved.contains("active_provider"));
        let saved_yaml: serde_yaml::Value = serde_yaml::from_str(&saved).unwrap();
        assert!(saved_yaml["providers"]["legacy"].get("model").is_none());
        assert!(saved.contains("models:"));
    }
}
