//! Shell execution tool with mandatory OS-level isolation.
//!
//! Every command is launched through the platform sandbox. The sandbox, rather
//! than a program allowlist or a post-execution workspace diff, prevents writes
//! outside the agent's assigned directory before the command can perform them.

use crate::codex_shell::CodexShell;
use crate::file_tools::FsCtx;
use crate::registry::{Tool, ToolExecutionContext, ToolOutput, arg_str};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub struct ExecTool {
    ctx: FsCtx,
    timeout: Duration,
    shell: Arc<CodexShell>,
    sandbox: autoreport_sandboxing::SandboxSpec,
    environment_home: Option<PathBuf>,
}

impl ExecTool {
    pub fn new(ctx: FsCtx, timeout_secs: u64) -> Self {
        let sandbox = autoreport_sandboxing::SandboxSpec::new(
            autoreport_sandboxing::SandboxMode::WorkspaceWrite,
            false,
        )
        .with_writable_root(ctx.allowed_write_dir());
        Self {
            ctx,
            timeout: Duration::from_secs(timeout_secs),
            shell: Arc::new(CodexShell::new()),
            sandbox,
            environment_home: None,
        }
    }

    /// Override the default OS sandbox. `DangerFullAccess` is an explicit
    /// opt-out selected by the caller, mirroring Codex's permission profile.
    pub fn with_sandbox(mut self, sandbox: autoreport_sandboxing::SandboxSpec) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn with_environment_home(mut self, home: PathBuf) -> Self {
        self.environment_home = Some(home);
        self
    }
}

#[async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }
    fn description(&self) -> &str {
        "Run a shell command in the project root via the detected user shell. By default, commands automatically use the globally selected Python environment recorded in ~/.autoreport/environment.toml: `/env` selects a detected conda/virtualenv/pyenv/PATH environment, a custom executable, or the AutoReport-managed global venv; its bin directory is prepended to PATH, so use `python`, `python3`, pip, and installed tools without manually activating or specifying an interpreter. To use another environment, invoke its absolute executable path or explicitly override the command environment. The OS sandbox restricts writes to this agent's assigned directory."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Full shell command line. The `/env`-selected AutoReport Python environment is the default; do not add conda/venv activation or an explicit Python path unless using another environment. Example: `python analyze.py && rg result Data/Processed`."},
                "command_description": {"type": "string", "description": "Short human description of what this does."},
                "sandbox_permissions": {"type": "string", "enum": ["use_default", "require_escalated"], "description": "Use `require_escalated` only when the command needs to run outside the normal sandbox; it requires user approval."},
                "justification": {"type": "string", "description": "User-facing explanation required with `require_escalated`."}
            },
            "required": ["command"]
        })
    }
    async fn call(&self, args: &Value) -> ToolOutput {
        self.call_with_context(args, ToolExecutionContext::default())
            .await
    }

    async fn call_with_context(&self, args: &Value, context: ToolExecutionContext) -> ToolOutput {
        let command = match arg_str(args, "command") {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(e),
        };
        let requested_escalation = match args.get("sandbox_permissions") {
            None => false,
            Some(Value::String(value)) if value == "use_default" => false,
            Some(Value::String(value)) if value == "require_escalated" => true,
            Some(_) => {
                return ToolOutput::err(
                    "sandbox_permissions must be use_default or require_escalated",
                );
            }
        };
        if requested_escalation && !context.allow_escalated_exec {
            return ToolOutput::err(
                "require_escalated was not approved; request approval through the runtime first",
            );
        }
        if requested_escalation
            && args
                .get("justification")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return ToolOutput::err("require_escalated requires a non-empty justification");
        }
        let sandbox = if requested_escalation && context.allow_escalated_exec {
            autoreport_sandboxing::SandboxSpec::new(
                autoreport_sandboxing::SandboxMode::DangerFullAccess,
                true,
            )
        } else {
            self.sandbox.clone()
        };
        let environment = self
            .environment_home
            .as_deref()
            .and_then(autoreport_core::environment::selected_python_process_environment);
        let selected_python = self
            .environment_home
            .as_deref()
            .and_then(autoreport_core::environment::selected_python_environment);
        let output = match self
            .shell
            .run(
                &self.ctx.workspace,
                &command,
                self.timeout,
                None,
                environment.as_ref().unwrap_or(&HashMap::new()),
                Some(&sandbox),
            )
            .await
        {
            Ok(out) => out,
            Err(e) => return ToolOutput::err(e),
        };
        ToolOutput::ok(json!({
            "command": command,
            "stdout": output.stdout,
            "stderr": output.stderr,
            "returncode": output.returncode,
            "timed_out": output.timed_out,
            "shell": self.shell.detected_shell().name(),
            "allowed_write_dir": self.ctx.allowed_write_dir().map(|p| p.display().to_string()),
            "sandbox": sandbox.mode.as_kebab(),
            "python_environment": selected_python.map(|python| json!({
                "label": python.label,
                "source": python.source,
                "executable": python.executable.display().to_string(),
                "package_manager": python.package_manager,
                "selection": "global ~/.autoreport/environment.toml; PATH is automatically prepended"
            })).unwrap_or(Value::Null),
        }))
    }
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

pub fn make_with_environment(
    ctx: FsCtx,
    timeout_secs: u64,
    sandbox: autoreport_sandboxing::SandboxSpec,
    home: PathBuf,
) -> Arc<dyn Tool> {
    let sandbox = sandbox.with_writable_root(ctx.allowed_write_dir());
    Arc::new(
        ExecTool::new(ctx, timeout_secs)
            .with_sandbox(sandbox)
            .with_environment_home(home),
    )
}
