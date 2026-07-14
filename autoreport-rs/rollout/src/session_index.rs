//! Session-file discovery and resume indexing.

use crate::sessions_dir;
use std::path::{Path, PathBuf};

/// The most recent rollout path for a given session UUID, if any (for resume).
/// Walks the `sessions/` tree (codex layout: `YYYY/MM/DD/`), parses the UUID
/// out of each filename, and matches by exact UUID equality (codex
/// `parse_timestamp_uuid_from_filename`, `list.rs:964`).
pub fn latest_for(workspace: &Path, session_uuid: &str) -> Option<PathBuf> {
    let root = sessions_dir(workspace);
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
