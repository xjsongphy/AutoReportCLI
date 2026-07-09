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

/// Where rollout files live for a workspace.
pub fn sessions_dir(workspace: &Path) -> PathBuf {
    workspace.join(".autoreport").join("sessions")
}

/// Append-only recorder: writes `SessionMeta` header, then one JSON item/line.
pub struct RolloutRecorder {
    path: PathBuf,
}

impl RolloutRecorder {
    /// Create (or resume) a rollout file for `conversation_id`.
    pub fn create(workspace: &Path, conversation_id: &str, timestamp: &str) -> Result<Self> {
        let dir = sessions_dir(workspace);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("rollout-{timestamp}-{conversation_id}.jsonl"));
        let meta = SessionMeta {
            conversation_id: conversation_id.to_string(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp: timestamp.to_string(),
        };
        let line = serde_json::to_string(&meta).context("encoding session meta")?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        std::fs::write(&path, format!("{line}\n"))
            .with_context(|| format!("writing {}", path.display()))?;
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
        let line = serde_json::to_string(item).context("encoding response item")?;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.path)
            .with_context(|| format!("opening {}", self.path.display()))?;
        writeln!(f, "{line}").with_context(|| format!("writing {}", self.path.display()))?;
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

/// Read a rollout file back into entries (for resume / inspection).
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

/// The most recent rollout path for a given conversation id, if any (for resume).
pub fn latest_for(workspace: &Path, conversation_id: &str) -> Option<PathBuf> {
    let dir = sessions_dir(workspace);
    let mut best: Option<(PathBuf, String)> = None;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        // filename: rollout-<timestamp>-<id>.jsonl
        if !name.contains(conversation_id) {
            continue;
        }
        let key = name.clone();
        match &best {
            Some((_, k)) if key.as_str() <= k.as_str() => {}
            _ => best = Some((entry.path(), key)),
        }
    }
    best.map(|(p, _)| p)
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

    #[test]
    fn round_trips_items_and_meta() {
        let dir = std::env::temp_dir().join(format!("rollout-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let rec = RolloutRecorder::create(ws, "conv-1", "2026-06-29T00:00:00Z").unwrap();
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

        // format compatibility: each line is a JSON object with a "type" tag.
        let raw = std::fs::read_to_string(rec.path()).unwrap();
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
        RolloutRecorder::create(ws, "conv-X", "2026-06-01T00:00:00Z").unwrap();
        RolloutRecorder::create(ws, "conv-X", "2026-06-29T00:00:00Z").unwrap();
        let latest = latest_for(ws, "conv-X").unwrap();
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
        let rec = RolloutRecorder::create(ws, "conv-R", "2026-06-29T00:00:00Z").unwrap();
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
}
