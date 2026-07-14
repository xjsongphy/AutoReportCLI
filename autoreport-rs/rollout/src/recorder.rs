//! Rollout file creation, append-only writing, and tolerant reading.

use crate::items::ResponseItem;
use crate::metadata::{RolloutLine, RolloutPayload, SessionMeta};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where rollout files live for a workspace.
pub(crate) fn sessions_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport").join("sessions")
}

/// `sessions/YYYY/MM/DD/` for a timestamp like `2026-07-12T13-45-07`
/// (codex directory layout, `rollout/src/list.rs:420`).
fn date_dir(root: &Path, ts: &str) -> PathBuf {
    let date = ts.split('T').next().unwrap_or("");
    let mut parts = date.split('-');
    let y = parts.next().unwrap_or("unknown");
    let m = parts.next().unwrap_or("00");
    let d = parts.next().unwrap_or("00");
    root.join(y).join(m).join(d)
}

/// Current UTC time as an RFC3339 string (per-line envelope timestamp).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Replace characters that are illegal in Windows filenames
/// (`< > : " | ? *` and control chars) with `-`, so a rollout filename is
/// valid on every platform regardless of what the caller passes. Forward
/// slashes (path separators) are likewise replaced to prevent directory
/// escape. Defense-in-depth; the canonical timestamp is already dash-separated.
fn sanitize_filename_component(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\' => '-',
            c if (c as u32) < 0x20 => '-',
            c => c,
        })
        .collect()
}

/// Append-only recorder: writes the `SessionMeta` header, then one JSON
/// item/line, each wrapped in the codex `RolloutLine` envelope.
pub struct RolloutRecorder {
    path: PathBuf,
}

impl RolloutRecorder {
    /// Create a new rollout file. `conversation_id` is recorded in the meta
    /// payload; `session_uuid` (a bare UUID) identifies the file on disk so
    /// resume can find it. `timestamp` is the `%Y-%m-%dT%H-%M-%S` creation
    /// stamp used for the filename and the `sessions/YYYY/MM/DD/` directory.
    pub fn create(
        workspace: &Path,
        conversation_id: &str,
        session_uuid: &str,
        timestamp: &str,
    ) -> Result<Self> {
        let root = sessions_dir(workspace);
        let ts = sanitize_filename_component(timestamp);
        let uuid = sanitize_filename_component(session_uuid);
        let dir = date_dir(&root, &ts);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("rollout-{ts}-{uuid}.jsonl"));
        let meta = SessionMeta {
            conversation_id: conversation_id.to_string(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp: timestamp.to_string(),
        };
        let line = RolloutLine {
            timestamp: now_rfc3339(),
            payload: RolloutPayload::SessionMeta(meta),
        };
        let encoded = serde_json::to_string(&line).context("encoding session meta")?;
        // Write through a create+append handle. Never `fs::write` — it
        // truncates, so a stray second `create()` would destroy history.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        writeln!(f, "{encoded}").with_context(|| format!("writing {}", path.display()))?;
        Ok(Self { path })
    }

    /// Open an existing rollout file for appending, without rewriting the
    /// header. Used on resume so the conversation continues in the *same* file
    /// instead of forking into a new one each restart.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("rollout file does not exist: {}", path.display());
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    /// Append one item. Flushes immediately so a crash mid-session still leaves
    /// a replayable file.
    pub fn append(&self, item: &ResponseItem) -> Result<()> {
        use std::io::Write;
        let line = RolloutLine {
            timestamp: now_rfc3339(),
            payload: RolloutPayload::ResponseItem(item.clone()),
        };
        let encoded = serde_json::to_string(&line).context("encoding response item")?;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        writeln!(f, "{encoded}").with_context(|| format!("writing {}", self.path.display()))?;
        f.flush().ok();
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A parsed rollout entry: either the header or an item.
#[derive(Debug)]
pub enum RolloutEntry {
    Meta(SessionMeta),
    Item(ResponseItem),
}

/// Read a rollout file back into entries (for resume / inspection). Accepts
/// both the codex `RolloutLine` envelope and the legacy bare format (lines
/// without a `payload` wrapper), so files written before the envelope migration
/// still resume.
pub fn read(path: &Path) -> Result<Vec<RolloutEntry>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                log::warn!("rollout {}:{} skipped: {e}", path.display(), i + 1);
                continue;
            }
        };
        // Envelope: {"timestamp":..., "type":"session_meta"|"response_item", "payload":{...}}
        if v.get("payload").is_some()
            && matches!(
                v.get("type").and_then(|t| t.as_str()),
                Some("session_meta" | "response_item")
            )
        {
            match serde_json::from_value::<RolloutLine>(v.clone()) {
                Ok(line) => match line.payload {
                    RolloutPayload::SessionMeta(m) => out.push(RolloutEntry::Meta(m)),
                    RolloutPayload::ResponseItem(item) => out.push(RolloutEntry::Item(item)),
                },
                Err(e) => log::warn!("rollout {}:{} bad envelope: {e}", path.display(), i + 1),
            }
            continue;
        }
        // Legacy bare format.
        match v.get("conversation_id").and_then(|x| x.as_str()) {
            Some(_) => match serde_json::from_value::<SessionMeta>(v) {
                Ok(m) => out.push(RolloutEntry::Meta(m)),
                Err(e) => log::warn!("rollout {}:{} bad meta: {e}", path.display(), i + 1),
            },
            None => match serde_json::from_value::<ResponseItem>(v) {
                Ok(item) => out.push(RolloutEntry::Item(item)),
                Err(e) => log::warn!("rollout {}:{} bad item: {e}", path.display(), i + 1),
            },
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{items, latest_for};
    use std::path::Path;

    fn stamp() -> String {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }

    fn new_uuid() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn round_trips_items_and_meta() {
        let dir = std::env::temp_dir().join(format!("rollout-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let rec =
            RolloutRecorder::create(ws, "conv-1", &new_uuid(), "2026-06-29T00-00-00Z").unwrap();
        rec.append(&ResponseItem::user_message("hello")).unwrap();
        rec.append(&ResponseItem::assistant_message("hi there"))
            .unwrap();
        rec.append(&ResponseItem::function_call(
            "c1",
            "write_file",
            "{\"path\":\"a\"}".into(),
        ))
        .unwrap();
        rec.append(&ResponseItem::function_call_output("c1", "ok"))
            .unwrap();

        let entries = read(rec.path()).unwrap();
        assert!(matches!(entries[0], RolloutEntry::Meta(_)));
        let items = items(&entries);
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].text().unwrap(), "hello");
        assert!(matches!(items[2], ResponseItem::FunctionCall { .. }));

        // Codex wire envelope: each line carries type + payload markers.
        let raw = std::fs::read_to_string(rec.path()).unwrap();
        assert!(raw.contains("\"type\":\"session_meta\""));
        assert!(raw.contains("\"type\":\"response_item\""));
        assert!(raw.contains("\"type\":\"message\""));
        assert!(raw.contains("\"type\":\"function_call\""));
        assert!(raw.contains("\"conversation_id\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn latest_for_finds_newest() {
        let dir = std::env::temp_dir().join(format!("rollout-latest-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        // Same session uuid, two files with different timestamps (different
        // date dirs); latest_for must walk the tree and pick the newest.
        let sid = new_uuid();
        RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-01T00-00-00Z").unwrap();
        RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-29T00-00-00Z").unwrap();
        let latest = latest_for(ws, &sid).unwrap();
        let name = latest.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains("2026-06-29"), "expected newest, got {name}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn open_continues_same_file_without_rewriting_header() {
        // Regression: resuming must append to the existing file, not fork into
        // a new one (otherwise multi-restart history is fragmented on disk).
        let dir = std::env::temp_dir().join(format!("rollout-open-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let rec =
            RolloutRecorder::create(ws, "conv-R", &new_uuid(), "2026-06-29T00-00-00Z").unwrap();
        rec.append(&ResponseItem::user_message("first")).unwrap();

        // Simulate a restart: reopen the same path and append a new item.
        let path = rec.path().to_path_buf();
        let reopened = RolloutRecorder::open(&path).unwrap();
        reopened
            .append(&ResponseItem::assistant_message("second"))
            .unwrap();

        let entries = read(&path).unwrap();
        let items = items(&entries);
        assert_eq!(items.len(), 2, "both items must persist in one file");
        assert_eq!(items[0].text().unwrap(), "first");
        assert_eq!(items[1].text().unwrap(), "second");
        // exactly one SessionMeta header (open did not rewrite it)
        let headers = entries
            .iter()
            .filter(|e| matches!(e, RolloutEntry::Meta(_)))
            .count();
        assert_eq!(headers, 1, "header must not be duplicated on resume");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_sanitizes_colons_in_timestamp_for_windows() {
        // Regression: an RFC3339 timestamp like `2026-06-29T00:00:00Z`
        // contains colons, which are illegal in Windows filenames (OS error
        // 123). The recorder must sanitize them so it works cross-platform
        // regardless of what the caller passes.
        let dir = std::env::temp_dir().join(format!("rollout-sanitize-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let rec =
            RolloutRecorder::create(&dir, "conv-C", &new_uuid(), "2026-06-29T00:00:00Z").unwrap();
        rec.append(&ResponseItem::user_message("hi")).unwrap();
        let name = rec
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            !name.contains(':'),
            "filename must not contain colons (invalid on Windows): {name}"
        );
        assert!(
            name.contains("2026-06-29T00-00-00Z"),
            "colons should be replaced with dashes: {name}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn latest_for_ignores_other_sessions() {
        // Two different session uuids; latest_for for one must not return the
        // other's file (exact-uuid match, not substring).
        let dir = std::env::temp_dir().join(format!("rollout-isolate-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let sid_a = new_uuid();
        let sid_b = new_uuid();
        RolloutRecorder::create(ws, "conv-A", &sid_a, "2026-06-29T00-00-00Z").unwrap();
        RolloutRecorder::create(ws, "conv-B", &sid_b, "2026-07-01T00-00-00Z").unwrap();
        let latest_a = latest_for(ws, &sid_a).unwrap();
        let name_a = latest_a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name_a.contains(&sid_a),
            "expected sid_a's file, got {name_a}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
