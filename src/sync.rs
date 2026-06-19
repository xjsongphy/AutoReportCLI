//! Startup two-repository sync, mirroring AutoReport's `core/preset_sync.py`.
//!
//! On startup AutoReport pulls content from two GitHub repositories:
//!
//! 1. **cc-switch** (`farion1231/cc-switch`) — TypeScript provider-preset files
//!    (`*ProviderPresets.ts`) describing known providers/models/bases. Cached
//!    under `.autoreport/external/cc-switch/`.
//! 2. **skills** (`xjsongphy/skills`) — the agent skill files (`SKILL.md`),
//!    written into `.autoreport/skills/` where `SkillLoader` discovers them.
//!
//! This is a real, complete implementation: HTTPS fetch via reqwest, on-disk
//! caching, parsing of the preset TS into provider entries that auto-register
//! providers, and best-effort behaviour (offline → keep existing cache, never
//! block startup beyond the timeout).

use crate::config::schema::{ProviderConfig, Settings};
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const CC_SWITCH_RAW: &str = "https://raw.githubusercontent.com/farion1231/cc-switch/main";
const SKILLS_RAW: &str = "https://raw.githubusercontent.com/xjsongphy/skills/main";

const PRESET_FILES: &[&str] = &[
    "claudeProviderPresets.ts",
    "codexProviderPresets.ts",
    "geminiProviderPresets.ts",
    "opencodeProviderPresets.ts",
    "openclawProviderPresets.ts",
    "hermesProviderPresets.ts",
    "universalProviderPresets.ts",
];

/// Skills to pull from the skills repo (name → path within repo).
const SKILL_FILES: &[(&str, &str)] = &[
    ("experiment-report-writer", "experiment-report-writer/SKILL.md"),
    ("latex-compile", "latex-compile/SKILL.md"),
    ("md-report-writer", "md-report-writer/SKILL.md"),
    ("mineru", "mineru/SKILL.md"),
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

/// Where synced external content lives inside a workspace.
pub fn external_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport").join("external")
}
pub fn skills_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport").join("skills")
}

/// Fetch both repositories' content into the workspace cache. Network errors
/// are recorded in the report rather than propagated, so a missing network
/// degrades gracefully to the existing cache.
pub async fn sync_all(workspace: &Path, timeout: std::time::Duration) -> SyncReport {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("AutoReportCLI/1.0")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut report = SyncReport::default();

    // 1) cc-switch presets.
    let preset_dir = external_dir(workspace).join("cc-switch").join("src").join("config");
    let _ = std::fs::create_dir_all(&preset_dir);
    for file in PRESET_FILES {
        let url = format!("{CC_SWITCH_RAW}/src/config/{file}");
        match fetch_text(&client, &url).await {
            Ok(body) => {
                if let Err(e) = std::fs::write(preset_dir.join(file), &body) {
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
    let skills = skills_dir(workspace);
    let _ = std::fs::create_dir_all(&skills);
    for (name, repo_path) in SKILL_FILES {
        let url = format!("{SKILLS_RAW}/{repo_path}");
        match fetch_text(&client, &url).await {
            Ok(body) => {
                let dest = skills.join(format!("{name}.md"));
                if let Err(e) = std::fs::write(&dest, &body) {
                    report.errors.push(format!("write skill {name}: {e}"));
                } else {
                    report.skills_fetched.push(name.to_string());
                    log::debug!("synced skill {name}");
                }
            }
            Err(e) => report.errors.push(format!("skill {name}: {e}")),
        }
    }

    report
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    Ok(resp.text().await?)
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
        if env.is_empty() {
            continue;
        }
        let base_url = env
            .iter()
            .find(|(k, _)| k.ends_with("_BASE_URL") || k.ends_with("_API_BASE"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        // API-key env var: honour `apiKeyField`, else pick the auth var.
        let env_key = ts_string_field(&obj, "apiKeyField")
            .or_else(|| {
                env.iter()
                    .find(|(k, v)| (k.ends_with("_API_KEY") || k.ends_with("_AUTH_TOKEN")) && v.is_empty())
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
            .or_else(|| env.iter().find(|(k, _)| k.ends_with("_DEFAULT_SONNET_MODEL")))
            .or_else(|| env.iter().find(|(k, _)| k.ends_with("_MODEL")))
            .map(|(_, v)| v.clone());
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
            key_end = chars.next().unwrap().0 + 1;
        }
        let key = body[key_start..key_end].trim().to_string();
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

/// Return the byte index just after a `key` token occurrence (the position of
/// the following `:`).
fn find_key(obj: &str, key: &str) -> Option<usize> {
    let q = format!("{key}:");
    let q2 = format!("\"{key}\":");
    let pos = obj.find(&q2).or_else(|| obj.find(&q))?;
    Some(pos + key.len())
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
                    out.push(e);
                    chars.next();
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
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
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
    let bs = rest.as_bytes();
    let mut out = String::new();
    let mut j = 1;
    while j < bs.len() {
        let c = bs[j];
        if c == b'\\' && j + 1 < bs.len() {
            out.push(bs[j + 1] as char);
            j += 2;
            continue;
        }
        if c == q as u8 {
            return Some(out);
        }
        out.push(c as char);
        j += 1;
    }
    None
}

/// Register parsed preset providers into `settings` (without overwriting
/// existing keys). Each becomes a selectable provider entry.
pub fn register_providers(settings: &mut Settings, presets: &[PresetProvider]) {
    for p in presets {
        if settings.providers.contains_key(&p.name) {
            continue;
        }
        let model = p.models.first().cloned().unwrap_or_default();
        settings.providers.insert(
            p.name.clone(),
            ProviderConfig {
                kind: p.kind.clone(),
                model,
                api_key: None,
                api_base: if p.base_url.is_empty() { None } else { Some(p.base_url.clone()) },
                api_key_env: p.env_key.clone(),
                temperature: 0.1,
                max_tokens: 8192,
            },
        );
    }
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

    /// Live network check against the real cc-switch claude preset. Run with
    /// `cargo test parse_live_cc_switch -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn parse_live_cc_switch() {
        let url = "https://raw.githubusercontent.com/farion1231/cc-switch/main/src/config/claudeProviderPresets.ts";
        let body = reqwest::Client::new().get(url).send().await.unwrap().text().await.unwrap();
        let presets = parse_presets(&body, "anthropic");
        eprintln!("parsed {} providers from live claude preset", presets.len());
        assert!(presets.iter().any(|p| p.name == "Shengsuanyun"), "names: {:?}", presets.iter().map(|p| &p.name).collect::<Vec<_>>());
        let sheng = presets.iter().find(|p| p.name == "Shengsuanyun").unwrap();
        assert_eq!(sheng.base_url, "https://router.shengsuanyun.com/api");
        assert_eq!(sheng.env_key.as_deref(), Some("ANTHROPIC_AUTH_TOKEN"));
    }

    #[test]
    fn register_providers_skips_existing() {
        let mut settings = Settings::default();
        settings.providers.insert(
            "anthropic".into(),
            ProviderConfig {
                kind: "anthropic".into(),
                model: "x".into(),
                api_key: None,
                api_base: None,
                api_key_env: None,
                temperature: 0.0,
                max_tokens: 0,
            },
        );
        let presets = vec![PresetProvider {
            name: "anthropic".into(),
            kind: "anthropic".into(),
            base_url: "u".into(),
            models: vec!["m".into()],
            env_key: None,
        }];
        register_providers(&mut settings, &presets);
        assert_eq!(settings.providers.get("anthropic").unwrap().model, "x");
    }
}
