//! Global settings/auth loading, API-key resolution and workspace folder creation.

use crate::config::schema::{ModelConfig, Settings};
use crate::policy::AskForApproval;
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const CONFIG_TOML_FILE: &str = "config.toml";
pub const AUTH_JSON_FILE: &str = "auth.json";

#[derive(Debug, Default, Deserialize, Serialize)]
struct AuthFile {
    #[serde(default)]
    providers: BTreeMap<String, String>,
}

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

/// Resolve AutoReport's global home. This mirrors Codex's `find_codex_home`:
/// `AUTOREPORT_HOME` overrides the default `~/.autoreport`.
pub fn find_autoreport_home() -> Result<PathBuf> {
    autoreport_utils_home_dir::find_autoreport_home()
        .map(|path| path.to_path_buf())
        .context("resolving AUTOREPORT_HOME")
}

/// Create the global home and its stable program-state subdirectories.
pub fn ensure_autoreport_home(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating AutoReport home {}", home.display()))?;
    for dir in ["skills", "external", "templates", "agents", "workspaces"] {
        std::fs::create_dir_all(home.join(dir))
            .with_context(|| format!("creating {}", home.join(dir).display()))?;
    }
    crate::bundled::materialize(home);
    Ok(())
}

/// Return the global state directory associated with one workspace.
pub fn workspace_state_dir(home: &Path, workspace: &Path) -> PathBuf {
    autoreport_utils_home_dir::workspace_state_dir(home, workspace)
}

/// Load settings from the global Codex-style `config.toml` plus `auth.json`.
/// Environment variables override individual fields.
pub fn load_settings(home: &Path) -> Result<Settings> {
    let path = home.join(CONFIG_TOML_FILE);
    if path.exists() {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut settings: Settings =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        merge_auth_keys(home, &mut settings)?;
        apply_env_overrides(&mut settings);
        normalize(&mut settings);
        Ok(settings)
    } else {
        log::info!(
            "no config file at {}; using defaults (set providers via env vars)",
            path.display()
        );
        let mut settings = Settings::default();
        merge_auth_keys(home, &mut settings)?;
        apply_env_overrides(&mut settings);
        normalize(&mut settings);
        Ok(settings)
    }
}

/// Pull provider credentials/URLs from the environment when the TOML omits
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

/// Post-parse normalization. The approval policy is honored as-written: `Never`
/// (the default) runs everything without asking; the other codex variants route
/// the `exec` tool through the interactive approval flow in `execute_tool_call`.
/// We only mirror `context_window` into the per-agent defaults here.
fn normalize(settings: &mut Settings) {
    let _ = AskForApproval::Never; // keep the import meaningful for future validation
    // Mirror the top-level user-facing `context_window` into the per-agent
    // defaults that runtime code (auto-compaction) actually reads.
    settings.agents.context_window = settings.context_window;
}

fn try_register(settings: &mut Settings, key: &str, kind: &str, env: &str, api_base: Option<&str>) {
    if let Ok(_k) = std::env::var(env) {
        settings.providers.insert(
            key.to_string(),
            crate::config::schema::ProviderConfig {
                kind: kind.to_string(),
                alias: Some(key.to_string()),
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
/// when its key can be resolved from auth.json or the environment.
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

/// Resolve the effective API key for a provider: auth.json value → preset env var →
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

/// Serialize settings to the global Codex-style `config.toml`.
pub fn save_settings(home: &Path, settings: &Settings) -> Result<()> {
    let path = home.join(CONFIG_TOML_FILE);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    save_auth(home, settings)?;
    let mut public_settings = settings.clone();
    for provider in public_settings.providers.values_mut() {
        provider.api_key = None;
    }
    let raw =
        toml::to_string_pretty(&public_settings).with_context(|| "serializing settings to TOML")?;
    std::fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn load_auth(home: &Path) -> Result<AuthFile> {
    let path = home.join(AUTH_JSON_FILE);
    if !path.exists() {
        return Ok(AuthFile::default());
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

fn merge_auth_keys(home: &Path, settings: &mut Settings) -> Result<()> {
    let auth = load_auth(home)?;
    for (name, key) in auth.providers {
        if !key.is_empty()
            && let Some(provider) = settings.providers.get_mut(&name)
            && provider.api_key.as_deref().is_none_or(str::is_empty)
        {
            provider.api_key = Some(key);
        }
    }
    Ok(())
}

fn save_auth(home: &Path, settings: &Settings) -> Result<()> {
    let path = home.join(AUTH_JSON_FILE);
    let mut auth = load_auth(home)?;
    for (name, provider) in &settings.providers {
        match provider.api_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => {
                auth.providers.insert(name.clone(), key.to_string());
            }
            _ => {
                auth.providers.remove(name);
            }
        }
    }
    if auth.providers.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(&auth).context("serializing auth.json")?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = file.metadata()?.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
    }
    use std::io::Write;
    file.write_all(raw.as_bytes())?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ProviderConfig;
    use tempfile::tempdir;

    #[test]
    fn ensure_workspace_creates_all_required_dirs_when_empty() {
        let dir = tempdir().unwrap();
        ensure_workspace(dir.path()).unwrap();
        for d in REQUIRED_DIRS {
            assert!(dir.path().join(d).exists(), "missing required dir {d}");
        }
    }

    #[test]
    fn save_settings_writes_toml_and_roundtrips() {
        let dir = tempdir().unwrap();
        let mut settings = Settings::default();
        settings.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                kind: "anthropic".into(),
                alias: None,
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
        assert!(dir.path().join(CONFIG_TOML_FILE).exists());
        assert!(dir.path().join(AUTH_JSON_FILE).exists());
        let public_config = std::fs::read_to_string(dir.path().join(CONFIG_TOML_FILE)).unwrap();
        assert!(!public_config.contains("sk-test"));

        let reloaded = load_settings(dir.path()).unwrap();
        assert_eq!(reloaded.models.main.provider, "anthropic");
        assert_eq!(reloaded.models.main.model, "claude-x");
        assert_eq!(
            reloaded.providers["anthropic"].api_key.as_deref(),
            Some("sk-test")
        );
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
    fn loader_preserves_non_never_approval() {
        let dir = tempdir().unwrap();
        // Non-`Never` policies are now honored: they route `exec` through the
        // interactive approval flow. The loader must NOT rewrite them.
        std::fs::write(
            dir.path().join(CONFIG_TOML_FILE),
            "[agents]\napproval_policy = \"on-request\"\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert_eq!(
            settings.agents.approval_policy,
            crate::policy::AskForApproval::OnRequest
        );
    }

    #[test]
    fn loader_keeps_never_approval() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_TOML_FILE),
            "[agents]\napproval_policy = \"never\"\n",
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
            dir.path().join(CONFIG_TOML_FILE),
            "[providers.openai]\nkind = \"openai\"\n",
        )
        .unwrap();
        let settings = load_settings(dir.path()).unwrap();
        assert!(needs_api_config(&settings));
    }

    #[test]
    fn removed_legacy_config_fields_are_rejected() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(CONFIG_TOML_FILE),
            "active_provider = \"openai\"\n[providers.openai]\nkind = \"openai\"\nmodel = \"gpt-old\"\n",
        )
        .unwrap();

        assert!(load_settings(dir.path()).is_err());
    }
}
