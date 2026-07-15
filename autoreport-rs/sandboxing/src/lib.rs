//! Cross-system command sandboxing, vendored from `codex-rs`.
//!
//! This module brings in codex's real sandbox sources — the split filesystem
//! policy model (`codex-protocol::permissions`), the cross-platform
//! [`sandboxing`] crate (macOS seatbelt, Linux bwrap/landlock, Windows
//! backends + the `SandboxManager`), and the supporting `AbsolutePathBuf` /
//! `PathUri` / `home_dir` / `windows_sandbox` utility crates — compiled as
//! local submodules so the codex source files stay byte-for-byte faithful
//! (only `use codex_*` paths are rewritten to `crate::*`).
//!
//! See [`sandboxing`] for the backend status and [`mode`] for the high-level
//! integration used by the `exec` tool.

// The following modules are vendored verbatim from codex-rs (only `use codex_*`
// paths rewritten). Silence clippy on upstream source rather than diverge from
// the originals.
#[allow(clippy::all)]
pub mod home_dir;
pub mod mode;
pub mod network_proxy;
#[allow(clippy::all)]
pub mod sandboxing;
#[allow(clippy::all)]
pub mod windows_sandbox;

pub use mode::SandboxMode;
pub use mode::SandboxSpec;
pub use mode::sandbox_command_argv;
pub use mode::seatbelt_command_argv;
#[cfg(target_os = "linux")]
pub use sandboxing::find_system_bwrap_in_path;
pub use sandboxing::landlock;
