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
        rel: "resources/latex/templates/main.tex",
        content: include_str!("../../../templates/latex/templates/main.tex"),
    },
    Bundled {
        rel: "resources/latex/themes/mpltx.cls",
        content: include_str!("../../../templates/latex/themes/mpltx.cls"),
    },
    Bundled {
        rel: "resources/typst/templates/main.typ",
        content: include_str!("../../../templates/typst/templates/main.typ"),
    },
    Bundled {
        rel: "resources/typst/templates/bibli.bib",
        content: include_str!("../../../templates/typst/templates/bibli.bib"),
    },
    Bundled {
        rel: "resources/typst/templates/american-physics-society.csl",
        content: include_str!("../../../templates/typst/templates/american-physics-society.csl"),
    },
    Bundled {
        rel: "resources/typst/themes/mplts.typ",
        content: include_str!("../../../templates/typst/themes/mplts.typ"),
    },
    Bundled {
        rel: "resources/typst/LICENSE",
        content: include_str!("../../../templates/typst/LICENSE"),
    },
    Bundled {
        rel: "resources/latex/skills/latex-compile/SKILL.md",
        content: include_str!("../../../templates/latex/skills/latex-compile/SKILL.md"),
    },
    Bundled {
        rel: "resources/latex/skills/experiment-report-writer/SKILL.md",
        content: include_str!("../../../templates/latex/skills/experiment-report-writer/SKILL.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/SKILL.md",
        content: include_str!("../../../templates/typst/skills/typst/SKILL.md"),
    },
    Bundled {
        rel: "resources/typst/skills/experiment-report-writer/SKILL.md",
        content: include_str!("../../../templates/typst/skills/experiment-report-writer/SKILL.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/basics.md",
        content: include_str!("../../../templates/typst/skills/typst/basics.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/types.md",
        content: include_str!("../../../templates/typst/skills/typst/types.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/styling.md",
        content: include_str!("../../../templates/typst/skills/typst/styling.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/tables.md",
        content: include_str!("../../../templates/typst/skills/typst/tables.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/academic.md",
        content: include_str!("../../../templates/typst/skills/typst/academic.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/conversion.md",
        content: include_str!("../../../templates/typst/skills/typst/conversion.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/cli.md",
        content: include_str!("../../../templates/typst/skills/typst/cli.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/query.md",
        content: include_str!("../../../templates/typst/skills/typst/query.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/advanced.md",
        content: include_str!("../../../templates/typst/skills/typst/advanced.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/template.md",
        content: include_str!("../../../templates/typst/skills/typst/template.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/package.md",
        content: include_str!("../../../templates/typst/skills/typst/package.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/debug.md",
        content: include_str!("../../../templates/typst/skills/typst/debug.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/perf.md",
        content: include_str!("../../../templates/typst/skills/typst/perf.md"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/basic-document.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/basic-document.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/styled-document.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/styled-document.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/template-report.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/template-report.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/tables-showcase.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/tables-showcase.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/academic-paper.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/academic-paper.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/examples/query-export.typ",
        content: include_str!("../../../templates/typst/skills/typst/examples/query-export.typ"),
    },
    Bundled {
        rel: "resources/typst/skills/typst/LICENSE",
        content: include_str!("../../../templates/typst/skills/typst/LICENSE"),
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
