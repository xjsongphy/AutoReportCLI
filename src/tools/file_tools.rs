//! File tools: `read`, `write_file`, `edit_file`, `delete_file`.
//!
//! Read access spans the whole workspace; write access is confined to the
//! agent's assigned directory (see [`AgentType::write_dir`]). Internal
//! `.autoreport` metadata is never writable.

use crate::tools::registry::{arg_opt_bool, arg_opt_u64, arg_str, Tool, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Shared filesystem context for the file tools.
#[derive(Clone)]
pub struct FsCtx {
    pub workspace: PathBuf,
    pub write_dir: Option<PathBuf>,
}

impl FsCtx {
    pub fn new(workspace: PathBuf, write_dir: Option<PathBuf>) -> Self {
        Self { workspace, write_dir }
    }
}

/// Resolve a possibly-relative path against the workspace and verify the
/// result stays inside the workspace. `..` components are collapsed lexically
/// (no symlink following) so we never escape the project root.
pub fn resolve_within(path: &str, workspace: &Path) -> Result<PathBuf, String> {
    let p = Path::new(path);
    let joined: PathBuf = if p.is_absolute() {
        normalize(p)
    } else {
        normalize(&workspace.join(p))
    };
    // Must be the workspace itself or a descendant.
    if joined == *workspace || joined.starts_with(workspace) {
        Ok(joined)
    } else {
        Err(format!(
            "path '{}' escapes the workspace",
            workspace.display()
        ))
    }
}

fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn check_write(ctx: &FsCtx, target: &Path) -> Result<(), String> {
    // Never allow touching internal metadata.
    let metadata = ctx.workspace.join(".autoreport");
    if target == metadata || target.starts_with(&metadata) {
        return Err("writing inside .autoreport is not permitted".to_string());
    }
    match &ctx.write_dir {
        Some(dir) if target.starts_with(dir) => Ok(()),
        Some(dir) => Err(format!(
            "this agent may only write under {}; '{}' is outside it",
            dir.display(),
            target.display()
        )),
        None => Err("this agent has no write access".to_string()),
    }
}

// ---------------------------------------------------------------------------
// read
// ---------------------------------------------------------------------------

pub struct ReadTool {
    ctx: FsCtx,
}

impl ReadTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file (optionally a line range) or list a directory's contents. Paths are relative to the project root."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File or directory path relative to project root."},
                "offset": {"type": "integer", "description": "Starting line (1-based) for partial reads.", "minimum": 1},
                "limit": {"type": "integer", "description": "Maximum number of lines to read.", "minimum": 1},
                "recursive": {"type": "boolean", "description": "When reading a directory, recurse into subdirectories."}
            },
            "required": ["path"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let path = match arg_str(args, "path") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let offset = arg_opt_u64(args, "offset").map(|v| v as usize).unwrap_or(0);
        let limit = arg_opt_u64(args, "limit").map(|v| v as usize);
        let recursive = arg_opt_bool(args, "recursive").unwrap_or(false);

        let resolved = match resolve_within(&path, &self.ctx.workspace) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };

        if resolved.is_dir() {
            return list_dir(&resolved, recursive);
        }
        if !resolved.exists() {
            return ToolOutput::err(format!("file not found: {}", resolved.display()));
        }

        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("read failed: {e}")),
        };
        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();
        let start = if offset > 0 { offset - 1 } else { 0 };
        let end = match limit {
            Some(n) => (start + n).min(total),
            None => total,
        };
        let body: String = lines[start.min(total)..end]
            .iter()
            .map(|l| format!("{l}\n"))
            .collect();
        ToolOutput::ok(json!({
            "path": resolved.display().to_string(),
            "content": body,
            "start_line": start + 1,
            "end_line": end,
            "line_count": total,
        }))
    }
}

fn list_dir(dir: &Path, recursive: bool) -> ToolOutput {
    fn walk(dir: &Path, recursive: bool, out: &mut Vec<Value>, depth: usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut names: Vec<(std::ffi::OsString, bool)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                // hide internal metadata from listings
                let nm = e.file_name();
                nm != ".autoreport"
            })
            .map(|e| (e.file_name(), e.path().is_dir()))
            .collect();
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, is_dir) in names {
            let path = dir.join(&name);
            out.push(json!({
                "path": path.display().to_string(),
                "name": name.to_string_lossy(),
                "type": if is_dir { "dir" } else { "file" },
                "depth": depth,
            }));
            if recursive && is_dir {
                walk(&path, recursive, out, depth + 1);
            }
        }
    }
    let mut entries = Vec::new();
    walk(dir, recursive, &mut entries, 0);
    ToolOutput::ok(json!({
        "path": dir.display().to_string(),
        "entries": entries,
    }))
}

// ---------------------------------------------------------------------------
// write_file
// ---------------------------------------------------------------------------

pub struct WriteFileTool {
    ctx: FsCtx,
}

impl WriteFileTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write UTF-8 content to a file (overwriting). Parent directories are created as needed. Restricted to this agent's write directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let path = match arg_str(args, "path") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let content = match args.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return ToolOutput::err("missing 'content'"),
        };
        let resolved = match resolve_within(&path, &self.ctx.workspace) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = check_write(&self.ctx, &resolved) {
            return ToolOutput::err(e);
        }
        if let Some(parent) = resolved.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::err(format!("create parent dir: {e}"));
            }
        }
        if let Err(e) = std::fs::write(&resolved, content) {
            return ToolOutput::err(format!("write failed: {e}"));
        }
        ToolOutput::ok(json!({
            "path": resolved.display().to_string(),
            "success": true,
        }))
    }
}

// ---------------------------------------------------------------------------
// edit_file
// ---------------------------------------------------------------------------

pub struct EditFileTool {
    ctx: FsCtx,
}

impl EditFileTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "Replace the first (or all) occurrence of `old_text` with `new_text` in a file. Restricted to this agent's write directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "old_text": {"type": "string"},
                "new_text": {"type": "string"},
                "replace_all": {"type": "boolean", "description": "Replace every occurrence (default: first only)."}
            },
            "required": ["path", "old_text", "new_text"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let path = match arg_str(args, "path") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let old_text = match arg_str(args, "old_text") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let new_text = match arg_str(args, "new_text") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let replace_all = arg_opt_bool(args, "replace_all").unwrap_or(false);

        let resolved = match resolve_within(&path, &self.ctx.workspace) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = check_write(&self.ctx, &resolved) {
            return ToolOutput::err(e);
        }
        if !resolved.exists() {
            return ToolOutput::err(format!("file not found: {}", resolved.display()));
        }
        let content = match std::fs::read_to_string(&resolved) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("read failed: {e}")),
        };

        if !content.contains(&old_text) {
            return ToolOutput::err(format!(
                "old_text not found in {} ({} bytes). No change made.",
                resolved.display(),
                content.len()
            ));
        }
        let new_content = if replace_all {
            content.replace(&old_text, &new_text)
        } else {
            content.replacen(&old_text, &new_text, 1)
        };
        if let Err(e) = std::fs::write(&resolved, &new_content) {
            return ToolOutput::err(format!("write failed: {e}"));
        }
        ToolOutput::ok(json!({
            "path": resolved.display().to_string(),
            "success": true,
            "replacements_made": if replace_all { content.matches(&old_text).count() } else { 1 },
        }))
    }
}

// ---------------------------------------------------------------------------
// delete_file
// ---------------------------------------------------------------------------

pub struct DeleteFileTool {
    ctx: FsCtx,
}

impl DeleteFileTool {
    pub fn new(ctx: FsCtx) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for DeleteFileTool {
    fn name(&self) -> &str {
        "delete_file"
    }
    fn description(&self) -> &str {
        "Delete a single file. Restricted to this agent's write directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let path = match arg_str(args, "path") {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let resolved = match resolve_within(&path, &self.ctx.workspace) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = check_write(&self.ctx, &resolved) {
            return ToolOutput::err(e);
        }
        if !resolved.exists() {
            return ToolOutput::err(format!("file not found: {}", resolved.display()));
        }
        if resolved.is_dir() {
            return ToolOutput::err("delete_file only removes files, not directories");
        }
        match std::fs::remove_file(&resolved) {
            Ok(_) => ToolOutput::ok(json!({
                "path": resolved.display().to_string(),
                "deleted": true,
            })),
            Err(e) => ToolOutput::err(format!("delete failed: {e}")),
        }
    }
}

/// Convenience constructor for the standard file-tool bundle an agent receives.
pub fn bundle(ctx: FsCtx) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ReadTool::new(ctx.clone())),
        Arc::new(WriteFileTool::new(ctx.clone())),
        Arc::new(EditFileTool::new(ctx.clone())),
        Arc::new(DeleteFileTool::new(ctx)),
    ]
}
