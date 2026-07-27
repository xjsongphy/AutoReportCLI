//! Skill loader.
//!
//! Codex loads user skills from the global home and project skills from the
//! project tree. AutoReport keeps pulled/user-wide skills in
//! `$AUTOREPORT_HOME/resources/<language>/skills/*/SKILL.md`; explicit project overrides under
//! `References/skills` remain readable but are never created automatically.

use crate::project::{ReportLanguage, selected_report_language};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const SKILL_FILENAME: &str = "SKILL.md";
const MAX_SCAN_DEPTH: usize = 6;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub source: PathBuf,
    pub raw: String,
    pub body: String,
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Clone)]
pub struct SkillLoader {
    home: PathBuf,
    workspace: PathBuf,
    cache: std::sync::Arc<std::sync::Mutex<HashMap<String, Skill>>>,
}

impl SkillLoader {
    pub fn new(home: &Path, workspace: &Path) -> Self {
        let resource_root = match selected_report_language(home, workspace) {
            Some(ReportLanguage::Typst) => "typst",
            _ => "latex",
        };
        let _ = std::fs::create_dir_all(home.join("resources").join(resource_root).join("skills"));
        Self {
            home: home.to_path_buf(),
            workspace: workspace.to_path_buf(),
            cache: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }

    fn roots(&self) -> Vec<PathBuf> {
        let language = match selected_report_language(&self.home, &self.workspace) {
            Some(ReportLanguage::Typst) => "typst",
            _ => "latex",
        };
        vec![
            self.workspace.join("References").join("skills"),
            self.home.join("resources").join(language).join("skills"),
        ]
    }

    pub fn list(&self) -> Vec<Skill> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        // Iterate roots in reverse so an explicit project override wins over a
        // same-named global skill, matching Codex's user/project precedence.
        let roots = self.roots();
        for root in roots.iter().rev() {
            for path in discover_skill_files(root) {
                let Ok(skill) = parse_skill(&path) else {
                    continue;
                };
                // codex requires a description (MissingField); a blank one
                // would pollute the catalog, so skip rather than inject
                // "no description".
                if skill.description.is_empty() {
                    log::warn!(
                        "skill {}: missing description, not injecting",
                        skill.source.display()
                    );
                    continue;
                }
                if seen.insert(skill.name.clone()) {
                    out.push(skill);
                }
            }
        }
        out
    }

    pub fn load(&self, name: &str) -> Option<Skill> {
        let skill = self.list().into_iter().find(|skill| skill.name == name)?;
        self.cache
            .lock()
            .ok()
            .map(|mut guard| guard.insert(name.to_string(), skill.clone()));
        Some(skill)
    }

    pub fn render_context(&self) -> String {
        let skills = self.list();
        if skills.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        out.push_str("## Skills\n");
        out.push_str("A skill is a set of local instructions to follow that is stored in a `SKILL.md` file. Below is the list of skills that can be used. Each entry includes a name, description, and a short path that can be expanded into an absolute path using the skill roots table.\n");
        out.push_str("### Skill roots\n");
        for (idx, root) in self.roots().iter().enumerate() {
            let _ = writeln!(&mut out, "- `r{idx}` = `{}`", root.display());
        }
        out.push_str("### Available skills\n");
        for skill in &skills {
            let _ = writeln!(
                &mut out,
                "- {}: {} (file: {})",
                skill.name,
                skill.description,
                self.short_skill_path(&skill.source)
            );
        }
        out.push_str("### How To Use Skills\n");
        out.push_str("- Discovery: The list above is the skills available in this session (name + description + short path). Skill bodies live on disk at the listed paths after expanding the matching alias from `### Skill roots`.\n");
        out.push_str("- Trigger rules: If the user names a skill (with `$SkillName` or plain text) OR the task clearly matches a skill's description shown above, you must use that skill for that turn. Multiple mentions mean use them all. Do not carry skills across turns unless re-mentioned.\n");
        out.push_str("- Missing/blocked: If a named skill is not in the list or the path cannot be read, say so briefly and continue with the best fallback.\n");
        out.push_str("- How to use a skill:\n");
        out.push_str("  1. After deciding to use a skill, expand the listed short path with the matching alias from `### Skill roots`, then read its `SKILL.md` completely with `exec` using commands like `cat`, `sed -n`, or `rg`. If a read is partial, continue until EOF.\n");
        out.push_str("  2. When `SKILL.md` references relative paths such as `scripts/foo.py` or `References/bar.md`, resolve them relative to the directory containing that `SKILL.md` first.\n");
        out.push_str("  3. If `scripts/`, templates, or assets exist, prefer reusing or patching them instead of retyping large blocks from scratch.\n");
        out.push_str("- Coordination and sequencing:\n");
        out.push_str("  - If multiple skills apply, choose the minimal set that covers the request and state the order you will use them.\n");
        out.push_str("  - Announce which skill(s) you are using and why in one short line.\n");
        out.push_str("- Context hygiene:\n");
        out.push_str("  - Read the selected `SKILL.md` fully before acting, but avoid loading unrelated references.\n");
        out.push_str("  - Prefer files directly linked from `SKILL.md`; avoid deep reference-chasing unless blocked.\n");
        out.push_str("- Safety and fallback: If a skill cannot be applied cleanly, state the issue, use the next-best approach, and continue.\n");
        out
    }

    fn short_skill_path(&self, path: &Path) -> String {
        for (idx, root) in self.roots().iter().enumerate() {
            if let Ok(relative) = path.strip_prefix(root) {
                return format!("r{idx}/{}", relative.display());
            }
        }
        path.display().to_string()
    }

    /// Render the bodies of every skill explicitly mentioned in `text` (via a
    /// `$skill-name` token), in catalog order. This is the second half of
    /// progressive disclosure — the catalog (rendered into the system prompt)
    /// tells the model which skills exist; this injects a mentioned skill's
    /// full `SKILL.md` body so the agent doesn't have to `cat` it itself.
    /// Mirrors codex `core-skills::injection::build_skill_injections`.
    pub fn render_injections(&self, mentioned: &HashSet<String>) -> String {
        if mentioned.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for skill in self.list() {
            if !mentioned.contains(&skill.name) {
                continue;
            }
            // `list()` already skipped skills with empty descriptions, but a
            // direct `load()` also returns the body — use the parsed body we
            // already have rather than re-reading.
            let body = if skill.body.is_empty() {
                match self.load(&skill.name) {
                    Some(loaded) => loaded.body,
                    None => continue,
                }
            } else {
                skill.body
            };
            out.push_str(&format!(
                "## Skill `{}` (from {})\n\n{}\n\n",
                skill.name,
                self.short_skill_path(&skill.source),
                body
            ));
        }
        out
    }
}

// ---- progressive-disclosure mention parsing (codex core-skills::injection) ----

/// Sigil introducing a skill mention, matching codex (`TOOL_MENTION_SIGIL`).
const SKILL_MENTION_SIGIL: char = '$';

/// Extract `$skill-name` mentions from `text`, skipping common environment
/// variables (`$HOME`, `$PATH`, …). Returns the set of mentioned names. Ported
/// from codex `extract_tool_mentions` (plain-name branch only — we have no
/// `[name](path)` linked / app / mcp / plugin skills to resolve).
pub fn extract_skill_mentions(text: &str) -> HashSet<String> {
    let bytes = text.as_bytes();
    let mut names = HashSet::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != SKILL_MENTION_SIGIL as u8 {
            index += 1;
            continue;
        }
        let name_start = index + 1;
        let Some(&first) = bytes.get(name_start) else {
            index += 1;
            continue;
        };
        if !is_mention_name_char(first) {
            index += 1;
            continue;
        }
        let mut name_end = name_start + 1;
        while let Some(&next) = bytes.get(name_end) {
            if is_mention_name_char(next) {
                name_end += 1;
            } else {
                break;
            }
        }
        let name = &text[name_start..name_end];
        if !is_common_env_var(name) {
            names.insert(name.to_string());
        }
        index = name_end;
    }
    names
}

/// A byte that may appear in a skill mention name (codex `is_mention_name_char`).
fn is_mention_name_char(byte: u8) -> bool {
    matches!(
        byte,
        b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':'
    )
}

/// Filter out environment-variable lookups mistaken for mentions (codex
/// `is_common_env_var`).
fn is_common_env_var(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

fn discover_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0usize));

    while let Some((dir, depth)) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries = entries.flatten().collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if depth < MAX_SCAN_DEPTH {
                    queue.push_back((path, depth + 1));
                }
                continue;
            }
            let is_skill_md = path.file_name().and_then(|s| s.to_str()) == Some(SKILL_FILENAME);
            let is_flat_compat =
                depth == 0 && path.extension().and_then(|s| s.to_str()) == Some("md");
            if is_skill_md || is_flat_compat {
                out.push(path);
            }
        }
    }
    out
}

fn parse_skill(path: &Path) -> Result<Skill> {
    let raw = std::fs::read_to_string(path).context("reading skill")?;
    let (frontmatter, body) = split_frontmatter(&raw);
    let body = body.trim().to_string();
    let metadata = frontmatter
        .map(serde_yaml::from_str::<Frontmatter>)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(Frontmatter {
            name: None,
            description: None,
        });
    let fallback_name = if path.file_name().and_then(|s| s.to_str()) == Some(SKILL_FILENAME) {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string()
    } else {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
            .to_string()
    };

    Ok(Skill {
        name: metadata.name.unwrap_or(fallback_name),
        description: metadata.description.unwrap_or_default(),
        source: path.to_path_buf(),
        raw,
        body,
    })
}

fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if let Some(rest) = raw
        .strip_prefix("---\n")
        .or_else(|| raw.strip_prefix("---\r\n"))
        && let Some(end) = rest.find("\n---")
    {
        let frontmatter = &rest[..end];
        let after = &rest[end..];
        let body = after
            .find('\n')
            .map(|idx| &after[idx + 1..])
            .unwrap_or(after);
        return (Some(frontmatter), body);
    }
    (None, raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_loader_reads_directory_skills() {
        let dir = std::env::temp_dir().join(format!("skills-dir-{}", stamp()));
        let skill_dir = dir.join("resources/latex/skills").join("latex-compile");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: latex-compile\ndescription: compile latex\n---\nbody text",
        )
        .unwrap();
        let loader = SkillLoader::new(&dir, &dir);
        let names: Vec<String> = loader.list().into_iter().map(|s| s.name).collect();
        assert!(
            names.iter().any(|n| n == "latex-compile"),
            "names: {names:?}"
        );
        let skill = loader.load("latex-compile").expect("latex-compile skill");
        assert!(skill.body.contains("body text"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skill_loader_does_not_read_legacy_flat_cache() {
        let dir = std::env::temp_dir().join(format!("skills-flat-{}", stamp()));
        let skills = dir.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("md-report-writer.md"),
            "---\nname: md-report-writer\ndescription: write markdown reports\n---\nbody text",
        )
        .unwrap();
        let loader = SkillLoader::new(&dir, &dir);
        assert!(loader.load("md-report-writer").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extract_skill_mentions_skips_env_vars_and_picks_names() {
        let m = extract_skill_mentions(
            "use $latex-compile and $md-report-writer; home is $HOME, path $PATH",
        );
        assert!(m.contains("latex-compile"));
        assert!(m.contains("md-report-writer"));
        assert!(!m.contains("HOME"));
        assert!(!m.contains("PATH"));
        // a bare `$` not followed by a name char is not a mention
        assert!(extract_skill_mentions("costs $ each and $$ total").is_empty());
    }

    #[test]
    fn render_injections_emits_bodies_for_mentioned_only() {
        let dir = std::env::temp_dir().join(format!("skills-inj-{}", stamp()));
        let s1 = dir.join("resources/latex/skills").join("alpha");
        let s2 = dir.join("resources/latex/skills").join("beta");
        std::fs::create_dir_all(&s1).unwrap();
        std::fs::create_dir_all(&s2).unwrap();
        std::fs::write(
            s1.join("SKILL.md"),
            "---\nname: alpha\ndescription: alpha skill\n---\nALPHA BODY",
        )
        .unwrap();
        std::fs::write(
            s2.join("SKILL.md"),
            "---\nname: beta\ndescription: beta skill\n---\nBETA BODY",
        )
        .unwrap();
        let loader = SkillLoader::new(&dir, &dir);
        let mut mentioned = HashSet::new();
        mentioned.insert("beta".to_string());
        let inj = loader.render_injections(&mentioned);
        assert!(inj.contains("BETA BODY"), "inj: {inj}");
        assert!(!inj.contains("ALPHA BODY"), "inj: {inj}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn language_switch_refreshes_roots_and_excludes_other_language_skills() {
        let dir = std::env::temp_dir().join(format!("skills-language-{}", stamp()));
        let home = dir.join("home");
        let latex = home.join("resources/latex/skills/latex-compile");
        let typst = home.join("resources/typst/skills/typst");
        std::fs::create_dir_all(&latex).unwrap();
        std::fs::create_dir_all(&typst).unwrap();
        let latex_skill = "---\nname: latex-compile\ndescription: latex\n---\nlatex";
        let typst_skill = "---\nname: typst\ndescription: typst\n---\ntypst";
        std::fs::write(latex.join("SKILL.md"), latex_skill).unwrap();
        std::fs::write(typst.join("SKILL.md"), typst_skill).unwrap();
        let workspace = dir.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        let loader = SkillLoader::new(&home, &workspace);
        assert!(loader.load("latex-compile").is_some());
        crate::project::save_project_config(
            &home,
            &workspace,
            &crate::project::ProjectConfig {
                report_language: crate::project::ReportLanguage::Typst,
            },
        )
        .unwrap();
        assert!(loader.load("typst").is_some());
        assert!(loader.load("latex-compile").is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn stamp() -> String {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
