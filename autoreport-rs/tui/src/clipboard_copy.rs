//! Clipboard copy backend aligned with Codex's TUI implementation.
//!
//! The order is deliberately environment-aware: remote sessions use terminal
//! clipboard transport, local sessions use the native clipboard first, and
//! tmux/OSC 52 are the terminal fallbacks. Linux keeps the native clipboard
//! owner alive for the lifetime of the TUI.

use base64::Engine;
use std::io::Write;

const OSC52_MAX_RAW_BYTES: usize = 100_000;
#[cfg(target_os = "macos")]
static STDERR_SUPPRESSION_MUTEX: std::sync::OnceLock<std::sync::Mutex<()>> =
    std::sync::OnceLock::new();

pub(crate) struct ClipboardLease {
    #[cfg(target_os = "linux")]
    clipboard: Option<arboard::Clipboard>,
}

impl ClipboardLease {
    #[cfg(target_os = "linux")]
    fn native(clipboard: arboard::Clipboard) -> Self {
        Self {
            clipboard: Some(clipboard),
        }
    }
}

pub(crate) fn copy_to_clipboard(text: &str) -> Result<Option<ClipboardLease>, String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "OSC 52 payload too large ({} bytes; max {OSC52_MAX_RAW_BYTES})",
            text.len()
        ));
    }

    copy_to_clipboard_with(
        text,
        CopyEnvironment {
            ssh_session: is_ssh_session(),
            wsl_session: is_wsl_session(),
            tmux_session: is_tmux_session(),
        },
        tmux_copy,
        osc52_copy,
        arboard_copy,
        wsl_clipboard_copy,
    )
}

#[derive(Clone, Copy)]
struct CopyEnvironment {
    ssh_session: bool,
    wsl_session: bool,
    tmux_session: bool,
}

fn copy_to_clipboard_with(
    text: &str,
    environment: CopyEnvironment,
    tmux_copy_fn: impl Fn(&str) -> Result<(), String>,
    osc52_copy_fn: impl Fn(&str) -> Result<(), String>,
    arboard_copy_fn: impl Fn(&str) -> Result<Option<ClipboardLease>, String>,
    wsl_copy_fn: impl Fn(&str) -> Result<(), String>,
) -> Result<Option<ClipboardLease>, String> {
    if environment.ssh_session {
        return terminal_clipboard_copy_with(
            text,
            environment.tmux_session,
            &tmux_copy_fn,
            &osc52_copy_fn,
        )
        .map(|()| None);
    }

    match arboard_copy_fn(text) {
        Ok(lease) => Ok(lease),
        Err(native_error) if environment.wsl_session => match wsl_copy_fn(text) {
            Ok(()) => Ok(None),
            Err(wsl_error) => terminal_clipboard_copy_with(
                text,
                environment.tmux_session,
                &tmux_copy_fn,
                &osc52_copy_fn,
            )
            .map(|()| None)
            .map_err(|fallback| {
                format!(
                    "native clipboard: {native_error}; WSL fallback: {wsl_error}; terminal fallback: {fallback}"
                )
            }),
        },
        Err(native_error) => terminal_clipboard_copy_with(
            text,
            environment.tmux_session,
            &tmux_copy_fn,
            &osc52_copy_fn,
        )
        .map(|()| None)
        .map_err(|fallback| format!("native clipboard: {native_error}; terminal fallback: {fallback}")),
    }
}

fn arboard_copy(text: &str) -> Result<Option<ClipboardLease>, String> {
    #[cfg(target_os = "macos")]
    let _stderr_lock = STDERR_SUPPRESSION_MUTEX
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .map_err(|_| "stderr suppression lock poisoned".to_string())?;
    let _stderr_guard = SuppressStderr::new();
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard unavailable: {error}"))?;
    clipboard
        .set_text(text)
        .map_err(|error| format!("failed to set clipboard text: {error}"))?;
    #[cfg(target_os = "linux")]
    {
        return Ok(Some(ClipboardLease::native(clipboard)));
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(None)
    }
}

#[cfg(target_os = "macos")]
struct SuppressStderr {
    saved_fd: Option<libc::c_int>,
}

#[cfg(target_os = "macos")]
impl SuppressStderr {
    fn new() -> Self {
        unsafe {
            let saved = libc::dup(2);
            if saved < 0 {
                return Self { saved_fd: None };
            }
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if devnull < 0 || libc::dup2(devnull, 2) < 0 {
                libc::close(saved);
                if devnull >= 0 {
                    libc::close(devnull);
                }
                return Self { saved_fd: None };
            }
            libc::close(devnull);
            Self {
                saved_fd: Some(saved),
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SuppressStderr {
    fn drop(&mut self) {
        if let Some(saved) = self.saved_fd {
            unsafe {
                libc::dup2(saved, 2);
                libc::close(saved);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
struct SuppressStderr;

#[cfg(not(target_os = "macos"))]
impl SuppressStderr {
    fn new() -> Self {
        Self
    }
}

fn terminal_clipboard_copy_with(
    text: &str,
    tmux: bool,
    tmux_copy_fn: &impl Fn(&str) -> Result<(), String>,
    osc52_copy_fn: &impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if tmux {
        match tmux_copy_fn(text) {
            Ok(()) => return Ok(()),
            Err(error) => {
                tracing::debug!("tmux clipboard copy failed: {error}; trying OSC 52")
            }
        }
    }
    osc52_copy_fn(text)
}

fn tmux_copy(text: &str) -> Result<(), String> {
    tmux_clipboard_copy_ready()?;
    let mut child = std::process::Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn tmux: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("failed to open tmux stdin".to_string());
    };
    stdin
        .write_all(text.as_bytes())
        .map_err(|error| format!("failed to write to tmux: {error}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for tmux: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("tmux exited with status {}", output.status)
        } else {
            format!("tmux failed: {stderr}")
        })
    }
}

fn tmux_clipboard_copy_ready() -> Result<(), String> {
    let set_clipboard = tmux_command_output(&["show-options", "-gv", "set-clipboard"])?;
    if set_clipboard.trim() == "off" {
        return Err("tmux clipboard forwarding is disabled".to_string());
    }
    let info = tmux_command_output(&["info"])?;
    if info.lines().any(|line| line.contains("Ms: [missing]")) {
        return Err("tmux clipboard forwarding is unavailable: missing Ms capability".to_string());
    }
    Ok(())
}

fn tmux_command_output(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("tmux")
        .args(args)
        .output()
        .map_err(|error| format!("failed to spawn tmux: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("tmux output was not UTF-8: {error}"))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("tmux exited with status {}", output.status)
        } else {
            format!("tmux failed: {stderr}")
        })
    }
}

fn osc52_copy(text: &str) -> Result<(), String> {
    let sequence = osc52_sequence(text, std::env::var_os("TMUX").is_some())?;
    #[cfg(unix)]
    if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty") {
        if write_osc52(tty, &sequence).is_ok() {
            return Ok(());
        }
    }
    write_osc52(std::io::stdout().lock(), &sequence)
}

fn write_osc52(mut writer: impl Write, sequence: &str) -> Result<(), String> {
    writer
        .write_all(sequence.as_bytes())
        .map_err(|error| format!("failed to write OSC 52: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("failed to flush OSC 52: {error}"))
}

fn osc52_sequence(text: &str, tmux: bool) -> Result<String, String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err("response is too large to copy".to_string());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    Ok(if tmux {
        format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")
    } else {
        format!("\x1b]52;c;{encoded}\x07")
    })
}

fn is_ssh_session() -> bool {
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
}

fn is_tmux_session() -> bool {
    std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
}

#[cfg(target_os = "linux")]
fn is_wsl_session() -> bool {
    std::fs::read_to_string("/proc/version")
        .map(|version| {
            let version = version.to_ascii_lowercase();
            version.contains("microsoft") || version.contains("wsl")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn is_wsl_session() -> bool {
    false
}

#[cfg(target_os = "linux")]
fn wsl_clipboard_copy(text: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("powershell.exe")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .args([
            "-NoProfile",
            "-Command",
            "[Console]::InputEncoding = [System.Text.Encoding]::UTF8; $ErrorActionPreference = 'Stop'; $text = [Console]::In.ReadToEnd(); Set-Clipboard -Value $text",
        ])
        .spawn()
        .map_err(|error| format!("failed to spawn powershell.exe: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err("failed to open powershell.exe stdin".to_string());
    };
    if let Err(error) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to write to powershell.exe: {error}"));
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for powershell.exe: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!(
                "powershell.exe exited with status {}",
                output.status
            ))
        } else {
            Err(format!("powershell.exe failed: {stderr}"))
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn wsl_clipboard_copy(_text: &str) -> Result<(), String> {
    Err("WSL clipboard fallback unavailable on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn rejects_oversized_copy_before_touching_clipboard() {
        assert!(copy_to_clipboard(&"x".repeat(OSC52_MAX_RAW_BYTES + 1)).is_err());
    }

    #[test]
    fn osc52_sequence_wraps_tmux_passthrough() {
        let plain = osc52_sequence("hello", false).unwrap();
        let tmux = osc52_sequence("hello", true).unwrap();
        assert!(plain.starts_with("\x1b]52;c;"));
        assert!(tmux.starts_with("\x1bPtmux;\x1b\x1b]52;c;"));
    }

    #[test]
    fn ssh_prefers_terminal_clipboard_and_skips_native() {
        let native_calls = Cell::new(0);
        let osc_calls = Cell::new(0);
        let result = copy_to_clipboard_with(
            "hello",
            CopyEnvironment {
                ssh_session: true,
                wsl_session: false,
                tmux_session: false,
            },
            |_| Ok(()),
            |_| {
                osc_calls.set(osc_calls.get() + 1);
                Ok(())
            },
            |_| {
                native_calls.set(native_calls.get() + 1);
                Ok(None)
            },
            |_| Ok(()),
        );
        assert!(result.is_ok());
        assert_eq!(native_calls.get(), 0);
        assert_eq!(osc_calls.get(), 1);
    }

    #[test]
    fn local_native_failure_uses_wsl_before_terminal_fallback() {
        let wsl_calls = Cell::new(0);
        let osc_calls = Cell::new(0);
        let result = copy_to_clipboard_with(
            "hello",
            CopyEnvironment {
                ssh_session: false,
                wsl_session: true,
                tmux_session: false,
            },
            |_| Ok(()),
            |_| {
                osc_calls.set(osc_calls.get() + 1);
                Ok(())
            },
            |_| Err("native unavailable".into()),
            |_| {
                wsl_calls.set(wsl_calls.get() + 1);
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(wsl_calls.get(), 1);
        assert_eq!(osc_calls.get(), 0);
    }
}
