//! Global append-only conversation history, matching Codex's `history.jsonl`.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const MAX_RETRIES: usize = 10;
const RETRY_SLEEP: Duration = Duration::from_millis(100);

#[derive(Serialize)]
struct HistoryEntry<'a> {
    session_id: &'a str,
    ts: u64,
    text: &'a str,
}

/// Append one user-visible message to the global `$AUTOREPORT_HOME/history.jsonl`.
/// Rollouts remain the complete source of truth; this file is the compact,
/// global history index used for quick conversation browsing.
pub async fn append(home: &Path, session_id: &str, text: &str) -> Result<()> {
    tokio::fs::create_dir_all(home)
        .await
        .with_context(|| format!("creating AutoReport home {}", home.display()))?;
    let path = home.join("history.jsonl");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.append(true);
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    options.append(true);
    let file = options
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        let mut permissions = file.metadata()?.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions)?;
        }
    }
    let entry = serde_json::to_string(&HistoryEntry {
        session_id,
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock is before the Unix epoch")?
            .as_secs(),
        text,
    })?;
    let mut line = entry;
    line.push('\n');
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut file = file;
        for _ in 0..MAX_RETRIES {
            match file.try_lock() {
                Ok(()) => {
                    file.seek(SeekFrom::End(0))?;
                    file.write_all(line.as_bytes())?;
                    file.flush()?;
                    return Ok(());
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(RETRY_SLEEP);
                }
                Err(error) => return Err(error.into()),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "could not acquire exclusive history lock after multiple attempts",
        ))
    })
    .await
    .context("history writer task failed")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_codex_history_shape() {
        let dir = tempfile::tempdir().unwrap();
        append(dir.path(), "session-1", "hello\nworld")
            .await
            .unwrap();
        let raw = std::fs::read_to_string(dir.path().join("history.jsonl")).unwrap();
        let value: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["text"], "hello\nworld");
        assert!(value["ts"].as_u64().is_some());
    }
}
