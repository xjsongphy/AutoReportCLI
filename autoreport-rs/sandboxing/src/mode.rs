//! High-level sandbox integration for the `exec` tool.
//!
//! Maps a coarse [`SandboxMode`] preset to the native AutoReport-derived
//! [`crate::sandboxing::SandboxManager`] request model. Restrictive modes fail
//! closed on platforms without a backend instead of silently running an
//! unrestricted command.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use autoreport_protocol::NetworkSandboxPolicy;
use autoreport_protocol::config_types::WindowsSandboxLevel;
use autoreport_protocol::models::PermissionProfile;
use autoreport_protocol::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath,
};
use autoreport_utils_absolute_path::AbsolutePathBuf;
use autoreport_utils_path_uri::PathUri;

use crate::sandboxing::SandboxCommand;
use crate::sandboxing::SandboxDirectSpawnTransformRequest;
use crate::sandboxing::SandboxManager;
use crate::sandboxing::SandboxTransformRequest;
use crate::sandboxing::SandboxablePreference;
use crate::windows_sandbox::WindowsSandboxProxySettingsMode;

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
) -> FileSystemSandboxPolicy {
    match spec.mode {
        SandboxMode::ReadOnly => FileSystemSandboxPolicy::read_only(),
        SandboxMode::WorkspaceWrite => {
            let mut entries = vec![FileSystemSandboxEntry {
                path: FileSystemPath::Special {
                    value: FileSystemSpecialPath::Root,
                },
                access: FileSystemAccessMode::Read,
            }];
            if let Some(root) = spec.writable_root.as_deref() {
                let root = AbsolutePathBuf::resolve_path_against_base(root, workspace_root);
                entries.push(FileSystemSandboxEntry {
                    path: FileSystemPath::Path { path: root },
                    access: FileSystemAccessMode::Write,
                });
            }
            // Commands commonly need temporary files; these are outside the
            // workspace and are discarded by the Linux backend's tmpfs mount.
            entries.extend([
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::SlashTmp,
                    },
                    access: FileSystemAccessMode::Write,
                },
                FileSystemSandboxEntry {
                    path: FileSystemPath::Special {
                        value: FileSystemSpecialPath::Tmpdir,
                    },
                    access: FileSystemAccessMode::Write,
                },
            ]);
            FileSystemSandboxPolicy::restricted(entries)
        }
        SandboxMode::DangerFullAccess => {
            let _ = workspace_root;
            FileSystemSandboxPolicy::unrestricted()
        }
    }
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
) -> Option<Vec<String>> {
    sandbox_command_argv(command, cwd, spec).ok().flatten()
}

#[cfg(not(target_os = "macos"))]
pub fn seatbelt_command_argv(
    _command: Vec<String>,
    _cwd: &Path,
    _spec: &SandboxSpec,
) -> Option<Vec<String>> {
    None
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

    let file_system_sandbox_policy = build_filesystem_policy(spec, cwd);
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
    let windows_sandbox_level = default_windows_sandbox_level();
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
                autoreport_linux_sandbox_exe: linux_helper.as_deref(),
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

/// Restrictive AutoReport modes use the same unelevated Windows backend that
/// AutoReport selects when Windows sandboxing is enabled. The backend refuses a
/// policy it cannot enforce, so this cannot silently downgrade a restrictive
/// command to direct execution. Elevated setup remains an explicit opt-in.
#[cfg(target_os = "windows")]
const fn default_windows_sandbox_level() -> WindowsSandboxLevel {
    WindowsSandboxLevel::RestrictedToken
}

#[cfg(not(target_os = "windows"))]
const fn default_windows_sandbox_level() -> WindowsSandboxLevel {
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
    use super::WindowsSandboxLevel;
    use super::default_windows_sandbox_level;

    #[cfg(target_os = "windows")]
    #[test]
    fn restrictive_modes_enable_the_windows_backend() {
        assert_eq!(
            default_windows_sandbox_level(),
            WindowsSandboxLevel::RestrictedToken
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_hosts_do_not_select_a_windows_backend() {
        assert_eq!(
            default_windows_sandbox_level(),
            WindowsSandboxLevel::Disabled
        );
    }
}
