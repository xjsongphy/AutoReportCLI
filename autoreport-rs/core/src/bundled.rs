//! Compile-time-embedded program templates. They live in AutoReport's global
//! home, matching Codex's global skills/config resources; report/data output
//! remains in the selected workspace.

use std::path::Path;

struct Bundled {
    rel: &'static str,
    content: &'static str,
}

const BUNDLED: &[Bundled] = &[
    Bundled {
        rel: "templates/template_mpl.tex",
        content: include_str!("../../../templates/reports/template_mpl.tex"),
    },
    Bundled {
        rel: "templates/mpltx.cls",
        content: include_str!("../../../templates/reports/mpltx.cls"),
    },
];

/// Write every bundled default that is missing on disk. Never overwrites.
pub fn materialize(home: &Path) {
    for item in BUNDLED {
        let target = home.join(item.rel);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&target, item.content) {
            log::warn!("failed to materialize {}: {e}", target.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_files_are_nonempty() {
        for b in BUNDLED {
            assert!(!b.content.is_empty(), "bundled file {} is empty", b.rel);
        }
    }
}
