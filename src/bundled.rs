//! Bundled, compile-time-embedded defaults so the binary runs standalone:
//! the AutoReport agent skills and the default LaTeX report template. On
//! workspace init these are materialized to disk (only when absent, so user
//! overrides always win) so `load_skill` finds them and the Report agent has a
//! template to start from.

use std::path::Path;

struct Bundled {
    /// Path under the workspace to write to.
    rel: &'static str,
    /// Embedded file contents.
    content: &'static str,
}

const BUNDLED: &[Bundled] = &[
    Bundled {
        rel: ".autoreport/skills/experiment-report-writer.md",
        content: include_str!("../templates/skills/experiment-report-writer.md"),
    },
    Bundled {
        rel: ".autoreport/skills/latex-compile.md",
        content: include_str!("../templates/skills/latex-compile.md"),
    },
    Bundled {
        rel: ".autoreport/skills/md-report-writer.md",
        content: include_str!("../templates/skills/md-report-writer.md"),
    },
    Bundled {
        rel: ".autoreport/skills/mineru.md",
        content: include_str!("../templates/skills/mineru.md"),
    },
    Bundled {
        rel: "references/templates/template_mpl.tex",
        content: include_str!("../templates/reports/template_mpl.tex"),
    },
    Bundled {
        rel: "references/templates/mpltx.cls",
        content: include_str!("../templates/reports/mpltx.cls"),
    },
];

/// Write every bundled default that is missing on disk. Never overwrites.
pub fn materialize(workspace: &Path) {
    for item in BUNDLED {
        let target = workspace.join(item.rel);
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
