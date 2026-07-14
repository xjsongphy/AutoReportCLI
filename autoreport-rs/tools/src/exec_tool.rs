//! Shell execution tool with an allowlist-based security policy.
//!
//! Commands are validated as a plain shell command sequence, then executed
//! through the same detected shell backend used by the rest of the CLI. We also
//! snapshot the workspace before and after execution to reject writes outside
//! the agent's allowed write directory.

use crate::codex_shell::{CodexShell, validate_command_for_shell};
use crate::file_tools::{FsCtx, resolve_within};
use crate::registry::{Tool, ToolOutput, arg_str};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Programs an agent may invoke. Build/plot/compute oriented.
fn allowlist() -> HashSet<&'static str> {
    [
        "python",
        "python3",
        "pip",
        "pip3",
        "uv",
        "xelatex",
        "lualatex",
        "pdflatex",
        "latexmk",
        "bibtex",
        "makeindex",
        "ls",
        "cat",
        "head",
        "tail",
        "grep",
        "rg",
        "find",
        "wc",
        "sort",
        "uniq",
        "cut",
        "tr",
        "sed",
        "awk",
        "cp",
        "mv",
        "rm",
        "mkdir",
        "touch",
        "rmdir",
        "chmod",
        "git",
        "echo",
        "printf",
        "which",
        "fc-list",
        "mineru-open-api",
    ]
    .into_iter()
    .collect()
}

pub struct ExecTool {
    ctx: FsCtx,
    timeout: Duration,
    shell: Arc<CodexShell>,
    sandbox: Option<autoreport_sandboxing::SandboxSpec>,
}

impl ExecTool {
    pub fn new(ctx: FsCtx, timeout_secs: u64) -> Self {
        Self {
            ctx,
            timeout: Duration::from_secs(timeout_secs),
            shell: Arc::new(CodexShell::new()),
            sandbox: None,
        }
    }

    /// Attach an OS-level sandbox scoped to this tool's agent write directory.
    pub fn with_sandbox(mut self, sandbox: autoreport_sandboxing::SandboxSpec) -> Self {
        self.sandbox = Some(sandbox);
        self
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }
    fn description(&self) -> &str {
        "Run an allow-listed shell command in the project root via the detected user shell. Writes inside the workspace are restricted to this agent's write directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Full shell command line, e.g. `python3 analyze.py && rg result Data/Processed`."},
                "command_description": {"type": "string", "description": "Short human description of what this does."}
            },
            "required": ["command"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        let command = match arg_str(args, "command") {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = block_internal_paths(&command) {
            return ToolOutput::err(e);
        }
        let parsed = match validate_command_for_shell(
            &command,
            &allowlist(),
            self.shell.detected_shell().shell_type,
        ) {
            Ok(commands) => commands,
            Err(e) => return ToolOutput::err(e),
        };
        let referenced_paths = match detect_workspace_paths(&parsed, &self.ctx.workspace) {
            Ok(paths) => paths,
            Err(e) => return ToolOutput::err(e),
        };
        if let Err(e) = validate_declared_write_targets(&self.ctx, &parsed) {
            return ToolOutput::err(e);
        }
        let before = match WorkspaceSnapshot::capture(&self.ctx.workspace) {
            Ok(snapshot) => snapshot,
            Err(e) => return ToolOutput::err(format!("failed to snapshot workspace: {e}")),
        };
        let output = match self
            .shell
            .run(
                &self.ctx.workspace,
                &command,
                self.timeout,
                None,
                &HashMap::new(),
                self.sandbox.as_ref(),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => return ToolOutput::err(e),
        };
        let after = match WorkspaceSnapshot::capture(&self.ctx.workspace) {
            Ok(snapshot) => snapshot,
            Err(e) => {
                return ToolOutput::err(format!("failed to snapshot workspace after exec: {e}"));
            }
        };
        let written_paths = after.diff(&before);
        if let Err(e) = ensure_writes_are_isolated(&self.ctx, &written_paths) {
            return ToolOutput::err(e);
        }
        ToolOutput::ok(json!({
            "command": command,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "returncode": output.returncode,
            "timed_out": output.timed_out,
            "shell": self.shell.detected_shell().name(),
            "allowed_write_dir": self.ctx.allowed_write_dir().map(|p| p.display().to_string()),
            "referenced_paths": referenced_paths.into_iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "written_paths": written_paths.into_iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        }))
    }
}

fn block_internal_paths(command: &str) -> Result<(), String> {
    // Reject any token that references the internal `.autoreport` metadata
    // tree. Tokenize by whitespace (respecting quotes) so this is a
    // path-aware check, not a brittle whole-string substring match — a
    // command like `cat report.autoreport-notes.txt` is fine, while
    // `cat .autoreport/sessions/x.jsonl` is blocked.
    for token in command.split_whitespace() {
        let mut probe = token.trim_matches(|c: char| c == '"' || c == '\'');
        // Drop leading `./` so `.autoreport` and `./.autoreport` match.
        if let Some(rest) = probe.strip_prefix("./") {
            probe = rest;
        }
        if probe == ".autoreport"
            || probe.starts_with(".autoreport/")
            || probe.starts_with(".autoreport\\")
        {
            return Err("accessing .autoreport metadata via exec is not permitted".to_string());
        }
    }
    Ok(())
}

fn detect_workspace_paths(
    commands: &[Vec<String>],
    workspace: &Path,
) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    for command in commands {
        for arg in command.iter().skip(1) {
            if let Some(path) = maybe_workspace_path(arg, workspace)? {
                out.push(path);
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn maybe_workspace_path(arg: &str, workspace: &Path) -> Result<Option<PathBuf>, String> {
    if arg.starts_with('-') {
        return Ok(None);
    }
    let looks_like_path = arg.starts_with('/')
        || arg.starts_with("./")
        || arg.starts_with("../")
        || arg.contains('/')
        || arg.contains('\\');
    if !looks_like_path {
        let candidate = workspace.join(arg);
        if candidate.exists() {
            return Ok(Some(candidate));
        }
        return Ok(None);
    }
    match resolve_within(arg, workspace) {
        Ok(path) => Ok(Some(path)),
        Err(e) => Err(e),
    }
}

fn ensure_writes_are_isolated(ctx: &FsCtx, written_paths: &[PathBuf]) -> Result<(), String> {
    for path in written_paths {
        ctx.assert_write_allowed(path).map_err(|_| {
            format!(
                "exec modified '{}' outside the allowed write directory",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn validate_declared_write_targets(ctx: &FsCtx, commands: &[Vec<String>]) -> Result<(), String> {
    for command in commands {
        let Some(program) = command.first() else {
            continue;
        };
        let base = std::path::Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(program);
        let mut targets = Vec::new();
        match base {
            "touch" | "mkdir" | "rm" | "rmdir" | "chmod" => {
                targets.extend(
                    command
                        .iter()
                        .skip(1)
                        .filter(|arg| !arg.starts_with('-'))
                        .cloned(),
                );
            }
            "cp" | "mv" => {
                if let Some(last) = command.iter().rev().find(|arg| !arg.starts_with('-')) {
                    targets.push(last.clone());
                }
            }
            // Destructive git subcommands operate workspace-wide (e.g.
            // `git clean -fdx` wipes untracked files, `git rm -rf` removes
            // tracked ones). The post-hoc workspace snapshot would detect
            // the damage only AFTER it is done — irreversible for `rm`/clean.
            // Require an explicit path target inside the write dir up front;
            // bare `git clean` / `git rm` are rejected pre-emptively.
            "git" => {
                let sub = command.get(1).map(String::as_str).unwrap_or("");
                if !matches!(sub, "clean" | "rm") {
                    continue;
                }
                let path_args: Vec<&String> = command
                    .iter()
                    .skip(2)
                    .filter(|a| !a.starts_with('-'))
                    .collect();
                if path_args.is_empty() {
                    return Err(format!(
                        "git {sub} without an explicit path inside your write directory is not permitted \
                         (it would affect files outside your allowed write dir)"
                    ));
                }
                for target in path_args {
                    let path = resolve_within(target, &ctx.workspace)?;
                    ctx.assert_write_allowed(&path)?;
                }
                continue;
            }
            _ => continue,
        }
        for target in targets {
            let path = resolve_within(&target, &ctx.workspace)?;
            ctx.assert_write_allowed(&path)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryFingerprint {
    is_dir: bool,
    len: u64,
    modified_ms: u128,
}

#[derive(Debug, Clone, Default)]
struct WorkspaceSnapshot {
    entries: HashMap<PathBuf, EntryFingerprint>,
}

impl WorkspaceSnapshot {
    fn capture(workspace: &Path) -> std::io::Result<Self> {
        let mut snapshot = Self::default();
        walk_workspace(workspace, workspace, &mut snapshot.entries)?;
        Ok(snapshot)
    }

    fn diff(&self, before: &Self) -> Vec<PathBuf> {
        let mut changed = Vec::new();
        for (path, after_entry) in &self.entries {
            match before.entries.get(path) {
                None => changed.push(path.clone()),
                Some(before_entry) if before_entry != after_entry && !after_entry.is_dir => {
                    changed.push(path.clone())
                }
                _ => {}
            }
        }
        for path in before.entries.keys() {
            if !self.entries.contains_key(path) {
                changed.push(path.clone());
            }
        }
        changed.sort();
        changed.dedup();
        changed
    }
}

fn walk_workspace(
    root: &Path,
    current: &Path,
    entries: &mut HashMap<PathBuf, EntryFingerprint>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.file_name().and_then(|s| s.to_str()) == Some(".autoreport") {
            continue;
        }
        let metadata = entry.metadata()?;
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let modified = metadata
            .modified()
            .ok()
            .and_then(|m| m.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let is_dir = metadata.is_dir();
        entries.insert(
            root.join(relative),
            EntryFingerprint {
                is_dir,
                len: metadata.len(),
                modified_ms: modified,
            },
        );
        if is_dir {
            walk_workspace(root, &path, entries)?;
        }
    }
    Ok(())
}

/// Convenience constructor.
pub fn make(
    ctx: FsCtx,
    timeout_secs: u64,
    sandbox: autoreport_sandboxing::SandboxSpec,
) -> Arc<dyn Tool> {
    let sandbox = sandbox.with_writable_root(ctx.allowed_write_dir());
    Arc::new(ExecTool::new(ctx, timeout_secs).with_sandbox(sandbox))
}
