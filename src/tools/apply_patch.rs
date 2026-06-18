//! `apply_patch` tool — codex-compatible patch application. Accepts the
//! `*** Begin Patch … *** End Patch` format codex uses (Add/Update/Delete File
//! with `+`/`-`/` ` hunk lines), applying it to the workspace under the same
//! per-agent write-directory isolation as the other file tools. Logic ported
//! from codex's `codex-apply-patch` parser (kept self-contained — no tree-sitter
//! dependency needed for our context/replace matching).

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
        "Apply a codex-style patch (*** Begin Patch ... *** End Patch) to add, update, or delete files. Prefer this over many edit_file calls for multi-location changes."
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
        match crate::tools::apply_patch::engine::apply(&patch, &self.ctx) {
            Ok(report) => ToolOutput::ok(json!({"applied": report})),
            Err(e) => ToolOutput::err(e),
        }
    }
}

pub fn make(ctx: FsCtx) -> Arc<dyn Tool> {
    Arc::new(ApplyPatchTool::new(ctx))
}

pub mod engine {
    //! Patch parser + applier.

    use super::FsCtx;
    use crate::tools::file_tools::resolve_within;
    use serde_json::json;

    /// One parsed patch operation.
    enum Op {
        Add { path: String, content: String },
        Update { path: String, hunks: Vec<Hunk> },
        Delete { path: String },
    }

    struct Hunk {
        before: Vec<String>,
        after: Vec<String>,
    }

    pub fn apply(patch: &str, ctx: &FsCtx) -> Result<Vec<serde_json::Value>, String> {
        let ops = parse(patch)?;
        let mut report = Vec::new();
        for op in ops {
            match op {
                Op::Add { path, content } => {
                    let resolved = resolve_within(&path, &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    if let Some(parent) = resolved.parent() {
                        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                    }
                    std::fs::write(&resolved, content).map_err(|e| e.to_string())?;
                    report.push(json!({"add": resolved.display().to_string()}));
                }
                Op::Update { path, hunks } => {
                    let resolved = resolve_within(&path, &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    let mut content =
                        std::fs::read_to_string(&resolved).map_err(|e| e.to_string())?;
                    let mut applied = 0;
                    for h in &hunks {
                        let before = h.before.join("\n");
                        let after = h.after.join("\n");
                        if before.is_empty() {
                            // Pure addition without anchor: append.
                            if !content.ends_with('\n') && !content.is_empty() {
                                content.push('\n');
                            }
                            content.push_str(&after);
                            applied += 1;
                            continue;
                        }
                        if let Some(idx) = content.find(&before) {
                            content.replace_range(idx..idx + before.len(), &after);
                            applied += 1;
                        } else {
                            return Err(format!(
                                "hunk context not found in {}: {}",
                                resolved.display(),
                                before.chars().take(80).collect::<String>()
                            ));
                        }
                    }
                    std::fs::write(&resolved, content).map_err(|e| e.to_string())?;
                    report.push(json!({"update": resolved.display().to_string(), "hunks": applied}));
                }
                Op::Delete { path } => {
                    let resolved = resolve_within(&path, &ctx.workspace)?;
                    check_write(ctx, &resolved)?;
                    if resolved.exists() {
                        std::fs::remove_file(&resolved).map_err(|e| e.to_string())?;
                    }
                    report.push(json!({"delete": resolved.display().to_string()}));
                }
            }
        }
        Ok(report)
    }

    fn check_write(ctx: &FsCtx, target: &std::path::Path) -> Result<(), String> {
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

    fn parse(patch: &str) -> Result<Vec<Op>, String> {
        let mut lines = patch.lines().peekable();
        // Skip to the Begin Patch marker.
        let mut began = false;
        for line in lines.by_ref() {
            let t = line.trim();
            if t == "*** Begin Patch" {
                began = true;
                break;
            }
        }
        if !began {
            return Err("patch must contain `*** Begin Patch`".into());
        }

        let mut ops = Vec::new();
        while let Some(line) = lines.next() {
            let t = line.trim_end();
            if t == "*** End Patch" {
                break;
            }
            if let Some(p) = t.strip_prefix("*** Add File: ") {
                let content = collect_add_body(&mut lines);
                ops.push(Op::Add {
                    path: p.trim().to_string(),
                    content,
                });
            } else if let Some(p) = t.strip_prefix("*** Delete File: ") {
                ops.push(Op::Delete {
                    path: p.trim().to_string(),
                });
            } else if let Some(p) = t.strip_prefix("*** Update File: ") {
                let hunks = collect_update_body(&mut lines);
                ops.push(Op::Update {
                    path: p.trim().to_string(),
                    hunks,
                });
            }
            // ignore *** End File: markers and stray lines
        }
        Ok(ops)
    }

    /// Collect `+`-prefixed lines until the next `***` directive.
    fn collect_add_body<'a, I: Iterator<Item = &'a str>>(lines: &mut std::iter::Peekable<I>) -> String {
        let mut out = String::new();
        while let Some(next) = lines.peek() {
            if next.trim_start().starts_with("***") {
                break;
            }
            let line = lines.next().unwrap();
            if let Some(rest) = line.strip_prefix('+') {
                out.push_str(rest);
            } else if line.is_empty() {
                out.push('\n');
                continue;
            }
            out.push('\n');
        }
        out
    }

    /// Collect hunk lines until the next `***` directive, splitting into hunks
    /// on `@@` anchors. Each hunk: `before` = context+removed, `after` =
    /// context+added.
    fn collect_update_body<'a, I: Iterator<Item = &'a str>>(
        lines: &mut std::iter::Peekable<I>,
    ) -> Vec<Hunk> {
        let mut raw: Vec<&str> = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim_start().starts_with("***") {
                break;
            }
            raw.push(lines.next().unwrap());
        }

        // Split into segments at standalone @@ lines.
        let mut segments: Vec<Vec<&str>> = vec![Vec::new()];
        for l in raw {
            if l.starts_with("@@") {
                segments.push(Vec::new());
            } else {
                segments.last_mut().unwrap().push(l);
            }
        }

        let mut hunks = Vec::new();
        for seg in segments {
            if seg.is_empty() {
                continue;
            }
            let mut before = Vec::new();
            let mut after = Vec::new();
            for line in seg {
                if let Some(rest) = line.strip_prefix('+') {
                    after.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix('-') {
                    before.push(rest.to_string());
                } else if let Some(rest) = line.strip_prefix(' ') {
                    before.push(rest.to_string());
                    after.push(rest.to_string());
                } else {
                    // bare line (no prefix) → treat as context on both sides
                    before.push(line.to_string());
                    after.push(line.to_string());
                }
            }
            hunks.push(Hunk { before, after });
        }
        hunks
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::path::PathBuf;

        fn ctx(ws: &std::path::Path) -> FsCtx {
            FsCtx::new(ws.to_path_buf(), Some(ws.to_path_buf()))
        }

        #[test]
        fn add_and_update_and_delete() {
            let dir = std::env::temp_dir().join(format!("ap-{}", nix()));
            std::fs::create_dir_all(&dir).unwrap();
            let ws = &dir;
            std::fs::write(ws.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

            let patch = "*** Begin Patch
*** Update File: a.txt
 alpha
-beta
+BETA
 gamma
*** Add File: b.txt
+hello
+world
*** Delete File: a.txt
*** End Patch";
            // Note: delete comes after update — update writes, then delete removes.
            let report = apply(patch, &ctx(ws)).unwrap();
            assert!(report.iter().any(|r| r.get("update").is_some()));
            // b.txt created
            assert_eq!(std::fs::read_to_string(ws.join("b.txt")).unwrap(), "hello\nworld\n");
            // a.txt deleted
            assert!(!ws.join("a.txt").exists());

            std::fs::remove_dir_all(&dir).ok();
        }

        fn nix() -> String {
            use std::time::SystemTime;
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                .to_string()
        }

        // silence unused PathBuf import in some toolchains
        #[allow(dead_code)]
        fn _pb(_: PathBuf) {}
    }
}
