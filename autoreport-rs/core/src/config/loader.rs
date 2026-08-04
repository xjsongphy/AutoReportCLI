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
    "Report",
    "Outline",
];

/// Return whether the workspace already contains the complete report layout.
///
/// This only checks the fixed directory structure; files inside the report
/// directories remain user-owned and are intentionally not inspected.
pub fn workspace_is_complete(workspace: &Path) -> bool {
    REQUIRED_DIRS.iter().all(|dir| workspace.join(dir).is_dir())
}

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
    for dir in [
        "resources/latex/skills",
        "resources/latex/templates",
        "resources/latex/themes",
        "resources/typst/skills",
        "resources/typst/templates",
        "resources/typst/themes",
        "external/providers",
        "agents",
        "workspaces",
    ] {
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

/// Post-parse normalization. The approval policy is honored as-written:
/// `OnRequest` (the default) runs safe commands silently and only prompts for
/// commands classified as dangerous; `Never` forbids dangerous commands
/// without asking; `untrusted`/`granular` offer finer control. All non-`Never`
/// variants route the `exec` tool through the interactive approval flow in
/// `execute_tool_call`. We only mirror `context_window` into the per-agent
/// defaults here.
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
        return Err(anyhow!("{label} model is not configured; run /model"));
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
    atomic_write(&path, raw.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
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
    atomic_write_secret(&path, raw.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Atomic file replacement — crash-safe writes for `config.toml` / `auth.json`.
// ---------------------------------------------------------------------------

/// A unique temp path in the SAME directory as `path`, so the final rename is
/// atomic on the same filesystem. A unique name also prevents two concurrent
/// writers (e.g. a sync + a settings save) from clobbering each other's staging
/// file. Mirrors the pattern in `sync::atomic_write` and
/// `project::save_project_config`.
fn sibling_temp_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()))
}

/// Rename `temp` over `target`; on Windows the target must be removed first
/// because `rename` refuses to overwrite an existing file.
fn rename_over(temp: &Path, target: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    if target.exists() {
        std::fs::remove_file(target)?;
    }
    std::fs::rename(temp, target)
}

/// Atomically replace `path` with `data` via a sibling temp file + rename, so a
/// crash mid-write can never leave a truncated/partial file visible to readers.
/// The temp lives in the same directory (so `rename` is atomic on the same
/// filesystem). Used for non-secret files such as `config.toml`. If the write or
/// rename fails the temp is removed so no stale staging file is left behind.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp = sibling_temp_path(path);
    let result: std::io::Result<()> = (|| {
        std::fs::write(&temp, data)?;
        rename_over(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Atomically replace `path` with `data` while pinning the file mode to `0o600`
/// on Unix, so secret data (API keys in `auth.json`) is never world-readable —
/// not even transiently. The mode is applied to the temp file BEFORE the rename,
/// so the visible file never appears with looser permissions than the final
/// `0o600`. On non-Unix targets this is equivalent to [`atomic_write`].
fn atomic_write_secret(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let temp = sibling_temp_path(path);
    let result: std::io::Result<()> = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Create the temp with `0o600` from the start so the secret bytes
            // are never readable by other users at any point.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&temp)?;
            file.write_all(data)?;
            file.flush()?;
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&temp, data)?;
        }
        rename_over(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
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
            assert!(dir.path().join(d).is_dir(), "missing required dir {d}");
        }
        assert!(workspace_is_complete(dir.path()));
    }

    #[test]
    fn workspace_is_incomplete_when_a_required_dir_is_missing() {
        let dir = tempdir().unwrap();
        ensure_workspace(dir.path()).unwrap();
        std::fs::remove_dir_all(dir.path().join("Plots/Fig")).unwrap();

        assert!(!workspace_is_complete(dir.path()));
    }

    #[test]
    fn workspace_is_incomplete_when_a_required_path_is_not_a_directory() {
        let dir = tempdir().unwrap();
        ensure_workspace(dir.path()).unwrap();
        std::fs::remove_dir(dir.path().join("Report")).unwrap();
        std::fs::write(dir.path().join("Report"), "not a directory").unwrap();

        assert!(!workspace_is_complete(dir.path()));
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
    fn default_approval_policy_is_on_request() {
        let settings = Settings::default();
        assert_eq!(
            settings.agents.approval_policy,
            crate::policy::AskForApproval::OnRequest
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

    // ---- Atomic write: crash-safe config/auth persistence ----

    /// A pre-existing target must be fully replaced, never appended to or left
    /// as a mix of old + new bytes (the failure mode of truncate-then-write).
    #[test]
    fn atomic_write_fully_replaces_preexisting_target() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(CONFIG_TOML_FILE);
        std::fs::write(&path, "OLD CONTENT THAT IS LONGER THAN THE NEW PAYLOAD").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "new",
            "target must be fully replaced, not appended/partial"
        );
    }

    /// Atomic write of a brand-new file (no pre-existing target) works.
    #[test]
    fn atomic_write_creates_new_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("fresh.toml");
        atomic_write(&path, b"payload").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "payload");
    }

    /// The secret path keeps content correct AND pins `0o600` on Unix — even
    /// when the previous file was world-readable, and even on first creation.
    #[test]
    fn atomic_write_secret_preserves_content_and_0600_on_unix() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(AUTH_JSON_FILE);
        // Start from a world-readable file to prove the write tightens perms.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&path, b"old-secret").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        atomic_write_secret(&path, b"super-secret-key").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "super-secret-key",
            "content must be exactly the new payload"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "auth file must keep 0o600 after atomic write");
        }
    }

    /// `save_auth` must persist keys AND keep `auth.json` at `0o600` on Unix,
    /// replacing any prior content atomically.
    #[test]
    fn save_auth_persists_keys_atomically_with_0600_on_unix() {
        let dir = tempdir().unwrap();
        let mut settings = Settings::default();
        settings.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                kind: "anthropic".into(),
                alias: None,
                api_key: Some("sk-secret".into()),
                api_base: None,
                api_key_env: None,
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
        // A stale, longer auth body — must be fully replaced, not merged.
        std::fs::write(
            dir.path().join(AUTH_JSON_FILE),
            "{\"providers\":{\"anthropic\":\"sk-OLD-VALUE-THAT-IS-LONGER\"}}",
        )
        .unwrap();
        save_auth(dir.path(), &settings).unwrap();

        let auth_path = dir.path().join(AUTH_JSON_FILE);
        let raw = std::fs::read_to_string(&auth_path).unwrap();
        assert!(raw.contains("sk-secret"), "new key must be present: {raw}");
        assert!(!raw.contains("sk-OLD"), "stale key must be gone: {raw}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&auth_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "auth.json must be 0o600");
        }
    }
}
