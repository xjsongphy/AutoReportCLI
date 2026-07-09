//! Shared filesystem guards used by shell/list_dir/apply_patch.
//!
//! The model no longer receives first-class read/write/edit/delete tools.
//! Instead, file access flows through `exec`, `list_dir`, and `apply_patch`,
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
        let metadata = self.workspace.join(".autoreport");
        if target == metadata || target.starts_with(&metadata) {
            return Err("writing inside .autoreport is not permitted".to_string());
        }
        match &self.write_dir {
            Some(dir) if target.starts_with(dir) => Ok(()),
            Some(dir) => Err(format!(
                "this agent may only write under {}; '{}' is outside it",
                dir.display(),
                target.display()
            )),
            None => Err("this agent has no write access".to_string()),
        }
    }
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
    if joined == *workspace || joined.starts_with(workspace) {
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
            Some(workspace.join("data").join("processed")),
        );
        assert!(
            ctx.assert_write_allowed(&workspace.join("data").join("processed").join("x.csv"))
                .is_ok()
        );
        assert!(
            ctx.assert_write_allowed(&workspace.join("theory").join("x.md"))
                .is_err()
        );
    }
}
