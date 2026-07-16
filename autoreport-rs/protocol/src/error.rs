//! Error types required by the native sandbox backends.
//!
//! This is the sandbox-relevant subset of Codex's protocol error model. It
//! preserves the upstream `SandboxErr` and the `CodexErr` variants used by the
//! Linux helper and the cross-platform sandbox manager, without importing the
//! unrelated session/authentication error surface.

use std::io;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, CodexErr>;

/// Errors produced while enforcing an operating-system sandbox.
#[derive(Error, Debug)]
pub enum SandboxErr {
    /// Error from Linux seccomp filter setup.
    #[cfg(target_os = "linux")]
    #[error("seccomp setup error")]
    SeccompInstall(#[from] seccompiler::Error),

    /// Error from the Linux seccomp backend.
    #[cfg(target_os = "linux")]
    #[error("seccomp backend error")]
    SeccompBackend(#[from] seccompiler::BackendError),

    /// Landlock could not fully enforce the requested rules.
    #[error("Landlock was not able to fully enforce all sandbox rules")]
    LandlockRestrict,
}

/// Cross-platform errors surfaced by the native sandbox boundary.
#[derive(Debug, Error)]
pub enum CodexErr {
    #[error("{0}")]
    InvalidRequest(String),

    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxErr),

    #[error("autoreport-linux-sandbox was required but not provided")]
    LandlockSandboxExecutableNotProvided,

    #[error("unsupported operation: {0}")]
    UnsupportedOperation(String),

    #[error("Fatal error: {0}")]
    Fatal(String),

    #[error(transparent)]
    Io(#[from] io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockRuleset(#[from] landlock::RulesetError),

    #[cfg(target_os = "linux")]
    #[error(transparent)]
    LandlockPathFd(#[from] landlock::PathFdError),
}
