//! Rollout file creation, append-only writing, and current-format reading.

use crate::items::ResponseItem;
use crate::metadata::{RolloutLine, RolloutPayload, SessionMeta};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Where rollout files live for one project's state directory.
pub(crate) fn sessions_dir(home: &Path) -> PathBuf {
    home.join("sessions")
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

/// Bounded channel capacity, matching codex's `mpsc::channel::<RolloutCmd>(256)`
/// (codex `rollout/src/recorder.rs:889-892`).
const ROLLOUT_CHANNEL_CAPACITY: usize = 256;

pub struct RolloutRecorder {
    path: PathBuf,
    tx: tokio::sync::mpsc::Sender<WriterMsg>,
}

impl RolloutRecorder {
    /// Create a new rollout file. `conversation_id` is recorded in the meta
    /// payload; `session_uuid` (a bare UUID) identifies the file on disk so
    /// resume can find it. `timestamp` is the `%Y-%m-%dT%H-%M-%S` creation
    /// stamp used for the filename and the `sessions/YYYY/MM/DD/` directory.
    pub fn create(
        home: &Path,
        conversation_id: &str,
        session_uuid: &str,
        timestamp: &str,
        workspace: &Path,
        agent: &str,
    ) -> Result<Self> {
        let root = sessions_dir(home);
        let ts = sanitize_filename_component(timestamp);
        let uuid = sanitize_filename_component(session_uuid);
        let dir = date_dir(&root, &ts);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join(format!("rollout-{ts}-{uuid}.jsonl"));
        let meta = SessionMeta {
            session_id: session_uuid.to_string(),
            id: session_uuid.to_string(),
            conversation_id: conversation_id.to_string(),
            cli_version: env!("CARGO_PKG_VERSION").to_string(),
            timestamp: timestamp.to_string(),
            cwd: workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf())
                .display()
                .to_string(),
            originator: "autoreport-cli".to_string(),
            source: "cli".to_string(),
            model_provider: None,
            agent_role: Some(agent.to_string()),
        };
        let line = RolloutLine {
            timestamp: now_rfc3339(),
            payload: RolloutPayload::SessionMeta(meta),
        };
        let first_line = encode_line(&line).context("encoding session meta")?;
        let (tx, rx) = tokio::sync::mpsc::channel::<WriterMsg>(ROLLOUT_CHANNEL_CAPACITY);
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
        let (tx, rx) = tokio::sync::mpsc::channel::<WriterMsg>(ROLLOUT_CHANNEL_CAPACITY);
        spawn_writer_task(path.to_path_buf(), rx, /*create*/ false, None);
        Ok(Self {
            path: path.to_path_buf(),
            tx,
        })
    }

    /// Append one item. Encodes the line and hands it to the writer task over
    /// a bounded channel (capacity 256). Under backpressure (slow disk / full
    /// disk) this send future yields instead of growing the buffer without
    /// bound — that is the intended fix; callers must `.await` it and must not
    /// assume it is instant. Errors from the actual file write surface in the
    /// writer task (logged), not here.
    pub async fn append(&self, item: &ResponseItem) -> Result<()> {
        let line = RolloutLine {
            timestamp: now_rfc3339(),
            payload: RolloutPayload::ResponseItem(item.clone()),
        };
        let encoded = encode_line(&line).context("encoding response item")?;
        self.tx
            .send(WriterMsg::Line(encoded))
            .await
            .map_err(|_| anyhow::anyhow!("rollout writer task closed"))
    }

    /// Block until every line appended so far has been written and flushed by
    /// the writer task. The agent loop does NOT call this (it tolerates
    /// deferred writes); it exists for tests and explicit shutdown/sync points.
    pub async fn flush(&self) -> Result<()> {
        let (ack_tx, ack_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(WriterMsg::Flush(ack_tx))
            .await
            .map_err(|_| anyhow::anyhow!("rollout writer task closed"))?;
        let _ = ack_rx.await;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Validate every emitted line with the same local envelope used by the
/// reader. The envelope mirrors Codex's `RolloutLine`/`RolloutItem` serde
/// layout, while keeping this crate buildable when the standalone AutoReport
/// repository is checked out without the sibling Codex repository.
fn encode_line(line: &RolloutLine) -> Result<String> {
    let value = serde_json::to_value(line).context("serializing rollout line")?;
    let _: RolloutLine =
        serde_json::from_value(value.clone()).context("validating rollout line")?;
    serde_json::to_string(&value).context("encoding rollout line")
}

/// The dedicated writer task: opens the rollout file once (create+append or
/// append-only) via async `tokio::fs` and processes each message as it arrives
/// on `rx` in FIFO order — `Line` is written+flushed, `Flush` is acknowledged
/// once all prior lines are on disk. Exits when the channel closes (all senders
/// dropped), after draining any buffered lines. Mirrors codex
/// `rollout::recorder` writer task, which deliberately uses `tokio::fs::File`
/// "to keep everything on the async I/O driver instead of blocking the
/// runtime" (codex `rollout/src/recorder.rs:893-895`).
fn spawn_writer_task(
    path: PathBuf,
    mut rx: tokio::sync::mpsc::Receiver<WriterMsg>,
    create: bool,
    first_line: Option<String>,
) {
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut f = match tokio::fs::OpenOptions::new()
            .create(create)
            .append(true)
            .open(&path)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                log::error!("rollout writer: opening {}: {e}", path.display());
                return;
            }
        };
        if let Some(line) = first_line {
            if let Err(e) = write_line(&mut f, &line).await {
                log::error!("rollout writer: writing {}: {e}", path.display());
            }
        }
        while let Some(msg) = rx.recv().await {
            match msg {
                WriterMsg::Line(line) => {
                    if let Err(e) = write_line(&mut f, &line).await {
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
        // Channel closed: all senders gone. Best-effort final flush; the file
        // handle would flush on drop anyway, but doing it explicitly surfaces
        // any trailing I/O error to the log.
        if let Err(e) = f.flush().await {
            log::error!("rollout writer: final flush {}: {e}", path.display());
        }
    });
}

/// Write one pre-encoded JSON line + trailing newline to the rollout file,
/// then flush so each appended item is durable ASAP (a crash mid-session still
/// leaves a replayable file). Bytes are assembled directly and written via
/// `tokio::fs` so nothing blocks a Tokio worker thread. (`writeln!`-style
/// formatting helpers require std I/O and would defeat the point.)
async fn write_line(f: &mut tokio::fs::File, line: &str) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    f.write_all(line.as_bytes()).await?;
    f.write_all(b"\n").await?;
    f.flush().await
}

/// A parsed rollout entry: either the header or an item.
#[derive(Debug)]
pub enum RolloutEntry {
    Meta(SessionMeta),
    Item(ResponseItem),
}

/// Read a current Codex-envelope rollout file back into entries for resume or
/// inspection. Invalid lines are an error: a rollout is an append-only record,
/// so silently skipping malformed data would make resumed context incomplete.
pub fn read(path: &Path) -> Result<Vec<RolloutEntry>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let line: RolloutLine = serde_json::from_str(line).with_context(|| {
            format!("parsing current rollout line {}:{}", path.display(), i + 1)
        })?;
        match line.payload {
            RolloutPayload::SessionMeta(m) => out.push(RolloutEntry::Meta(m)),
            RolloutPayload::ResponseItem(item) => out.push(RolloutEntry::Item(item)),
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
        let rec = RolloutRecorder::create(
            ws,
            "conv-1",
            &new_uuid(),
            "2026-06-29T00-00-00Z",
            ws,
            "main",
        )
        .unwrap();
        rec.append(&ResponseItem::user_message("hello")).await.unwrap();
        rec.append(&ResponseItem::assistant_message("hi there"))
            .await
            .unwrap();
        rec.append(&ResponseItem::function_call(
            "c1",
            "write_file",
            "{\"path\":\"a\"}".into(),
        ))
        .await
        .unwrap();
        rec.append(&ResponseItem::function_call_output("c1", "ok"))
            .await
            .unwrap();
        rec.append(&ResponseItem::reasoning("thinking")).await.unwrap();
        rec.flush().await.unwrap();

        let entries = read(rec.path()).unwrap();
        assert!(matches!(entries[0], RolloutEntry::Meta(_)));
        let items = items(&entries);
        assert_eq!(items.len(), 5);
        assert_eq!(items[0].text().unwrap(), "hello");
        assert!(matches!(items[2], ResponseItem::FunctionCall { .. }));

        // Codex wire envelope: each line carries type + payload markers.
        let raw = std::fs::read_to_string(rec.path()).unwrap();
        assert!(raw.contains("\"type\":\"session_meta\""));
        assert!(raw.contains("\"type\":\"response_item\""));
        assert!(raw.contains("\"type\":\"message\""));
        assert!(raw.contains("\"type\":\"function_call\""));
        assert!(raw.contains("\"conversation_id\""));

        // Reparse every emitted line through the local Codex-shaped envelope;
        // this catches malformed tags and nested payloads without requiring
        // a sibling checkout of Codex in CI.
        for line in raw.lines() {
            let _: RolloutLine = serde_json::from_str(line).expect("rollout line must deserialize");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_rejects_bare_legacy_lines() {
        let dir = std::env::temp_dir().join(format!("rollout-strict-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("legacy.jsonl");
        std::fs::write(
            &path,
            r#"{"conversation_id":"main-old","cli_version":"old","timestamp":"old"}
{"type":"message","role":"user","content":[]}
"#,
        )
        .unwrap();

        assert!(read(&path).is_err());
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
        let r1 = RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-01T00-00-00Z", ws, "main")
            .unwrap();
        let r2 = RolloutRecorder::create(ws, "conv-X", &sid, "2026-06-29T00-00-00Z", ws, "main")
            .unwrap();
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
        let rec = RolloutRecorder::create(
            ws,
            "conv-R",
            &new_uuid(),
            "2026-06-29T00-00-00Z",
            ws,
            "main",
        )
        .unwrap();
        rec.append(&ResponseItem::user_message("first")).await.unwrap();
        rec.flush().await.unwrap();

        // Simulate a restart: reopen the same path and append a new item.
        let path = rec.path().to_path_buf();
        let reopened = RolloutRecorder::open(&path).unwrap();
        reopened
            .append(&ResponseItem::assistant_message("second"))
            .await
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
        let rec = RolloutRecorder::create(
            &dir,
            "conv-C",
            &new_uuid(),
            "2026-06-29T00:00:00Z",
            &dir,
            "main",
        )
        .unwrap();
        rec.append(&ResponseItem::user_message("hi")).await.unwrap();
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
        let ra = RolloutRecorder::create(ws, "conv-A", &sid_a, "2026-06-29T00-00-00Z", ws, "main")
            .unwrap();
        let rb = RolloutRecorder::create(ws, "conv-B", &sid_b, "2026-07-01T00-00-00Z", ws, "main")
            .unwrap();
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

    #[test]
    fn channel_is_bounded_at_codex_capacity() {
        // Regression guard: the recorder must use a *bounded* channel sized to
        // codex's proven capacity (`mpsc::channel::<RolloutCmd>(256)`,
        // codex `rollout/src/recorder.rs:892`). An unbounded channel would let
        // the buffer grow without limit if the writer stalls on slow disk.
        assert_eq!(
            ROLLOUT_CHANNEL_CAPACITY, 256,
            "recorder channel must stay bounded at codex's capacity (256)"
        );
    }

    #[tokio::test]
    async fn bounded_channel_drains_in_order_past_capacity() {
        // Capacity is 256, so appending more items than fit in the channel at
        // once forces the bounded send to yield (backpressure) while the async
        // writer task drains. Every item must still persist exactly once and in
        // FIFO order — no drops, no reordering through the bounded buffer +
        // async tokio::fs writer.
        let dir = std::env::temp_dir().join(format!("rollout-backpressure-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws: &Path = &dir;
        let rec = RolloutRecorder::create(
            ws,
            "conv-BP",
            &new_uuid(),
            "2026-06-29T00-00-00Z",
            ws,
            "main",
        )
        .unwrap();
        let n = 600; // well past the 256-slot buffer
        for i in 0..n {
            rec.append(&ResponseItem::user_message(format!("item-{i}")))
                .await
                .unwrap();
        }
        rec.flush().await.unwrap();

        let items = items(&read(rec.path()).unwrap());
        assert_eq!(items.len(), n, "every item must persist through backpressure");
        for (i, item) in items.iter().enumerate() {
            assert_eq!(
                item.text().unwrap(),
                format!("item-{i}"),
                "ordering broke at index {i}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
