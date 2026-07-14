//! Write-directory isolation: an agent may only write into its assigned folder.

use autoreport_tools::apply_patch::ApplyPatchTool;
use autoreport_tools::exec_tool::ExecTool;
use autoreport_tools::file_tools::{FsCtx, resolve_within};
use autoreport_tools::registry::Tool;
use serde_json::json;
use std::path::PathBuf;

fn workspace() -> PathBuf {
    let d = std::env::temp_dir().join(format!("autoreport-iso-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    autoreport_core::config::ensure_workspace(&d).unwrap();
    d
}

#[tokio::test]
async fn data_agent_cannot_write_theory_dir() {
    let ws = workspace();
    let ctx = FsCtx::new(ws.clone(), Some(ws.join("Data").join("Processed")));
    let patch = ApplyPatchTool::new(ctx);

    let ok = patch
        .call(&json!({"patch": "*** Begin Patch\n*** Add File: Data/Processed/a.csv\n+x\n*** End Patch\n"}))
        .await;
    assert!(
        ok.error.is_none(),
        "patch should be allowed: {:?}",
        ok.error
    );

    let blocked = patch
        .call(
            &json!({"patch": "*** Begin Patch\n*** Add File: Theory/x.md\n+nope\n*** End Patch\n"}),
        )
        .await;
    assert!(
        blocked.error.is_some(),
        "patch outside write_dir must be rejected"
    );

    let meta = patch
        .call(&json!({"patch": "*** Begin Patch\n*** Add File: .autoreport/secrets.txt\n+nope\n*** End Patch\n"}))
        .await;
    assert!(meta.error.is_some(), ".autoreport must be non-writable");

    std::fs::remove_dir_all(&ws).ok();
}

#[tokio::test]
async fn path_escape_is_blocked() {
    let ws = workspace();
    let escaped = resolve_within("../../etc/autoreport-escape", &ws);
    assert!(escaped.is_err(), "path traversal must be blocked");
    std::fs::remove_dir_all(&ws).ok();
}

#[tokio::test]
async fn exec_respects_write_dir() {
    let ws = workspace();
    let ctx = FsCtx::new(ws.clone(), Some(ws.join("Data").join("Processed")));
    let exec = ExecTool::new(ctx, 10);

    let ok = exec
        .call(&json!({"command": "touch Data/Processed/ok.txt"}))
        .await;
    assert!(
        ok.error.is_none(),
        "exec write in write_dir should pass: {:?}",
        ok.error
    );

    let blocked = exec
        .call(&json!({"command": "touch Theory/nope.txt"}))
        .await;
    assert!(
        blocked.error.is_some(),
        "exec write outside write_dir must be rejected"
    );

    std::fs::remove_dir_all(&ws).ok();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn exec_uses_seatbelt_to_block_undeclared_writes() {
    let ws = workspace();
    let agent_root = ws.join("Data").join("Processed");
    let ctx = FsCtx::new(ws.clone(), Some(agent_root.clone()));
    let exec = ExecTool::new(ctx, 10).with_sandbox(
        autoreport_sandboxing::SandboxSpec::new(
            autoreport_sandboxing::SandboxMode::WorkspaceWrite,
            false,
        )
        .with_writable_root(Some(&agent_root)),
    );

    let output = exec
        .call(&json!({
            "command": "python3 -c \"from pathlib import Path; Path('Data/Processed/sandboxed.txt').touch(); Path('Theory/seatbelt-blocked.txt').touch()\""
        }))
        .await;

    assert!(
        output.error.is_none(),
        "exec should launch: {:?}",
        output.error
    );
    assert_eq!(
        output.result["returncode"].as_i64(),
        Some(1),
        "the second touch must be denied by Seatbelt: {}",
        output.result
    );
    assert!(agent_root.join("sandboxed.txt").is_file());
    assert!(
        !ws.join("Theory").join("seatbelt-blocked.txt").exists(),
        "the OS sandbox must prevent the undeclared write"
    );

    std::fs::remove_dir_all(&ws).ok();
}
