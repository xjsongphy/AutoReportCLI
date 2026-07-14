//! `list_dir` tool — paginated, depth-bounded directory listing.
//!
//! Ported from codex's `codex-rs/core/src/tools/handlers/list_dir.rs`. The
//! codex original depends on `codex_protocol::permissions::ReadDenyMatcher`
//! and `codex_utils_string`; we drop the deny-matcher (the CLI's workspace IS
//! the sandbox — reads are unrestricted within it via `resolve_within`) and
//! inline a char-boundary truncation helper. The listing algorithm (BFS by
//! depth, sorted entries, `/` `@` `?` type markers, offset/limit pagination
//! with a "More than N entries" sentinel) is verbatim from codex.

use crate::file_tools::{FsCtx, resolve_within};
use crate::registry::{Tool, ToolOutput};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::OsStr;
use std::fs::FileType;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_ENTRY_LENGTH: usize = 500;
const INDENTATION_SPACES: usize = 2;

pub struct ListDirTool {
    ctx: FsCtx,
}

impl ListDirTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }

    pub fn make(ctx: FsCtx) -> Arc<dyn Tool> {
        Arc::new(Self::new(ctx))
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List the contents of a directory (paginated, recursive up to `depth`). \
         Returns a sorted tree with `/` marking directories, `@` symlinks, `?` \
         other. Use this to explore the project layout before reading specific files."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory path (absolute, or relative to the workspace)."},
                "offset": {"type": "integer", "minimum": 1, "default": 1, "description": "1-indexed entry to start at."},
                "limit": {"type": "integer", "minimum": 1, "default": 25, "description": "Maximum entries to return."},
                "depth": {"type": "integer", "minimum": 1, "default": 2, "description": "How deep to recurse."}
            },
            "required": ["path"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let path = match args.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return ToolOutput::err("path is required"),
        };
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(25) as usize;
        let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;

        if offset == 0 {
            return ToolOutput::err("offset must be a 1-indexed entry number");
        }
        if limit == 0 {
            return ToolOutput::err("limit must be greater than zero");
        }
        if depth == 0 {
            return ToolOutput::err("depth must be greater than zero");
        }

        let resolved = match resolve_within(path, &self.ctx.workspace) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };

        let entries = match list_dir_slice(&resolved, offset, limit, depth) {
            Ok(e) => e,
            Err(e) => return ToolOutput::err(e.to_string()),
        };
        let mut output = Vec::with_capacity(entries.len() + 1);
        output.push(format!("Absolute path: {}", resolved.display()));
        output.extend(entries);
        ToolOutput::ok(Value::String(output.join("\n")))
    }
}

fn list_dir_slice(
    path: &Path,
    offset: usize,
    limit: usize,
    depth: usize,
) -> std::io::Result<Vec<String>> {
    let mut entries = Vec::new();
    collect_entries(path, Path::new(""), depth, &mut entries)?;

    if entries.is_empty() {
        return Ok(Vec::new());
    }

    entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));

    let start_index = offset - 1;
    if start_index >= entries.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "offset exceeds directory entry count",
        ));
    }

    let remaining_entries = entries.len() - start_index;
    let capped_limit = limit.min(remaining_entries);
    let end_index = start_index + capped_limit;
    let selected_entries = &entries[start_index..end_index];
    let mut formatted = Vec::with_capacity(selected_entries.len());
    for entry in selected_entries {
        formatted.push(format_entry_line(entry));
    }
    if end_index < entries.len() {
        formatted.push(format!("More than {capped_limit} entries found"));
    }
    Ok(formatted)
}

fn collect_entries(
    dir_path: &Path,
    relative_prefix: &Path,
    depth: usize,
    entries: &mut Vec<DirEntry>,
) -> std::io::Result<()> {
    let mut queue = VecDeque::new();
    queue.push_back((dir_path.to_path_buf(), relative_prefix.to_path_buf(), depth));

    while let Some((current_dir, prefix, remaining_depth)) = queue.pop_front() {
        let read_dir = std::fs::read_dir(&current_dir)?;
        let mut dir_entries = Vec::new();

        for entry in read_dir {
            let entry = entry?;
            let entry_path = entry.path();
            let file_type = entry.file_type()?;
            let file_name = entry.file_name();
            let relative_path = if prefix.as_os_str().is_empty() {
                PathBuf::from(&file_name)
            } else {
                prefix.join(&file_name)
            };

            let display_name = format_entry_component(&file_name);
            let display_depth = prefix.components().count();
            let sort_key = format_entry_name(&relative_path);
            let kind = DirEntryKind::from(&file_type);
            dir_entries.push((
                entry_path,
                relative_path,
                kind,
                DirEntry {
                    name: sort_key,
                    display_name,
                    depth: display_depth,
                    kind,
                },
            ));
        }

        dir_entries.sort_unstable_by(|a, b| a.3.name.cmp(&b.3.name));
        for (entry_path, relative_path, kind, dir_entry) in dir_entries {
            if kind == DirEntryKind::Directory && remaining_depth > 1 {
                queue.push_back((entry_path, relative_path, remaining_depth - 1));
            }
            entries.push(dir_entry);
        }
    }
    Ok(())
}

/// Truncate at a UTF-8 char boundary near `max_bytes` (stand-in for codex's
/// `take_bytes_at_char_boundary`).
fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn format_entry_name(path: &Path) -> String {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.len() > MAX_ENTRY_LENGTH {
        truncate_at_char_boundary(&normalized, MAX_ENTRY_LENGTH).to_string()
    } else {
        normalized
    }
}

fn format_entry_component(name: &OsStr) -> String {
    let normalized = name.to_string_lossy();
    if normalized.len() > MAX_ENTRY_LENGTH {
        truncate_at_char_boundary(&normalized, MAX_ENTRY_LENGTH).to_string()
    } else {
        normalized.to_string()
    }
}

fn format_entry_line(entry: &DirEntry) -> String {
    let indent = " ".repeat(entry.depth * INDENTATION_SPACES);
    let mut name = entry.display_name.clone();
    match entry.kind {
        DirEntryKind::Directory => name.push('/'),
        DirEntryKind::Symlink => name.push('@'),
        DirEntryKind::Other => name.push('?'),
        DirEntryKind::File => {}
    }
    format!("{indent}{name}")
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    display_name: String,
    depth: usize,
    kind: DirEntryKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DirEntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

impl From<&FileType> for DirEntryKind {
    fn from(file_type: &FileType) -> Self {
        if file_type.is_symlink() {
            DirEntryKind::Symlink
        } else if file_type.is_dir() {
            DirEntryKind::Directory
        } else if file_type.is_file() {
            DirEntryKind::File
        } else {
            DirEntryKind::Other
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_dir_paginates_and_marks_dirs() {
        let tmp = std::env::temp_dir().join(format!("listdir-{}", stamp()));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.txt"), "x").unwrap();
        std::fs::write(tmp.join("sub").join("b.txt"), "y").unwrap();

        let ctx = FsCtx::new(tmp.clone(), None);
        let tool = ListDirTool::new(ctx);
        let out = futures::executor::block_on(tool.call(&json!({"path": ".", "depth": 2})));
        assert!(out.error.is_none());
        let text = out.result.as_str().unwrap();
        assert!(text.contains("Absolute path:"));
        assert!(text.contains("a.txt"));
        assert!(text.contains("sub/"));
        assert!(text.contains("b.txt"));

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn list_dir_offset_beyond_range_errors() {
        let tmp = std::env::temp_dir().join(format!("listdir2-{}", stamp()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("only.txt"), "x").unwrap();
        let ctx = FsCtx::new(tmp.clone(), None);
        let tool = ListDirTool::new(ctx);
        let out = futures::executor::block_on(tool.call(&json!({"path": ".", "offset": 99})));
        assert!(out.error.is_some());
        std::fs::remove_dir_all(&tmp).ok();
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
