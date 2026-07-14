//! High-level sandbox integration for the `exec` tool.
//!
//! Maps a coarse [`SandboxMode`] preset (the three codex `PermissionProfile`
//! flavors AutoReportCLI exposes) to codex's split [`FileSystemSandboxPolicy`]
//! and platform launchers. macOS uses the vendored `sandbox-exec` backend;
//! Linux uses Bubblewrap. Restrictive modes fail closed on platforms without
//! a backend instead of silently running an unrestricted command.

use std::path::{Path, PathBuf};

use autoreport_protocol::NetworkSandboxPolicy;
use autoreport_protocol::{
    FileSystemAccessMode, FileSystemPath, FileSystemSandboxEntry, FileSystemSandboxPolicy,
    FileSystemSpecialPath,
};
use autoreport_utils_absolute_path::AbsolutePathBuf;

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

/// Build the codex network policy for a preset.
pub fn build_network_policy(spec: &SandboxSpec) -> NetworkSandboxPolicy {
    if spec.network_enabled {
        NetworkSandboxPolicy::Enabled
    } else {
        NetworkSandboxPolicy::Restricted
    }
}

/// On macOS, return the full `sandbox-exec` argv that runs `command` under the
/// seatbelt policy for `spec`.
#[cfg(target_os = "macos")]
pub fn seatbelt_command_argv(
    command: Vec<String>,
    cwd: &Path,
    spec: &SandboxSpec,
) -> Option<Vec<String>> {
    if matches!(spec.mode, SandboxMode::DangerFullAccess) {
        return None;
    }
    let file_system_sandbox_policy = build_filesystem_policy(spec, cwd);
    let network_sandbox_policy = build_network_policy(spec);
    let params = crate::sandboxing::seatbelt::CreateSeatbeltCommandArgsParams {
        command,
        file_system_sandbox_policy: &file_system_sandbox_policy,
        network_sandbox_policy,
        sandbox_policy_cwd: cwd,
        enforce_managed_network: false,
        managed_network: None,
        environment_id: None,
        network: None,
        extra_allow_unix_sockets: &[],
    };
    let tail = crate::sandboxing::seatbelt::create_seatbelt_command_args(params).ok()?;
    let mut argv = Vec::with_capacity(tail.len() + 1);
    argv.push(crate::sandboxing::seatbelt::MACOS_PATH_TO_SEATBELT_EXECUTABLE.to_string());
    argv.extend(tail);
    Some(argv)
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
/// unrestricted `DangerFullAccess`; restrictive modes either return a launcher
/// or an error, never an unrestricted fallback.
#[allow(clippy::needless_return)]
pub fn sandbox_command_argv(
    command: Vec<String>,
    cwd: &Path,
    spec: &SandboxSpec,
) -> Result<Option<Vec<String>>, String> {
    // Each platform arm lives in its own `#[cfg]` block that `return`s; clippy
    // reads the macOS block as a needless tail-return, but the cfg structure
    // (multiple gated arms + a fallthrough) requires explicit returns.
    if matches!(spec.mode, SandboxMode::DangerFullAccess) {
        return Ok(None);
    }

    #[cfg(target_os = "macos")]
    {
        return seatbelt_command_argv(command, cwd, spec)
            .map(Some)
            .ok_or_else(|| "failed to build the macOS seatbelt sandbox command".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        let bubblewrap = std::process::Command::new("bwrap")
            .arg("--version")
            .output()
            .map_err(|e| format!("failed to locate bubblewrap: {e}"))?;
        if !bubblewrap.status.success() {
            return Err("workspace-write/read-only requires bubblewrap (bwrap) on Linux; install it or select danger-full-access".to_string());
        }
        let mut argv = vec![
            "bwrap".to_string(),
            "--die-with-parent".to_string(),
            "--new-session".to_string(),
            "--ro-bind".to_string(),
            "/".to_string(),
            "/".to_string(),
            "--tmpfs".to_string(),
            "/tmp".to_string(),
        ];
        if matches!(spec.mode, SandboxMode::WorkspaceWrite) {
            if let Some(root) = spec.writable_root.as_deref() {
                let root = root.to_string_lossy().into_owned();
                argv.extend(["--bind".to_string(), root.clone(), root]);
            }
        }
        if !spec.network_enabled {
            argv.push("--unshare-net".to_string());
        }
        argv.extend([
            "--chdir".to_string(),
            cwd.to_string_lossy().into_owned(),
            "--".to_string(),
        ]);
        argv.extend(command);
        return Ok(Some(argv));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (command, cwd, spec);
        Err("workspace-write/read-only sandboxing is not available on this platform; select danger-full-access to run commands".to_string())
    }
}
