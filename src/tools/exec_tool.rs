//! Shell execution tool with an allowlist-based security policy, mirroring
//! AutoReport's `exec_tools.py`. Commands are tokenized with `shlex`; the base
//! program must be on the allowlist. Paths into `.autoreport` are blocked.

use crate::tools::registry::{arg_str, Tool, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

/// Programs an agent may invoke. Build/plot/compute oriented — deliberately
/// narrow, like the original.
fn allowlist() -> HashSet<&'static str> {
    [
        "python", "python3", "pip", "pip3", "uv",
        "xelatex", "lualatex", "pdflatex", "latexmk", "bibtex", "makeindex",
        "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "sort", "uniq", "cut", "tr", "sed", "awk",
        "cp", "mv", "rm", "mkdir", "touch", "rmdir", "chmod",
        "git",
        "echo", "printf",
        "which", "fc-list", "mineru-open-api",
    ]
    .into_iter()
    .collect()
}

pub struct ExecTool {
    working_dir: PathBuf,
    timeout: Duration,
}

impl ExecTool {
    pub fn new(working_dir: PathBuf, timeout_secs: u64) -> Self {
        Self {
            working_dir,
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

fn block_internal_paths(command: &str) -> Result<(), String> {
    if command.contains(".autoreport") {
        return Err("accessing .autoreport metadata via exec is not permitted".to_string());
    }
    Ok(())
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }
    fn description(&self) -> &str {
        "Run an allow-listed shell command in the project root. Use for data analysis, plotting and LaTeX compilation. The base program must be on the allowlist."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Full command line, e.g. `python3 analyze.py`."},
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

        // Split into program + args using a simple shell-quoter (handles quotes).
        let parts = match shell_split(&command) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        if parts.is_empty() {
            return ToolOutput::err("empty command");
        }
        let program = parts[0].as_str();
        let allowed = allowlist();
        let base = std::path::Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(program);
        if !allowed.contains(base) {
            return ToolOutput::err(format!(
                "command '{base}' is not on the allowlist"
            ));
        }

        let mut cmd = Command::new(program);
        cmd.args(&parts[1..]);
        cmd.current_dir(&self.working_dir);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("spawn failed: {e}")),
        };

        match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                ToolOutput::ok(json!({
                    "command": command,
                    "stdout": stdout,
                    "stderr": stderr,
                    "returncode": out.status.code().unwrap_or(-1),
                    "timed_out": false,
                }))
            }
            Ok(Err(e)) => ToolOutput::err(format!("exec failed: {e}")),
            Err(_) => ToolOutput::err(format!(
                "command timed out after {}s",
                self.timeout.as_secs()
            )),
        }
    }
}

/// Minimal shell-style splitter (handles single/double quotes and escapes).
fn shell_split(input: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_word = false;
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    out.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                while let Some(&n) = chars.peek() {
                    if n == '\'' {
                        chars.next();
                        break;
                    }
                    current.push(chars.next().unwrap());
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => {
                            if let Some(e) = chars.next() {
                                current.push(e);
                            }
                        }
                        Some(n) => current.push(n),
                        None => break,
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    current.push(n);
                }
            }
            _ => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word || !current.is_empty() {
        out.push(current);
    }
    Ok(out)
}

/// Convenience constructor.
pub fn make(working_dir: PathBuf, timeout_secs: u64) -> Arc<dyn Tool> {
    Arc::new(ExecTool::new(working_dir, timeout_secs))
}
