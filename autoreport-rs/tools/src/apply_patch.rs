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

use crate::file_tools::FsCtx;
use crate::registry::{Tool, ToolOutput, arg_str};
use async_trait::async_trait;
use serde_json::{Value, json};
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

/// Parser, sequence matching, and filesystem application for the patch
/// protocol. Kept separate from the model-tool adapter above so the engine is
/// independently testable and reusable.
mod engine;
