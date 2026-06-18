//! Skill loader. Skills are Markdown files with YAML frontmatter (`name`,
//! `description`). Discovered from project-local directories so users can drop
//! their own skills in. `load_skill(name)` returns the full skill body for an
//! agent to follow.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub source: PathBuf,
    pub body: String,
}

#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Clone)]
pub struct SkillLoader {
    /// Ordered search roots; first match wins.
    roots: Vec<PathBuf>,
    cache: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Skill>>>,
}

impl SkillLoader {
    /// Roots, in priority order: project `references/skills`, then project
    /// `.autoreport/skills`.
    pub fn new(workspace: &Path) -> Self {
        let roots = vec![
            workspace.join("references").join("skills"),
            workspace.join(".autoreport").join("skills"),
        ];
        for r in &roots {
            let _ = std::fs::create_dir_all(r);
        }
        Self {
            roots,
            cache: std::sync::Arc::new(std::sync::Mutex::new(Default::default())),
        }
    }

    /// Scan all roots and return every discovered skill.
    pub fn list(&self) -> Vec<Skill> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for root in &self.roots {
            let Ok(entries) = std::fs::read_dir(root) else {
                continue;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if let Ok(skill) = parse_skill(&p) {
                    if seen.insert(skill.name.clone()) {
                        out.push(skill);
                    }
                }
            }
        }
        out
    }

    /// Load a single skill by name (cached).
    pub fn load(&self, name: &str) -> Option<Skill> {
        if let Ok(g) = self.cache.lock() {
            if let Some(s) = g.get(name).cloned() {
                return Some(s);
            }
        }
        for skill in self.list() {
            if skill.name == name {
                self.cache
                    .lock()
                    .ok()
                    .map(|mut g| g.insert(name.to_string(), skill.clone()));
                return Some(skill);
            }
        }
        None
    }

    /// Compact summary for embedding in a system prompt.
    pub fn summary(&self) -> String {
        let skills = self.list();
        if skills.is_empty() {
            return String::new();
        }
        let mut s = String::from("Available skills (load with the load_skill tool):\n");
        for sk in skills {
            s.push_str(&format!("- {}: {}\n", sk.name, sk.description));
        }
        s
    }
}

fn parse_skill(path: &Path) -> Result<Skill> {
    let raw = std::fs::read_to_string(path).context("reading skill")?;
    let (fm, body) = split_frontmatter(&raw);
    let fm: Frontmatter = if let Some(f) = fm {
        serde_yaml::from_str(f).unwrap_or(Frontmatter {
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("skill")
                .to_string(),
            description: String::new(),
        })
    } else {
        Frontmatter {
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("skill")
                .to_string(),
            description: String::new(),
        }
    };
    Ok(Skill {
        name: fm.name,
        description: fm.description,
        source: path.to_path_buf(),
        body: body.trim().to_string(),
    })
}

/// Split a `---\n...\n---\n<body>` document into (frontmatter_yaml, body).
fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    if let Some(rest) = raw.strip_prefix("---\n").or_else(|| raw.strip_prefix("---\r\n")) {
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            let after = &rest[end..];
            // skip the closing fence line
            let body = after
                .find('\n')
                .map(|i| &after[i + 1..])
                .unwrap_or(after);
            return (Some(fm), body);
        }
    }
    (None, raw)
}
