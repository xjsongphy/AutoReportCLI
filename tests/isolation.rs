//! Write-directory isolation: an agent may only write into its assigned folder.

use autoreport_cli::tools::file_tools::{self, FsCtx};
use autoreport_cli::tools::registry::Tool;
use serde_json::json;
use std::path::PathBuf;

fn workspace() -> PathBuf {
    let d = std::env::temp_dir().join(format!("autoreport-iso-{}", stamp()));
    std::fs::create_dir_all(&d).unwrap();
    autoreport_cli::config::ensure_workspace(&d).unwrap();
    d
}

fn stamp() -> String {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_string()
}

#[tokio::test]
async fn data_agent_cannot_write_theory_dir() {
    let ws = workspace();
    // Data analysis agent: write_dir = data/processed.
    let ctx = FsCtx::new(ws.clone(), Some(ws.join("data").join("processed")));
    let write = file_tools::WriteFileTool::new(ctx);

    // Allowed: inside data/processed.
    let ok = write
        .call(&json!({"path": "data/processed/a.csv", "content": "x"}))
        .await;
    assert!(ok.error.is_none(), "write should be allowed: {:?}", ok.error);

    // Blocked: inside theory/.
    let blocked = write
        .call(&json!({"path": "theory/x.md", "content": "nope"}))
        .await;
    assert!(blocked.error.is_some(), "write outside write_dir must be rejected");

    // Blocked: writing into internal metadata.
    let meta = write
        .call(&json!({"path": ".autoreport/secrets.txt", "content": "nope"}))
        .await;
    assert!(meta.error.is_some(), ".autoreport must be non-writable");

    std::fs::remove_dir_all(&ws).ok();
}

#[tokio::test]
async fn path_escape_is_blocked() {
    let ws = workspace();
    let ctx = FsCtx::new(ws.clone(), Some(ws.join("data").join("processed")));
    let write = file_tools::WriteFileTool::new(ctx);
    let escaped = write
        .call(&json!({"path": "../../etc/autoreport-escape", "content": "x"}))
        .await;
    assert!(escaped.error.is_some(), "path traversal must be blocked");
    std::fs::remove_dir_all(&ws).ok();
}
