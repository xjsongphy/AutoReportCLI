use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tree_sitter::{Node, Parser, Tree};
use tree_sitter_bash::LANGUAGE as BASH;

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
    ) -> Result<ShellOutput, String> {
        let mut cmd = Command::new(&self.shell.shell_path);
        for arg in shell_args(self.shell.shell_type, command) {
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
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        if let Some(input) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            pipe.write_all(&input)
                .await
                .map_err(|e| format!("stdin write failed: {e}"))?;
        }

        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(Ok(out)) => Ok(ShellOutput {
                stdout: String::from_utf8_lossy(&out.stdout).to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).to_string(),
                returncode: out.status.code().unwrap_or(-1),
                timed_out: false,
            }),
            Ok(Err(e)) => Err(format!("exec failed: {e}")),
            Err(_) => Err(format!("command timed out after {}s", timeout.as_secs())),
        }
    }
}

fn shell_args(shell_type: ShellType, command: &str) -> Vec<&str> {
    match shell_type {
        ShellType::Zsh | ShellType::Bash | ShellType::Sh => vec!["-lc", command],
        ShellType::PowerShell => vec!["-NoProfile", "-Command", command],
        ShellType::Cmd => vec!["/C", command],
    }
}

pub fn validate_plain_command_script(
    script: &str,
    allowlist: &std::collections::HashSet<&'static str>,
) -> Result<Vec<Vec<String>>, String> {
    let commands = parse_shell_script_into_commands(script).ok_or_else(|| {
        "command must be a plain shell command sequence without redirects, subshells, or substitutions".to_string()
    })?;
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
        let err =
            validate_plain_command_script("cat foo > out.txt", &["cat"].into_iter().collect())
                .unwrap_err();
        assert!(err.contains("plain shell command sequence"));
    }
}
