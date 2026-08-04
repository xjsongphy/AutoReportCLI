//! Built-in resource loading.
//!
//! Debug builds read from the repository so editing a template, skill, or
//! agent prompt does not invalidate `autoreport-core`. Release-like builds
//! embed the same files so installed binaries remain self-contained.

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;

#[cfg(not(debug_assertions))]
use include_dir::{Dir, include_dir};

#[cfg(not(debug_assertions))]
static EMBEDDED_LATEX: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../templates/latex");

#[cfg(not(debug_assertions))]
static EMBEDDED_TYPST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../templates/typst");

#[cfg(not(debug_assertions))]
static EMBEDDED_AGENTS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/../../templates/agents");

#[cfg(not(debug_assertions))]
static EMBEDDED_REPORT_LANGUAGES: Dir<'static> =
    include_dir!("$CARGO_MANIFEST_DIR/../../templates/report-languages");

#[derive(Debug)]
pub(crate) struct ResourceError {
    source: &'static str,
    path: PathBuf,
    detail: String,
}

impl ResourceError {
    #[cfg(debug_assertions)]
    fn io(source: &'static str, path: PathBuf, error: std::io::Error) -> Self {
        Self {
            source,
            path,
            detail: error.to_string(),
        }
    }

    #[cfg(not(debug_assertions))]
    fn missing(source: &'static str, path: PathBuf) -> Self {
        Self {
            source,
            path,
            detail: "resource was not found".to_string(),
        }
    }

    #[cfg(not(debug_assertions))]
    fn not_utf8(source: &'static str, path: PathBuf) -> Self {
        Self {
            source,
            path,
            detail: "resource is not valid UTF-8".to_string(),
        }
    }
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to load built-in resource {} at {}: {}",
            self.source,
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for ResourceError {}

/// Load a resource using the current profile's source.
pub(crate) fn load(source: &'static str) -> Result<Cow<'static, str>, ResourceError> {
    #[cfg(debug_assertions)]
    {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(source);
        return std::fs::read_to_string(&path)
            .map(Cow::Owned)
            .map_err(|error| ResourceError::io(source, path, error));
    }

    #[cfg(not(debug_assertions))]
    {
        let relative = source.strip_prefix("templates/").unwrap_or(source);
        let path = PathBuf::from("embedded templates").join(relative);
        let file = if let Some(relative) = source.strip_prefix("templates/latex/") {
            EMBEDDED_LATEX.get_file(relative)
        } else if let Some(relative) = source.strip_prefix("templates/typst/") {
            EMBEDDED_TYPST.get_file(relative)
        } else if let Some(relative) = source.strip_prefix("templates/agents/") {
            EMBEDDED_AGENTS.get_file(relative)
        } else if let Some(relative) = source.strip_prefix("templates/report-languages/") {
            EMBEDDED_REPORT_LANGUAGES.get_file(relative)
        } else {
            None
        };
        let Some(file) = file else {
            return Err(ResourceError::missing(source, path));
        };
        let Some(contents) = file.contents_utf8() else {
            return Err(ResourceError::not_utf8(source, path));
        };
        Ok(Cow::Borrowed(contents))
    }
}

#[cfg(test)]
mod tests {
    use super::load;

    #[test]
    fn built_in_source_is_available() {
        let content = load("templates/agents/Common.md").expect("Common.md should load");
        assert!(!content.trim().is_empty());
    }
}
