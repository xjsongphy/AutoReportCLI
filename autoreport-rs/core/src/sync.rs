//! Startup two-repository sync, mirroring AutoReport's `core/preset_sync.py`.
//!
//! On startup AutoReport pulls content from two GitHub repositories:
//!
//! 1. **cc-switch** (`farion1231/cc-switch`) — TypeScript provider-preset files
//!    (`*ProviderPresets.ts`) describing known providers/models/bases. Cached
//!    under `$AUTOREPORT_HOME/external/providers/cc-switch/`.
//! 2. **skills** (`xjsongphy/skills`) — the agent skill files (`SKILL.md`),
//!    written into language-specific `$AUTOREPORT_HOME/resources/<language>/skills/`
//!    discovers them.
//!
//! This is a real, complete implementation: HTTPS fetch via reqwest, on-disk
//! caching, parsing of the preset TS into provider entries that auto-register
//! providers, and best-effort behaviour (offline → keep existing cache, never
//! block startup beyond the timeout).

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// Keep startup inputs immutable between releases. Refresh these revisions
// deliberately when the upstream repositories have been reviewed.
const CC_SWITCH_RAW: &str = "https://raw.githubusercontent.com/farion1231/cc-switch/f6e37ed99443890a865669e28bf1caf5e85d466d";
const SKILLS_RAW: &str =
    "https://raw.githubusercontent.com/xjsongphy/skills/2d4557328cd56d3bc922a0e61bb7cb34dbd42011";
const PKUMPL_TYPST_RAW: &str = "https://raw.githubusercontent.com/xjsongphy/pkumpl-typst/fa3afe997fdc390ea0b15d41df32c7750cf68858";
const TYPST_SKILL_RAW: &str = "https://raw.githubusercontent.com/lucifer1004/claude-skill-typst/8069963bb563f8354ac6a43aeb750f1753e37556";

const PRESET_FILES: &[&str] = &[
    "claudeProviderPresets.ts",
    "codexProviderPresets.ts",
    "geminiProviderPresets.ts",
    "opencodeProviderPresets.ts",
    "openclawProviderPresets.ts",
    "hermesProviderPresets.ts",
    "universalProviderPresets.ts",
];

/// Read the cached cc-switch templates without adding any of them to the
/// user's configured providers. A template becomes a provider only after the
/// user explicitly adds it in `/model`.
pub fn load_presets(home: &Path) -> Vec<PresetProvider> {
    let cfg_dir = external_dir(home)
        .join("providers")
        .join("cc-switch")
        .join("src")
        .join("config");
    let mut seen = BTreeSet::new();
    let mut presets = Vec::new();
    for file in PRESET_FILES {
        let Ok(body) = std::fs::read_to_string(cfg_dir.join(file)) else {
            continue;
        };
        let kind = file_kind(file).map(|(kind, _)| kind).unwrap_or("openai");
        for preset in parse_presets(&body, kind) {
            let identity = (
                preset.kind.clone(),
                preset.name.clone(),
                preset.base_url.clone(),
                preset.env_key.clone(),
            );
            if seen.insert(identity) {
                presets.push(preset);
            }
        }
    }
    presets
}

/// Skills to pull from the skills repo (name → path within repo).
///
/// Mirrors AutoReport's `core/preset_sync.py`: only `latex-compile` and
/// `experiment-report-writer` are external skills. `mineru` is not pulled —
/// it is exposed as a tool (the `mineru-open-api` CLI is on the exec
/// allowlist), not a skill. `md-report-writer` is not pulled — report
/// writing is AutoReport's own purpose and lives in the agent templates.
const SKILL_FILES: &[(&str, &str)] = &[
    (
        "experiment-report-writer",
        "experiment-report-writer/SKILL.md",
    ),
    ("latex-compile", "latex-compile/SKILL.md"),
];
const TYPST_FILES: &[(&str, &str)] = &[
    ("LICENSE", "LICENSE"),
    ("mplts.typ", "themes/mplts.typ"),
    ("template/main.typ", "templates/main.typ"),
    ("template/bibli.bib", "templates/bibli.bib"),
    (
        "template/american-physics-society.csl",
        "templates/american-physics-society.csl",
    ),
];
const TYPST_SKILL_FILES: &[&str] = &[
    "basics.md",
    "types.md",
    "styling.md",
    "tables.md",
    "academic.md",
    "conversion.md",
    "cli.md",
    "query.md",
    "advanced.md",
    "template.md",
    "package.md",
    "debug.md",
    "perf.md",
    "examples/basic-document.typ",
    "examples/styled-document.typ",
    "examples/template-report.typ",
    "examples/tables-showcase.typ",
    "examples/academic-paper.typ",
    "examples/query-export.typ",
];

#[derive(Debug, Default, Clone)]
pub struct SyncReport {
    pub presets_fetched: usize,
    pub skills_fetched: Vec<String>,
    pub errors: Vec<String>,
}

impl SyncReport {
    pub fn total(&self) -> usize {
        self.presets_fetched + self.skills_fetched.len()
    }
}

/// Where synced external content lives inside the global home.
pub fn external_dir(home: &Path) -> PathBuf {
    home.join("external")
}
pub fn skills_dir(home: &Path) -> PathBuf {
    home.join("resources")
}

/// Whether the local cache has the minimum files needed to skip startup sync.
pub fn cache_is_warm(home: &Path) -> bool {
    let preset_dir = external_dir(home)
        .join("providers")
        .join("cc-switch")
        .join("src")
        .join("config");
    let skills = skills_dir(home).join("latex").join("skills");
    let typst = skills_dir(home).join("typst");
    PRESET_FILES
        .iter()
        .all(|file| preset_dir.join(file).is_file())
        && SKILL_FILES.iter().all(|(name, _)| {
            skills.join(name).join("SKILL.md").is_file()
                || skills.join(format!("{name}.md")).is_file()
        })
        && typst.join("skills/typst/SKILL.md").is_file()
        && typst
            .join("skills/experiment-report-writer/SKILL.md")
            .is_file()
        && typst.join("templates/main.typ").is_file()
        && typst.join("themes/mplts.typ").is_file()
}

/// Fetch both repositories' content into the global cache. Network errors
/// are recorded in the report rather than propagated, so a missing network
/// degrades gracefully to the existing cache.
pub async fn sync_all(home: &Path, timeout: std::time::Duration) -> SyncReport {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(crate::user_agent::app_user_agent())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut report = SyncReport::default();
    let staging = home
        .join(".sync-staging")
        .join(uuid::Uuid::new_v4().to_string());
    let staging_preset_dir = staging.join("external/providers/cc-switch/src/config");
    let staging_skills = staging.join("resources/latex/skills");
    let staging_typst = staging.join("resources/typst");
    let _ = std::fs::create_dir_all(&staging_preset_dir);
    let _ = std::fs::create_dir_all(&staging_skills);
    let _ = std::fs::create_dir_all(staging_typst.join("themes"));
    let _ = std::fs::create_dir_all(staging_typst.join("templates"));
    let _ = std::fs::create_dir_all(staging_typst.join("skills/typst"));
    let typst_writer = staging_typst.join("skills/experiment-report-writer/SKILL.md");
    if let Err(e) = atomic_write(
        &typst_writer,
        include_str!("../../../templates/typst/skills/experiment-report-writer/SKILL.md"),
    ) {
        report
            .errors
            .push(format!("write bundled Typst writer skill: {e}"));
    }

    // 1) cc-switch presets.
    let preset_dir = staging_preset_dir.clone();
    for file in PRESET_FILES {
        let url = format!("{CC_SWITCH_RAW}/src/config/{file}");
        let dest = preset_dir.join(file);
        match fetch_text(&client, &url).await {
            Ok(body) => {
                if let Err(e) = atomic_write(&dest, &body) {
                    report.errors.push(format!("write {file}: {e}"));
                } else {
                    report.presets_fetched += 1;
                    log::debug!("synced preset {file}");
                }
            }
            Err(e) => report.errors.push(format!("preset {file}: {e}")),
        }
    }

    // 2) skills repo.
    let skills = staging_skills.clone();
    let _ = std::fs::create_dir_all(&skills);
    for (name, repo_path) in SKILL_FILES {
        let url = format!("{SKILLS_RAW}/{repo_path}");
        match fetch_text(&client, &url).await {
            Ok(body) => {
                if let Err(e) = validate_skill_text(&body) {
                    report.errors.push(format!("skill {name}: {e}"));
                    continue;
                }
                let skill_dir = skills.join(name);
                let _ = std::fs::create_dir_all(&skill_dir);
                let dest = skill_dir.join("SKILL.md");
                if let Err(e) = atomic_write(&dest, &body) {
                    report.errors.push(format!("write skill {name}: {e}"));
                } else {
                    report.skills_fetched.push(name.to_string());
                    log::debug!("synced skill {name}");
                }
            }
            Err(e) => report.errors.push(format!("skill {name}: {e}")),
        }
    }

    for (source, target) in TYPST_FILES {
        match fetch_text(&client, &format!("{PKUMPL_TYPST_RAW}/{source}")).await {
            Ok(body) => {
                let body = if *target == "templates/main.typ" {
                    body.replace(
                        "#import \"@preview/unofficial-pku-mpl:0.1.0\": *",
                        "#import \"mplts.typ\": *",
                    )
                } else {
                    body
                };
                if let Err(e) = atomic_write(&staging_typst.join(target), &body) {
                    report.errors.push(format!("write Typst {target}: {e}"));
                }
            }
            Err(e) => report.errors.push(format!("Typst {target}: {e}")),
        }
    }
    match fetch_text(&client, &format!("{TYPST_SKILL_RAW}/skills/typst/SKILL.md")).await {
        Ok(body) => {
            let body = body.replace("(examples/package-example/)", "(package.md)");
            if let Err(e) = validate_skill_text(&body) {
                report.errors.push(format!("Typst skill: {e}"));
            } else if let Err(e) = atomic_write(&staging_typst.join("skills/typst/SKILL.md"), &body)
            {
                report.errors.push(format!("write Typst skill: {e}"));
            }
        }
        Err(e) => report.errors.push(format!("Typst skill: {e}")),
    }
    for file in TYPST_SKILL_FILES {
        match fetch_text(&client, &format!("{TYPST_SKILL_RAW}/skills/typst/{file}")).await {
            Ok(body) => {
                let body = body
                    .replace("**Complete example**: See [examples/package-example/](examples/package-example/) for a minimal publishable package with submodules.", "**Complete example**: This bundled report skill omits package-development fixtures; use the package patterns in this document.")
                    .replace("See [package search](scripts/search-packages.py) for alternatives.", "consult the Typst package documentation when selecting alternatives.");
                if let Err(e) =
                    atomic_write(&staging_typst.join(format!("skills/typst/{file}")), &body)
                {
                    report
                        .errors
                        .push(format!("write Typst reference {file}: {e}"));
                }
            }
            Err(e) => report.errors.push(format!("Typst reference {file}: {e}")),
        }
    }
    match fetch_text(&client, &format!("{TYPST_SKILL_RAW}/LICENSE")).await {
        Ok(body) => {
            if let Err(e) = atomic_write(&staging_typst.join("skills/typst/LICENSE"), &body) {
                report
                    .errors
                    .push(format!("write Typst skill LICENSE: {e}"));
            }
        }
        Err(e) => report.errors.push(format!("Typst skill LICENSE: {e}")),
    }

    if report.errors.is_empty() {
        if let Err(e) =
            validate_relative_markdown_links(&staging_typst.join("skills/typst/SKILL.md"))
        {
            report.errors.push(format!("Typst skill links: {e}"));
        }
        for file in TYPST_SKILL_FILES
            .iter()
            .filter(|file| file.ends_with(".md"))
        {
            if let Err(e) = validate_relative_markdown_links(
                &staging_typst.join(format!("skills/typst/{file}")),
            ) {
                report
                    .errors
                    .push(format!("Typst reference links {file}: {e}"));
            }
        }
    }

    if report.errors.is_empty() {
        let published_preset = external_dir(home).join("providers/cc-switch/src/config");
        let published_skills = skills_dir(home).join("latex/skills");
        let published_typst = skills_dir(home).join("typst");
        for target in [
            published_preset.parent().unwrap(),
            published_skills.as_path(),
            published_typst.as_path(),
        ] {
            if let Err(e) = validate_managed_target(home, target) {
                report
                    .errors
                    .push(format!("unsafe sync target {}: {e}", target.display()));
            }
        }
        if !report.errors.is_empty() {
            let _ = std::fs::remove_dir_all(&staging);
            return report;
        }
        let _ = std::fs::create_dir_all(published_preset.parent().unwrap());
        let _ = std::fs::create_dir_all(published_skills.parent().unwrap());
        let backup = home
            .join(".sync-staging")
            .join(format!("backup-{}", uuid::Uuid::new_v4()));
        let old_preset = published_preset.parent().unwrap().to_path_buf();
        let old_skills = published_skills.clone();
        let old_typst = published_typst.clone();
        let backup_preset = backup.join("cc-switch");
        let backup_skills = backup.join("skills");
        let backup_typst = backup.join("typst");
        let _ = std::fs::create_dir_all(&backup);
        let mut moved_old_preset = false;
        let mut moved_old_skills = false;
        let mut moved_old_typst = false;
        if old_preset.exists() {
            moved_old_preset = std::fs::rename(&old_preset, &backup_preset).is_ok();
        }
        if old_skills.exists() {
            moved_old_skills = std::fs::rename(&old_skills, &backup_skills).is_ok();
        }
        if old_typst.exists() {
            moved_old_typst = std::fs::rename(&old_typst, &backup_typst).is_ok();
        }
        let publish_preset =
            std::fs::rename(staging.join("external/providers/cc-switch"), &old_preset);
        let publish_skills = std::fs::rename(staging.join("resources/latex/skills"), &old_skills);
        let publish_typst = std::fs::rename(staging.join("resources/typst"), &old_typst);
        let published_preset_ok = publish_preset.is_ok();
        let published_skills_ok = publish_skills.is_ok();
        let published_typst_ok = publish_typst.is_ok();
        if let Err(e) = publish_preset {
            report.errors.push(format!("publish presets: {e}"));
        }
        if let Err(e) = publish_skills {
            report.errors.push(format!("publish skills: {e}"));
        }
        if let Err(e) = publish_typst {
            report.errors.push(format!("publish Typst resources: {e}"));
        }
        if !report.errors.is_empty() {
            if published_preset_ok {
                let _ = std::fs::remove_dir_all(&old_preset);
            }
            if published_skills_ok {
                let _ = std::fs::remove_dir_all(&old_skills);
            }
            if published_typst_ok {
                let _ = std::fs::remove_dir_all(&old_typst);
            }
            if moved_old_preset {
                let _ = std::fs::rename(&backup_preset, &old_preset);
            }
            if moved_old_skills {
                let _ = std::fs::rename(&backup_skills, &old_skills);
            }
            if moved_old_typst {
                let _ = std::fs::rename(&backup_typst, &old_typst);
            }
        }
        let _ = std::fs::remove_dir_all(&backup);
        if report.errors.is_empty() {
            if let Err(e) = remove_legacy_managed_dirs(home) {
                report.errors.push(format!("legacy cache cleanup: {e}"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    report
}

/// Replace a cache file in the same directory so readers never observe a
/// truncated response after an interrupted sync. A unique sibling temp file
/// also prevents concurrent syncs from clobbering one another's staging data.
fn atomic_write(path: &Path, body: &str) -> std::io::Result<()> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("cache");
    let temp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        std::fs::write(&temp, body)?;
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    Ok(resp.text().await?)
}

fn validate_skill_text(body: &str) -> Result<()> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with("---") {
        anyhow::bail!("missing YAML frontmatter");
    }
    let end = trimmed[3..]
        .find("---")
        .ok_or_else(|| anyhow::anyhow!("unterminated YAML frontmatter"))?;
    let front = &trimmed[3..3 + end];
    if !front
        .lines()
        .any(|line| line.trim_start().starts_with("name:"))
    {
        anyhow::bail!("frontmatter missing name");
    }
    if !front
        .lines()
        .any(|line| line.trim_start().starts_with("description:"))
    {
        anyhow::bail!("frontmatter missing description");
    }
    Ok(())
}

fn validate_relative_markdown_links(path: &Path) -> Result<()> {
    let body = std::fs::read_to_string(path)?;
    for line in body.lines() {
        let mut rest = line;
        while let Some(start) = rest.find("](") {
            let raw_target = &rest[start + 2..];
            let Some(end) = raw_target.find(')') else {
                break;
            };
            let link = &raw_target[..end];
            if !link.starts_with('#')
                && !link.starts_with("http://")
                && !link.starts_with("https://")
            {
                let target = link.split('#').next().unwrap_or(link);
                if target.starts_with('/') || target.split('/').any(|part| part == "..") {
                    anyhow::bail!("unsafe relative link {}", target);
                }
                if !target.is_empty() && !path.parent().unwrap().join(target).exists() {
                    anyhow::bail!("missing relative link {}", target);
                }
            }
            rest = &rest[start + 2 + end + 1..];
        }
    }
    Ok(())
}

fn validate_managed_target(home: &Path, target: &Path) -> Result<()> {
    let home = home
        .canonicalize()
        .context("canonicalizing AutoReport home")?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent"))?;
    let parent = parent
        .canonicalize()
        .context("canonicalizing managed target parent")?;
    if !parent.starts_with(&home) {
        anyhow::bail!("target escapes AutoReport home");
    }
    if target.exists() && std::fs::symlink_metadata(target)?.file_type().is_symlink() {
        anyhow::bail!("target is a symlink");
    }
    Ok(())
}

fn remove_legacy_managed_dirs(home: &Path) -> Result<()> {
    let home = home
        .canonicalize()
        .context("canonicalizing AutoReport home")?;
    for relative in ["skills", "templates", "external/cc-switch"] {
        let target = home.join(relative);
        if !target.exists() {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&target)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("legacy target is a symlink: {}", target.display());
        }
        let parent = target.parent().unwrap().canonicalize()?;
        if parent != home && !parent.starts_with(&home) {
            anyhow::bail!("legacy target escapes home: {}", target.display());
        }
        std::fs::remove_dir_all(target)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Preset parsing — turn a `*ProviderPresets.ts` file into provider entries,
// matching cc-switch's real shape: each entry carries `name`, an optional
// `apiKeyField`, and a `settingsConfig.env: { VAR: "value", ... }` block whose
// `*_BASE_URL` / auth-token key / `*_MODEL` define the provider.
// ---------------------------------------------------------------------------

/// One provider entry scraped from a preset TS file.
#[derive(Debug, Clone, Deserialize)]
pub struct PresetProvider {
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub models: Vec<String>,
    pub env_key: Option<String>,
}

/// Map a preset file name to the provider `kind` and the env-var prefix its
/// `settingsConfig.env` block uses.
pub fn file_kind(file: &str) -> Option<(&'static str, &'static str)> {
    // (kind, primary env prefix)
    match file {
        "claudeProviderPresets.ts" => Some(("anthropic", "ANTHROPIC")),
        "codexProviderPresets.ts" | "openclawProviderPresets.ts" => Some(("openai", "OPENAI")),
        "openaiProviderPresets.ts" | "opencodeProviderPresets.ts" => Some(("openai", "OPENAI")),
        "geminiProviderPresets.ts" => Some(("google", "GEMINI")),
        "hermesProviderPresets.ts" => Some(("openai", "OPENAI")),
        "universalProviderPresets.ts" => Some(("openai", "")),
        _ => None,
    }
}

/// Parse a preset TS file body into provider entries. `kind_hint` is the
/// provider kind inferred from the file name (see `file_kind`).
pub fn parse_presets(body: &str, kind_hint: &str) -> Vec<PresetProvider> {
    let mut out = Vec::new();
    for obj in iter_top_level_objects(body) {
        let Some(name) = ts_string_field(&obj, "name") else {
            continue;
        };
        let env = extract_env_block(&obj);
        let base_url = env
            .iter()
            .find(|(k, _)| k.ends_with("_BASE_URL") || k.ends_with("_API_BASE"))
            .map(|(_, v)| v.clone())
            .or_else(|| ts_string_field(&obj, "base_url"))
            .or_else(|| ts_string_field(&obj, "baseUrl"))
            .or_else(|| config_string_field(&obj, "base_url"))
            .or_else(|| config_call_arg(&obj, "config", 1))
            .unwrap_or_default();
        // API-key env var: honour `apiKeyField`, else pick the auth var.
        let env_key = ts_string_field(&obj, "apiKeyField")
            .or_else(|| {
                env.iter()
                    .find(|(k, v)| {
                        (k.ends_with("_API_KEY") || k.ends_with("_AUTH_TOKEN")) && v.is_empty()
                    })
                    .map(|(k, _)| k.clone())
            })
            .or_else(|| {
                env.iter()
                    .find(|(k, _)| k.ends_with("_API_KEY") || k.ends_with("_AUTH_TOKEN"))
                    .map(|(k, _)| k.clone())
            });
        // Model: prefer the bare `<PREFIX>_MODEL`, else a sonnet/default model.
        let model = env
            .iter()
            .find(|(k, _)| {
                k.ends_with("_MODEL")
                    && !k.contains("_DEFAULT_")
                    && !k.contains("_HAIKU")
                    && !k.contains("_OPUS")
            })
            .or_else(|| {
                env.iter()
                    .find(|(k, _)| k.ends_with("_DEFAULT_SONNET_MODEL"))
            })
            .or_else(|| env.iter().find(|(k, _)| k.ends_with("_MODEL")))
            .map(|(_, v)| v.clone())
            .or_else(|| ts_string_field(&obj, "model"))
            .or_else(|| ts_string_field(&obj, "id"))
            .or_else(|| config_string_field(&obj, "model"))
            .or_else(|| config_call_arg(&obj, "config", 2));
        let models = model.into_iter().collect::<Vec<_>>();
        if base_url.is_empty() && models.is_empty() {
            continue;
        }
        out.push(PresetProvider {
            name,
            kind: kind_hint.to_string(),
            base_url,
            models,
            env_key,
        });
    }
    out
}

/// Extract the `settingsConfig.env: { ... }` block of an object as a key/value
/// map (string values only).
fn extract_env_block(obj: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    let Some(env_pos) = find_key(obj, "env") else {
        return map;
    };
    let rest = &obj[env_pos..];
    let Some(lbrace) = rest.find('{') else {
        return map;
    };
    let body = match_brace(&rest[lbrace..]);
    // Parse `KEY: "value",` pairs.
    let mut chars = body.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() || c == ',' {
            chars.next();
            continue;
        }
        // read key until ':'
        let key_start = i;
        let mut key_end = i;
        while let Some(&(_, c)) = chars.peek() {
            if c == ':' {
                break;
            }
            if c.is_whitespace() {
                break;
            }
            let (key_index, key_char) = chars.next().unwrap();
            key_end = key_index + key_char.len_utf8();
        }
        let key = body[key_start..key_end]
            .trim()
            .trim_matches(['"', '\'', '`'])
            .to_string();
        // skip to ':'
        while let Some(&(_, c)) = chars.peek() {
            if c == ':' {
                chars.next();
                break;
            }
            if !c.is_whitespace() {
                break;
            }
            chars.next();
        }
        // read value
        let val = read_ts_value(&mut chars);
        if let Some(v) = val {
            map.insert(key, v);
        }
    }
    map
}

/// Return the byte index of the `:` that follows the first occurrence of the
/// identifier `key` as an object key. The match is anchored: the byte before
/// `key` must not be an identifier character (`[A-Za-z0-9_$]`), otherwise we
/// would match a suffix of a longer key — e.g. `name` inside `displayName`,
/// which would silently parse the wrong value out of untrusted external data.
/// Quoted keys (`"name":`) are anchored by their opening quote.
fn find_key(obj: &str, key: &str) -> Option<usize> {
    // Quoted forms (`"key":` / `'key':`) are inherently anchored by the quotes
    // — try them first. The colon sits at opening-quote + key.len() + 2.
    for quote in [b'"', b'\''] {
        let needle = format!("{}{}{}:", quote as char, key, quote as char);
        if let Some(pos) = obj.find(&needle) {
            return Some(pos + key.len() + 2);
        }
    }
    // Unquoted identifier form: anchor the match so we don't match a suffix of a
    // longer key — e.g. `name` inside `displayName`, which would silently parse
    // the wrong value out of untrusted external data.
    let needle = format!("{key}:");
    let bytes = obj.as_bytes();
    let mut from = 0;
    while let Some(rel) = obj[from..].find(&needle) {
        let pos = from + rel;
        let prev_is_ident = pos > 0 && {
            let prev = bytes[pos - 1];
            prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'$'
        };
        if !prev_is_ident {
            return Some(pos + key.len());
        }
        from = pos + 1;
    }
    None
}

/// Given a slice starting at `{`, return the inner text up to the matching `}`.
fn match_brace(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut quote = 0u8;
    for (i, &b) in bytes.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == quote {
                in_str = false;
            }
        } else if b == b'"' || b == b'\'' || b == b'`' {
            in_str = true;
            quote = b;
        } else if b == b'{' {
            depth += 1;
        } else if b == b'}' {
            depth -= 1;
            if depth == 0 {
                return &s[1..i];
            }
        }
    }
    s
}

/// Read a TS scalar value (string or bareword) from `chars`, advancing past it.
fn read_ts_value(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> Option<String> {
    while let Some(&(_, c)) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
    let &(_, q) = chars.peek()?;
    if q == '"' || q == '\'' || q == '`' {
        chars.next();
        let mut out = String::new();
        while let Some(&(_, c)) = chars.peek() {
            chars.next();
            if c == '\\' {
                if let Some(&(_, e)) = chars.peek() {
                    chars.next();
                    match e {
                        'n' => out.push('\n'),
                        'r' => out.push('\r'),
                        't' => out.push('\t'),
                        '"' | '\'' | '\\' | '`' => out.push(e),
                        'u' => {
                            let mut hex = String::new();
                            for _ in 0..4 {
                                if let Some(&(_, h)) = chars.peek() {
                                    chars.next();
                                    hex.push(h);
                                } else {
                                    break;
                                }
                            }
                            if let Ok(code) = u32::from_str_radix(&hex, 16)
                                && let Some(ch) = char::from_u32(code)
                            {
                                out.push(ch);
                            }
                        }
                        other => out.push(other),
                    }
                }
                continue;
            }
            if c == q {
                return Some(out);
            }
            out.push(c);
        }
        return Some(out);
    }
    // bareword until comma/newline
    let mut out = String::new();
    while let Some(&(_, c)) = chars.peek() {
        if c == ',' || c == '\n' || c == '}' {
            break;
        }
        out.push(c);
        chars.next();
    }
    let v = out.trim().to_string();
    if v.is_empty() { None } else { Some(v) }
}

/// Iterate the body of every `{ ... }` object that is a direct array element
/// (depth-1 braces), i.e. each preset entry.
fn iter_top_level_objects(body: &str) -> Vec<String> {
    let mut res = Vec::new();
    let bytes = body.as_bytes();
    let mut depth: i32 = 0;
    let mut start: Option<usize> = None;
    let mut in_string = false;
    let mut esc = false;
    let mut quote = b'\0';
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == quote {
                in_string = false;
            }
        } else if c == b'"' || c == b'\'' || c == b'`' {
            in_string = true;
            quote = c;
        } else if c == b'{' {
            depth += 1;
            if depth == 1 {
                // Each preset entry is a top-level object inside the exported array.
                start = Some(i);
            }
        } else if c == b'}' {
            if depth == 1 {
                if let Some(s) = start {
                    res.push(body[s + 1..i].to_string());
                }
                start = None;
            }
            depth -= 1;
        }
        i += 1;
    }
    res
}

/// Extract a `"field": "value"` string (for top-level scalar fields like name).
fn ts_string_field(obj: &str, field: &str) -> Option<String> {
    let pos = find_key(obj, field)?;
    let mut rest = &obj[pos..];
    rest = rest.trim_start();
    rest = rest.strip_prefix([':', '=']).unwrap_or(rest);
    rest = rest.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' && q != '`' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    let mut chars = rest.chars();
    let _ = chars.next();
    while let Some(c) = chars.next() {
        if escaped {
            match c {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' | '\'' | '\\' | '`' => out.push(c),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(code)
                    {
                        out.push(ch);
                    }
                }
                other => out.push(other),
            }
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if c == q {
            return Some(out);
        }
        out.push(c);
    }
    None
}

fn config_string_field(obj: &str, key: &str) -> Option<String> {
    let config = ts_string_field(obj, "config")?;
    let pattern = format!("{key} = \"");
    let start = config.find(&pattern)? + pattern.len();
    let rest = &config[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn config_call_arg(obj: &str, field: &str, arg_index: usize) -> Option<String> {
    let pos = find_key(obj, field)?;
    let mut rest = &obj[pos..];
    rest = rest.trim_start();
    rest = rest.strip_prefix([':', '=']).unwrap_or(rest);
    let open = rest.find('(')?;
    let call = &rest[open + 1..];
    let mut args = Vec::new();
    let mut chars = call.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c == ')' {
            break;
        }
        if c == '"' || c == '\'' || c == '`' {
            let mut out = String::new();
            let mut escaped = false;
            while let Some((_, ch)) = chars.next() {
                if escaped {
                    out.push(ch);
                    escaped = false;
                    continue;
                }
                if ch == '\\' {
                    escaped = true;
                    continue;
                }
                if ch == c {
                    args.push(out);
                    break;
                }
                out.push(ch);
            }
        }
    }
    args.get(arg_index).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_cc_switch_claude_shape() {
        // Mirrors the real claudeProviderPresets.ts entry shape.
        let body = r#"export const providerPresets: ProviderPreset[] = [
  {
    name: "Shengsuanyun",
    websiteUrl: "https://x",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://router.shengsuanyun.com/api",
        ANTHROPIC_AUTH_TOKEN: "",
        ANTHROPIC_MODEL: "anthropic/claude-sonnet-4.6",
        ANTHROPIC_DEFAULT_HAIKU_MODEL: "anthropic/claude-haiku-4.5",
      },
    },
  },
  {
    name: "PatewayAI",
    apiKeyField: "ANTHROPIC_API_KEY",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: "https://api.pateway.ai",
        ANTHROPIC_API_KEY: "",
      },
    },
  },
];"#;
        let presets = parse_presets(body, "anthropic");
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].name, "Shengsuanyun");
        assert_eq!(presets[0].base_url, "https://router.shengsuanyun.com/api");
        assert_eq!(presets[0].models, vec!["anthropic/claude-sonnet-4.6"]);
        assert_eq!(presets[0].env_key.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
        // apiKeyField overrides the detected env var.
        assert_eq!(presets[1].env_key.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(presets[1].base_url, "https://api.pateway.ai");
    }

    #[test]
    fn parses_quoted_env_keys() {
        let body = r#"export const providerPresets = [{
  "name": "Quoted",
  "settingsConfig": { "env": {
    "OPENAI_BASE_URL": "https://example.test/v1",
    "OPENAI_API_KEY": ""
  }}
}];"#;
        let presets = parse_presets(body, "openai");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "Quoted");
        assert_eq!(presets[0].base_url, "https://example.test/v1");
        assert_eq!(presets[0].env_key.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn parses_env_keys_with_unicode_punctuation() {
        let body = r#"export const providerPresets = [{
  name: "Unicode",
  settingsConfig: { env: {
    "备注，说明": "ignored",
    "OPENAI_BASE_URL": "https://example.test/v1",
    "OPENAI_API_KEY": ""
  }}
}];"#;
        let presets = parse_presets(body, "openai");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "Unicode");
        assert_eq!(presets[0].base_url, "https://example.test/v1");
        assert_eq!(presets[0].env_key.as_deref(), Some("OPENAI_API_KEY"));
    }

    /// Live network check against the real cc-switch claude preset. Run with
    /// `cargo test parse_live_cc_switch -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn parse_live_cc_switch() {
        let url = "https://raw.githubusercontent.com/farion1231/cc-switch/main/src/config/claudeProviderPresets.ts";
        let body = reqwest::Client::new()
            .get(url)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let presets = parse_presets(&body, "anthropic");
        eprintln!("parsed {} providers from live claude preset", presets.len());
        assert!(
            presets.iter().any(|p| p.name == "Shengsuanyun"),
            "names: {:?}",
            presets.iter().map(|p| &p.name).collect::<Vec<_>>()
        );
        let sheng = presets.iter().find(|p| p.name == "Shengsuanyun").unwrap();
        assert_eq!(sheng.base_url, "https://router.shengsuanyun.com/api");
        assert_eq!(sheng.env_key.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
    }

    #[test]
    fn parses_unicode_provider_name_without_mojibake() {
        let body = r#"export const geminiProviderPresets = [
  {
    name: "自定义网关",
    settingsConfig: {
      env: {
        GOOGLE_GEMINI_BASE_URL: "https://example.com",
        GEMINI_MODEL: "gemini-3.5-flash",
      },
    },
  },
];"#;
        let presets = parse_presets(body, "google");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "自定义网关");
    }

    #[test]
    fn parses_codex_config_string_shape() {
        let body = r#"export const codexProviderPresets = [
  {
    name: "PatewayAI",
    config: generateThirdPartyConfig(
      "patewayai",
      "https://api.pateway.ai/v1",
      "gpt-5.5",
    ),
  },
];"#;
        let presets = parse_presets(body, "openai");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].name, "PatewayAI");
        assert_eq!(presets[0].base_url, "https://api.pateway.ai/v1");
        assert_eq!(presets[0].models, vec!["gpt-5.5"]);
    }

    #[test]
    fn typst_allowlist_fixture_has_no_dangling_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("skills/typst");
        std::fs::create_dir_all(root.join("examples")).unwrap();
        std::fs::write(root.join("basics.md"), "# basics\n").unwrap();
        std::fs::write(root.join("examples/basic.typ"), "").unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            "See [basics](basics.md) and [example](examples/basic.typ).",
        )
        .unwrap();
        validate_relative_markdown_links(&root.join("SKILL.md")).unwrap();
        std::fs::write(root.join("bad.md"), "[missing](examples/nope/)").unwrap();
        assert!(validate_relative_markdown_links(&root.join("bad.md")).is_err());
    }
}
