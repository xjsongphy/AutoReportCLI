//! Codex-style conversation persistence (rollout).
//!
//! Vendored data model + on-disk format from codex (`codex-protocol::ResponseItem`
//! and `codex-rollout`): every conversation item is a `ResponseItem` tagged
//! `{"type": ...}` (snake_case), serialized one-per-line as append-only JSONL
//! under `.autoreport/sessions/rollout-<timestamp>-<id>.jsonl`, preceded by a
//! `SessionMeta` header line — the same shape codex writes, so files are
//! inspectable/replayable with the same tools (e.g. `jq`).
//!
//! We keep the variants our direct-API provider layer actually produces:
//! `Message`, `FunctionCall`, `FunctionCallOutput`, `Reasoning`, and a
//! `Compaction` marker. codex's richer variants (local shell, web search, etc.)
//! round-trip through `Other` on read.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One content piece of a message. codex uses `input_text` / `output_text` /
/// `text`; we accept all three on read and emit the role-appropriate one.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText { text: String },
    OutputText { text: String },
    Text { text: String },
}

impl ContentItem {
    pub fn text(&self) -> &str {
        match self {
            ContentItem::InputText { text }
            | ContentItem::OutputText { text }
            | ContentItem::Text { text } => text,
        }
    }
    pub fn input(text: impl Into<String>) -> Self {
        ContentItem::InputText { text: text.into() }
    }
    pub fn output(text: impl Into<String>) -> Self {
        ContentItem::OutputText { text: text.into() }
    }
}

/// A single conversation item, codex `ResponseItem` shape (subset).
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
    },
    Reasoning {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        #[serde(default)]
        summary: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<String>>,
        /// Opaque signed reasoning blob to echo back on the next turn (codex
        /// `encrypted_content`; Anthropic thinking `signature`). Absent on
        /// providers that don't sign reasoning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    FunctionCall {
        #[serde(default, skip_serializing)]
        id: Option<String>,
        call_id: String,
        name: String,
        /// JSON-encoded arguments string (codex serializes arguments as a string).
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    /// codex emits a Compaction item when the context is summarized.
    Compaction {
        encrypted_content: String,
    },
    /// Catch-all for unknown / codex-only variants (e.g. `local_shell_call`,
    /// `web_search_call`, `compaction_trigger`) encountered when resuming a
    /// rollout written by codex or a future writer. Mirrors codex's `Other`
    /// arm (`protocol/src/models.rs`); `#[serde(other)]` makes reads tolerate
    /// forward-compatible type tags instead of dropping the whole line.
    /// We don't act on these items — they're skipped during history conversion.
    #[serde(other)]
    Other,
}

impl ResponseItem {
    pub fn user_message(text: impl Into<String>) -> Self {
        ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![ContentItem::input(text)],
        }
    }
    pub fn assistant_message(text: impl Into<String>) -> Self {
        ResponseItem::Message {
            id: None,
            role: "assistant".into(),
            content: vec![ContentItem::output(text)],
        }
    }
    pub fn function_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments_json: String,
    ) -> Self {
        ResponseItem::FunctionCall {
            id: None,
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments_json,
        }
    }
    pub fn function_call_output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        ResponseItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
        }
    }
    pub fn reasoning(text: impl Into<String>) -> Self {
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![text.into()]),
            encrypted_content: None,
        }
    }

    /// Reasoning with a signed blob (Anthropic `signature` / codex
    /// `encrypted_content`), so it can be echoed back to continue an
    /// extended-thinking turn.
    pub fn reasoning_signed(text: impl Into<String>, signature: impl Into<String>) -> Self {
        ResponseItem::Reasoning {
            id: None,
            summary: Vec::new(),
            content: Some(vec![text.into()]),
            encrypted_content: Some(signature.into()),
        }
    }

    /// The signed reasoning blob, if any (for echo-back on the next turn).
    pub fn reasoning_signature(&self) -> Option<&str> {
        match self {
            ResponseItem::Reasoning {
                encrypted_content: Some(s),
                ..
            } => Some(s),
            _ => None,
        }
    }

    /// Plain-text view for display / transcript summarization.
    pub fn text(&self) -> Option<String> {
        match self {
            ResponseItem::Message { content, .. } => Some(
                content
                    .iter()
                    .map(|c| c.text().to_string())
                    .collect::<Vec<_>>()
                    .join(""),
            ),
            ResponseItem::FunctionCall {
                name, arguments, ..
            } => Some(format!("{}({})", name, arguments)),
            ResponseItem::FunctionCallOutput { output, .. } => Some(output.clone()),
            ResponseItem::Reasoning {
                content, summary, ..
            } => {
                let joined = content.clone().unwrap_or_default().join("\n");
                let trimmed = joined.trim();
                if !trimmed.is_empty() {
                    Some(trimmed.to_string())
                } else {
                    Some(summary.join(" "))
                }
            }
            ResponseItem::Compaction { .. } => None,
            ResponseItem::Other => None,
        }
    }
}

/// First line of a rollout file (codex `SessionMeta`).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SessionMeta {
    pub conversation_id: String,
    pub cli_version: String,
    pub timestamp: String,
}

/// Codex on-disk wire envelope. Every rollout line is one of these, never a
/// bare item:
///   {"timestamp":"...","type":"session_meta","payload":{...}}
///   {"timestamp":"...","type":"response_item","payload":{"type":"message",...}}
/// This makes files inspectable with `jq 'select(.type=="response_item")'`
/// and listable by codex tooling (`codex list-threads`), which deserialize
/// `RolloutLine` directly.
#[derive(Serialize, Deserialize, Clone, Debug)]
struct RolloutLine {
    timestamp: String,
    #[serde(flatten)]
    payload: RolloutPayload,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
enum RolloutPayload {
    SessionMeta(SessionMeta),
    ResponseItem(ResponseItem),
}

/// Where rollout files live for a workspace.
pub fn sessions_dir(workspace: &Path) -> PathBuf {
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

/// Items only (drops the meta header) from a rollout read.
pub fn items(entries: &[RolloutEntry]) -> Vec<ResponseItem> {
    entries
        .iter()
        .filter_map(|e| match e {
            RolloutEntry::Item(i) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
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
