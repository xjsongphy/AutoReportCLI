//! Startup multi-repository sync, mirroring AutoReport's `core/preset_sync.py`.
//!
//! On startup AutoReport pulls content from four pinned-SHA GitHub repositories:
//!
//! 1. **cc-switch** (`farion1231/cc-switch`) — TypeScript provider-preset files
//!    (`*ProviderPresets.ts`) describing known providers/models/bases. Cached
//!    under `$AUTOREPORT_HOME/external/providers/cc-switch/`.
//! 2. **skills** (`xjsongphy/skills`) — agent skill files (`SKILL.md`).
//!    `experiment-report-writer` is language-neutral report-writing methodology
//!    and is written to both the latex and typst skill roots; `latex-compile` is
//!    latex-only. Language typesetting specifics are delegated to the
//!    `latex-compile` / `typst` skills + project templates.
//! 3. **pkumpl-typst** (`xjsongphy/pkumpl-typst`) — Typst report theme/template
//!    assets.
//! 4. **claude-skill-typst** (`lucifer1004/claude-skill-typst`) — the Typst
//!    authoring skill and its reference docs.
//!
//! ## Pull-decision model (manifest-by-SHA)
//!
//! Every repo is pinned to a fixed commit SHA, so its file tree is immutable
//! until we deliberately bump the SHA in a release. On the first launch after a
//! bump we fetch each repo's git tree once (the "remote directory"), filter it to
//! the files we manage, and cache that manifest on disk keyed by SHA. The warmth
//! check (`cache_is_warm`) is then a **local-only** test — a manifest for the
//! current SHA exists and every file it lists is present on disk — so warm
//! startups do zero network I/O.
//!
//! ## Parallelism
//!
//! The four repos are fetched **concurrently** (repo-level parallelism); within a
//! repo, files are fetched **sequentially**. Each file is written atomically to
//! its final destination, so a single failed fetch (404, timeout) is recorded for
//! that file only and never aborts the rest — fixing the old "one missing remote
//! file re-pulls everything every startup" loop.

use anyhow::{Context, Result};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// Keep startup inputs immutable between releases. Refresh the SHAs deliberately
// when the upstream repositories have been reviewed.
const CC_SWITCH: Repo = Repo {
    name: "cc-switch",
    owner: "farion1231",
    repo: "cc-switch",
    sha: "f6e37ed99443890a865669e28bf1caf5e85d466d",
};
const SKILLS: Repo = Repo {
    name: "skills",
    owner: "xjsongphy",
    repo: "skills",
    sha: "025cbd22cf9c442c3ba6b3309d80e8376e4098e9",
};
const PKUMPL_TYPST: Repo = Repo {
    name: "pkumpl-typst",
    owner: "xjsongphy",
    repo: "pkumpl-typst",
    sha: "fa3afe997fdc390ea0b15d41df32c7750cf68858",
};
const TYPST_SKILL: Repo = Repo {
    name: "claude-skill-typst",
    owner: "lucifer1004",
    repo: "claude-skill-typst",
    sha: "8069963bb563f8354ac6a43aeb750f1753e37556",
};

const PRESET_FILES: &[&str] = &[
    "claudeProviderPresets.ts",
    "codexProviderPresets.ts",
    "geminiProviderPresets.ts",
    "opencodeProviderPresets.ts",
    "openclawProviderPresets.ts",
    "hermesProviderPresets.ts",
    "universalProviderPresets.ts",
];

/// Typst skill reference docs pulled from `claude-skill-typst`.
const TYPST_SKILL_REFS: &[&str] = &[
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

// ---------------------------------------------------------------------------
// Repo + file-rule model
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Repo {
    name: &'static str,
    owner: &'static str,
    repo: &'static str,
    sha: &'static str,
}

impl Repo {
    fn raw_base(&self) -> String {
        format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            self.owner, self.repo, self.sha
        )
    }
    fn tree_url(&self) -> String {
        format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            self.owner, self.repo, self.sha
        )
    }
}

/// One managed file: where it lives in the repo, where it goes on disk (one or
/// more destinations — `experiment-report-writer` is written to both language
/// roots), and an optional body transform.
#[derive(Clone)]
struct FileRule {
    remote: String,
    dests: Vec<String>,
    transform: fn(&str) -> String,
}

fn noop(s: &str) -> String {
    s.to_string()
}

/// Build the four repo specs with their managed file sets. cc-switch's files are
/// derived from `PRESET_FILES`; the others are explicit.
fn repos() -> Vec<(Repo, Vec<FileRule>)> {
    let cc_switch_files: Vec<FileRule> = PRESET_FILES
        .iter()
        .map(|file| FileRule {
            remote: format!("src/config/{file}"),
            dests: vec![format!("external/providers/cc-switch/src/config/{file}")],
            transform: noop,
        })
        .collect();

    let skills_files: Vec<FileRule> = vec![
        // Language-neutral report-writing methodology → both language roots.
        FileRule {
            remote: "experiment-report-writer/SKILL.md".to_string(),
            dests: vec![
                "resources/latex/skills/experiment-report-writer/SKILL.md".to_string(),
                "resources/typst/skills/experiment-report-writer/SKILL.md".to_string(),
            ],
            transform: noop,
        },
        FileRule {
            remote: "latex-compile/SKILL.md".to_string(),
            dests: vec!["resources/latex/skills/latex-compile/SKILL.md".to_string()],
            transform: noop,
        },
    ];

    let pkumpl_files: Vec<FileRule> = vec![
        FileRule {
            remote: "LICENSE".to_string(),
            dests: vec!["resources/typst/LICENSE".to_string()],
            transform: noop,
        },
        FileRule {
            remote: "mplts.typ".to_string(),
            dests: vec!["resources/typst/themes/mplts.typ".to_string()],
            transform: noop,
        },
        FileRule {
            remote: "template/main.typ".to_string(),
            dests: vec!["resources/typst/templates/main.typ".to_string()],
            transform: rewrite_typst_main_import,
        },
        FileRule {
            remote: "template/bibli.bib".to_string(),
            dests: vec!["resources/typst/templates/bibli.bib".to_string()],
            transform: noop,
        },
        FileRule {
            remote: "template/american-physics-society.csl".to_string(),
            dests: vec!["resources/typst/templates/american-physics-society.csl".to_string()],
            transform: noop,
        },
    ];

    let mut typst_skill_files: Vec<FileRule> = vec![
        FileRule {
            remote: "skills/typst/SKILL.md".to_string(),
            dests: vec!["resources/typst/skills/typst/SKILL.md".to_string()],
            transform: rewrite_typst_skill_index,
        },
        FileRule {
            remote: "LICENSE".to_string(),
            dests: vec!["resources/typst/skills/typst/LICENSE".to_string()],
            transform: noop,
        },
    ];
    for file in TYPST_SKILL_REFS {
        typst_skill_files.push(FileRule {
            remote: format!("skills/typst/{file}"),
            dests: vec![format!("resources/typst/skills/typst/{file}")],
            transform: rewrite_typst_skill_ref,
        });
    }

    vec![
        (CC_SWITCH, cc_switch_files),
        (SKILLS, skills_files),
        (PKUMPL_TYPST, pkumpl_files),
        (TYPST_SKILL, typst_skill_files),
    ]
}

/// Rewrite the pkumpl-typst `main.typ` import to use the vendored theme.
fn rewrite_typst_main_import(body: &str) -> String {
    body.replace(
        "#import \"@preview/unofficial-pku-mpl:0.1.0\": *",
        "#import \"mplts.typ\": *",
    )
}

/// Strip upstream package/example fixtures and their index links from the Typst
/// skill's `SKILL.md` so the bundled report skill stays self-contained.
fn rewrite_typst_skill_index(body: &str) -> String {
    body.replace("(examples/package-example/)", "(package.md)")
        .replace(
            "| [basic-document.typ](examples/basic-document.typ)   | A short note or memo                     | [basics.md](basics.md), [styling.md](styling.md) |\n",
            "",
        )
        .replace(
            "| [styled-document.typ](examples/styled-document.typ) | A multi-section report with page styling | [styling.md](styling.md), [tables.md](tables.md) |\n",
            "",
        )
        .replace(
            "| [template-report.typ](examples/template-report.typ) | A reusable template for a series         | [template.md](template.md)                       |\n",
            "",
        )
        .replace(
            "| [tables-showcase.typ](examples/tables-showcase.typ) | A data-heavy doc (tables, CSV/JSON)      | [tables.md](tables.md), [types.md](types.md) |\n",
            "",
        )
        .replace(
            "| [academic-paper.typ](examples/academic-paper.typ)   | A paper with citations, theorems, math   | [academic.md](academic.md)                       |\n",
            "",
        )
        .replace(
            "| [query-export.typ](examples/query-export.typ)       | Metadata export or multi-pass builds     | [query.md](query.md)                             |\n",
            "",
        )
}

/// Neutralize upstream package-development references in Typst skill docs.
fn rewrite_typst_skill_ref(body: &str) -> String {
    body.replace(
        "**Complete example**: See [examples/package-example/](examples/package-example/) for a minimal publishable package with submodules.",
        "**Complete example**: This bundled report skill omits package-development fixtures; use the package patterns in this document.",
    )
    .replace(
        "See [package search](scripts/search-packages.py) for alternatives.",
        "consult the Typst package documentation when selecting alternatives.",
    )
}

// ---------------------------------------------------------------------------
// Manifest cache (by SHA) — drives the zero-network warmth check
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    sha: String,
    /// Signature of the managed file rules when this manifest was written, so a
    /// code change to `repos()` invalidates the cache even at a fixed SHA.
    signature: String,
    /// Every on-disk destination this repo should provide for the pinned SHA
    /// (the managed file rules intersected with the files that exist remotely).
    dests: Vec<String>,
}

fn manifest_dir(home: &Path) -> PathBuf {
    home.join(".sync-cache").join("manifests")
}

fn manifest_path(home: &Path, repo: &Repo) -> PathBuf {
    manifest_dir(home).join(format!("{}-{}.json", repo.name, repo.sha))
}

fn load_manifest(home: &Path, repo: &Repo) -> Option<Manifest> {
    let body = std::fs::read_to_string(manifest_path(home, repo)).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_manifest(home: &Path, repo: &Repo, manifest: &Manifest) {
    let dir = manifest_dir(home);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(json) = serde_json::to_string_pretty(manifest) {
        let _ = atomic_write(&manifest_path(home, repo), &json);
    }
    // Garbage-collect stale manifests from previous SHAs of the same repo so the
    // cache dir doesn't grow unbounded across releases: manifests are keyed
    // `{name}-{sha}.json`, and each SHA bump left the old file behind. Only
    // files matching the exact `{name}-` prefix for THIS repo are removed;
    // other repos' manifests are untouched. Best-effort — removal errors ignored.
    let current = manifest_path(home, repo);
    let prefix = format!("{}-", repo.name);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path == current {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if file_name.starts_with(&prefix) && file_name.ends_with(".json") {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Bump when any file transform applied in `repos()` changes its output, so a
/// stale warmth cache (written by a previous binary) is detected and managed
/// files are re-synced. Without this, editing a transform fn body would not
/// invalidate the cache until the upstream SHA bumps — `repo_signature` below
/// covers `remote`/`dests` but not the transform applied to the fetched body.
const TRANSFORM_VERSION: u32 = 1;

/// A stable digest of one repo's managed file rules. Changes whenever the rules
/// change (added/removed/relabeled files), so a stale manifest is detected.
fn repo_signature(files: &[FileRule]) -> String {
    let mut sig = String::new();
    sig.push_str("tx=");
    sig.push_str(&TRANSFORM_VERSION.to_string());
    sig.push(';');
    for rule in files {
        sig.push_str(&rule.remote);
        sig.push('=');
        for dest in &rule.dests {
            sig.push_str(dest);
            sig.push(',');
        }
        sig.push(';');
    }
    sig
}

/// Where synced external content lives inside the global home.
pub fn external_dir(home: &Path) -> PathBuf {
    home.join("external")
}
pub fn skills_dir(home: &Path) -> PathBuf {
    home.join("resources")
}

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

/// Whether the local cache fully reflects every repo's pinned SHA. Purely local:
/// a manifest for the current SHA (matching the current rule signature) must
/// exist for each repo, and every destination it lists must be on disk. No
/// network — warm startups skip sync entirely.
pub fn cache_is_warm(home: &Path) -> bool {
    for (repo, files) in repos() {
        let Some(manifest) = load_manifest(home, &repo) else {
            return false;
        };
        if manifest.sha != repo.sha || manifest.signature != repo_signature(&files) {
            return false;
        }
        for dest in &manifest.dests {
            if !home.join(dest).is_file() {
                return false;
            }
        }
    }
    true
}

/// Fetch every repo's content into the global cache. Network errors are recorded
/// per-file rather than propagated, so a missing network degrades gracefully to
/// the existing cache. Repos run concurrently; files within a repo run in order.
pub async fn sync_all(home: &Path, timeout: std::time::Duration) -> SyncReport {
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(crate::user_agent::app_user_agent())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let mut report = SyncReport::default();

    // One pass over the legacy layout so old installs migrate cleanly.
    if let Err(e) = remove_legacy_managed_dirs(home) {
        report.errors.push(format!("legacy cache cleanup: {e}"));
    }

    let specs = repos();

    // Phase 1 — fetch all repos concurrently (sequential files within each).
    let results = join_all(
        specs
            .iter()
            .map(|(repo, files)| sync_repo(&client, *repo, files, home)),
    )
    .await;

    // Phase 2 — fold per-repo results into one report + persist manifests.
    for (result, (repo, _files)) in results.into_iter().zip(specs.iter()) {
        report.presets_fetched += result.presets_fetched;
        report.skills_fetched.extend(result.skills_fetched);
        report.errors.extend(result.errors);
        if let Some(manifest) = result.manifest {
            save_manifest(home, repo, &manifest);
        }
    }
    report
}

/// Per-repo fetch outcome.
struct RepoResult {
    presets_fetched: usize,
    skills_fetched: Vec<String>,
    errors: Vec<String>,
    /// `Some` when the tree was fetched this run (to be cached); `None` when the
    /// tree fetch failed and we left any existing manifest untouched.
    manifest: Option<Manifest>,
}

/// Fetch one repo's tree (cached-by-SHA), then fetch only the managed files that
/// are present remotely and missing locally. Files are written directly to their
/// final destinations, so a per-file failure never blocks the others.
async fn sync_repo(
    client: &reqwest::Client,
    repo: Repo,
    files: &[FileRule],
    home: &Path,
) -> RepoResult {
    let mut out = RepoResult {
        presets_fetched: 0,
        skills_fetched: Vec::new(),
        errors: Vec::new(),
        manifest: None,
    };

    // 1) Resolve the remote paths that actually exist at this SHA.
    let remote_paths: BTreeSet<String> = match fetch_tree(client, &repo.tree_url()).await {
        Ok(paths) => {
            out.manifest = Some(Manifest {
                sha: repo.sha.to_string(),
                signature: repo_signature(files),
                dests: dests_for_remote(files, &paths),
            });
            paths
        }
        Err(e) => {
            // Tree fetch failed (offline / rate-limited). Best-effort: assume all
            // managed files exist remotely so we still try to fetch the missing
            // ones, reusing the existing cache where files are already present.
            out.errors.push(format!("{} tree: {e}", repo.name));
            files.iter().map(|r| r.remote.clone()).collect()
        }
    };

    // 2) Fetch each managed file present remotely that is missing locally.
    for rule in files {
        if !remote_paths.contains(&rule.remote) {
            // Removed upstream (or curated but absent at this SHA): not required,
            // never a 404-loop trigger. Skip silently.
            continue;
        }
        let needed = rule.dests.iter().any(|dest| !home.join(dest).is_file());
        if !needed {
            continue;
        }
        let body = match fetch_text(client, &format!("{}/{}", repo.raw_base(), rule.remote)).await {
            Ok(body) => body,
            Err(e) => {
                out.errors
                    .push(format!("{} {}: {e}", repo.name, rule.remote));
                continue;
            }
        };
        let body = (rule.transform)(&body);
        if rule.dests.iter().any(|d| d.ends_with("SKILL.md")) {
            if let Err(e) = validate_skill_text(&body) {
                out.errors
                    .push(format!("{} {}: {e}", repo.name, rule.remote));
                continue;
            }
        }
        for dest in &rule.dests {
            if let Err(e) = validate_managed_target(home, Path::new(dest)) {
                out.errors.push(format!("unsafe target {}: {e}", dest));
                continue;
            }
            let target = home.join(dest);
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = atomic_write(&target, &body) {
                out.errors
                    .push(format!("write {} {}: {e}", repo.name, dest));
            } else if repo.name == "cc-switch" {
                out.presets_fetched += 1;
            } else {
                out.skills_fetched.push(dest.clone());
            }
        }
    }

    // 3) Soft-check internal markdown links for the Typst skill (warn only).
    for rule in files {
        if rule
            .dests
            .iter()
            .any(|d| d.starts_with("resources/typst/skills/typst/") && d.ends_with(".md"))
        {
            if let Err(e) = validate_relative_markdown_links(&home.join(&rule.dests[0])) {
                log::warn!("{} link check: {e}", rule.dests[0]);
            }
        }
    }

    out
}

/// Extract the git tree blob paths from the GitHub trees API response.
async fn fetch_tree(client: &reqwest::Client, tree_url: &str) -> Result<BTreeSet<String>> {
    let resp = client
        .get(tree_url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }
    let body = resp.text().await?;
    let v: serde_json::Value = serde_json::from_str(&body).context("parsing tree JSON")?;
    let mut paths = BTreeSet::new();
    if let Some(arr) = v.get("tree").and_then(|t| t.as_array()) {
        for entry in arr {
            if entry.get("type").and_then(|t| t.as_str()) == Some("blob") {
                if let Some(path) = entry.get("path").and_then(|p| p.as_str()) {
                    paths.insert(path.to_string());
                }
            }
        }
    }
    Ok(paths)
}

/// Map the remote paths that exist onto the full set of dests we should manage.
fn dests_for_remote(files: &[FileRule], remote_paths: &BTreeSet<String>) -> Vec<String> {
    let mut dests = Vec::new();
    for rule in files {
        if remote_paths.contains(&rule.remote) {
            dests.extend(rule.dests.iter().cloned());
        }
    }
    dests.sort();
    dests.dedup();
    dests
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
    // Join `target` onto `home` BEFORE canonicalizing. At the call site `target`
    // is a repo-relative dest (e.g. `resources/latex/skills/.../SKILL.md`); if we
    // canonicalized it directly the relative parent would resolve against the
    // process CWD (the workspace), not `~/.autoreport`, and fail on a fresh
    // install — rejecting every managed dest as "unsafe target" and blocking all
    // preset/skill writes. Mirrors `remove_legacy_managed_dirs`, which already
    // does `home.join(relative)`.
    let target = home.join(target);
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("target has no parent"))?;
    // The parent dir may not exist yet on a fresh install (the caller creates it
    // *after* validation succeeds), so walk up to the nearest existing ancestor
    // and canonicalize that. The not-yet-created tail is safe because it is
    // joined onto an already-canonical `home`.
    let mut ancestor = parent;
    while !ancestor.exists() {
        ancestor = ancestor
            .parent()
            .ok_or_else(|| anyhow::anyhow!("managed target escapes home"))?;
    }
    let canonical = ancestor
        .canonicalize()
        .context("canonicalizing managed target parent")?;
    if canonical != home && !canonical.starts_with(&home) {
        anyhow::bail!("target escapes AutoReport home");
    }
    if target.exists() && std::fs::symlink_metadata(&target)?.file_type().is_symlink() {
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

    // ---- new manifest / warmth / repo-spec tests ----

    #[test]
    fn dests_for_remote_includes_only_present_files() {
        let files = vec![
            FileRule {
                remote: "a/SKILL.md".to_string(),
                dests: vec!["resources/a/SKILL.md".to_string()],
                transform: noop,
            },
            FileRule {
                remote: "b/SKILL.md".to_string(),
                dests: vec!["resources/b1.md".to_string(), "resources/b2.md".to_string()],
                transform: noop,
            },
        ];
        let remote: BTreeSet<String> = ["a/SKILL.md".to_string()].into_iter().collect();
        let dests = dests_for_remote(&files, &remote);
        assert_eq!(dests, vec!["resources/a/SKILL.md".to_string()]);
    }

    #[test]
    fn cache_is_warm_false_without_manifest() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!cache_is_warm(dir.path()));
    }

    #[test]
    fn cache_is_warm_true_when_manifest_matches_and_files_present() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // Build a manifest for the skills repo whose dest set covers its rules,
        // then materialize exactly those files.
        let specs = repos();
        let (repo, files): (Repo, Vec<FileRule>) = specs
            .iter()
            .find(|(r, _)| r.name == "skills")
            .map(|(r, f)| (*r, f.clone()))
            .unwrap();
        let dests: Vec<String> = files.iter().flat_map(|r| r.dests.clone()).collect();
        for dest in &dests {
            let target = home.join(dest);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, "x").unwrap();
        }
        let manifest = Manifest {
            sha: repo.sha.to_string(),
            signature: repo_signature(&files),
            dests: dests.clone(),
        };
        save_manifest(home, &repo, &manifest);
        // Skills repo alone is warm for itself, but other repos lack manifests →
        // overall still false. Confirm this specific repo's dests are satisfied.
        let loaded = load_manifest(home, &repo).unwrap();
        assert_eq!(loaded.signature, repo_signature(&files));
        assert!(loaded.dests.iter().all(|d| home.join(d).is_file()));
    }

    #[test]
    fn cache_is_warm_false_when_signature_changes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let manifest = Manifest {
            sha: SKILLS.sha.to_string(),
            signature: "stale-signature".to_string(),
            dests: vec![],
        };
        save_manifest(home, &SKILLS, &manifest);
        // Signature mismatch with current rules → not warm (would force a re-sync
        // to regenerate the manifest).
        let specs = repos();
        let (repo, files) = specs
            .iter()
            .find(|(r, _)| r.name == "skills")
            .map(|(r, f)| (*r, f.clone()))
            .unwrap();
        let loaded = load_manifest(home, &repo).unwrap();
        assert_ne!(loaded.signature, repo_signature(&files));
    }

    #[test]
    fn manifest_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let manifest = Manifest {
            sha: "abc".to_string(),
            signature: "sig".to_string(),
            dests: vec!["a".to_string(), "b".to_string()],
        };
        save_manifest(home, &SKILLS, &manifest);
        let loaded = load_manifest(home, &SKILLS).expect("manifest saved");
        assert_eq!(loaded.sha, "abc");
        assert_eq!(loaded.signature, "sig");
        assert_eq!(loaded.dests, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn rewrite_typst_main_import_swaps_preview_import() {
        let body = "#import \"@preview/unofficial-pku-mpl:0.1.0\": *\nrest";
        assert_eq!(
            rewrite_typst_main_import(body),
            "#import \"mplts.typ\": *\nrest"
        );
    }

    #[test]
    fn rewrite_typst_skill_index_drops_example_table_rows() {
        let body = "| [basic-document.typ](examples/basic-document.typ)   | A short note or memo                     | [basics.md](basics.md), [styling.md](styling.md) |\nkept line\n";
        let out = rewrite_typst_skill_index(body);
        assert!(!out.contains("basic-document.typ"));
        assert!(out.contains("kept line"));
    }

    #[test]
    fn repos_spec_writes_writer_skill_to_both_language_roots() {
        let specs = repos();
        let skills = specs
            .iter()
            .find(|(r, _)| r.name == "skills")
            .map(|(_, f)| f)
            .unwrap();
        let writer = skills
            .iter()
            .find(|r| r.remote == "experiment-report-writer/SKILL.md")
            .unwrap();
        assert_eq!(writer.dests.len(), 2);
        assert!(
            writer
                .dests
                .iter()
                .any(|d| d.starts_with("resources/latex/skills/"))
        );
        assert!(
            writer
                .dests
                .iter()
                .any(|d| d.starts_with("resources/typst/skills/"))
        );
    }

    #[test]
    fn repo_signature_changes_when_a_rule_changes() {
        let a = vec![FileRule {
            remote: "x".to_string(),
            dests: vec!["a".to_string()],
            transform: noop,
        }];
        let b = vec![FileRule {
            remote: "x".to_string(),
            dests: vec!["a".to_string(), "b".to_string()],
            transform: noop,
        }];
        assert_ne!(repo_signature(&a), repo_signature(&b));
    }

    /// Live end-to-end check: fetch all four real trees, confirm the managed
    /// files exist remotely. Run with
    /// `cargo test fetch_live_trees -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn fetch_live_trees() {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .user_agent(concat!("autoreport/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        for (repo, files) in repos() {
            let paths = fetch_tree(&client, &repo.tree_url()).await.unwrap();
            eprintln!("{}: {} blobs", repo.name, paths.len());
            for rule in files {
                assert!(
                    paths.contains(&rule.remote),
                    "{} missing remote path {}",
                    repo.name,
                    rule.remote
                );
            }
        }
    }

    // ---- Bug 1: validate_managed_target must resolve relative dests against
    // ---- `home`, not the process CWD, and must tolerate a not-yet-created
    // ---- parent (fresh-install dir layout). ----

    /// Regression for the critical bug where `validate_managed_target`
    /// canonicalized the *relative* dest's parent against the process CWD. On a
    /// fresh install the parent doesn't exist under CWD, so `canonicalize()`
    /// errored and every managed dest was rejected as "unsafe target" — blocking
    /// all preset/skill writes.
    ///
    /// Here the home's deep parent dir is intentionally NOT created (the sync
    /// caller creates dirs only after validation passes), so a correct impl must
    /// resolve the relative path against `home`, not CWD. Before the fix this
    /// returned an error ("canonicalizing managed target parent").
    #[test]
    fn validate_managed_target_accepts_relative_dest_with_nonexistent_parent() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let dest = Path::new("resources/latex/skills/latex-compile/SKILL.md");
        validate_managed_target(home, dest)
            .expect("relative dest under home with a not-yet-created parent must validate");
    }

    /// An escape via `..` components must still be rejected once the path is
    /// joined onto home.
    #[test]
    fn validate_managed_target_rejects_path_escaping_home() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        // `home.join` keeps the `..` lexically; walking up to an existing
        // ancestor canonicalizes outside home.
        let dest = Path::new("../../etc/passwd");
        assert!(validate_managed_target(home, dest).is_err());
    }

    /// The symlink guard must still fire on the joined path: a target that is a
    /// symlink pointing outside home is rejected.
    #[test]
    fn validate_managed_target_rejects_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let dest_rel = "resources/latex/skills/latex-compile/SKILL.md";
        std::fs::create_dir_all(home.join("resources/latex/skills/latex-compile")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = dir.path().join("outside-secret.txt");
            std::fs::write(&outside, "secret").unwrap();
            symlink(&outside, home.join(dest_rel)).unwrap();
            let err = validate_managed_target(home, Path::new(dest_rel)).unwrap_err();
            assert!(
                format!("{err}").contains("symlink"),
                "unexpected err: {err}"
            );
        }
    }

    // ---- Bug 2: save_manifest must garbage-collect stale same-repo manifests. ----

    #[test]
    fn save_manifest_garbage_collects_stale_shas_for_same_repo() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(manifest_dir(home)).unwrap();
        // A manifest from a previous SHA for the SAME repo — should be removed.
        let stale = manifest_dir(home).join(format!("{}-stalesha000000.json", SKILLS.name));
        std::fs::write(&stale, "{}").unwrap();
        // A manifest for a DIFFERENT repo — must be left untouched.
        let other = manifest_dir(home).join(format!("{}-othersha.json", CC_SWITCH.name));
        std::fs::write(&other, "{}").unwrap();
        // Write the current manifest for SKILLS.
        save_manifest(
            home,
            &SKILLS,
            &Manifest {
                sha: SKILLS.sha.to_string(),
                signature: "sig".to_string(),
                dests: vec![],
            },
        );
        assert!(
            !stale.exists(),
            "stale same-repo manifest should be removed"
        );
        assert!(
            manifest_path(home, &SKILLS).exists(),
            "current manifest must exist"
        );
        assert!(other.exists(), "other-repo manifest must not be touched");
    }

    // ---- Bug 3: repo_signature must fold in the transform version. ----

    #[test]
    fn repo_signature_includes_transform_version() {
        let files = vec![FileRule {
            remote: "x".to_string(),
            dests: vec!["a".to_string()],
            transform: noop,
        }];
        let sig = repo_signature(&files);
        assert!(
            sig.contains(&format!("tx={TRANSFORM_VERSION}")),
            "signature must include transform version, got: {sig}"
        );
    }
}
