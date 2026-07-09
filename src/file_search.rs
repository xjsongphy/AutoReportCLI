//! `@` file search using the **same fuzzy engine codex uses** (`nucleo`) and
//! codex's gitignore-aware walker (`ignore`). This replaces a hand-rolled
//! scorer with codex's actual matching: `Pattern` (fuzzy, case-insensitive,
//! smart-normalized) scored against each path via `nucleo::Matcher`, then
//! sorted by descending score and ascending path — exactly codex's
//! `cmp_by_score_desc_then_path_asc` ordering.

use nucleo::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directories never surfaced in `@` matches.
const SKIP_DIRS: &[&str] = &[
    ".autoreport",
    ".git",
    "target",
    "node_modules",
    "__pycache__",
];

pub struct FileIndex {
    root: PathBuf,
    cache: Mutex<Option<Cached>>,
}

struct Cached {
    /// Relative (POSIX-style) paths.
    entries: Vec<String>,
}

impl FileIndex {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: Mutex::new(None),
        }
    }

    /// Rebuild the index from disk using codex's `ignore` walker.
    pub fn refresh(&self) {
        let entries = walk(&self.root);
        *self.cache.lock().unwrap() = Some(Cached { entries });
    }

    fn ensure(&self) {
        let mut g = self.cache.lock().unwrap();
        if g.is_none() {
            let entries = walk(&self.root);
            *g = Some(Cached { entries });
        }
    }

    /// Return up to `limit` relative paths matching `query`, best first, using
    /// nucleo fuzzy scoring (codex's engine). Empty query → first `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        self.ensure();
        let g = self.cache.lock().unwrap();
        let entries = match g.as_ref() {
            Some(c) => &c.entries,
            None => return Vec::new(),
        };
        let q = query.trim();
        if q.is_empty() {
            return entries.iter().take(limit).cloned().collect();
        }

        let pattern = Pattern::new(
            q,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let mut matcher = Matcher::new(Config::DEFAULT);
        let mut buf: Vec<char> = Vec::new();
        let mut scored: Vec<(u32, String)> = entries
            .iter()
            .filter_map(|p| {
                let haystack = Utf32Str::new(p, &mut buf);
                pattern
                    .score(haystack, &mut matcher)
                    .map(|s| (s, p.clone()))
            })
            .collect();
        // codex ordering: descending score, then ascending path.
        scored.sort_by(|a, b| match b.0.cmp(&a.0) {
            std::cmp::Ordering::Equal => a.1.cmp(&b.1),
            other => other,
        });
        scored.into_iter().take(limit).map(|(_, p)| p).collect()
    }
}

/// Walk `root` with `ignore`, skipping the standard internal/build dirs.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .filter_entry(move |entry| {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref())
        })
        .build();
    for entry in walker.flatten() {
        let path = entry.path();
        if path == root {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            let s: String = rel.to_string_lossy().replace('\\', "/");
            if !s.is_empty() {
                out.push(s);
            }
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_filename() {
        let dir = std::env::temp_dir().join(format!("fs-{}", stamp()));
        std::fs::create_dir_all(dir.join("data/processed")).unwrap();
        std::fs::create_dir_all(dir.join("tex")).unwrap();
        std::fs::write(dir.join("data/processed/out.csv"), "x").unwrap();
        std::fs::write(dir.join("tex/main.tex"), "x").unwrap();
        let idx = FileIndex::new(&dir);
        idx.refresh();
        let m = idx.search("out", 10);
        assert!(m.iter().any(|p| p.contains("out.csv")), "matches: {m:?}");
        let none = idx.search("zzzzz", 10);
        assert!(none.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_internal_dirs() {
        let dir = std::env::temp_dir().join(format!("fs2-{}", stamp()));
        std::fs::create_dir_all(dir.join(".autoreport/manifests")).unwrap();
        std::fs::create_dir_all(dir.join("code")).unwrap();
        std::fs::write(dir.join(".autoreport/manifests/main.json"), "x").unwrap();
        std::fs::write(dir.join("code/plot.py"), "x").unwrap();
        let idx = FileIndex::new(&dir);
        idx.refresh();
        let m = idx.search("", 100);
        assert!(m.iter().any(|p| p.contains("plot.py")));
        assert!(!m.iter().any(|p| p.contains(".autoreport")), "{m:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn stamp() -> String {
        use std::time::SystemTime;
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
