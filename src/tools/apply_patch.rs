//! `apply_patch` tool — codex's patch format and application algorithm, ported
//! faithfully from `codex-rs/apply-patch`:
//!  - `seek_sequence` (verbatim) — robust line-sequence matching with
//!    whitespace + Unicode-punctuation tolerance.
//!  - `UpdateFileChunk` / `Hunk` data structures (codex's).
//!  - `compute_replacements` + `apply_replacements` (verbatim) — locate each
//!    hunk's old lines via `seek_sequence` and splice in the new lines.
//!  - the `*** Begin Patch` parser (codex's grammar).
//!
//! The only deviation from codex is the file-system layer: codex routes reads
//! and writes through `codex_exec_server::ExecutorFileSystem`; we use `std::fs`
//! under the same per-agent write-directory isolation as the other file tools.

use crate::tools::file_tools::FsCtx;
use crate::tools::registry::{arg_str, Tool, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ApplyPatchTool {
    ctx: FsCtx,
}

impl ApplyPatchTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn description(&self) -> &str {
        // Reuse codex's own tool description so model behaviour matches codex.
        concat!(
            "Apply a codex-style patch (*** Begin Patch ... *** End Patch) to add, update, ",
            "or delete files. Update hunks use `@@ <context>` anchors and ` ` (context), `+` ",
            "(add), `-` (remove) lines; `*** End of File` marks end-of-file edits; ",
            "`*** Move to:` renames. Prefer this over many edit_file calls."
        )
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patch": {"type": "string", "description": "Full patch text beginning with `*** Begin Patch` and ending with `*** End Patch`."}
            },
            "required": ["patch"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let patch = match arg_str(args, "patch") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        match engine::apply(&patch, &self.ctx) {
            Ok(report) => ToolOutput::ok(json!({"applied": report})),
            Err(e) => ToolOutput::err(e),
        }
    }
}

pub fn make(ctx: FsCtx) -> Arc<dyn Tool> {
    Arc::new(ApplyPatchTool::new(ctx))
}

pub mod engine {
    //! codex's apply-patch engine: parser + seek_sequence + replacements.

    use super::FsCtx;
    use crate::tools::file_tools::resolve_within;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    // ---- codex data structures (verbatim semantics) ----

    #[derive(Debug, Clone)]
    pub enum Hunk {
        AddFile { path: PathBuf, contents: String },
        DeleteFile { path: PathBuf },
        UpdateFile {
            path: PathBuf,
            move_path: Option<PathBuf>,
            chunks: Vec<UpdateFileChunk>,
        },
    }

    impl Hunk {
        pub fn path(&self) -> &Path {
            match self {
                Hunk::AddFile { path, .. } | Hunk::DeleteFile { path } => path,
                Hunk::UpdateFile {
                    move_path: Some(p), ..
                } => p,
                Hunk::UpdateFile { path, .. } => path,
            }
        }
    }

    #[derive(Debug, PartialEq, Clone)]
    pub struct UpdateFileChunk {
        pub change_context: Option<String>,
        pub old_lines: Vec<String>,
        pub new_lines: Vec<String>,
        pub is_end_of_file: bool,
    }

    const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
    const END_PATCH_MARKER: &str = "*** End Patch";
    const ADD_FILE_MARKER: &str = "*** Add File: ";
    const DELETE_FILE_MARKER: &str = "*** Delete File: ";
    const UPDATE_FILE_MARKER: &str = "*** Update File: ";
    const MOVE_TO_MARKER: &str = "*** Move to: ";
    const EOF_MARKER: &str = "*** End of File";
    const CHANGE_CONTEXT_MARKER: &str = "@@ ";
    const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

    // ---- parser ----

    pub fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
        let mut lines: Vec<&str> = patch.lines().collect();
        // Trim leading heredoc/invocation noise until Begin Patch.
        while let Some(first) = lines.first() {
            if first.trim_start() == BEGIN_PATCH_MARKER
                || first.trim_start().starts_with(BEGIN_PATCH_MARKER)
            {
                break;
            }
            // allow leading `apply_patch <<'EOF'` lines
            if lines.len() <= 1 {
                return Err("patch must contain `*** Begin Patch`".into());
            }
            lines.remove(0);
        }
        let Some(first) = lines.first() else {
            return Err("patch must contain `*** Begin Patch`".into());
        };
        if first.trim_start() != BEGIN_PATCH_MARKER {
            return Err("first patch line must be `*** Begin Patch`".into());
        }
        lines.remove(0);

        let mut hunks = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line == END_PATCH_MARKER {
                break;
            }
            if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
                i += 1;
                let (contents, consumed) = parse_add_body(&lines[i..]);
                i += consumed;
                hunks.push(Hunk::AddFile {
                    path: PathBuf::from(path.trim()),
                    contents,
                });
            } else if let Some(path) = line.strip_prefix(DELETE_FILE_MARKER) {
                i += 1;
                hunks.push(Hunk::DeleteFile {
                    path: PathBuf::from(path.trim()),
                });
            } else if let Some(path) = line.strip_prefix(UPDATE_FILE_MARKER) {
                i += 1;
                let mut move_path = None;
                if i < lines.len() {
                    if let Some(dest) = lines[i].strip_prefix(MOVE_TO_MARKER) {
                        move_path = Some(PathBuf::from(dest.trim()));
                        i += 1;
                    }
                }
                let (chunks, consumed) = parse_update_chunks(&lines[i..]);
                i += consumed;
                if chunks.is_empty() {
                    return Err(format!(
                        "Update File hunk for `{}` has no change chunks",
                        path.trim()
                    ));
                }
                hunks.push(Hunk::UpdateFile {
                    path: PathBuf::from(path.trim()),
                    move_path,
                    chunks,
                });
            } else {
                // Unknown line at hunk-header position.
                i += 1;
            }
        }
        Ok(hunks)
    }

    /// Collect `+`-prefixed add lines (codex: each line's first `+` stripped,
    /// joined by `\n` with a trailing newline).
    fn parse_add_body(lines: &[&str]) -> (String, usize) {
        let mut out = String::new();
        let mut consumed = 0;
        for line in lines {
            if line.starts_with("***") {
                break;
            }
            // Only `+` lines belong to an add body.
            if let Some(rest) = line.strip_prefix('+') {
                out.push_str(rest);
            }
            out.push('\n');
            consumed += 1;
        }
        (out, consumed)
    }

    /// Parse one or more `@@`-delimited change chunks for an Update File hunk.
    fn parse_update_chunks(lines: &[&str]) -> (Vec<UpdateFileChunk>, usize) {
        let mut chunks = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.starts_with("***") {
                break;
            }
            // Each chunk may start with @@ / @@ <ctx>. The first chunk requires
            // a context marker (codex lenient mode tolerates its absence).
            let (change_context, start) = if line == EMPTY_CHANGE_CONTEXT_MARKER {
                (None, 1)
            } else if let Some(ctx) = line.strip_prefix(CHANGE_CONTEXT_MARKER) {
                (Some(ctx.to_string()), 1)
            } else if !chunks.is_empty() {
                // No new context marker and we already have a chunk → done.
                break;
            } else {
                (None, 0)
            };
            i += start;

            let mut chunk = UpdateFileChunk {
                change_context,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                is_end_of_file: false,
            };
            let mut parsed = 0;
            while i < lines.len() {
                let l = lines[i];
                if l == EOF_MARKER {
                    chunk.is_end_of_file = true;
                    i += 1;
                    break;
                }
                if l.starts_with("***") || l == EMPTY_CHANGE_CONTEXT_MARKER || l.starts_with(CHANGE_CONTEXT_MARKER) {
                    break;
                }
                match l.chars().next() {
                    None => {
                        chunk.old_lines.push(String::new());
                        chunk.new_lines.push(String::new());
                    }
                    Some(' ') => {
                        let body = l[1..].to_string();
                        chunk.old_lines.push(body.clone());
                        chunk.new_lines.push(body);
                    }
                    Some('+') => chunk.new_lines.push(l[1..].to_string()),
                    Some('-') => chunk.old_lines.push(l[1..].to_string()),
                    Some(_) => {
                        if parsed == 0 {
                            // unexpected; bail to outer loop
                            break;
                        }
                        break;
                    }
                }
                parsed += 1;
                i += 1;
            }
            if parsed > 0 || chunk.is_end_of_file {
                chunks.push(chunk);
            } else if chunks.is_empty() {
                // no progress possible
                break;
            }
        }
        (chunks, i)
    }

    // ---- codex seek_sequence (verbatim) ----

    mod seek_sequence {
        //! Verbatim from codex `apply-patch/src/seek_sequence.rs`.

        pub fn seek_sequence(
            lines: &[String],
            pattern: &[String],
            start: usize,
            eof: bool,
        ) -> Option<usize> {
            if pattern.is_empty() {
                return Some(start);
            }
            if pattern.len() > lines.len() {
                return None;
            }
            let search_start = if eof && lines.len() >= pattern.len() {
                lines.len() - pattern.len()
            } else {
                start
            };
            for i in search_start..=lines.len().saturating_sub(pattern.len()) {
                if lines[i..i + pattern.len()] == *pattern {
                    return Some(i);
                }
            }
            for i in search_start..=lines.len().saturating_sub(pattern.len()) {
                let mut ok = true;
                for (p_idx, pat) in pattern.iter().enumerate() {
                    if lines[i + p_idx].trim_end() != pat.trim_end() {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Some(i);
                }
            }
            for i in search_start..=lines.len().saturating_sub(pattern.len()) {
                let mut ok = true;
                for (p_idx, pat) in pattern.iter().enumerate() {
                    if lines[i + p_idx].trim() != pat.trim() {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Some(i);
                }
            }
            fn normalise(s: &str) -> String {
                s.trim()
                    .chars()
                    .map(|c| match c {
                        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
                        | '\u{2212}' => '-',
                        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
                        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
                        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
                        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}'
                        | '\u{205F}' | '\u{3000}' => ' ',
                        other => other,
                    })
                    .collect::<String>()
            }
            for i in search_start..=lines.len().saturating_sub(pattern.len()) {
                let mut ok = true;
                for (p_idx, pat) in pattern.iter().enumerate() {
                    if normalise(&lines[i + p_idx]) != normalise(pat) {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Some(i);
                }
            }
            None
        }
    }

    // ---- codex compute_replacements + apply_replacements (verbatim) ----

    fn compute_replacements(
        original_lines: &[String],
        path: &Path,
        chunks: &[UpdateFileChunk],
    ) -> Result<Vec<(usize, usize, Vec<String>)>, String> {
        let mut replacements: Vec<(usize, usize, Vec<String>)> = Vec::new();
        let mut line_index: usize = 0;

        for chunk in chunks {
            if let Some(ctx_line) = &chunk.change_context {
                if let Some(idx) =
                    seek_sequence::seek_sequence(original_lines, std::slice::from_ref(ctx_line), line_index, false)
                {
                    line_index = idx + 1;
                } else {
                    return Err(format!(
                        "Failed to find context '{}' in {}",
                        ctx_line,
                        path.display()
                    ));
                }
            }

            if chunk.old_lines.is_empty() {
                let insertion_idx = if original_lines.last().is_some_and(String::is_empty) {
                    original_lines.len() - 1
                } else {
                    original_lines.len()
                };
                replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
                continue;
            }

            let mut pattern: &[String] = &chunk.old_lines;
            let mut found =
                seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
            let mut new_slice: &[String] = &chunk.new_lines;

            if found.is_none() && pattern.last().is_some_and(String::is_empty) {
                pattern = &pattern[..pattern.len() - 1];
                if new_slice.last().is_some_and(String::is_empty) {
                    new_slice = &new_slice[..new_slice.len() - 1];
                }
                found = seek_sequence::seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
            }

            if let Some(start_idx) = found {
                replacements.push((start_idx, pattern.len(), new_slice.to_vec()));
                line_index = start_idx + pattern.len();
            } else {
                return Err(format!(
                    "Failed to find expected lines in {}:\n{}",
                    path.display(),
                    chunk.old_lines.join("\n"),
                ));
            }
        }

        // Verbatim from codex; `sort_by_key` is equivalent but this preserves the
        // upstream algorithm text.
        #[allow(clippy::unnecessary_sort_by)]
        replacements.sort_by(|(lhs_idx, _, _), (rhs_idx, _, _)| lhs_idx.cmp(rhs_idx));
        Ok(replacements)
    }

    fn apply_replacements(
        mut lines: Vec<String>,
        replacements: &[(usize, usize, Vec<String>)],
    ) -> Vec<String> {
        for (start_idx, old_len, new_segment) in replacements.iter().rev() {
            let start_idx = *start_idx;
            let old_len = *old_len;
            for _ in 0..old_len {
                if start_idx < lines.len() {
                    lines.remove(start_idx);
                }
            }
            for (offset, new_line) in new_segment.iter().enumerate() {
                lines.insert(start_idx + offset, new_line.clone());
            }
        }
        lines
    }

    // ---- application (std::fs + write isolation) ----

    pub fn apply(patch: &str, ctx: &FsCtx) -> Result<Vec<serde_json::Value>, String> {
        let hunks = parse(patch)?;
        if hunks.is_empty() {
            return Err("No files were modified.".into());
        }
        let mut report = Vec::new();
        for hunk in &hunks {
            match hunk {
                Hunk::AddFile { path, contents } => {
                    let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    write_with_parents(&resolved, contents)?;
                    report.push(json!({"add": resolved.display().to_string()}));
                }
                Hunk::DeleteFile { path } => {
                    let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    if resolved.is_dir() {
                        return Err(format!("{} is a directory", resolved.display()));
                    }
                    if resolved.exists() {
                        std::fs::remove_file(&resolved).map_err(|e| e.to_string())?;
                    }
                    report.push(json!({"delete": resolved.display().to_string()}));
                }
                Hunk::UpdateFile {
                    path,
                    move_path,
                    chunks,
                } => {
                    let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    let original =
                        std::fs::read_to_string(&resolved).map_err(|e| e.to_string())?;
                    let mut original_lines: Vec<String> =
                        original.split('\n').map(String::from).collect();
                    if original_lines.last().is_some_and(String::is_empty) {
                        original_lines.pop();
                    }
                    let replacements = compute_replacements(&original_lines, &resolved, chunks)?;
                    let mut new_lines = apply_replacements(original_lines, &replacements);
                    if !new_lines.last().is_some_and(String::is_empty) {
                        new_lines.push(String::new());
                    }
                    let new_contents = new_lines.join("\n");

                    if let Some(dest) = move_path {
                        let dest_abs = resolve_within(&dest.to_string_lossy(), &ctx.workspace)?;
                        check_write(ctx, &dest_abs)?;
                        write_with_parents(&dest_abs, &new_contents)?;
                        std::fs::remove_file(&resolved).map_err(|e| e.to_string())?;
                        report.push(json!({"move": resolved.display().to_string(), "to": dest_abs.display().to_string()}));
                    } else {
                        std::fs::write(&resolved, new_contents).map_err(|e| e.to_string())?;
                        report.push(json!({"update": resolved.display().to_string()}));
                    }
                }
            }
        }
        Ok(report)
    }

    fn check_write(ctx: &FsCtx, target: &Path) -> Result<(), String> {
        let metadata = ctx.workspace.join(".autoreport");
        if target == metadata || target.starts_with(&metadata) {
            return Err("writing inside .autoreport is not permitted".into());
        }
        match &ctx.write_dir {
            Some(dir) if target.starts_with(dir) => Ok(()),
            Some(dir) => Err(format!(
                "this agent may only write under {}; '{}' is outside it",
                dir.display(),
                target.display()
            )),
            None => Err("this agent has no write access".into()),
        }
    }

    fn write_with_parents(path: &Path, contents: &str) -> Result<(), String> {
        match std::fs::write(path, contents) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e2| e2.to_string())?;
                }
                std::fs::write(path, contents).map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        fn ctx(ws: &Path) -> FsCtx {
            FsCtx::new(ws.to_path_buf(), Some(ws.to_path_buf()))
        }

        fn stamp() -> String {
            use std::time::SystemTime;
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string()
        }

        #[test]
        fn update_replace_and_add_and_delete() {
            let dir = std::env::temp_dir().join(format!("ap-{}", stamp()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

            let patch = "*** Begin Patch
*** Update File: a.txt
@@
 alpha
-beta
+BETA
 gamma
*** Add File: b.txt
+hello
+world
*** End Patch";
            let report = apply(patch, &ctx(&dir)).unwrap();
            assert!(report.iter().any(|r| r.get("update").is_some()));
            assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "alpha\nBETA\ngamma\n");
            assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "hello\nworld\n");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn end_of_file_addition() {
            let dir = std::env::temp_dir().join(format!("ap-{}", stamp()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("f.txt"), "one\ntwo\n").unwrap();
            let patch = "*** Begin Patch
*** Update File: f.txt
@@
 two
+three
*** End of File
*** End Patch";
            apply(patch, &ctx(&dir)).unwrap();
            assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "one\ntwo\nthree\n");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn whitespace_tolerant_match() {
            let dir = std::env::temp_dir().join(format!("ap-{}", stamp()));
            std::fs::create_dir_all(&dir).unwrap();
            // file has trailing spaces on the unchanged context line; the patch's
            // seek_sequence rstrip pass still *locates* the hunk (matching "foo   "
            // against pattern "foo"), then rewrites the context line from the patch.
            std::fs::write(dir.join("g.txt"), "foo   \nbar\n").unwrap();
            let patch = "*** Begin Patch
*** Update File: g.txt
@@
 foo
-bar
+BAR
*** End Patch";
            apply(patch, &ctx(&dir)).unwrap();
            assert_eq!(std::fs::read_to_string(dir.join("g.txt")).unwrap(), "foo\nBAR\n");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn move_file() {
            let dir = std::env::temp_dir().join(format!("ap-{}", stamp()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("old.txt"), "x\ny\n").unwrap();
            let patch = "*** Begin Patch
*** Update File: old.txt
*** Move to: new.txt
@@
-x
+X
*** End Patch";
            apply(patch, &ctx(&dir)).unwrap();
            assert!(!dir.join("old.txt").exists());
            assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "X\ny\n");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[allow(dead_code)]
        fn _pb(_: PathBuf) {}
    }
}
