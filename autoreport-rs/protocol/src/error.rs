//! `CodexErr` — trimmed adapter.
//!
//! Codex's real `CodexErr` (`codex-rs/protocol/src/error.rs`, ~23 KB) is the
//! crate-wide error enum that pulls in the entire protocol surface (sessions,
//! MCP, approvals, …). The vendored sandbox crate references it in exactly one
//! place — the `impl From<SandboxTransformError> for CodexErr` conversion in
//! [`crate::sandboxing`] — and that conversion only constructs three
//! variants: `InvalidRequest(String)`, `LandlockSandboxExecutableNotProvided`,
//! and `UnsupportedOperation(String)`.
//!
//! Rather than vendor the whole cross-cutting error model, this module provides
//! those three variants with matching names + `Display`/`Error` impls so the
//! verbatim `From` impl compiles unchanged. AutoReportCLI does not surface
//! `CodexErr` itself (it uses `anyhow`); this type exists only as the
//! conversion target the vendored source expects.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodexErr {
    #[error("{0}")]
    InvalidRequest(String),

    #[error("landlock sandbox executable was not provided")]
    LandlockSandboxExecutableNotProvided,

    #[error("{0}")]
    UnsupportedOperation(String),
}
