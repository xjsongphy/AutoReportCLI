//! High-level sandbox integration for the `exec` tool.
//!
//! Maps a coarse [`SandboxMode`] preset to the native AutoReport-derived
//! [`crate::SandboxManager`] request model. Restrictive modes fail
//! closed on platforms without a backend instead of silently running an
//! unrestricted command.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use autoreport_codex_protocol::permissions::NetworkSandboxPolicy;
use autoreport_codex_protocol::config_types::WindowsSandboxLevel;
use autoreport_codex_protocol::models::PermissionProfile;
use autoreport_codex_protocol::permissions::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath,
};
use autoreport_utils_absolute_path::AbsolutePathBuf;
use autoreport_utils_path_uri::PathUri;

use crate::SandboxCommand;
use crate::SandboxDirectSpawnTransformRequest;
use crate::SandboxManager;
use crate::SandboxTransformRequest;
use crate::SandboxablePreference;
use crate::WindowsSandboxProxySettingsMode;

/// Coarse sandbox preset, mirroring the codex `PermissionProfile` flavors that
/// matter for an unattended report-writing CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Read the whole disk; write only the assigned agent directory (+ tmp).
    /// This is the default.
    #[default]
    WorkspaceWrite,
    /// Read the whole disk; no writes at all.
    ReadOnly,
    /// No filesystem restrictions. Disables the OS sandbox entirely.
    DangerFullAccess,
}

impl SandboxMode {
    pub fn from_kebab(s: &str) -> Option<Self> {
        match s {
            "workspace-write" | "workspace_write" => Some(Self::WorkspaceWrite),
            "read-only" | "read_only" => Some(Self::ReadOnly),
            "danger-full-access" | "full-access" | "unrestricted" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
    pub fn as_kebab(self) -> &'static str {
        match self {
            Self::WorkspaceWrite => "workspace-write",
            Self::ReadOnly => "read-only",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

/// Resolved sandbox configuration for one command launch.
#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub mode: SandboxMode,
    /// Whether outbound network access is allowed inside the sandbox.
    pub network_enabled: bool,
    /// The sole workspace subtree this command may modify. It is filled from
    /// the agent's `FsCtx` when the exec tool is built.
    pub writable_root: Option<PathBuf>,
}

impl SandboxSpec {
    pub fn new(mode: SandboxMode, network_enabled: bool) -> Self {
        Self {
            mode,
            network_enabled,
            writable_root: None,
        }
    }

    pub fn with_writable_root(mut self, writable_root: Option<&Path>) -> Self {
        self.writable_root = writable_root.map(Path::to_path_buf);
        self
    }
}

/// Build the filesystem policy for a workspace root + preset. Unlike codex's
/// broad `WorkspaceWrite` preset, this grants writes only to the agent's own
/// output directory, keeping `.autoreport`, other agent directories, and the
/// rest of the workspace protected before the command runs.
pub fn build_filesystem_policy(
    spec: &SandboxSpec,
    workspace_root: &Path,
) -> Result<FileSystemSandboxPolicy, String> {
    match spec.mode {
        SandboxMode::ReadOnly => Ok(FileSystemSandboxPolicy::read_only()),
        SandboxMode::WorkspaceWrite => {
            let mut entries = vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
                missing_path_behavior: None,            }];
            if let Some(root) = spec.writable_root.as_deref() {
                let root = resolve_agent_writable_root(root, workspace_root)?;
                entries.push(FileSystemSandboxEntry {
                    path: FileSystemPath::Path { path: root },
                    access: FileSystemAccessMode::Write,
                    missing_path_behavior: None,                });
            }
            // Commands commonly need temporary files; these are outside the
            // workspace and are discarded by the Linux backend's tmpfs mount.
            // A workspace placed under /tmp or TMPDIR is different: granting
            // its parent temporary root would silently make sibling projects
            // writable, so the workspace write root is the only temp write
            // authority in that case.
            if !workspace_is_inside_temporary_root(workspace_root) {
                entries.extend([
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::SlashTmp,
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,                    },
                    FileSystemSandboxEntry {
                        path: FileSystemPath::Special {
                            value: FileSystemSpecialPath::Tmpdir,
                        },
                        access: FileSystemAccessMode::Write,
                        missing_path_behavior: None,                    },
                ]);
            }
            Ok(FileSystemSandboxPolicy::restricted(entries))
        }
        SandboxMode::DangerFullAccess => {
            let _ = workspace_root;
            Ok(FileSystemSandboxPolicy::unrestricted())
        }
    }
}

/// Resolve and validate the one writable directory assigned to an agent.
///
/// This is deliberately stricter than a lexical `starts_with`: a writable
/// symlink such as `workspace/agent -> /outside` must not turn an apparently
/// workspace-scoped policy into a grant for the symlink target.
fn resolve_agent_writable_root(
    writable_root: &Path,
    workspace_root: &Path,
) -> Result<AbsolutePathBuf, String> {
    let workspace = canonicalize_path_with_missing_tail(workspace_root)?;
    let root = AbsolutePathBuf::resolve_path_against_base(writable_root, workspace_root);
    let root_canonical = canonicalize_path_with_missing_tail(root.as_path())?;
    if root_canonical == workspace || !root_canonical.starts_with(&workspace) {
        return Err(format!(
            "agent writable root '{}' must be a strict descendant of workspace '{}'",
            writable_root.display(),
            workspace_root.display()
        ));
    }
    // Keep the logical path in the policy. The platform backends already
    // normalize aliases and preserve nested symlinks where required; using
    // the canonical form here would make macOS `/var` versus `/private/var`
    // callers fail policy matching. Canonical paths are used only to validate
    // containment above.
    Ok(root)
}

/// Canonicalize as much of a path as exists, retaining a missing tail. This
/// catches symlink escapes in existing ancestors while still allowing a tool
/// to create its assigned output directory on first use.
fn canonicalize_path_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let path = AbsolutePathBuf::from_absolute_path(path)
        .map_err(|err| format!("path is invalid: {err}"))?
        .into_path_buf();
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            format!(
                "could not resolve an existing ancestor for '{}'",
                path.display()
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            format!(
                "could not resolve an existing ancestor for '{}'",
                path.display()
            )
        })?;
    }
    let mut canonical = dunce::canonicalize(existing)
        .map_err(|err| format!("could not canonicalize '{}': {err}", existing.display()))?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

fn workspace_is_inside_temporary_root(workspace_root: &Path) -> bool {
    let workspace_root =
        dunce::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    [
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        std::env::temp_dir(),
    ]
    .into_iter()
    .map(|root| dunce::canonicalize(&root).unwrap_or(root))
    .any(|root| workspace_root.starts_with(root))
}

/// Build the network policy for a preset.
pub fn build_network_policy(spec: &SandboxSpec) -> NetworkSandboxPolicy {
    if spec.network_enabled {
        NetworkSandboxPolicy::Enabled
    } else {
        NetworkSandboxPolicy::Restricted
    }
}

/// On macOS, return the full `sandbox-exec` argv that runs `command` under the
/// manager-produced seatbelt policy for `spec`.
#[cfg(target_os = "macos")]
pub fn seatbelt_command_argv(
    command: Vec<String>,
    cwd: &Path,
    spec: &SandboxSpec,
) -> Result<Option<Vec<String>>, String> {
    sandbox_command_argv(command, cwd, spec)
}

#[cfg(not(target_os = "macos"))]
pub fn seatbelt_command_argv(
    _command: Vec<String>,
    _cwd: &Path,
    _spec: &SandboxSpec,
) -> Result<Option<Vec<String>>, String> {
    Ok(None)
}

/// Return a platform sandbox launcher for `command`. `None` means explicitly
/// unrestricted `DangerFullAccess`; restrictive modes are constructed by the
/// shared `SandboxManager`, never by a hand-written platform argv builder.
pub fn sandbox_command_argv(
    command: Vec<String>,
    cwd: &Path,
    spec: &SandboxSpec,
) -> Result<Option<Vec<String>>, String> {
    if matches!(spec.mode, SandboxMode::DangerFullAccess) {
        return Ok(None);
    }

    let file_system_sandbox_policy = build_filesystem_policy(spec, cwd)?;
    let network_sandbox_policy = build_network_policy(spec);
    let permission_profile = PermissionProfile::from_runtime_permissions(
        &file_system_sandbox_policy,
        network_sandbox_policy,
    );
    let cwd_uri = PathUri::from_host_native_path(cwd)
        .map_err(|err| format!("sandbox cwd is invalid: {err}"))?;
    let workspace_root = AbsolutePathBuf::from_absolute_path_checked(cwd)
        .map_err(|err| format!("sandbox workspace root is invalid: {err}"))?;
    let (program, args) = command
        .split_first()
        .ok_or_else(|| "cannot sandbox an empty command".to_string())?;
    let manager = SandboxManager::new();
    let windows_sandbox_level = default_windows_sandbox_level(spec.network_enabled);
    let sandbox = manager.select_initial(
        &file_system_sandbox_policy,
        network_sandbox_policy,
        SandboxablePreference::Require,
        windows_sandbox_level,
        false,
    );
    let linux_helper = resolve_linux_sandbox_executable()?;
    let transformed = manager
        .transform_for_direct_spawn(SandboxDirectSpawnTransformRequest {
            transform: SandboxTransformRequest {
                command: SandboxCommand {
                    program: OsString::from(program),
                    args: args.to_vec(),
                    cwd: cwd_uri.clone(),
                    env: HashMap::new(),
                    managed_network: None,
                    additional_permissions: None,
                },
                permissions: &permission_profile,
                sandbox,
                enforce_managed_network: false,
                environment_id: None,
                network: None,
                sandbox_policy_cwd: &cwd_uri,
                codex_linux_sandbox_exe: linux_helper.as_deref(),
                use_legacy_landlock: false,
                windows_sandbox_level,
                windows_sandbox_private_desktop: false,
            },
            workspace_roots: std::slice::from_ref(&workspace_root),
            windows_sandbox_proxy_settings_mode: WindowsSandboxProxySettingsMode::Reconcile,
        })
        .map_err(|err| format!("failed to prepare sandbox: {err}"))?;
    Ok(Some(transformed.command))
}

/// Restrictive AutoReport modes use the native Windows backends instead of a
/// direct spawn. The restricted-token backend is sufficient when network is
/// enabled. Offline commands require the elevated backend: it provisions the
/// offline WFP identity that blocks outbound traffic. Both paths refuse a
/// policy they cannot enforce, so restrictive commands never downgrade to
/// direct execution.
#[cfg(target_os = "windows")]
const fn default_windows_sandbox_level(network_enabled: bool) -> WindowsSandboxLevel {
    if network_enabled {
        WindowsSandboxLevel::RestrictedToken
    } else {
        WindowsSandboxLevel::Elevated
    }
}

#[cfg(not(target_os = "windows"))]
const fn default_windows_sandbox_level(_network_enabled: bool) -> WindowsSandboxLevel {
    WindowsSandboxLevel::Disabled
}

#[cfg(target_os = "linux")]
fn resolve_linux_sandbox_executable() -> Result<Option<PathBuf>, String> {
    let helper = std::env::var_os("AUTOREPORT_LINUX_SANDBOX_EXE")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(|dir| dir.join("autoreport-linux-sandbox")))
        })
        .ok_or_else(|| "could not resolve the autoreport-linux-sandbox helper".to_string())?;
    if helper.is_file() {
        Ok(Some(helper))
    } else {
        Err(format!(
            "missing autoreport-linux-sandbox helper at {}; reinstall the matching AutoReport package or set AUTOREPORT_LINUX_SANDBOX_EXE",
            helper.display()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn resolve_linux_sandbox_executable() -> Result<Option<PathBuf>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::SandboxMode;
    use super::SandboxSpec;
    use super::WindowsSandboxLevel;
    use super::build_filesystem_policy;
    use super::build_network_policy;
    use super::default_windows_sandbox_level;
    use autoreport_codex_protocol::permissions::NetworkSandboxPolicy;

    #[test]
    fn workspace_write_preserves_metadata_protection() {
        let workspace = tempfile::tempdir().expect("workspace");
        let agent_root = workspace.path().join("agent");
        let spec = SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(&agent_root));
        let policy = build_filesystem_policy(&spec, workspace.path()).expect("policy");

        assert!(policy.can_write_path_with_cwd(&agent_root.join("report.md"), workspace.path()));
        for protected_path in [".git/config", ".agents/state", ".autoreport/config.toml"] {
            assert!(
                !policy.can_write_path_with_cwd(
                    &workspace.path().join(protected_path),
                    workspace.path()
                ),
                "workspace-write must protect {protected_path}"
            );
        }
    }

    #[test]
    fn sandbox_spec_maps_network_and_full_access_without_downgrade() {
        let workspace = tempfile::tempdir().expect("workspace");
        let offline = SandboxSpec::new(SandboxMode::ReadOnly, false);
        assert_eq!(
            build_network_policy(&offline),
            NetworkSandboxPolicy::Restricted
        );

        let full_access = SandboxSpec::new(SandboxMode::DangerFullAccess, true);
        assert_eq!(
            build_network_policy(&full_access),
            NetworkSandboxPolicy::Enabled
        );
        let policy = build_filesystem_policy(&full_access, workspace.path()).expect("policy");
        assert!(
            policy.can_write_path_with_cwd(
                &workspace.path().join("unrestricted.txt"),
                workspace.path()
            )
        );
    }

    #[test]
    fn temporary_workspaces_do_not_gain_writes_to_their_temp_parent() {
        let workspace = tempfile::tempdir().expect("workspace");
        let agent_root = workspace.path().join("agent");
        std::fs::create_dir_all(&agent_root).expect("agent root");
        let spec = SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(&agent_root));
        let policy = build_filesystem_policy(&spec, workspace.path()).expect("policy");

        assert!(policy.can_write_path_with_cwd(&agent_root.join("report.md"), workspace.path()));
        assert!(
            !policy.can_write_path_with_cwd(&workspace.path().join("sibling.md"), workspace.path()),
            "a workspace under the system temporary directory must not inherit write access to its temp parent"
        );
    }

    // `Tmpdir` resolves from TMPDIR, which is intentionally absent from the
    // Windows policy evaluator. The Windows backend applies temporary-file
    // access separately, so this direct path assertion is Unix-specific.
    #[cfg(unix)]
    #[test]
    fn regular_workspaces_keep_temporary_file_write_access() {
        let workspace = tempfile::tempdir_in(std::env::current_dir().expect("current directory"))
            .expect("workspace");
        let agent_root = workspace.path().join("agent");
        std::fs::create_dir_all(&agent_root).expect("agent root");
        let spec = SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(&agent_root));
        let policy = build_filesystem_policy(&spec, workspace.path()).expect("policy");

        assert!(policy.can_write_path_with_cwd(
            &std::env::temp_dir().join("autoreport-sandbox-temp-probe"),
            workspace.path()
        ));
    }

    #[test]
    fn workspace_write_rejects_external_writable_root() {
        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let spec = SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(outside.path()));

        let err = build_filesystem_policy(&spec, workspace.path()).expect_err("outside root");
        assert!(err.contains("strict descendant"), "unexpected error: {err}");
    }

    #[cfg(unix)]
    #[test]
    fn workspace_write_rejects_writable_root_symlink_escape() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        let agent_link = workspace.path().join("agent");
        symlink(outside.path(), &agent_link).expect("agent symlink");
        let spec = SandboxSpec::new(SandboxMode::WorkspaceWrite, false)
            .with_writable_root(Some(&agent_link));

        let err = build_filesystem_policy(&spec, workspace.path()).expect_err("symlink escape");
        assert!(err.contains("strict descendant"), "unexpected error: {err}");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn network_enabled_modes_use_the_unelevated_windows_backend() {
        assert_eq!(
            default_windows_sandbox_level(true),
            WindowsSandboxLevel::RestrictedToken
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn offline_modes_use_the_wfp_enforcing_windows_backend() {
        assert_eq!(
            default_windows_sandbox_level(false),
            WindowsSandboxLevel::Elevated
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_hosts_do_not_select_a_windows_backend() {
        assert_eq!(
            default_windows_sandbox_level(false),
            WindowsSandboxLevel::Disabled
        );
    }
}
