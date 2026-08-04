//! Session-file discovery and resume indexing.

use crate::sessions_dir;
use crate::{RolloutEntry, SessionMeta, read};
use std::path::{Path, PathBuf};

/// The most recent rollout path for a given session UUID, if any (for resume).
/// Walks this project's `sessions/` tree (codex layout: `YYYY/MM/DD/`), parses the UUID
/// out of each filename, and matches by exact UUID equality (codex
/// `parse_timestamp_uuid_from_filename`, `list.rs:964`).
pub fn latest_for(home: &Path, session_uuid: &str) -> Option<PathBuf> {
    let root = sessions_dir(home);
    let mut files: Vec<(PathBuf, String, String)> = Vec::new(); // (path, ts_key, uuid)
    collect_rollout_files(&root, &mut files);
    // Match by exact UUID; among matches keep the lexicographically largest
    // timestamp (fixed-width `%Y-%m-%dT%H-%M-%S` ⇒ newest).
    let mut best: Option<(PathBuf, String)> = None;
    for (path, ts, uuid) in files {
        if uuid != session_uuid {
            continue;
        }
        match &best {
            Some((_, k)) if ts.as_str() <= k.as_str() => {}
            _ => best = Some((path, ts)),
        }
    }
    best.map(|(p, _)| p)
}

/// Find the newest session belonging to one agent and one canonical project.
/// The caller supplies this project's state directory, so another project's
/// rollout tree is not even searched.
pub fn latest_for_agent(
    home: &Path,
    agent: &str,
    workspace: &Path,
) -> Option<(PathBuf, SessionMeta)> {
    list_for_agent(home, agent, workspace).into_iter().next()
}

/// List this project's persisted sessions for one agent, newest first.
/// Repeated rollout files for the same logical conversation are deduplicated.
pub fn list_for_agent(home: &Path, agent: &str, workspace: &Path) -> Vec<(PathBuf, SessionMeta)> {
    let expected_cwd = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let root = sessions_dir(home);
    let mut files = Vec::new();
    collect_rollout_files(&root, &mut files);
    files.sort_by(|a, b| b.1.cmp(&a.1));
    let mut seen = std::collections::HashSet::new();
    files
        .into_iter()
        .filter_map(|(path, _, _)| {
            let meta = read(&path)
                .ok()?
                .into_iter()
                .find_map(|entry| match entry {
                    RolloutEntry::Meta(meta) => Some(meta),
                    RolloutEntry::Item(_) => None,
                })?;
            if meta.cwd == expected_cwd && meta.agent_role.as_deref() == Some(agent) {
                seen.insert(meta.conversation_id.clone())
                    .then_some((path, meta))
            } else {
                None
            }
        })
        .collect()
}

/// Recursively gather `rollout-*.jsonl` files under `root`, parsing each
/// filename into `(path, timestamp, uuid)`. Mirrors codex's filename grammar
/// `rollout-YYYY-MM-DDThh-mm-ss-<uuid>.jsonl`.
fn collect_rollout_files(root: &Path, out: &mut Vec<(PathBuf, String, String)>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            collect_rollout_files(&path, out);
            continue;
        }
        if !ft.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(core) = name
            .strip_prefix("rollout-")
            .and_then(|s| s.strip_suffix(".jsonl"))
        else {
            continue;
        };
        // Scan from the right for a `-` whose suffix parses as a UUID (codex
        // `parse_timestamp_uuid_from_filename`); the remainder is the timestamp.
        let Some((ts, uuid)) = parse_ts_uuid(core) else {
            continue;
        };
        out.push((path, ts, uuid));
    }
}

/// Split `YYYY-MM-DDThh-mm-ss-<uuid>` into `(timestamp, uuid)` by finding the
/// rightmost `-` boundary whose suffix is a valid UUID.
fn parse_ts_uuid(core: &str) -> Option<(String, String)> {
    for (i, _) in core.rmatch_indices('-') {
        if let Ok(_) = uuid::Uuid::parse_str(&core[i + 1..]) {
            return Some((core[..i].to_string(), core[i + 1..].to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::latest_for_agent;
    use crate::{ResponseItem, RolloutRecorder};
    use tempfile::TempDir;

    #[tokio::test]
    async fn latest_for_agent_never_crosses_project_cwd() {
        let root = TempDir::new().unwrap();
        let workspace_a = root.path().join("project-a");
        let workspace_b = root.path().join("project-b");
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();
        let home = root.path().join("autoreport-home");

        let recorder = RolloutRecorder::create(
            &home,
            "main-session",
            &uuid::Uuid::new_v4().to_string(),
            "2026-07-16T00-00-00Z",
            &workspace_a,
            "main",
        )
        .unwrap();
        recorder
            .append(&ResponseItem::user_message("only A"))
            .await
            .unwrap();
        recorder.flush().await.unwrap();

        assert!(latest_for_agent(&home, "main", &workspace_a).is_some());
        assert!(latest_for_agent(&home, "main", &workspace_b).is_none());
        assert!(latest_for_agent(&root.path().join("scoped-b"), "main", &workspace_a).is_none());
    }
}
