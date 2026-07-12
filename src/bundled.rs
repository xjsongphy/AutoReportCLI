//! Compile-time-embedded *report template* so the binary can seed a new
//! project's LaTeX without any network access. Skills are intentionally NOT
//! bundled here: like AutoReport, they are pulled from the `xjsongphy/skills`
//! repository at startup (see `sync.rs`), and the cc-switch provider presets
//! from the `farion1231/cc-switch` repository.

use std::path::Path;

struct Bundled {
    rel: &'static str,
    content: &'static str,
}

const BUNDLED: &[Bundled] = &[
    Bundled {
        rel: "References/templates/template_mpl.tex",
        content: include_str!("../templates/reports/template_mpl.tex"),
    },
    Bundled {
        rel: "References/templates/mpltx.cls",
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
