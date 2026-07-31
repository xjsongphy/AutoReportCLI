//! Minimal cargo-mode port of codex's `codex-utils-cargo-bin`.
//!
//! codex's full crate supports both `cargo test` and `bazel test` for locating
//! test resources, pulling in `runfiles` (Bazel) + `assert_cmd`. We don't use
//! Bazel, so this ports only the cargo-mode path of `find_resource!`: resolve a
//! resource path relative to the calling crate's `CARGO_MANIFEST_DIR`. This is
//! exactly the branch codex's macro takes when `runfiles_available()` is false.

use std::path::PathBuf;

/// Resolve a resource path relative to the calling crate's manifest dir.
///
/// Expected to be used exclusively in test/dev code (schema export etc.).
#[macro_export]
macro_rules! find_resource {
    ($resource:expr) => {{
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        Ok::<std::path::PathBuf, std::io::Error>(manifest_dir.join($resource))
    }};
}

/// Stub retained for API compatibility (unused in cargo mode).
pub fn cargo_bin(_name: &str) -> Result<PathBuf, std::io::Error> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "cargo_bin() Bazel/assert_cmd path is not vendored",
    ))
}
