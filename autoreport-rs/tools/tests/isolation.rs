//! Write-directory isolation: an agent may only write into its assigned folder.

use autoreport_tools::apply_patch::ApplyPatchTool;
use autoreport_tools::exec_tool::ExecTool;
use autoreport_tools::file_tools::{FsCtx, resolve_within};
use autoreport_tools::registry::{Tool, ToolExecutionContext};
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
        blocked.error.is_none(),
        "the sandboxed command should launch: {:?}",
        blocked.error
    );
    assert_ne!(blocked.result["returncode"].as_i64(), Some(0));
    assert!(
        !ws.join("Theory").join("nope.txt").exists(),
        "the OS sandbox must prevent writes outside the agent directory"
    );

    std::fs::remove_dir_all(&ws).ok();
}

#[cfg(unix)]
#[tokio::test]
async fn apply_patch_rejects_symlink_write_escape() {
    use std::os::unix::fs::symlink;

    let ws = workspace();
    let outside = tempfile::tempdir().unwrap();
    let link = ws.join("Data").join("Processed").join("outside");
    symlink(outside.path(), &link).unwrap();

    let patch = ApplyPatchTool::new(FsCtx::new(
        ws.clone(),
        Some(ws.join("Data").join("Processed")),
    ));
    let result = patch
        .call(&json!({
            "patch": "*** Begin Patch\n*** Add File: Data/Processed/outside/escape.txt\n+nope\n*** End Patch\n"
        }))
        .await;

    assert!(result.error.is_some(), "symlink escape must be rejected");
    assert!(!outside.path().join("escape.txt").exists());
    std::fs::remove_dir_all(&ws).ok();
}

#[tokio::test]
async fn escalation_requires_runtime_only_execution_context() {
    let ws = workspace();
    let ctx = FsCtx::new(ws.clone(), Some(ws.join("Data").join("Processed")));
    let exec = ExecTool::new(ctx, 10);
    let args = json!({
        "command": "touch Theory/escalated.txt",
        "sandbox_permissions": "require_escalated",
        "justification": "write a shared report artifact"
    });

    let forged = exec.call(&args).await;
    assert!(
        forged.error.is_some(),
        "model arguments cannot self-escalate"
    );
    assert!(!ws.join("Theory").join("escalated.txt").exists());

    let approved = exec
        .call_with_context(
            &args,
            ToolExecutionContext {
                allow_escalated_exec: true,
            },
        )
        .await;
    assert!(
        approved.error.is_none(),
        "approved escalation: {approved:?}"
    );
    assert_eq!(
        approved.result["sandbox"].as_str(),
        Some("danger-full-access")
    );
    assert!(ws.join("Theory").join("escalated.txt").is_file());

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
