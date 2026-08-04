//! codex's apply-patch engine: parser + seek_sequence + replacements.

use super::FsCtx;
use crate::file_tools::resolve_within;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---- codex data structures (verbatim semantics) ----

#[derive(Debug, Clone)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
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
const ENVIRONMENT_ID_MARKER: &str = "*** Environment ID:";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const CHANGE_CONTEXT_MARKER: &str = "@@ ";
const EMPTY_CHANGE_CONTEXT_MARKER: &str = "@@";

// ---- parser ----

pub fn parse(patch: &str) -> Result<Vec<Hunk>, String> {
    // Trim leading/trailing whitespace (codex `parse_patch_text`,
    // apply-patch/src/parser.rs:179 — `patch.trim().lines().collect()`).
    // A leading newline (`"\n*** Begin Patch\n..."`) or two+ trailing
    // newlines after `*** End Patch` would otherwise be rejected by the
    // strict boundary check below even though codex accepts them.
    let lines: Vec<&str> = patch.trim().lines().collect();
    // Verify patch boundaries (codex `check_patch_boundaries`,
    // apply-patch/src/parser.rs). Two modes:
    //   - strict:  first trimmed line == `*** Begin Patch`,
    //              last trimmed line == `*** End Patch`
    //   - lenient: tolerates a heredoc wrapper (`<<EOF` / `<<'EOF'` /
    //              `<<"EOF"` … trailing `EOF`, >=4 lines) by stripping the
    //              two marker lines and re-applying the strict check on the
    //              inner text.
    // We deliberately do NOT silently skip arbitrary leading lines (the old
    // ad-hoc behaviour): codex-trained models emit either the bare patch or
    // one of the heredoc forms, and silent skipping can mask real errors.
    let inner = check_patch_boundaries(&lines)?;
    // `inner` still includes the Begin/End markers. Drop the first
    // (`*** Begin Patch`); the terminating `*** End Patch` is consumed by
    // the main loop below.
    let body = &inner[1..];

    // Optional `*** Environment ID: <id>` preamble (codex
    // `streaming_parser::handle_hunk_headers_and_end_patch`). We don't use
    // the id locally but must accept and validate it so patches produced by
    // codex-trained models aren't rejected.
    let mut start = 0;
    if let Some(first) = body.first() {
        if let Some(rest) = first.trim().strip_prefix(ENVIRONMENT_ID_MARKER) {
            let id = rest.trim();
            if id.is_empty() {
                return Err("apply_patch environment_id cannot be empty".into());
            }
            start = 1;
            // Only enforce uniqueness against a second env_id line when the
            // first line was itself an env_id preamble. Without this guard a
            // misplaced first-occurrence env_id after a hunk header (e.g.
            // `*** Update File: foo` followed by `*** Environment ID:`) is
            // mis-reported as "cannot be specified more than once" instead of
            // being surfaced as an invalid hunk header by the main loop.
            if let Some(second) = body.get(1) {
                if second.trim().starts_with(ENVIRONMENT_ID_MARKER) {
                    return Err(
                        "apply_patch environment_id cannot be specified more than once".into()
                    );
                }
            }
        }
    }

    let invalid_header = |trimmed: &str| {
        format!(
            "'{trimmed}' is not a valid hunk header. Valid hunk headers: \
             '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'"
        )
    };

    let mut hunks = Vec::new();
    let mut i = start;
    let mut saw_end = false;
    while i < body.len() {
        let line = body[i];
        if line.trim() == END_PATCH_MARKER {
            saw_end = true;
            break;
        }
        if let Some(path) = line.strip_prefix(ADD_FILE_MARKER) {
            i += 1;
            let (contents, consumed) = parse_add_body(&body[i..])?;
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
            if i < body.len() {
                if let Some(dest) = body[i].strip_prefix(MOVE_TO_MARKER) {
                    move_path = Some(PathBuf::from(dest.trim()));
                    i += 1;
                }
            }
            let (chunks, consumed) = parse_update_chunks(&body[i..])?;
            i += consumed;
            if chunks.is_empty() {
                return Err(format!(
                    "Update file hunk for path '{}' is empty",
                    path.trim()
                ));
            }
            hunks.push(Hunk::UpdateFile {
                path: PathBuf::from(path.trim()),
                move_path,
                chunks,
            });
        } else {
            // Unknown line at a hunk-header position is an error, not
            // silently skipped (codex `streaming_parser`: invalid hunk
            // header). Lets the model self-correct on the next try.
            return Err(invalid_header(line.trim()));
        }
    }
    if !saw_end {
        return Err("The last line of the patch must be '*** End Patch'".into());
    }
    Ok(hunks)
}

/// Verify the patch's first/last lines, accepting either the bare form
/// (`*** Begin Patch` … `*** End Patch`) or a heredoc wrapper. Returns the
/// slice of patch lines that includes the Begin/End markers (caller drops
/// Begin before walking hunks). Ported from codex `check_patch_boundaries`
/// (`apply-patch/src/parser.rs`).
fn check_patch_boundaries<'a>(lines: &'a [&'a str]) -> Result<&'a [&'a str], String> {
    match check_start_and_end(lines) {
        Ok(()) => Ok(lines),
        Err(strict_err) => {
            // Lenient: a `<<EOF` / `<<'EOF'` / `<<"EOF"` … `EOF` heredoc
            // wrapper (>=4 lines: 2 markers + >=2 patch lines).
            match lines {
                [first, .., last] => {
                    let is_heredoc_start =
                        *first == "<<EOF" || *first == "<<'EOF'" || *first == "<<\"EOF\"";
                    if is_heredoc_start && last.ends_with("EOF") && lines.len() >= 4 {
                        let inner = &lines[1..lines.len() - 1];
                        check_start_and_end(inner).map_err(|_| strict_err)?;
                        Ok(inner)
                    } else {
                        Err(strict_err)
                    }
                }
                _ => Err(strict_err),
            }
        }
    }
}

fn check_start_and_end(lines: &[&str]) -> Result<(), String> {
    let first = lines.first().map(|l| l.trim());
    let last = lines.last().map(|l| l.trim());
    match (first, last) {
        (Some(f), Some(l)) if f == BEGIN_PATCH_MARKER && l == END_PATCH_MARKER => Ok(()),
        (Some(f), _) if f != BEGIN_PATCH_MARKER => {
            Err("The first line of the patch must be '*** Begin Patch'".into())
        }
        _ => Err("The last line of the patch must be '*** End Patch'".into()),
    }
}

/// Collect `+`-prefixed add lines (codex: each line's first `+` stripped,
/// joined by `\n` with a trailing newline). Any non-`+` line that is not a
/// hunk header / End Patch is an error — previously it was silently turned
/// into a blank line, corrupting the created file.
fn parse_add_body(lines: &[&str]) -> Result<(String, usize), String> {
    let mut out = String::new();
    let mut consumed = 0;
    for line in lines {
        if line.starts_with("***") {
            break;
        }
        if let Some(rest) = line.strip_prefix('+') {
            out.push_str(rest);
            out.push('\n');
            consumed += 1;
        } else {
            return Err(format!(
                "'{}' is not a valid hunk header. Valid hunk headers: \
                 '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'",
                line.trim()
            ));
        }
    }
    Ok((out, consumed))
}

/// Parse one or more `@@`-delimited change chunks for an Update File hunk.
/// Error strings mirror codex's `streaming_parser` so model self-correction
/// heuristics key off the same substrings.
fn parse_update_chunks(lines: &[&str]) -> Result<(Vec<UpdateFileChunk>, usize), String> {
    let invalid_line = |line: &str| {
        format!(
            "Unexpected line found in update hunk: '{line}'. Every line should start with \
             ' ' (context line), '+' (added line), or '-' (removed line)"
        )
    };
    let mut chunks: Vec<UpdateFileChunk> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let update_line = line.trim_end();

        // End of File marker belongs to the current chunk (note: it
        // starts with `***`, so handle it before the hunk-header break).
        if update_line == EOF_MARKER {
            if chunks
                .last()
                .is_some_and(|c| c.old_lines.is_empty() && c.new_lines.is_empty())
            {
                return Err("Update hunk does not contain any lines".to_string());
            }
            if let Some(c) = chunks.last_mut() {
                c.is_end_of_file = true;
            }
            i += 1;
            continue;
        }

        // A hunk header or End Patch ends this update's chunks.
        if update_line.starts_with("***") {
            break;
        }

        // After an end-of-file marker, only a new @@ context marker (or a
        // hunk header, handled above) may follow.
        if chunks.last().is_some_and(|c| c.is_end_of_file) {
            if update_line.is_empty() {
                i += 1;
                continue;
            }
            if update_line != EMPTY_CHANGE_CONTEXT_MARKER
                && !update_line.starts_with(CHANGE_CONTEXT_MARKER)
            {
                return Err(format!(
                    "Expected update hunk to start with a @@ context marker, got: '{line}'"
                ));
            }
        }

        // @@ context marker starts a new chunk. Reject if the previous
        // chunk is still empty (codex: unexpected line in update hunk).
        if update_line == EMPTY_CHANGE_CONTEXT_MARKER
            || update_line.starts_with(CHANGE_CONTEXT_MARKER)
        {
            if chunks
                .last()
                .is_some_and(|c| c.old_lines.is_empty() && c.new_lines.is_empty())
            {
                return Err(invalid_line(line));
            }
            let ctx = update_line
                .strip_prefix(CHANGE_CONTEXT_MARKER)
                .map(String::from);
            chunks.push(UpdateFileChunk {
                change_context: ctx,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                is_end_of_file: false,
            });
            i += 1;
            continue;
        }

        // Bare empty line → empty context (preserved on both sides).
        if line.is_empty() {
            if chunks.is_empty() {
                chunks.push(UpdateFileChunk {
                    change_context: None,
                    old_lines: Vec::new(),
                    new_lines: Vec::new(),
                    is_end_of_file: false,
                });
            }
            if let Some(c) = chunks.last_mut() {
                c.old_lines.push(String::new());
                c.new_lines.push(String::new());
            }
            i += 1;
            continue;
        }

        // Body line. Auto-open a chunk if none exists yet (lenient: the
        // first chunk may omit its `@@` marker — matches the prior
        // behaviour and codex's empty-line auto-chunk).
        if chunks.is_empty() {
            chunks.push(UpdateFileChunk {
                change_context: None,
                old_lines: Vec::new(),
                new_lines: Vec::new(),
                is_end_of_file: false,
            });
        }
        match line.chars().next() {
            Some(' ') => {
                let body = line[1..].to_string();
                if let Some(c) = chunks.last_mut() {
                    c.old_lines.push(body.clone());
                    c.new_lines.push(body);
                }
            }
            Some('+') => {
                if let Some(c) = chunks.last_mut() {
                    c.new_lines.push(line[1..].to_string());
                }
            }
            Some('-') => {
                if let Some(c) = chunks.last_mut() {
                    c.old_lines.push(line[1..].to_string());
                }
            }
            Some(_) => return Err(invalid_line(line)),
            None => {}
        }
        i += 1;
    }
    Ok((chunks, i))
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
            if let Some(idx) = seek_sequence::seek_sequence(
                original_lines,
                std::slice::from_ref(ctx_line),
                line_index,
                false,
            ) {
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
            found = seek_sequence::seek_sequence(
                original_lines,
                pattern,
                line_index,
                chunk.is_end_of_file,
            );
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

    // Defensive invariant: replacements must be non-overlapping. Each match
    // search starts at `line_index` past the previous match, so this holds by
    // construction today — but `apply_replacements` applies in reverse and a
    // future change to the seek logic could otherwise silently corrupt the file
    // via overlapping edits. Fail closed instead.
    let mut cursor = 0usize;
    for &(start_idx, old_len, _) in &replacements {
        if start_idx < cursor {
            return Err(format!(
                "Overlapping edit detected in {} at line {} (previous edit ended at line {}); refusing to apply a corrupting patch",
                path.display(),
                start_idx,
                cursor
            ));
        }
        cursor = start_idx + old_len;
    }
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

enum PreparedChange {
    Add {
        path: PathBuf,
        contents: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        contents: String,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        contents: String,
    },
}

pub fn apply(patch: &str, ctx: &FsCtx) -> Result<Vec<serde_json::Value>, String> {
    let hunks = parse(patch)?;
    if hunks.is_empty() {
        return Err("No files were modified.".into());
    }

    // Resolve and validate every hunk, including all context matches, before
    // touching the filesystem. A later bad hunk must not leave earlier hunks
    // applied.
    let mut changes = Vec::with_capacity(hunks.len());
    for hunk in &hunks {
        match hunk {
            Hunk::AddFile { path, contents } => {
                let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                ctx.assert_write_allowed(&resolved)?;
                if std::fs::symlink_metadata(&resolved)
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
                {
                    return Err(format!("{} is a directory", resolved.display()));
                }
                changes.push(PreparedChange::Add {
                    path: resolved,
                    contents: contents.clone(),
                });
            }
            Hunk::DeleteFile { path } => {
                let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                ctx.assert_write_allowed(&resolved)?;
                if std::fs::symlink_metadata(&resolved)
                    .map(|m| m.is_dir())
                    .unwrap_or(false)
                {
                    return Err(format!("{} is a directory", resolved.display()));
                }
                // A delete targeting a missing file is an error, not a
                // silent success — the model should learn the path was
                // wrong (codex aborts the patch on NotFound).
                if std::fs::symlink_metadata(&resolved).is_err() {
                    return Err(format!(
                        "Failed to delete file {}: not found",
                        resolved.display()
                    ));
                }
                changes.push(PreparedChange::Delete { path: resolved });
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let resolved = resolve_within(&path.to_string_lossy(), &ctx.workspace)?;
                ctx.assert_write_allowed(&resolved)?;
                // Chain multiple `Update File` hunks (and `Add File` then
                // `Update File`) on the same path: when a prior hunk already
                // has pending in-memory contents for this path, compute
                // replacements against THAT rather than re-reading the
                // original on-disk file. The apply loop writes each pending
                // change once, in order, so without this chaining two
                // `Update File` hunks on the same path would both derive
                // from the original and the second write would silently
                // clobber the first. Matches codex's single-loop semantics
                // (`apply-patch/src/lib.rs:390-559`,
                // `apply_hunks_to_files` → `derive_new_contents_from_chunks`
                // reads the current file then writes immediately, so the
                // next hunk observes the post-write state) while preserving
                // AutoReport's validate-all-then-apply-all model: every hunk
                // is still validated before any disk write, and a later
                // validation failure still leaves earlier hunks un-applied.
                let (base_contents, existing_idx) = match find_pending_for_path(&changes, &resolved)
                {
                    Some(idx) => (
                        changes[idx]
                            .pending_contents()
                            .expect("non-Delete pending change carries contents")
                            .to_owned(),
                        Some(idx),
                    ),
                    None => (
                        std::fs::read_to_string(&resolved).map_err(|e| e.to_string())?,
                        None,
                    ),
                };
                let mut original_lines: Vec<String> =
                    base_contents.split('\n').map(String::from).collect();
                if original_lines.last().is_some_and(String::is_empty) {
                    original_lines.pop();
                }
                let replacements = compute_replacements(&original_lines, &resolved, chunks)?;
                let mut new_lines = apply_replacements(original_lines, &replacements);
                if !new_lines.last().is_some_and(String::is_empty) {
                    new_lines.push(String::new());
                }
                let new_contents = new_lines.join("\n");

                if let Some(idx) = existing_idx {
                    // Fold this hunk's edits into the existing pending change
                    // so the apply loop writes the combined contents once.
                    if let Some(dest) = move_path {
                        let dest_abs = resolve_within(&dest.to_string_lossy(), &ctx.workspace)?;
                        ctx.assert_write_allowed(&dest_abs)?;
                        if resolved == dest_abs {
                            return Err(format!("cannot move {} onto itself", resolved.display()));
                        }
                        if std::fs::symlink_metadata(&dest_abs)
                            .map(|m| m.is_dir())
                            .unwrap_or(false)
                        {
                            return Err(format!("{} is a directory", dest_abs.display()));
                        }
                        changes[idx].set_pending_contents(new_contents.clone());
                        changes.push(PreparedChange::Move {
                            from: resolved,
                            to: dest_abs,
                            contents: new_contents,
                        });
                    } else {
                        changes[idx].set_pending_contents(new_contents);
                    }
                } else if let Some(dest) = move_path {
                    let dest_abs = resolve_within(&dest.to_string_lossy(), &ctx.workspace)?;
                    ctx.assert_write_allowed(&dest_abs)?;
                    if resolved == dest_abs {
                        return Err(format!("cannot move {} onto itself", resolved.display()));
                    }
                    if std::fs::symlink_metadata(&dest_abs)
                        .map(|m| m.is_dir())
                        .unwrap_or(false)
                    {
                        return Err(format!("{} is a directory", dest_abs.display()));
                    }
                    changes.push(PreparedChange::Move {
                        from: resolved,
                        to: dest_abs,
                        contents: new_contents,
                    });
                } else {
                    changes.push(PreparedChange::Update {
                        path: resolved,
                        contents: new_contents,
                    });
                }
            }
        }
    }

    let mut backups = HashMap::<PathBuf, Option<Vec<u8>>>::new();
    for path in changes.iter().flat_map(PreparedChange::paths) {
        if !backups.contains_key(path) {
            let contents = match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_file() => Some(
                    std::fs::read(path)
                        .map_err(|e| format!("reading backup {}: {e}", path.display()))?,
                ),
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!("refusing to modify symlink {}", path.display()));
                }
                Ok(_) => return Err(format!("{} is not a regular file", path.display())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(format!("reading {}: {err}", path.display())),
            };
            backups.insert(path.clone(), contents);
        }
    }

    let mut report = Vec::new();
    let result = (|| {
        for change in &changes {
            match change {
                PreparedChange::Add { path, contents } => {
                    secure_write(ctx, path, contents.as_bytes())?;
                    report.push(json!({"add": path.display().to_string()}));
                }
                PreparedChange::Delete { path } => {
                    ctx.assert_write_allowed(path)?;
                    std::fs::remove_file(path).map_err(|e| e.to_string())?;
                    report.push(json!({"delete": path.display().to_string()}));
                }
                PreparedChange::Update { path, contents } => {
                    secure_write(ctx, path, contents.as_bytes())?;
                    report.push(json!({"update": path.display().to_string()}));
                }
                PreparedChange::Move { from, to, contents } => {
                    secure_write(ctx, to, contents.as_bytes())?;
                    ctx.assert_write_allowed(from)?;
                    std::fs::remove_file(from).map_err(|e| e.to_string())?;
                    report.push(
                        json!({"move": from.display().to_string(), "to": to.display().to_string()}),
                    );
                }
            }
        }
        Ok::<(), String>(())
    })();
    if let Err(err) = result {
        if let Err(restore_err) = restore_backups(&backups) {
            return Err(format!("{err}; rollback failed: {restore_err}"));
        }
        return Err(err);
    }
    Ok(report)
}

impl PreparedChange {
    fn paths(&self) -> Vec<&PathBuf> {
        match self {
            Self::Add { path, .. } | Self::Delete { path } | Self::Update { path, .. } => {
                vec![path]
            }
            Self::Move { from, to, .. } => vec![from, to],
        }
    }

    /// In-memory contents this pending change will write to its target, or
    /// `None` for `Delete`. Used to chain multiple `Update File` hunks (and
    /// `Add File` then `Update File`) on the same path during validation so
    /// the apply loop writes the combined contents exactly once.
    fn pending_contents(&self) -> Option<&str> {
        match self {
            Self::Add { contents, .. } | Self::Update { contents, .. } | Self::Move {
                contents,
                ..
            } => Some(contents),
            Self::Delete { .. } => None,
        }
    }

    fn set_pending_contents(&mut self, new_contents: String) {
        match self {
            Self::Add { contents, .. }
            | Self::Update { contents, .. }
            | Self::Move { contents, .. } => *contents = new_contents,
            Self::Delete { .. } => {}
        }
    }
}

/// Index of the most recent pending change whose result will live at
/// `target` — an `Add`/`Update` on `target`, or a `Move` whose destination is
/// `target`. `Delete` and a `Move`'s source are NOT chaining targets (the
/// file is being removed in both cases). Returns the latest match so the most
/// recent pending state is the chaining base.
fn find_pending_for_path(changes: &[PreparedChange], target: &Path) -> Option<usize> {
    changes.iter().rposition(|c| match c {
        PreparedChange::Add { path, .. } | PreparedChange::Update { path, .. } => path == target,
        PreparedChange::Move { to, .. } => to == target,
        PreparedChange::Delete { .. } => false,
    })
}

fn restore_backups(backups: &HashMap<PathBuf, Option<Vec<u8>>>) -> Result<(), String> {
    for (path, contents) in backups {
        match contents {
            Some(contents) => write_bytes_with_parents(path, contents),
            None => match std::fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_file() => {
                    std::fs::remove_file(path).map_err(|e| e.to_string())
                }
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    std::fs::remove_file(path).map_err(|e| e.to_string())
                }
                Ok(_) => Err(format!("cannot remove non-file {}", path.display())),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err.to_string()),
            },
        }?;
    }
    Ok(())
}

fn write_bytes_with_parents(path: &Path, contents: &[u8]) -> Result<(), String> {
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

/// Write `contents` to `path` with the leaf opened via `O_NOFOLLOW` (Unix) so a
/// path swapped to a symlink between validation and write cannot redirect the
/// write through the symlink target. Falls back to a plain write off-Unix.
#[cfg(unix)]
fn write_file_nofollow(path: &Path, contents: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    opts.custom_flags(libc::O_NOFOLLOW);
    let mut f = opts.open(path).map_err(|e| e.to_string())?;
    f.write_all(contents).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_file_nofollow(path: &Path, contents: &[u8]) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| e.to_string())
}

/// Secured write used by apply_patch. `assert_write_allowed` already ran during
/// hunk validation, but a concurrently-run `exec` command (which is itself
/// sandboxed but can still create symlinks *inside* the writable dir pointing
/// outside) could swap a regular path for a symlink in the gap between
/// validation and write. To shrink that TOCTOU window this re-validates
/// containment immediately before creating parents and writing the leaf, and
/// opens the leaf with `O_NOFOLLOW` so a symlinked leaf is rejected.
fn secure_write(ctx: &FsCtx, path: &Path, contents: &[u8]) -> Result<(), String> {
    ctx.assert_write_allowed(path)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        // create_dir_all follows symlinked intermediate components; after it
        // runs, the canonical parent must still be inside the writable dir.
        ctx.assert_write_allowed(parent)?;
    }
    write_file_nofollow(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ctx(ws: &Path) -> FsCtx {
        FsCtx::new(ws.to_path_buf(), Some(ws.to_path_buf()))
    }

    fn stamp() -> String {
        // Unit tests run in parallel; timestamp resolution is not enough
        // to keep their temporary workspaces distinct on every platform.
        uuid::Uuid::new_v4().to_string()
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
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "alpha\nBETA\ngamma\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "hello\nworld\n"
        );
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
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_end_patch_is_rejected() {
        // Regression: a patch without `*** End Patch` (e.g. truncated by the
        // model) must error, not silently apply partial hunks.
        let dir = std::env::temp_dir().join(format!("ap-end-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+hi\n";
        let err = apply(patch, &ctx(&dir)).unwrap_err();
        assert!(
            err.contains("must be '*** End Patch'"),
            "expected end-patch error, got: {err}"
        );
        assert!(!dir.join("a.txt").exists(), "no partial write on error");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn dirty_add_body_line_is_rejected() {
        // A non-`+` line in an Add File body is an error, not a silent
        // blank-line corruption (codex streaming_parser InvalidHunkError).
        let dir = std::env::temp_dir().join(format!("ap-dirty-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch = "*** Begin Patch\n*** Add File: a.txt\n+ok\nbare line\n*** End Patch";
        let err = apply(patch, &ctx(&dir)).unwrap_err();
        assert!(
            err.contains("is not a valid hunk header"),
            "expected invalid-hunk-header error, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_missing_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("ap-del-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch = "*** Begin Patch\n*** Delete File: nope.txt\n*** End Patch";
        let err = apply(patch, &ctx(&dir)).unwrap_err();
        assert!(
            err.contains("not found"),
            "expected not-found error, got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn later_hunk_failure_does_not_partially_apply_earlier_hunks() {
        let dir = std::env::temp_dir().join(format!("ap-rollback-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "old\n").unwrap();
        std::fs::write(dir.join("b.txt"), "keep\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: a.txt
@@
-old
+new
*** Update File: b.txt
@@
-missing
+changed
*** End Patch";

        let err = apply(patch, &ctx(&dir)).unwrap_err();
        assert!(err.contains("Failed to find expected lines"));
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "old\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("b.txt")).unwrap(),
            "keep\n"
        );
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
        assert_eq!(
            std::fs::read_to_string(dir.join("g.txt")).unwrap(),
            "foo\nBAR\n"
        );
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
        assert_eq!(
            std::fs::read_to_string(dir.join("new.txt")).unwrap(),
            "X\ny\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[allow(dead_code)]
    fn _pb(_: PathBuf) {}

    #[test]
    fn leading_newline_before_begin_patch_is_accepted() {
        // Regression (Bug I3): codex does `patch.trim().lines().collect()`
        // (`apply-patch/src/parser.rs:179`), so a leading `\n` before
        // `*** Begin Patch` (and/or two+ trailing newlines after
        // `*** End Patch`) must parse. Without the trim the strict boundary
        // check rejected these even though codex-trained models emit them.
        let dir = std::env::temp_dir().join(format!("ap-trim-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        let patch = "\n*** Begin Patch\n*** Update File: a.txt\n@@\n-alpha\n+ALPHA\n*** End Patch\n\n";
        apply(patch, &ctx(&dir)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("a.txt")).unwrap(),
            "ALPHA\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trim_strips_surrounding_blank_lines_at_parse_layer() {
        // Parse-layer coverage for Bug I3: a body that is otherwise empty
        // must yield zero hunks once the surrounding blank lines are trimmed.
        let patch = "\n*** Begin Patch\n*** End Patch\n\n";
        let hunks = parse(patch).unwrap();
        assert!(hunks.is_empty(), "expected no hunks, got {hunks:?}");
    }

    #[test]
    fn two_update_hunks_on_same_file_chain() {
        // Regression (Bug I4): two `*** Update File` hunks editing different
        // lines of the same file must BOTH land. Previously the validate-all
        // loop read the ORIGINAL file for each hunk, so the apply loop wrote
        // both in order and the second silently clobbered the first.
        let dir = std::env::temp_dir().join(format!("ap-chain-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c.txt"), "alpha\nbeta\ngamma\ndelta\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: c.txt
@@
-alpha
+ALPHA
*** Update File: c.txt
@@
-delta
+DELTA
*** End Patch";
        apply(patch, &ctx(&dir)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("c.txt")).unwrap(),
            "ALPHA\nbeta\ngamma\nDELTA\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_then_update_on_same_file_chains() {
        // Regression (Bug I4, Add-then-Update variant): an `Add File`
        // immediately followed by `Update File` on the same path must read
        // the just-added in-memory contents. Without chaining the Update
        // read the (non-existent) on-disk file and failed with NotFound.
        let dir = std::env::temp_dir().join(format!("ap-addup-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        let patch = "*** Begin Patch
*** Add File: d.txt
+one
+two
*** Update File: d.txt
@@
-one
+ONE
*** End Patch";
        apply(patch, &ctx(&dir)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("d.txt")).unwrap(),
            "ONE\ntwo\n"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chained_update_hunks_still_rollback_on_later_failure() {
        // Chaining must not weaken validate-all-then-apply-all: a later
        // hunk that fails validation must leave the file untouched, even
        // when earlier chained Update hunks on the same path had pending
        // contents computed.
        let dir = std::env::temp_dir().join(format!("ap-crollback-{}", stamp()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("c.txt"), "alpha\nbeta\n").unwrap();
        let patch = "*** Begin Patch
*** Update File: c.txt
@@
-alpha
+ALPHA
*** Update File: c.txt
@@
-missing
+changed
*** End Patch";
        let err = apply(patch, &ctx(&dir)).unwrap_err();
        assert!(
            err.contains("Failed to find expected lines"),
            "expected seek failure, got: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("c.txt")).unwrap(),
            "alpha\nbeta\n",
            "no partial write on validation failure"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[test]
fn trailing_whitespace_on_end_marker_is_accepted() {
    let patch = "*** Begin Patch\n*** Add File: a.txt\n+ok\n*** End Patch   ";
    let hunks = parse(patch).unwrap();
    assert_eq!(hunks.len(), 1);
}
