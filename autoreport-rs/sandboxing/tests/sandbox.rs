//! Integration tests for the vendored codex sandbox (macOS seatbelt path).
//!
//! These exercise the real codex policy resolver (`FileSystemSandboxPolicy`)
//! and seatbelt command builder end-to-end, without invoking `sandbox-exec`.

#![cfg(target_os = "macos")]

use std::path::Path;

use autoreport_protocol::FileSystemAccessMode;
use autoreport_protocol::FileSystemSandboxPolicy;
use autoreport_protocol::NetworkSandboxPolicy;
use autoreport_sandboxing::SandboxMode;
use autoreport_sandboxing::SandboxSpec;
use autoreport_sandboxing::mode::build_filesystem_policy;
use autoreport_sandboxing::network_proxy::ManagedNetworkSandboxContext;
use autoreport_sandboxing::sandboxing::seatbelt::CreateSeatbeltCommandArgsParams;
use autoreport_sandboxing::sandboxing::seatbelt::MACOS_PATH_TO_SEATBELT_EXECUTABLE;
use autoreport_sandboxing::sandboxing::seatbelt::create_seatbelt_command_args;
use autoreport_sandboxing::seatbelt_command_argv;

fn argv_for(mode: SandboxMode, cwd: &Path, command: &[&str]) -> Vec<String> {
    let spec = SandboxSpec::new(mode, false).with_writable_root(Some(&cwd.join("Outline")));
    seatbelt_command_argv(command.iter().map(|s| s.to_string()).collect(), cwd, &spec)
        .expect("seatbelt argv should be produced on macOS for non-full-access modes")
}

#[test]
fn workspace_write_wraps_with_seatbelt_exec() {
    let cwd = std::env::current_dir().unwrap();
    let argv = argv_for(
        SandboxMode::WorkspaceWrite,
        &cwd,
        &["/bin/sh", "-lc", "echo hi"],
    );

    // argv[0] is the seatbelt launcher; the codex builder produces the rest.
    assert_eq!(argv[0], MACOS_PATH_TO_SEATBELT_EXECUTABLE);
    assert_eq!(argv[1], "-p");
    // The original command must appear verbatim after `--`.
    let sep = argv.iter().position(|a| a == "--").unwrap();
    assert_eq!(&argv[sep + 1..], &["/bin/sh", "-lc", "echo hi"]);
}

#[test]
fn workspace_write_grants_only_agent_root_write_param() {
    let cwd = std::env::current_dir().unwrap();
    let argv = argv_for(
        SandboxMode::WorkspaceWrite,
        &cwd,
        &["/bin/sh", "-lc", "true"],
    );

    // The agent directory must be bound as a writable root via a `-D WRITABLE_ROOT_0=<path>`
    // definition so the seatbelt `file-write*` allow rule can reference it.
    let agent_root = cwd.join("Outline").to_string_lossy().to_string();
    let has_writable_root_def = argv
        .iter()
        .any(|a| a.starts_with("-DWRITABLE_ROOT_0=") && a.ends_with(&format!("={agent_root}")));
    assert!(
        has_writable_root_def,
        "expected a -DWRITABLE_ROOT_0=<agent-root> definition; got {argv:?}"
    );
}

#[test]
fn read_only_policy_denies_all_writes() {
    let cwd = std::env::current_dir().unwrap();
    let policy = FileSystemSandboxPolicy::read_only();
    assert!(policy.has_full_disk_read_access());
    assert!(!policy.has_full_disk_write_access());
    // Writes inside the workspace are denied under read-only.
    assert!(!policy.can_write_path_with_cwd(&cwd.join("out.txt"), &cwd));
}

#[test]
fn workspace_write_protected_metadata_is_read_only() {
    let cwd = std::env::current_dir().unwrap();
    let policy = build_filesystem_policy(
        &SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(&cwd.join("Outline"))),
        &cwd,
    );

    assert!(policy.can_write_path_with_cwd(&cwd.join("Outline/outline.txt"), &cwd));
    assert!(!policy.can_write_path_with_cwd(&cwd.join("Theory/report.md"), &cwd));
    assert!(!policy.can_write_path_with_cwd(&cwd.join(".git/config"), &cwd));
    assert!(!policy.can_write_path_with_cwd(&cwd.join(".autoreport/sessions/x.jsonl"), &cwd));
}

#[test]
fn resolve_access_uses_most_specific_entry() {
    // Faithfulness check: the codex precedence algorithm — longest prefix wins,
    // deny beats write beats read — survives the vendoring intact.
    use autoreport_protocol::FileSystemPath;
    use autoreport_protocol::FileSystemSandboxEntry;
    use autoreport_protocol::FileSystemSpecialPath;
    use autoreport_utils_absolute_path::AbsolutePathBuf;

    let cwd = std::env::current_dir().unwrap();
    let docs = AbsolutePathBuf::resolve_path_against_base("docs", &cwd);
    let docs_private = AbsolutePathBuf::resolve_path_against_base("docs/private", &cwd);
    let policy = FileSystemSandboxPolicy::restricted(vec![
        FileSystemSandboxEntry {
            path: FileSystemPath::Special {
                value: FileSystemSpecialPath::project_roots(None),
            },
            access: FileSystemAccessMode::Write,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs },
            access: FileSystemAccessMode::Read,
        },
        FileSystemSandboxEntry {
            path: FileSystemPath::Path { path: docs_private },
            access: FileSystemAccessMode::Deny,
        },
    ]);

    assert_eq!(
        policy.resolve_access_with_cwd(&cwd, &cwd),
        FileSystemAccessMode::Write
    );
    // docs/ inherits Read, docs/private is Deny (deny > write).
    let docs_path = AbsolutePathBuf::resolve_path_against_base("docs", &cwd);
    let private_path = AbsolutePathBuf::resolve_path_against_base("docs/private", &cwd);
    assert_eq!(
        policy.resolve_access_with_cwd(docs_path.as_path(), &cwd),
        FileSystemAccessMode::Read
    );
    assert_eq!(
        policy.resolve_access_with_cwd(private_path.as_path(), &cwd),
        FileSystemAccessMode::Deny
    );
}

#[test]
fn managed_network_context_serializes() {
    // The vendored ManagedNetworkSandboxContext stays serde-compatible with codex.
    let ctx = ManagedNetworkSandboxContext {
        loopback_ports: vec![8080],
        allow_local_binding: true,
    };
    let json = serde_json::to_string(&ctx).unwrap();
    assert!(json.contains("\"loopbackPorts\":[8080]"));
    assert!(json.contains("\"allowLocalBinding\":true"));
}

#[test]
fn network_disabled_when_not_enabled() {
    let cwd = std::env::current_dir().unwrap();
    let policy = FileSystemSandboxPolicy::workspace_write(&[], false, false);
    let params = CreateSeatbeltCommandArgsParams {
        command: vec!["/bin/sh".to_string(), "-lc".to_string(), "true".to_string()],
        file_system_sandbox_policy: &policy,
        network_sandbox_policy: NetworkSandboxPolicy::Restricted,
        sandbox_policy_cwd: &cwd,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    };
    let argv = create_seatbelt_command_args(params).unwrap();
    let policy_blob_idx = argv.iter().position(|a| a == "-p").unwrap();
    let policy_blob = &argv[policy_blob_idx + 1];
    // Network is Restricted and no proxy is configured: no blanket
    // `(allow network-outbound)` rule is emitted.
    assert!(
        !policy_blob.contains("(allow network-outbound)\n(allow network-inbound)"),
        "network should be denied, but full-network allow was emitted"
    );
}
