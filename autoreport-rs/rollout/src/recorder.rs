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
///
/// Mirrors codex's `rollout::recorder` design: a dedicated writer task owns a
/// single open file handle and drains an mpsc channel of pre-encoded lines.
/// `append`/`create` only hand lines to that task over the channel, so the
/// agent-loop hot path never blocks on file I/O — and unlike the old
/// per-call `OpenOptions` reopen, the file is opened once per session. Each
/// line is flushed as written, so a crash mid-session still leaves a
/// replayable file. When the last sender drops the channel closes, the writer
/// drains any buffered lines, and exits.
/// Control message for the writer task: either a pre-encoded line to append,
/// or a flush barrier (acknowledged once all prior lines are on disk).
enum WriterMsg {
    Line(String),
    Flush(tokio::sync::oneshot::Sender<()>),
}

pub struct RolloutRecorder {
    path: PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<WriterMsg>,
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
        let first_line =
            serde_json::to_string(&line).context("encoding session meta")?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<WriterMsg>();
        spawn_writer_task(path.clone(), rx, /*create*/ true, Some(first_line));
        Ok(Self { path, tx })
    }

    /// Open an existing rollout file for appending, without rewriting the
    /// header. Used on resume so the conversation continues in the *same* file
    /// instead of forking into a new one each restart.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("rollout file does not exist: {}", path.display());
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<WriterMsg>();
        spawn_writer_task(path.to_path_buf(), rx, /*create*/ false, None);
        Ok(Self {
            path: path.to_path_buf(),
            tx,
        })
    }

    /// Append one item. Encodes the line and hands it to the writer task over
    /// an unbounded channel — non-blocking, returns immediately. Errors from
    /// the actual file write surface in the writer task (logged), not here.
    pub fn append(&self, item: &ResponseItem) -> Result<()> {
        let line = RolloutLine {
            timestamp: now_rfc3339(),
            payload: RolloutPayload::ResponseItem(item.clone()),
        };
        let encoded = serde_json::to_string(&line).context("encoding response item")?;
        self.tx
            .send(WriterMsg::Line(encoded))
            .map_err(|_| anyhow::anyhow!("rollout writer task closed"))
    }

    /// Block until every line appended so far has been written and flushed by
    /// the writer task. The agent loop does NOT call this (it tolerates
    /// deferred writes); it exists for tests and explicit shutdown/sync points.
    pub async fn flush(&self) -> Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(WriterMsg::Flush(ack_tx))
            .map_err(|_| anyhow::anyhow!("rollout writer task closed"))?;
        let _ = ack_rx.await;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// The dedicated writer task: opens the rollout file once (create+append or
/// append-only) and processes each message as it arrives on `rx` in FIFO order
/// — `Line` is written+flushed, `Flush` is acknowledged once all prior lines
/// are on disk. Exits when the channel closes (all senders dropped), after
/// draining any buffered lines. Mirrors codex `rollout::recorder` writer task.
fn spawn_writer_task(
    path: PathBuf,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<WriterMsg>,
    create: bool,
    first_line: Option<String>,
) {
    tokio::spawn(async move {
        use std::io::Write;
        let mut f = match std::fs::OpenOptions::new()
            .create(create)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                log::error!("rollout writer: opening {}: {e}", path.display());
                return;
            }
        };
        let mut write_line = |line: &str| -> std::io::Result<()> {
            writeln!(f, "{line}")?;
            f.flush()
        };
        if let Some(line) = first_line {
            if let Err(e) = write_line(&line) {
                log::error!("rollout writer: writing {}: {e}", path.display());
            }
        }
        while let Some(msg) = rx.recv().await {
            match msg {
                WriterMsg::Line(line) => {
                    if let Err(e) = write_line(&line) {
                        log::error!("rollout writer: writing {}: {e}", path.display());
                    }
                }
                WriterMsg::Flush(ack) => {
                    // Lines are written in FIFO order before this message, so
                    // by the time we get here all prior appends are flushed.
                    let _ = ack.send(());
                }
            }
        }
        // Channel closed: all senders gone; the file handle flushes on drop.
    });
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

    #[tokio::test]
    async fn round_trips_items_and_meta() {
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
        rec.flush().await.unwrap();

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

    #[tokio::test]
    async fn latest_for_finds_newest() {
        let dir = std::env::temp_dir().join(format!("rollout-latest-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        // Same session uuid, two files with different timestamps (different
        // date dirs); latest_for must walk the tree and pick the newest.
        let sid = new_uuid();
        let r1 = RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-01T00-00-00Z").unwrap();
        let r2 = RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-29T00-00-00Z").unwrap();
        r1.flush().await.unwrap();
        r2.flush().await.unwrap();
        let latest = latest_for(ws, &sid).unwrap();
        let name = latest.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.contains("2026-06-29"), "expected newest, got {name}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn open_continues_same_file_without_rewriting_header() {
        // Regression: resuming must append to the existing file, not fork into
        // a new one (otherwise multi-restart history is fragmented on disk).
        let dir = std::env::temp_dir().join(format!("rollout-open-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let rec =
            RolloutRecorder::create(ws, "conv-R", &new_uuid(), "2026-06-29T00-00-00Z").unwrap();
        rec.append(&ResponseItem::user_message("first")).unwrap();
        rec.flush().await.unwrap();

        // Simulate a restart: reopen the same path and append a new item.
        let path = rec.path().to_path_buf();
        let reopened = RolloutRecorder::open(&path).unwrap();
        reopened
            .append(&ResponseItem::assistant_message("second"))
            .unwrap();
        reopened.flush().await.unwrap();

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

    #[tokio::test]
    async fn create_sanitizes_colons_in_timestamp_for_windows() {
        // Regression: an RFC3339 timestamp like `2026-06-29T00:00:00Z`
        // contains colons, which are illegal in Windows filenames (OS error
        // 123). The recorder must sanitize them so it works cross-platform
        // regardless of what the caller passes.
        let dir = std::env::temp_dir().join(format!("rollout-sanitize-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let rec =
            RolloutRecorder::create(&dir, "conv-C", &new_uuid(), "2026-06-29T00:00:00Z").unwrap();
        rec.append(&ResponseItem::user_message("hi")).unwrap();
        rec.flush().await.unwrap();
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

    #[tokio::test]
    async fn latest_for_ignores_other_sessions() {
        // Two different session uuids; latest_for for one must not return the
        // other's file (exact-uuid match, not substring).
        let dir = std::env::temp_dir().join(format!("rollout-isolate-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let sid_a = new_uuid();
        let sid_b = new_uuid();
        let ra = RolloutRecorder::create(ws, "conv-A", &sid_a, "2026-06-29T00-00-00Z").unwrap();
        let rb = RolloutRecorder::create(ws, "conv-B", &sid_b, "2026-07-01T00-00-00Z").unwrap();
        ra.flush().await.unwrap();
        rb.flush().await.unwrap();
        let latest_a = latest_for(ws, &sid_a).unwrap();
        let name_a = latest_a.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name_a.contains(&sid_a),
            "expected sid_a's file, got {name_a}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
