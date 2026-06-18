//! `@` file search: a cached index of workspace files with subsequence fuzzy
//! ranking. Codex uses a heavier async `codex-file-search` session; we keep a
//! compact synchronous scorer that is plenty for a project-sized workspace.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Directories we never surface in `@` matches.
const SKIP_DIRS: &[&str] = &[".autoreport", ".git", "target", "node_modules", "__pycache__"];

pub struct FileIndex {
    root: PathBuf,
    cache: Mutex<Option<Cached>>,
}

struct Cached {
    /// Relative (POSIX-style) paths, sorted.
    entries: Vec<String>,
}

impl FileIndex {
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: Mutex::new(None),
        }
    }

    /// Rebuild the index from disk.
    pub fn refresh(&self) {
        let mut entries = Vec::new();
        walk(&self.root, &self.root, &mut entries);
        entries.sort();
        *self.cache.lock().unwrap() = Some(Cached { entries });
    }

    fn ensure(&self) {
        let mut g = self.cache.lock().unwrap();
        if g.is_none() {
            let mut entries = Vec::new();
            walk(&self.root, &self.root, &mut entries);
            entries.sort();
            *g = Some(Cached { entries });
        }
    }

    /// Return up to `limit` relative paths matching `query`, best first.
    /// Empty query returns the first `limit` entries (recent-ish listing).
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
        let mut scored: Vec<(i64, String)> = entries
            .iter()
            .filter_map(|p| score(p, q).map(|s| (s, p.clone())))
            .collect();
        // Stable-ish: higher score first; tie-break alphabetically.
        scored.sort_by(|a, b| match b.0.cmp(&a.0) {
            std::cmp::Ordering::Equal => a.1.cmp(&b.1),
            o => o,
        });
        scored.into_iter().take(limit).map(|(_, p)| p).collect()
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if SKIP_DIRS.contains(&name_str.as_ref()) {
            continue;
        }
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            if let Ok(rel) = path.strip_prefix(root) {
                let s: String = rel.to_string_lossy().replace('\\', "/");
                out.push(s);
            }
        }
    }
}

/// Subsequence fuzzy score. Higher is better.
/// Bonuses: matched in filename, consecutive chars, start-of-token, short path.
fn score(path: &str, query: &str) -> Option<i64> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let lower_path: Vec<char> = path.to_lowercase().chars().collect();
    let lower_q: Vec<char> = query.to_lowercase().chars().collect();

    let mut pi = 0usize;
    let mut score: i64 = 0;
    let mut consecutive = 0i64;
    let mut first_match: Option<usize> = None;
    for &qc in &lower_q {
        let mut found = false;
        while pi < lower_path.len() {
            if lower_path[pi] == qc {
                found = true;
                if first_match.is_none() {
                    first_match = Some(pi);
                }
                // bonuses
                let at_token_start = pi == 0 || matches!(lower_path[pi - 1], '/' | '_' | '-' | '.');
                if at_token_start {
                    score += 12;
                }
                if consecutive > 0 {
                    score += 4 * consecutive;
                }
                consecutive += 1;
                pi += 1;
                break;
            } else {
                consecutive = 0;
                pi += 1;
            }
        }
        if !found {
            return None;
        }
    }

    // Strong bonus when the match lands inside the filename.
    let last_slash = lower_path.iter().rposition(|&c| c == '/').map(|i| i + 1).unwrap_or(0);
    if let Some(fm) = first_match {
        if fm >= last_slash {
            score += 30;
        }
        // prefer earlier matches
        score -= (fm as i64) / 4;
    }
    // prefer shorter paths
    score -= (path.len() as i64) / 8;
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_basic() {
        assert!(score("data/processed/out.csv", "out").is_some());
        assert!(score("tex/main.tex", "maintex").is_some());
        assert!(score("tex/main.tex", "xyz").is_none());
    }

    #[test]
    fn filename_beats_deep_path() {
        let a = score("code/figures/long/path/power.png", "power").unwrap();
        let b = score("code/power.py", "power").unwrap();
        assert!(b >= a, "shorter path should rank >= deep path: {a} vs {b}");
    }
}
