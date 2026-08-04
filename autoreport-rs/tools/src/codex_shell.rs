use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tree_sitter::{Node, Parser, Tree};
use tree_sitter_bash::LANGUAGE as BASH;

/// Chunk size for each `read` call inside `read_capped`. Mirrors codex's
/// `READ_CHUNK_SIZE` (`core/src/exec.rs:69`).
const READ_CHUNK_SIZE: usize = 8192; // bytes per read

/// Hard cap on bytes retained from exec stdout/stderr.
///
/// A command producing output at memory-bandwidth speed (`cat /dev/urandom`,
/// `yes`, an infinite `print` loop) would otherwise fill all memory before the
/// timeout fires, OOM-killing the process. This is reachable even under the
/// `WorkspaceWrite` sandbox, which restricts file writes — not stdout volume.
///
/// Matches codex's `EXEC_OUTPUT_MAX_BYTES`/`DEFAULT_OUTPUT_BYTES_CAP`
/// (`core/src/exec.rs:76`, `utils/pty/src/lib.rs:12`): 1 MiB per stream. The
/// cap is enforced DURING streaming reads via `read_capped`, which keeps
/// draining the pipe to EOF (so the child is never blocked on a full pipe)
/// but only RETAINS the first 1 MiB.
const EXEC_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

/// Read `r` to EOF, retaining at most `cap` bytes of the output.
///
/// This replaces `read_to_end` for child stdout/stderr so a runaway command
/// cannot grow the buffer without bound. Mirrors codex's `read_output` +
/// `append_capped` (`core/src/exec.rs:1114`, `:849-856`): it loops `read` into
/// a small temp buffer and appends to `out` ONLY while `out.len() < cap`;
/// once the cap is reached subsequent chunks are discarded but the loop keeps
/// reading until EOF (`Ok(0)`) so the child is not stalled by a full pipe.
async fn read_capped<R: AsyncRead + Unpin>(r: &mut R, cap: usize) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(cap.min(READ_CHUNK_SIZE));
    let mut tmp = [0u8; READ_CHUNK_SIZE];
    loop {
        let n = r.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        if out.len() < cap {
            let remaining = cap - out.len();
            let take = remaining.min(n);
            out.extend_from_slice(&tmp[..take]);
        }
        // Once `out` is full we keep draining `tmp` to EOF but discard the
        // bytes — avoids back-pressuring the child while bounding retention.
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Serialize, Deserialize)]
pub enum ShellType {
    Zsh,
    Bash,
    PowerShell,
    Sh,
    Cmd,
}

impl ShellType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Zsh => "zsh",
            Self::Bash => "bash",
            Self::PowerShell => "powershell",
            Self::Sh => "sh",
            Self::Cmd => "cmd",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedShell {
    pub shell_type: ShellType,
    pub shell_path: PathBuf,
}

impl DetectedShell {
    pub fn name(&self) -> &'static str {
        self.shell_type.name()
    }
}

#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub returncode: i32,
    pub timed_out: bool,
}

#[derive(Debug, Clone)]
pub struct CodexShell {
    shell: DetectedShell,
}

impl Default for CodexShell {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexShell {
    pub fn new() -> Self {
        Self {
            shell: default_user_shell(),
        }
    }

    pub fn detected_shell(&self) -> &DetectedShell {
        &self.shell
    }

    pub async fn run(
        &self,
        cwd: &Path,
        command: &str,
        timeout: Duration,
        stdin: Option<Vec<u8>>,
        env: &HashMap<String, String>,
        sandbox: Option<&autoreport_sandboxing::SandboxSpec>,
    ) -> Result<ShellOutput, String> {
        // Resolve the program + argv. Restrictive modes get a platform sandbox
        // launcher; if none is available they fail closed rather than running
        // the detected login shell unrestricted.
        let (program, args): (std::path::PathBuf, Vec<String>) = match sandbox {
            Some(spec) => {
                let shell_invocation = [self.shell.shell_path.to_string_lossy().into_owned()]
                    .into_iter()
                    .chain(shell_args_owned(self.shell.shell_type, command))
                    .collect::<Vec<_>>();
                let wrapped = autoreport_sandboxing::sandbox_command_argv(
                    shell_invocation.clone(),
                    cwd,
                    spec,
                )?;
                // Fail closed: only `DangerFullAccess` may legitimately run
                // without a platform sandbox wrapper. Any restrictive mode that
                // gets no wrapper (unsupported Unix, missing backend) must NOT
                // fall back to running the bare shell unrestricted — return an
                // error so the turn surfaces it instead of silently escaping.
                let wrapped = match wrapped {
                    Some(wrapped) => wrapped,
                    None if spec.mode == autoreport_sandboxing::SandboxMode::DangerFullAccess => {
                        shell_invocation
                    }
                    None => {
                        return Err(format!(
                            "no platform sandbox backend available for mode {:?}; refusing to run command unsandboxed",
                            spec.mode
                        ));
                    }
                };
                let prog = wrapped
                    .first()
                    .ok_or_else(|| "empty seatbelt command".to_string())?
                    .clone();
                (std::path::PathBuf::from(prog), wrapped[1..].to_vec())
            }
            None => (
                self.shell.shell_path.clone(),
                shell_args_owned(self.shell.shell_type, command),
            ),
        };

        let mut cmd = Command::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.current_dir(cwd);
        cmd.stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        });
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        // Kill the child if the handle is dropped (e.g. the timeout future is
        // dropped mid-wait). Without this a timed-out command keeps running
        // detached — a leaked process that outlives the agent turn.
        cmd.kill_on_drop(true);
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        // Bubblewrap mounts a fresh writable /tmp. Make standard temporary
        // file users target it even if the parent process exported TMPDIR.
        #[cfg(target_os = "linux")]
        if sandbox.is_some_and(|spec| {
            !matches!(
                spec.mode,
                autoreport_sandboxing::SandboxMode::DangerFullAccess
            )
        }) {
            cmd.env("TMPDIR", "/tmp");
            cmd.env("TMP", "/tmp");
            cmd.env("TEMP", "/tmp");
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        if let Some(input) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            pipe.write_all(&input)
                .await
                .map_err(|e| format!("stdin write failed: {e}"))?;
        }

        // Read stdout/stderr ourselves rather than via `wait_with_output` (which
        // owns the pipe handles) so that on timeout we can kill the child and
        // still drain whatever it already wrote. Buffers live in the outer
        // scope so partial reads survive a dropped timeout future.
        let mut stdout_pipe = child.stdout.take();
        let mut stderr_pipe = child.stderr.take();
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        match tokio::time::timeout(timeout, async {
            let (a, b) = tokio::join!(
                async {
                    match stdout_pipe.as_mut() {
                        Some(p) => read_capped(p, EXEC_OUTPUT_MAX_BYTES).await.map(|v| {
                            stdout_buf = v;
                            0
                        }),
                        None => Ok(0),
                    }
                },
                async {
                    match stderr_pipe.as_mut() {
                        Some(p) => read_capped(p, EXEC_OUTPUT_MAX_BYTES).await.map(|v| {
                            stderr_buf = v;
                            0
                        }),
                        None => Ok(0),
                    }
                },
            );
            a.and(b).map_err(|e| format!("pipe read failed: {e}"))?;
            child.wait().await.map_err(|e| format!("exec failed: {e}"))
        })
        .await
        {
            Ok(Ok(status)) => Ok(ShellOutput {
                stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
                stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
                returncode: status.code().unwrap_or(-1),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                // Timed out: kill the whole process group so descendants that
                // inherited stdout/stderr cannot keep pipe draining alive.
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
                }
                let _ = child.kill().await;
                let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
                let _ = tokio::time::timeout(Duration::from_secs(2), async {
                    if let Some(p) = stdout_pipe.as_mut() {
                        if let Ok(v) = read_capped(p, EXEC_OUTPUT_MAX_BYTES).await {
                            // Preserve any bytes already read before timeout;
                            // the cap applies to the combined retained volume.
                            let cap = EXEC_OUTPUT_MAX_BYTES;
                            let already = stdout_buf.len();
                            let take = (cap - already).min(v.len());
                            stdout_buf.extend_from_slice(&v[..take]);
                        }
                    }
                    if let Some(p) = stderr_pipe.as_mut() {
                        if let Ok(v) = read_capped(p, EXEC_OUTPUT_MAX_BYTES).await {
                            let cap = EXEC_OUTPUT_MAX_BYTES;
                            let already = stderr_buf.len();
                            let take = (cap - already).min(v.len());
                            stderr_buf.extend_from_slice(&v[..take]);
                        }
                    }
                })
                .await;
                Ok(ShellOutput {
                    stdout: String::from_utf8_lossy(&stdout_buf).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr_buf).into_owned(),
                    returncode: -1,
                    timed_out: true,
                })
            }
        }
    }
}

/// Shell arguments are owned because the sandbox launcher builds a complete
/// `Vec<String>` command line before spawning the process.
fn shell_args_owned(shell_type: ShellType, command: &str) -> Vec<String> {
    match shell_type {
        ShellType::Zsh | ShellType::Bash | ShellType::Sh => {
            vec!["-lc".to_string(), command.to_string()]
        }
        ShellType::PowerShell => vec![
            "-NoProfile".to_string(),
            "-Command".to_string(),
            command.to_string(),
        ],
        ShellType::Cmd => vec!["/C".to_string(), command.to_string()],
    }
}

pub fn validate_plain_command_script(
    script: &str,
    allowlist: &std::collections::HashSet<&'static str>,
) -> Result<Vec<Vec<String>>, String> {
    validate_command_for_shell(script, allowlist, default_user_shell().shell_type)
}

/// Validate a command script for a specific shell family.
///
/// Codex ships separate parsers per shell (`codex_shell_command::bash` vs
/// `::powershell`). We mirror that split rather than running PowerShell
/// commands through the bash grammar (which falsely rejects legitimate PS
/// constructs on Windows):
/// - **POSIX shells** (bash/zsh/sh): the bash tree-sitter grammar, which
///   rejects redirects/subshells/substitutions and recovers a tokenized
///   command sequence for write-target extraction.
/// - **PowerShell/Cmd**: a conservative tokenizer (quote-aware split on
///   `;`, `|`, `&&`, `||`, newlines), program allowlist, and rejection of
///   unsafe operators (`>`, `<`, backticks, `$(...)`). A full PowerShell AST
///   parser like codex's is the long-term path; this is a safe stopgap that
///   does not falsely reject the simple `prog arg ...` invocations an agent
///   actually issues.
pub fn validate_command_for_shell(
    script: &str,
    allowlist: &std::collections::HashSet<&'static str>,
    shell_type: ShellType,
) -> Result<Vec<Vec<String>>, String> {
    let commands = match shell_type {
        ShellType::Bash | ShellType::Zsh | ShellType::Sh => {
            parse_shell_script_into_commands(script).ok_or_else(|| {
                "command must be a plain shell command sequence without redirects, subshells, or substitutions".to_string()
            })?
        }
        ShellType::PowerShell | ShellType::Cmd => {
            parse_simple_command_sequence(script).ok_or_else(|| {
                "command must be a plain command sequence without redirects, subshells, or substitutions".to_string()
            })?
        }
    };
    if commands.is_empty() {
        return Err("empty command".to_string());
    }
    for command in &commands {
        let Some(program) = command.first() else {
            return Err("empty command".to_string());
        };
        let base = std::path::Path::new(program)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(program);
        if !allowlist.contains(base) {
            return Err(format!("command '{base}' is not on the allowlist"));
        }
    }
    Ok(commands)
}

/// Conservative quote-aware tokenizer for PowerShell/Cmd. Splits a script into
/// commands on `;`, `|`, `&&`, `||`, and newlines (only when unquoted), rejects
/// unsafe operators, and tokenizes each command on whitespace while preserving
/// quoted strings. Returns `None` on anything it cannot safely classify —
/// fail-closed.
fn parse_simple_command_sequence(script: &str) -> Option<Vec<Vec<String>>> {
    // Reject operators that change semantics in ways this tokenizer cannot
    // reason about safely.
    if script.contains('>') || script.contains('<') {
        return None;
    }
    // PowerShell uses backtick as the escape char and supports `$(...)`
    // subexpressions; reject both so we never misclassify an embedded command.
    if script.contains('`') {
        return None;
    }
    // `--%` is PowerShell's stop-parsing marker: everything after it is passed
    // literally, letting a model smuggle args (e.g. `git log --% --output=x`)
    // past token-based checks. Reject outright (codex's PS parser does too).
    if script.contains("--%") {
        return None;
    }

    let mut commands = Vec::new();
    for raw_cmd in split_on_separators(script) {
        let tokens = tokenize_command(raw_cmd.trim())?;
        if tokens.is_empty() {
            continue;
        }
        // Reject `$(` subexpressions and any `$`-prefixed/dollar-interpolated
        // token (`$foo`, `$env:PATH`, `"text $var"`) — these are dynamic and
        // cannot be statically allowlisted (codex's PS AST parser only accepts
        // constant/expandable-with-no-nesting expressions).
        if tokens.iter().any(|t| t.contains("$(") || t.contains('$')) {
            return None;
        }
        // Intrinsic dangerous-cmdlet blocklist (codex
        // `windows_safe_commands::is_safe_powershell_words`): reject these
        // regardless of the caller's allowlist, even when nested. The allowlist
        // gates ordinary programs; these cmdlets are file/process mutating and
        // must never be auto-approved.
        if let Some(program) = tokens.first() {
            if is_dangerous_powershell_cmdlet(program) {
                return None;
            }
        }
        commands.push(tokens);
    }
    if commands.is_empty() {
        return None;
    }
    Some(commands)
}

/// PowerShell cmdlets that mutate the filesystem or spawn/control processes.
/// They are blocked unconditionally on the PowerShell path (defense-in-depth
/// on top of the caller allowlist), mirroring codex's intrinsic danger set.
fn is_dangerous_powershell_cmdlet(program: &str) -> bool {
    // Normalize: PowerShell is case-insensitive and accepts cmdlets with or
    // without the leading verb-prefix dash (e.g. `Remove-Item` / `rm`).
    let lower = program.to_ascii_lowercase();
    const DANGEROUS: &[&str] = &[
        "remove-item",
        "del",
        "erase",
        "rd",
        "rmdir",
        "move-item",
        "copy-item",
        "rename-item",
        "new-item",
        "out-file",
        "set-content",
        "add-content",
        "clear-content",
        "start-process",
        "stop-process",
        "stop-job",
        "invoke-expression",
        "iex",
    ];
    DANGEROUS.contains(&lower.as_str())
}

/// Split a script into raw command strings on top-level `;`, `|`, `&&`, `||`,
/// and newlines, respecting `"..."` and `'...'` quoting.
fn split_on_separators(script: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let chars: Vec<char> = script.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                out.last_mut().unwrap().push(c);
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                    out.last_mut().unwrap().push(c);
                } else if c == ';' || c == '|' || c == '\n' || c == '\r' {
                    // `&&` and `||` are two-char; a single `|` is also a
                    // boundary. Push a fresh command segment either way.
                    if c == '|' && i + 1 < chars.len() && chars[i + 1] == '|' {
                        i += 1;
                    }
                    out.push(String::new());
                } else if c == '&' && i + 1 < chars.len() && chars[i + 1] == '&' {
                    i += 1;
                    out.push(String::new());
                } else {
                    out.last_mut().unwrap().push(c);
                }
            }
        }
        i += 1;
    }
    out
}

/// Tokenize one command into words, respecting `"..."` and `'...'`. Returns
/// `None` on an unterminated quote (fail-closed).
fn tokenize_command(cmd: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_token = false;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '\'' => {
                in_token = true;
                let quote = c;
                loop {
                    match chars.next() {
                        Some(q) if q == quote => break,
                        Some(q) => current.push(q),
                        None => return None, // unterminated quote
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_token {
                    tokens.push(std::mem::take(&mut current));
                    in_token = false;
                }
            }
            c => {
                in_token = true;
                current.push(c);
            }
        }
    }
    if in_token {
        tokens.push(current);
    }
    Some(tokens)
}

pub fn detect_shell_type(shell_path: impl AsRef<std::path::Path>) -> Option<ShellType> {
    let shell_path = shell_path.as_ref();
    match shell_path.as_os_str().to_str() {
        Some("zsh") => Some(ShellType::Zsh),
        Some("sh") => Some(ShellType::Sh),
        Some("cmd") => Some(ShellType::Cmd),
        Some("bash") => Some(ShellType::Bash),
        Some("pwsh") => Some(ShellType::PowerShell),
        Some("powershell") => Some(ShellType::PowerShell),
        _ => {
            let shell_name = shell_path.file_stem();
            if let Some(shell_name) = shell_name {
                let shell_name_path = std::path::Path::new(shell_name);
                if shell_name_path != shell_path {
                    return detect_shell_type(shell_name_path);
                }
            }
            None
        }
    }
}

#[cfg(unix)]
fn get_user_shell_path() -> Option<PathBuf> {
    let uid = unsafe { libc::getuid() };
    use std::ffi::CStr;
    use std::mem::MaybeUninit;
    use std::ptr;

    let mut passwd = MaybeUninit::<libc::passwd>::uninit();
    let suggested_buffer_len = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_len = usize::try_from(suggested_buffer_len)
        .ok()
        .filter(|len| *len > 0)
        .unwrap_or(1024);
    let mut buffer = vec![0; buffer_len];

    loop {
        let mut result = ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid,
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == 0 {
            if result.is_null() {
                return None;
            }
            let passwd = unsafe { passwd.assume_init_ref() };
            if passwd.pw_shell.is_null() {
                return None;
            }
            let shell_path = unsafe { CStr::from_ptr(passwd.pw_shell) }
                .to_string_lossy()
                .into_owned();
            return Some(PathBuf::from(shell_path));
        }
        if status != libc::ERANGE {
            return None;
        }
        let new_len = buffer.len().checked_mul(2)?;
        if new_len > 1024 * 1024 {
            return None;
        }
        buffer.resize(new_len, 0);
    }
}

#[cfg(not(unix))]
fn get_user_shell_path() -> Option<PathBuf> {
    None
}

fn file_exists(path: &std::path::Path) -> Option<PathBuf> {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        Some(PathBuf::from(path))
    } else {
        None
    }
}

fn get_shell_path(
    shell_type: ShellType,
    provided_path: Option<&PathBuf>,
    binary_name: &str,
    fallback_paths: &[&str],
) -> Option<PathBuf> {
    if let Some(path) = provided_path.and_then(|path| file_exists(path)) {
        return Some(path);
    }

    let default_shell_path = get_user_shell_path();
    if let Some(default_shell_path) = default_shell_path
        && detect_shell_type(&default_shell_path) == Some(shell_type)
        && file_exists(&default_shell_path).is_some()
    {
        return Some(default_shell_path);
    }

    if let Ok(path) = which::which(binary_name) {
        return Some(path);
    }

    for path in fallback_paths {
        if let Some(path) = file_exists(std::path::Path::new(path)) {
            return Some(path);
        }
    }

    None
}

const ZSH_FALLBACK_PATHS: &[&str] = &["/bin/zsh"];
const BASH_FALLBACK_PATHS: &[&str] = &["/bin/bash", "/usr/bin/bash"];
const SH_FALLBACK_PATHS: &[&str] = &["/bin/sh"];
#[cfg(windows)]
const PWSH_FALLBACK_PATHS: &[&str] = &[r#"C:\Program Files\PowerShell\7\pwsh.exe"#];
#[cfg(not(windows))]
const PWSH_FALLBACK_PATHS: &[&str] = &["/usr/local/bin/pwsh"];
#[cfg(windows)]
const POWERSHELL_FALLBACK_PATHS: &[&str] =
    &[r#"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"#];
#[cfg(not(windows))]
const POWERSHELL_FALLBACK_PATHS: &[&str] = &[];

fn get_shell(shell_type: ShellType, path: Option<&PathBuf>) -> Option<DetectedShell> {
    let shell_path = match shell_type {
        ShellType::Zsh => get_shell_path(ShellType::Zsh, path, "zsh", ZSH_FALLBACK_PATHS),
        ShellType::Bash => get_shell_path(ShellType::Bash, path, "bash", BASH_FALLBACK_PATHS),
        ShellType::Sh => get_shell_path(ShellType::Sh, path, "sh", SH_FALLBACK_PATHS),
        ShellType::PowerShell => {
            get_shell_path(ShellType::PowerShell, path, "pwsh", PWSH_FALLBACK_PATHS).or_else(|| {
                get_shell_path(
                    ShellType::PowerShell,
                    path,
                    "powershell",
                    POWERSHELL_FALLBACK_PATHS,
                )
            })
        }
        ShellType::Cmd => get_shell_path(ShellType::Cmd, path, "cmd", &[]),
    }?;
    Some(DetectedShell {
        shell_type,
        shell_path,
    })
}

fn ultimate_fallback_shell() -> DetectedShell {
    if cfg!(windows) {
        DetectedShell {
            shell_type: ShellType::Cmd,
            shell_path: PathBuf::from("cmd.exe"),
        }
    } else {
        DetectedShell {
            shell_type: ShellType::Sh,
            shell_path: PathBuf::from("/bin/sh"),
        }
    }
}

pub fn default_user_shell() -> DetectedShell {
    if cfg!(windows) {
        get_shell(ShellType::PowerShell, None).unwrap_or_else(ultimate_fallback_shell)
    } else {
        let user_default_shell = get_user_shell_path()
            .and_then(|shell| detect_shell_type(&shell))
            .and_then(|shell_type| get_shell(shell_type, None));
        let shell_with_fallback = if cfg!(target_os = "macos") {
            user_default_shell
                .or_else(|| get_shell(ShellType::Zsh, None))
                .or_else(|| get_shell(ShellType::Bash, None))
        } else {
            user_default_shell
                .or_else(|| get_shell(ShellType::Bash, None))
                .or_else(|| get_shell(ShellType::Zsh, None))
        };
        shell_with_fallback.unwrap_or_else(ultimate_fallback_shell)
    }
}

fn try_parse_shell(shell_lc_arg: &str) -> Option<Tree> {
    let lang = BASH.into();
    let mut parser = Parser::new();
    parser.set_language(&lang).ok()?;
    parser.parse(shell_lc_arg, None)
}

fn parse_shell_script_into_commands(script: &str) -> Option<Vec<Vec<String>>> {
    let tree = try_parse_shell(script)?;
    try_parse_word_only_commands_sequence(&tree, script)
}

fn try_parse_word_only_commands_sequence(tree: &Tree, src: &str) -> Option<Vec<Vec<String>>> {
    if tree.root_node().has_error() {
        return None;
    }

    const ALLOWED_KINDS: &[&str] = &[
        "program",
        "list",
        "pipeline",
        "command",
        "command_name",
        "word",
        "string",
        "string_content",
        "raw_string",
        "number",
        "concatenation",
    ];
    const ALLOWED_PUNCT_TOKENS: &[&str] = &["&&", "||", ";", "|", "\"", "'"];

    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut stack = vec![root];
    let mut command_nodes = Vec::new();
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if node.is_named() {
            if !ALLOWED_KINDS.contains(&kind) {
                return None;
            }
            if kind == "command" {
                command_nodes.push(node);
            }
        } else {
            if kind.chars().any(|c| "&;|".contains(c)) && !ALLOWED_PUNCT_TOKENS.contains(&kind) {
                return None;
            }
            if !(ALLOWED_PUNCT_TOKENS.contains(&kind) || kind.trim().is_empty()) {
                return None;
            }
        }
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    command_nodes.sort_by_key(Node::start_byte);
    let mut commands = Vec::new();
    for node in command_nodes {
        commands.push(parse_plain_command_from_node(node, src)?);
    }
    Some(commands)
}

fn parse_plain_command_from_node(cmd: Node<'_>, src: &str) -> Option<Vec<String>> {
    if cmd.kind() != "command" {
        return None;
    }
    let mut words = Vec::new();
    let mut cursor = cmd.walk();
    for child in cmd.named_children(&mut cursor) {
        match child.kind() {
            "command_name" => {
                let word_node = child.named_child(0)?;
                if word_node.kind() != "word" {
                    return None;
                }
                words.push(word_node.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "word" | "number" => {
                words.push(child.utf8_text(src.as_bytes()).ok()?.to_owned());
            }
            "string" => {
                words.push(parse_double_quoted_string(child, src)?);
            }
            "raw_string" => {
                words.push(parse_raw_string(child, src)?);
            }
            "concatenation" => {
                let mut concatenated = String::new();
                let mut concat_cursor = child.walk();
                for part in child.named_children(&mut concat_cursor) {
                    match part.kind() {
                        "word" | "number" => {
                            concatenated.push_str(part.utf8_text(src.as_bytes()).ok()?);
                        }
                        "string" => concatenated.push_str(&parse_double_quoted_string(part, src)?),
                        "raw_string" => concatenated.push_str(&parse_raw_string(part, src)?),
                        _ => return None,
                    }
                }
                if concatenated.is_empty() {
                    return None;
                }
                words.push(concatenated);
            }
            _ => return None,
        }
    }
    Some(words)
}

fn parse_double_quoted_string(node: Node<'_>, src: &str) -> Option<String> {
    let raw = node.utf8_text(src.as_bytes()).ok()?;
    let mut out = String::new();
    let mut chars = raw.chars();
    if chars.next()? != '"' {
        return None;
    }
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some(out),
            '\\' => out.push(chars.next()?),
            _ => out.push(ch),
        }
    }
    None
}

fn parse_raw_string(node: Node<'_>, src: &str) -> Option<String> {
    let raw = node.utf8_text(src.as_bytes()).ok()?;
    let stripped = raw.strip_prefix('\'')?.strip_suffix('\'')?;
    Some(stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_command_sequence() {
        let parsed = validate_plain_command_script(
            r#"rg "foo" src && sed -n "1,10p" Cargo.toml"#,
            &["rg", "sed"].into_iter().collect(),
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0][0], "rg");
        assert_eq!(parsed[1][0], "sed");
    }

    #[test]
    fn rejects_redirects() {
        // Explicitly exercise the bash grammar path (host-shell-agnostic).
        let err = validate_command_for_shell(
            "cat foo > out.txt",
            &["cat"].into_iter().collect(),
            ShellType::Bash,
        )
        .unwrap_err();
        assert!(err.contains("plain shell command sequence"));
    }

    #[test]
    fn powershell_path_accepts_simple_sequence() {
        // On Windows the default shell is PowerShell; the bash grammar path
        // would reject cmdlet-style or `;`-separated sequences. The
        // shell-aware path must accept a simple allowlisted sequence and
        // still reject redirects / subshells.
        let allowlist: std::collections::HashSet<&str> = ["python", "rg"].into_iter().collect();
        let parsed = validate_command_for_shell(
            "python analyze.py; rg result data",
            &allowlist,
            ShellType::PowerShell,
        )
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0][0], "python");
        assert_eq!(parsed[1][0], "rg");
    }

    #[test]
    fn powershell_path_rejects_redirects_and_subshells() {
        let allowlist: std::collections::HashSet<&str> = ["python"].into_iter().collect();
        let err =
            validate_command_for_shell("python a.py > out.txt", &allowlist, ShellType::PowerShell)
                .unwrap_err();
        assert!(err.contains("plain command sequence"));
        let err =
            validate_command_for_shell("python $(whoami)", &allowlist, ShellType::Cmd).unwrap_err();
        assert!(err.contains("plain command sequence"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_background_process_group_promptly() {
        let shell = CodexShell::new();
        let started = std::time::Instant::now();
        let output = shell
            .run(
                Path::new("/tmp"),
                "sleep 5 & wait",
                Duration::from_millis(100),
                None,
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();
        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// Regression for the OOM-via-unbounded-buffer DoS: `read_capped` must
    /// retain EXACTLY `EXEC_OUTPUT_MAX_BYTES` bytes regardless of how much the
    /// producer emits, instead of growing the buffer to multi-MB. Input is 4 MiB
    /// of non-zero filler — well above the 1 MiB cap.
    #[tokio::test]
    async fn read_capped_truncates_at_exec_output_max_bytes() {
        let payload: Vec<u8> = vec![0xAB; EXEC_OUTPUT_MAX_BYTES * 4];
        assert!(payload.len() > EXEC_OUTPUT_MAX_BYTES);
        let mut reader = std::io::Cursor::new(payload);
        let out = read_capped(&mut reader, EXEC_OUTPUT_MAX_BYTES)
            .await
            .expect("read_capped must drain to EOF");
        assert_eq!(out.len(), EXEC_OUTPUT_MAX_BYTES);
        assert!(out.iter().all(|&b| b == 0xAB));
    }

    /// Under-cap input is returned verbatim (cap is an upper bound, not a pad).
    #[tokio::test]
    async fn read_capped_keeps_all_data_under_cap() {
        let payload = b"hello world".to_vec();
        let mut reader = std::io::Cursor::new(payload.clone());
        let out = read_capped(&mut reader, EXEC_OUTPUT_MAX_BYTES)
            .await
            .unwrap();
        assert_eq!(out, payload);
    }

    /// Zero-length input yields an empty buffer (no spurious reads / no panic).
    #[tokio::test]
    async fn read_capped_empty_input_yields_empty() {
        let mut reader = std::io::Cursor::new(Vec::<u8>::new());
        let out = read_capped(&mut reader, EXEC_OUTPUT_MAX_BYTES)
            .await
            .unwrap();
        assert!(out.is_empty());
    }

    /// The cap matches codex's `DEFAULT_OUTPUT_BYTES_CAP` (1 MiB). If this
    /// drifts, the bugfix silently degrades — pin it.
    #[test]
    fn exec_output_max_bytes_matches_codex_default_output_bytes_cap() {
        assert_eq!(EXEC_OUTPUT_MAX_BYTES, 1024 * 1024);
    }
}
