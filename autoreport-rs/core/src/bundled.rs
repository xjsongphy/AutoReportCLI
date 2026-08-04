//! Built-in resources installed into AutoReport's global home.
//!
//! The resource contents come from the source tree in debug builds and from
//! the embedded template directory in release-like builds. Report/data output
//! remains in the selected workspace.
//!
//! Only program *templates* and themes are bundled here. Agent skills are
//! pulled at runtime by [`crate::sync`] from their upstream repositories and
//! are not embedded — the app requires network for the model regardless, and
//! the sync cache is the local copy.

use crate::resources;
use std::path::{Path, PathBuf};

struct Bundled {
    source: &'static str,
    output: &'static str,
}

const BUNDLED: &[Bundled] = &[
    Bundled {
        source: "templates/latex/templates/main.tex",
        output: "resources/latex/templates/main.tex",
    },
    Bundled {
        source: "templates/latex/themes/mpltx.cls",
        output: "resources/latex/themes/mpltx.cls",
    },
    Bundled {
        source: "templates/typst/templates/main.typ",
        output: "resources/typst/templates/main.typ",
    },
    Bundled {
        source: "templates/typst/templates/bibli.bib",
        output: "resources/typst/templates/bibli.bib",
    },
    Bundled {
        source: "templates/typst/templates/american-physics-society.csl",
        output: "resources/typst/templates/american-physics-society.csl",
    },
    Bundled {
        source: "templates/typst/themes/mplts.typ",
        output: "resources/typst/themes/mplts.typ",
    },
    Bundled {
        source: "templates/typst/LICENSE",
        output: "resources/typst/LICENSE",
    },
];

/// Write every bundled default that is missing on disk. Never overwrites.
pub fn materialize(home: &Path) {
    for item in BUNDLED {
        let target = home.join(item.output);
        if target.exists() {
            continue;
        }
        let content = match resources::load(item.source) {
            Ok(content) => content,
            Err(error) => {
                log::warn!("failed to load bundled {}: {error}", item.source);
                continue;
            }
        };
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = atomic_write(&target, content.as_bytes()) {
            log::warn!("failed to materialize {}: {error}", target.display());
        }
    }
}

/// Atomically write `data` to `path` via a sibling temp file then rename.
///
/// A crash mid-`fs::write` leaves a partial file that `target.exists()` would
/// then treat as complete, so the bundled asset would never be re-materialized.
/// Writing to `.«name».tmp` in the same directory and renaming over the target
/// is atomic on the same filesystem (POSIX `rename` / Win32 `MoveFileEx`), so
/// readers either see the old file or the full new file — never a partial write.
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let temp = temp_path_for(path);
    // Clean up the temp file on any failure so a later run doesn't pick up a
    // stale partial write through the temp name.
    if let Err(err) = std::fs::write(&temp, data) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(())
}

/// Build a sibling temp path `.«name».tmp` for the final `path`.
fn temp_path_for(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "bundled".to_string());
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!(".{file_name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_files_are_nonempty() {
        for item in BUNDLED {
            let content = resources::load(item.source)
                .unwrap_or_else(|error| panic!("{}: {error}", item.source));
            assert!(!content.is_empty(), "bundled file {} is empty", item.output);
        }
    }

    #[test]
    fn atomic_write_fully_replaces_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        // Pre-existing content strictly longer than the replacement: catches
        // both append-on-top bugs and partial-write bugs.
        std::fs::write(&target, "this is much longer than the replacement").unwrap();
        atomic_write(&target, b"short").expect("atomic write succeeds");
        let read_back = std::fs::read_to_string(&target).unwrap();
        assert_eq!(
            read_back, "short",
            "target should hold only the new content"
        );
        // The temp file must not be left behind in the directory.
        let remaining: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(remaining, vec!["target.txt".to_string()]);
    }

    #[test]
    fn atomic_write_into_nested_path() {
        // The helper must create the sibling temp next to a nested target.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("a/b/c.txt");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        atomic_write(&target, b"hello").expect("atomic write succeeds");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello");
    }
}
