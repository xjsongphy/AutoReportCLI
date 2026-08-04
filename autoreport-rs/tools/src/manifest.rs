//! Manifest store: agent-local file visibility plus editable descriptions and
//! notes. Stored as JSON under the global workspace state directory.

use crate::registry::{Tool, ToolOutput, arg_opt_str};
use async_trait::async_trait;
use autoreport_core::types::AgentType;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ManifestFile {
    pub path: String,
    #[serde(default)]
    pub description: String,
    pub description_updated_at: Option<String>,
    pub file_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentManifest {
    pub agent_type: String,
    pub updated_at: String,
    #[serde(default)]
    pub files: Vec<ManifestFile>,
    #[serde(default)]
    pub notes: String,
    pub notes_updated_at: Option<String>,
}

#[derive(Clone)]
pub struct ManifestStore {
    workspace: PathBuf,
    dir: PathBuf,
    cache: Arc<Mutex<BTreeMap<String, AgentManifest>>>,
}

impl ManifestStore {
    pub fn new(workspace: &Path, state_dir: &Path) -> Self {
        let dir = state_dir.join("manifests");
        let _ = std::fs::create_dir_all(&dir);
        Self {
            workspace: workspace.to_path_buf(),
            dir,
            cache: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    fn path_for(&self, agent: AgentType) -> PathBuf {
        self.dir.join(format!("{}.json", agent.as_str()))
    }

    fn now() -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
    }

    fn default_manifest(agent: AgentType) -> AgentManifest {
        AgentManifest {
            agent_type: agent.as_str().to_string(),
            updated_at: Self::now(),
            files: Vec::new(),
            notes: String::new(),
            notes_updated_at: None,
        }
    }

    fn manifest_dirs(agent: AgentType) -> &'static [&'static str] {
        match agent {
            AgentType::Main => &["Outline"],
            AgentType::DataAnalysis => &["Data/Processed"],
            AgentType::Plotting => &["Plots"],
            AgentType::Theory => &["Theory"],
            AgentType::Report => &["Report"],
        }
    }

    fn should_ignore_dir(name: &str) -> bool {
        matches!(name, ".git" | "__pycache__" | ".autoreport" | "target")
    }

    fn should_ignore_file(name: &str) -> bool {
        matches!(name, ".DS_Store" | "Thumbs.db")
            || name.ends_with('~')
            || name.ends_with(".tmp")
            || name.ends_with(".bak")
            || name.ends_with(".swp")
            || name.ends_with(".swo")
            || name.ends_with(".aux")
            || name.ends_with(".log")
            || name.ends_with(".out")
            || name.ends_with(".toc")
            || name.ends_with(".lof")
            || name.ends_with(".lot")
            || name.ends_with(".fls")
            || name.ends_with(".fdb_latexmk")
            || name.ends_with(".synctex.gz")
            || name.ends_with(".bbl")
            || name.ends_with(".blg")
            || name.ends_with(".bcf")
            || name.ends_with(".dvi")
            || name.ends_with(".ps")
            || name.ends_with(".idx")
            || name.ends_with(".ilg")
            || name.ends_with(".ind")
            || name.ends_with(".nav")
            || name.ends_with(".snm")
            || name.ends_with(".vrb")
    }

    fn normalize_rel(&self, path: &str) -> Option<String> {
        let resolved = crate::file_tools::resolve_within(path, &self.workspace).ok()?;
        let rel = resolved.strip_prefix(&self.workspace).ok()?;
        let rel = rel.to_string_lossy().replace('\\', "/");
        (!rel.is_empty()).then_some(rel)
    }

    fn file_updated_at(path: &Path) -> Option<String> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        Some(chrono::DateTime::<Utc>::from(modified).to_rfc3339_opts(SecondsFormat::Secs, true))
    }

    fn scan_manifest_files(&self, agent: AgentType) -> Vec<ManifestFile> {
        let mut out = Vec::new();
        for rel_dir in Self::manifest_dirs(agent) {
            let dir = self.workspace.join(rel_dir);
            let Ok(metadata) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                self.walk_dir(&dir, &mut out);
            }
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        out
    }

    fn walk_dir(&self, dir: &Path, out: &mut Vec<ManifestFile>) {
        self.walk_dir_bounded(dir, out, 0);
    }

    /// Bounded recursive walk. Depth and total-entry caps stop a pathological
    /// tree (deeply nested or huge) from consuming unbounded CPU/memory on
    /// every `record()` after a mutating tool call. Symlinks are still skipped.
    fn walk_dir_bounded(&self, dir: &Path, out: &mut Vec<ManifestFile>, depth: usize) {
        const MAX_DEPTH: usize = 16;
        const MAX_ENTRIES: usize = 50_000;
        if depth > MAX_DEPTH || out.len() >= MAX_ENTRIES {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            if out.len() >= MAX_ENTRIES {
                return;
            }
            let path = entry.path();
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            // Manifests describe files produced inside the workspace. Do not
            // follow symlinks: a linked directory can create cycles and a
            // linked file can expose content outside the project boundary.
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                if Self::should_ignore_dir(&file_name) {
                    continue;
                }
                self.walk_dir_bounded(&path, out, depth + 1);
                continue;
            }
            if Self::should_ignore_file(&file_name) {
                continue;
            }
            let Some(rel) = path
                .strip_prefix(&self.workspace)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
            else {
                continue;
            };
            out.push(ManifestFile {
                path: rel,
                description: String::new(),
                description_updated_at: None,
                file_updated_at: Self::file_updated_at(&path),
            });
        }
    }

    fn sync_with_filesystem(&self, agent: AgentType, mut manifest: AgentManifest) -> AgentManifest {
        let actual = self.scan_manifest_files(agent);
        let existing: BTreeMap<String, ManifestFile> = manifest
            .files
            .into_iter()
            .map(|item| (item.path.clone(), item))
            .collect();
        manifest.files = actual
            .into_iter()
            .map(|item| {
                if let Some(prev) = existing.get(&item.path) {
                    ManifestFile {
                        path: item.path,
                        description: prev.description.clone(),
                        description_updated_at: prev.description_updated_at.clone(),
                        file_updated_at: item.file_updated_at,
                    }
                } else {
                    item
                }
            })
            .collect();
        manifest
    }

    fn load_from_disk(&self, agent: AgentType) -> AgentManifest {
        let path = self.path_for(agent);
        let manifest = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<AgentManifest>(&text).ok())
            .unwrap_or_else(|| Self::default_manifest(agent));
        self.sync_with_filesystem(agent, manifest)
    }

    fn persist(&self, agent: AgentType, manifest: &AgentManifest) {
        let _ = std::fs::write(
            self.path_for(agent),
            serde_json::to_string_pretty(manifest).unwrap_or_default(),
        );
    }

    pub fn load(&self, agent: AgentType) -> AgentManifest {
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(manifest) = cache.get(agent.as_str()) {
                return self.sync_with_filesystem(agent, manifest.clone());
            }
        }
        let manifest = self.load_from_disk(agent);
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.as_str().to_string(), manifest.clone());
        manifest
    }

    pub fn save(&self, agent: AgentType, mut manifest: AgentManifest) {
        manifest.agent_type = agent.as_str().to_string();
        manifest.updated_at = Self::now();
        manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(agent.as_str().to_string(), manifest.clone());
        self.persist(agent, &manifest);
    }

    /// Record or refresh a file entry after a mutating tool call.
    pub fn record(&self, agent: AgentType, path: String) {
        let Some(rel_path) = self.normalize_rel(&path) else {
            return;
        };
        let abs = self.workspace.join(&rel_path);
        let mut manifest = self.load(agent);
        let updated_at = Self::file_updated_at(&abs).or_else(|| Some(Self::now()));
        if let Some(existing) = manifest.files.iter_mut().find(|item| item.path == rel_path) {
            existing.file_updated_at = updated_at;
        } else {
            manifest.files.push(ManifestFile {
                path: rel_path,
                description: String::new(),
                description_updated_at: None,
                file_updated_at: updated_at,
            });
        }
        self.save(agent, manifest);
    }

    pub fn snapshot(&self, agent: Option<AgentType>) -> Value {
        match agent {
            Some(agent) => serde_json::to_value(self.load(agent)).unwrap_or(Value::Null),
            None => {
                let mut map = serde_json::Map::new();
                for agent in AgentType::ALL {
                    map.insert(
                        agent.as_str().to_string(),
                        serde_json::to_value(self.load(agent)).unwrap_or(Value::Null),
                    );
                }
                Value::Object(map)
            }
        }
    }
}

fn parse_optional_json(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::String(raw)) => serde_json::from_str(raw)
            .ok()
            .or_else(|| Some(Value::String(raw.clone()))),
        Some(other) => Some(other.clone()),
        None => None,
    }
}

#[derive(Debug)]
struct NotesPatchChunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    eof: bool,
}

fn parse_notes_patch(patch: &str) -> Result<Vec<NotesPatchChunk>, String> {
    let lines: Vec<&str> = patch.lines().collect();
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line == "@@" || line.starts_with("@@ ") {
            i += 1;
        }
        let mut chunk = NotesPatchChunk {
            old_lines: Vec::new(),
            new_lines: Vec::new(),
            eof: false,
        };
        let mut parsed = 0usize;
        while i < lines.len() {
            let line = lines[i];
            if line == "@@" || line.starts_with("@@ ") {
                break;
            }
            if line == "*** End of File" {
                chunk.eof = true;
                i += 1;
                break;
            }
            match line.chars().next() {
                Some(' ') => {
                    let body = line[1..].to_string();
                    chunk.old_lines.push(body.clone());
                    chunk.new_lines.push(body);
                }
                Some('+') => chunk.new_lines.push(line[1..].to_string()),
                Some('-') => chunk.old_lines.push(line[1..].to_string()),
                _ => return Err(format!("invalid notes_patch line: {line}")),
            }
            parsed += 1;
            i += 1;
        }
        if parsed == 0 && !chunk.eof {
            return Err("notes_patch must contain at least one change line".into());
        }
        chunks.push(chunk);
    }
    Ok(chunks)
}

fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').map(|line| line.to_string()).collect()
    }
}

fn join_lines(lines: &[String]) -> String {
    lines.join("\n")
}

fn seek_sequence(lines: &[String], pattern: &[String], start: usize, eof: bool) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start.min(lines.len()));
    }
    if pattern.len() > lines.len() {
        return None;
    }
    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start.min(lines.len().saturating_sub(pattern.len()))
    };
    for idx in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[idx..idx + pattern.len()] == *pattern {
            return Some(idx);
        }
    }
    for idx in search_start..=lines.len().saturating_sub(pattern.len()) {
        let ok = pattern
            .iter()
            .enumerate()
            .all(|(offset, pat)| lines[idx + offset].trim_end() == pat.trim_end());
        if ok {
            return Some(idx);
        }
    }
    None
}

fn apply_notes_patch(current: &str, patch: &str) -> Result<String, String> {
    let chunks = parse_notes_patch(patch)?;
    let mut lines = split_lines(current);
    let mut cursor = 0usize;
    for chunk in chunks {
        let start = if chunk.old_lines.is_empty() {
            cursor.min(lines.len())
        } else {
            seek_sequence(&lines, &chunk.old_lines, cursor, chunk.eof)
                .ok_or_else(|| "notes_patch did not match current notes".to_string())?
        };
        let end = start + chunk.old_lines.len();
        lines.splice(start..end, chunk.new_lines.clone());
        cursor = start + chunk.new_lines.len();
    }
    Ok(join_lines(&lines))
}

fn unified_diff(old: &str, new: &str) -> Option<String> {
    if old == new {
        return None;
    }
    Some(
        diffy::PatchFormatter::new()
            .missing_newline_message(false)
            .fmt_patch(&diffy::create_patch(old, new))
            .to_string(),
    )
}

/// `manifest` tool — read or update agent manifests.
pub struct ManifestTool {
    store: ManifestStore,
    agent: AgentType,
}

impl ManifestTool {
    pub fn new(store: ManifestStore, agent: AgentType) -> Self {
        Self { store, agent }
    }
}

#[async_trait]
impl Tool for ManifestTool {
    fn name(&self) -> &str {
        "manifest"
    }

    fn description(&self) -> &str {
        "Read or update agent manifests. Use it to inspect available files, update short file descriptions, and add free-form notes about file relationships. The file list is driven by the local filesystem: you can only annotate files that already exist in your manifest. Descriptions and notes are edited incrementally via `files` and `notes_patch`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["read", "update"], "default": "read"},
                "agent": {"type": "string", "enum": ["main", "data_analysis", "plotting", "theory", "report"]},
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "description_old": {"type": "string"},
                            "description_new": {"type": "string"}
                        },
                        "required": ["path"]
                    }
                },
                "notes_patch": {"type": "string"}
            }
        })
    }

    async fn call(&self, args: &Value) -> ToolOutput {
        let action = arg_opt_str(args, "action").unwrap_or_else(|| "read".to_string());
        let target_agent = match arg_opt_str(args, "agent") {
            Some(agent) => match agent.parse::<AgentType>() {
                Ok(agent) => agent,
                Err(err) => return ToolOutput::err(err),
            },
            None => self.agent,
        };

        if action == "read" {
            return ToolOutput::ok(
                serde_json::to_value(self.store.load(target_agent)).unwrap_or(Value::Null),
            );
        }
        if action != "update" {
            return ToolOutput::err(format!("unknown action '{action}'"));
        }
        if target_agent != self.agent {
            return ToolOutput::err(format!(
                "cannot update other agent's manifest; you can only update {}",
                self.agent.as_str()
            ));
        }

        let mut manifest = self.store.load(self.agent);
        let now = ManifestStore::now();
        let file_map: BTreeMap<String, usize> = manifest
            .files
            .iter()
            .enumerate()
            .map(|(index, item)| (item.path.clone(), index))
            .collect();
        let mut not_found = Vec::<String>::new();
        let mut description_changes = Vec::<Value>::new();
        let mut description_mismatches = Vec::<Value>::new();

        if let Some(Value::Array(records)) = parse_optional_json(args.get("files")) {
            for record in records {
                let Some(path) = record.get("path").and_then(|v| v.as_str()) else {
                    continue;
                };
                let path = path.trim();
                if path.is_empty() {
                    continue;
                }
                let Some(&index) = file_map.get(path) else {
                    not_found.push(path.to_string());
                    continue;
                };
                let Some(description_new) = record.get("description_new").and_then(|v| v.as_str())
                else {
                    continue;
                };
                let current = manifest.files[index].description.clone();
                let description_old = record
                    .get("description_old")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&current);
                if description_old != current {
                    description_mismatches.push(json!({
                        "path": path,
                        "expected": description_old,
                        "actual": current,
                    }));
                    continue;
                }
                if description_new != current {
                    manifest.files[index].description = description_new.to_string();
                    manifest.files[index].description_updated_at = Some(now.clone());
                    description_changes.push(json!({
                        "path": path,
                        "old": current,
                        "new": description_new,
                    }));
                }
            }
        }

        let mut notes_diff = None::<String>;
        if let Some(Value::String(notes_patch)) = parse_optional_json(args.get("notes_patch")) {
            if !notes_patch.trim().is_empty() {
                let current = manifest.notes.clone();
                let updated = match apply_notes_patch(&current, &notes_patch) {
                    Ok(text) => text,
                    Err(err) => {
                        return ToolOutput::err(format!("failed to apply notes_patch: {err}"));
                    }
                };
                if updated != current {
                    manifest.notes = updated.clone();
                    manifest.notes_updated_at = Some(now);
                    notes_diff = unified_diff(&current, &updated);
                }
            }
        }

        self.store.save(self.agent, manifest.clone());
        ToolOutput::ok(json!({
            "status": "ok",
            "manifest": manifest,
            "not_found": not_found,
            "description_changes": description_changes,
            "description_mismatches": description_mismatches,
            "notes_diff": notes_diff,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn load_scans_agent_directory_and_preserves_annotations() {
        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.path().join("Plots/Scripts")).unwrap();
        std::fs::write(
            workspace.path().join("Plots/Scripts/plot.py"),
            "print('x')\n",
        )
        .unwrap();

        let store = ManifestStore::new(workspace.path(), workspace.path());
        let mut manifest = store.load(AgentType::Plotting);
        assert_eq!(manifest.files.len(), 1);
        manifest.files[0].description = "main plotting script".into();
        manifest.files[0].description_updated_at = Some("2026-01-01T00:00:00Z".into());
        store.save(AgentType::Plotting, manifest);

        std::fs::write(
            workspace.path().join("Plots/Scripts/other.py"),
            "print('y')\n",
        )
        .unwrap();
        let loaded = store.load(AgentType::Plotting);
        assert_eq!(loaded.files.len(), 2);
        let plot = loaded
            .files
            .iter()
            .find(|item| item.path == "Plots/Scripts/plot.py")
            .unwrap();
        assert_eq!(plot.description, "main plotting script");
    }

    #[test]
    fn notes_patch_updates_text() {
        let updated = apply_notes_patch("alpha\nbeta\n", "@@\n alpha\n-beta\n+gamma\n").unwrap();
        assert_eq!(updated, "alpha\ngamma\n");
    }

    #[cfg(unix)]
    #[test]
    fn manifest_scan_does_not_follow_symlinks() {
        use std::os::unix::fs::symlink;

        let workspace = temp_workspace();
        std::fs::create_dir_all(workspace.path().join("Plots")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside\n").unwrap();
        symlink(
            outside.path().join("secret.txt"),
            workspace.path().join("Plots/linked.txt"),
        )
        .unwrap();

        let store = ManifestStore::new(workspace.path(), workspace.path());
        let manifest = store.load(AgentType::Plotting);
        assert!(manifest.files.is_empty());
    }

    #[test]
    fn load_and_save_survive_poisoned_cache() {
        // A panic on another thread while holding the cache lock poisons it.
        // Every subsequent cache acquisition must recover (match `taskboard.rs`)
        // instead of cascading the poison panic into all manifest operations.
        let workspace = temp_workspace();
        let store = ManifestStore::new(workspace.path(), workspace.path());
        let cache = Arc::clone(&store.cache);
        let handle = std::thread::spawn(move || {
            let _guard = cache.lock().unwrap();
            panic!("intentional poison");
        });
        let join_err = handle.join();
        assert!(join_err.is_err(), "spawned thread should have panicked");

        // `load` reads the cache (miss), falls through to a second lock to
        // insert — both must recover from the poison rather than panicking.
        let manifest = store.load(AgentType::Main);
        assert_eq!(manifest.agent_type, "main");

        // `save` also takes the cache lock; it must recover too.
        store.save(AgentType::Main, manifest);
        // And a subsequent load reflects the saved state.
        let reloaded = store.load(AgentType::Main);
        assert_eq!(reloaded.agent_type, "main");
    }
}
