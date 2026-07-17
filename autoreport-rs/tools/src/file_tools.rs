//! Shared filesystem guards used by shell/apply_patch.
//!
//! The model no longer receives first-class read/write/edit/delete tools.
//! Instead, file access flows through `exec` and `apply_patch`,
//! all of which rely on this module for workspace scoping and per-agent write
//! isolation.

use std::path::{Component, Path, PathBuf};

#[derive(Clone)]
pub struct FsCtx {
    pub workspace: PathBuf,
    pub write_dir: Option<PathBuf>,
}

impl FsCtx {
    pub fn new(workspace: PathBuf, write_dir: Option<PathBuf>) -> Self {
        Self {
            workspace,
            write_dir,
        }
    }

    pub fn allowed_write_dir(&self) -> Option<&Path> {
        self.write_dir.as_deref()
    }

    pub fn assert_write_allowed(&self, target: &Path) -> Result<(), String> {
        let canonical_workspace = canonicalize_with_missing_tail(&self.workspace)?;
        let canonical_target = canonicalize_with_missing_tail(target)?;
        let metadata = canonicalize_with_missing_tail(&self.workspace.join(".autoreport"))?;
        if path_eq(&canonical_target, &metadata) || path_starts_with(&canonical_target, &metadata) {
            return Err("writing inside .autoreport is not permitted".to_string());
        }
        match &self.write_dir {
            Some(dir) => {
                let canonical_dir = canonicalize_with_missing_tail(dir)?;
                if !path_starts_with(&canonical_dir, &canonical_workspace) {
                    return Err(format!(
                        "agent write directory '{}' is outside the workspace",
                        dir.display()
                    ));
                }
                if path_starts_with(&canonical_target, &canonical_dir) {
                    Ok(())
                } else {
                    Err(format!(
                        "this agent may only write under {}; '{}' is outside it",
                        dir.display(),
                        target.display()
                    ))
                }
            }
            None => Err("this agent has no write access".to_string()),
        }
    }
}

/// Canonicalize every existing component while preserving a missing tail.
/// This makes write checks reject symlinked ancestors without requiring the
/// target file to exist yet.
fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let mut missing = Vec::new();
    let mut existing = path;
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = existing.file_name() else {
                    return Err(format!("cannot resolve write path '{}'", path.display()));
                };
                missing.push(name.to_os_string());
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("cannot resolve write path '{}'", path.display()))?;
            }
            Err(err) => {
                return Err(format!(
                    "cannot resolve write path '{}': {err}",
                    path.display()
                ));
            }
        }
    }

    let mut canonical = std::fs::canonicalize(existing)
        .map_err(|err| format!("resolving write path '{}': {err}", path.display()))?;
    for component in missing.iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

/// Resolve a possibly-relative path against the workspace and verify the
/// result stays inside the workspace. `..` components are collapsed lexically
/// (no symlink following) so paths never escape the project root.
pub fn resolve_within(path: &str, workspace: &Path) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let joined = if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&workspace.join(p))
    };
    if path_eq(&joined, workspace) || path_starts_with(&joined, workspace) {
        Ok(joined)
    } else {
        Err(format!("path '{}' escapes the workspace", path))
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Compare two path components with the filesystem's case sensitivity:
/// case-insensitive on Windows (NTFS default), case-sensitive elsewhere.
/// `Path::starts_with` / `==` are always case-sensitive and would falsely
/// reject an agent that writes `Data/Processed/x.csv` against a canonical
/// `Data/Processed` write dir on Windows.
fn component_eq(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    // Case-insensitive on filesystems whose default comparison is
    // case-insensitive: Windows (NTFS) and macOS (APFS default). Linux stays
    // case-sensitive. Without this, a write to `Data/Processed/...` is
    // rejected as outside a `data/processed` write-dir on macOS even though
    // they are the same path on disk.
    #[cfg(any(windows, target_os = "macos"))]
    {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        a == b
    }
}

/// `target == base`, filesystem-case-aware.
fn path_eq(target: &Path, base: &Path) -> bool {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    t.len() == b.len()
        && t.iter()
            .zip(b.iter())
            .all(|(tc, bc)| component_eq(tc.as_os_str(), bc.as_os_str()))
}

/// `target` is `base` or nested under it, filesystem-case-aware.
fn path_starts_with(target: &Path, base: &Path) -> bool {
    let t: Vec<_> = target.components().collect();
    let b: Vec<_> = base.components().collect();
    if t.len() < b.len() {
        return false;
    }
    t.iter()
        .zip(b.iter())
        .all(|(tc, bc)| component_eq(tc.as_os_str(), bc.as_os_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_escape_is_blocked() {
        let workspace = std::env::temp_dir().join("autoreport-file-tools");
        let escaped = resolve_within("../../etc/passwd", &workspace);
        assert!(escaped.is_err());
    }

    #[test]
    fn write_dir_is_enforced() {
        let workspace = std::env::temp_dir().join("autoreport-file-tools-write");
        let ctx = FsCtx::new(
            workspace.clone(),
            Some(workspace.join("Data").join("Processed")),
        );
        assert!(
            ctx.assert_write_allowed(&workspace.join("Data").join("Processed").join("x.csv"))
                .is_ok()
        );
        assert!(
            ctx.assert_write_allowed(&workspace.join("Theory").join("x.md"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_cannot_escape_write_dir() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let write_dir = root.path().join("Plots");
        std::fs::create_dir_all(&write_dir).unwrap();
        symlink("/tmp/does-not-exist-autoreport", write_dir.join("link")).unwrap();
        let ctx = FsCtx::new(root.path().to_path_buf(), Some(write_dir.clone()));
        assert!(
            ctx.assert_write_allowed(&write_dir.join("link/new.txt"))
                .is_err()
        );
    }
}
