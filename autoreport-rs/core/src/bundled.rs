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
use std::path::Path;

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
        if let Err(error) = std::fs::write(&target, content.as_bytes()) {
            log::warn!("failed to materialize {}: {error}", target.display());
        }
    }
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
}
