//! Global local-tool environment discovery and persistence.
//!
//! The selected Python interpreter is user state, not workspace state. It is
//! therefore stored below `~/.autoreport` and reused by every project opened
//! with AutoReportCLI.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const ENVIRONMENT_TOML_FILE: &str = "environment.toml";
pub const MANAGED_VENV_DIR: &str = "venv";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PythonConfig {
    /// `conda`, `virtualenv`, `pyenv`, `path`, or `managed`.
    pub source: String,
    pub executable: PathBuf,
    /// `uv` is preferred when present; otherwise `pip` is used through the
    /// selected interpreter.
    pub package_manager: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnvironmentConfig {
    #[serde(default)]
    pub python: Option<PythonConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonCandidate {
    pub label: String,
    pub source: String,
    pub executable: PathBuf,
    pub version: String,
    pub package_manager: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolStatus {
    pub ready: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    pub system: String,
    pub shell: String,
    pub python: ToolStatus,
    pub latex: ToolStatus,
    pub typst: ToolStatus,
    pub mineru: ToolStatus,
}

/// The Python environment that shell-like tools should use by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonExecutionEnvironment {
    pub executable: PathBuf,
    pub bin_dir: PathBuf,
    pub source: String,
    pub label: String,
    pub package_manager: String,
}

pub fn environment_path(home: &Path) -> PathBuf {
    home.join(ENVIRONMENT_TOML_FILE)
}

pub fn managed_venv_path(home: &Path) -> PathBuf {
    home.join(MANAGED_VENV_DIR)
}

pub fn load_environment(home: &Path) -> Result<Option<EnvironmentConfig>> {
    let path = environment_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let config = toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    Ok(Some(config))
}

pub fn save_environment(home: &Path, config: &EnvironmentConfig) -> Result<()> {
    std::fs::create_dir_all(home)
        .with_context(|| format!("creating AutoReport home {}", home.display()))?;
    let path = environment_path(home);
    let raw = toml::to_string_pretty(config).context("serializing environment.toml")?;
    std::fs::write(&path, raw).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn needs_python_config(home: &Path) -> Result<bool> {
    let Some(config) = load_environment(home)? else {
        return Ok(true);
    };
    let Some(python) = config.python else {
        return Ok(true);
    };
    Ok(python_version(&python.executable).is_none())
}

/// Resolve the globally selected interpreter for a child process. This reads
/// the file on every call so `/env` changes take effect without restarting the
/// already-running agent loops.
pub fn selected_python_environment(home: &Path) -> Option<PythonExecutionEnvironment> {
    let config = load_environment(home).ok().flatten()?.python?;
    let executable = config.executable;
    if python_version(&executable).is_none() {
        return None;
    }
    let bin_dir = executable.parent()?.to_path_buf();
    Some(PythonExecutionEnvironment {
        executable,
        bin_dir,
        source: config.source,
        label: config.label,
        package_manager: config.package_manager,
    })
}

/// Build the process environment used by `exec`. The selected Python's bin
/// directory is prepended to PATH, making `python`, `python3`, and its installed
/// command-line tools resolve without an explicit activation command.
pub fn selected_python_process_environment(
    home: &Path,
) -> Option<std::collections::HashMap<String, String>> {
    let selected = selected_python_environment(home)?;
    let mut env = std::collections::HashMap::new();
    let path_separator = if cfg!(windows) { ';' } else { ':' };
    let path = std::env::var_os("PATH")
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .map(|path| format!("{}{}{}", selected.bin_dir.display(), path_separator, path))
        .unwrap_or_else(|| selected.bin_dir.display().to_string());
    env.insert("PATH".into(), path);
    env.insert(
        "AUTOREPORT_PYTHON".into(),
        selected.executable.display().to_string(),
    );
    env.insert("AUTOREPORT_PYTHON_SOURCE".into(), selected.source.clone());
    if matches!(selected.source.as_str(), "virtualenv" | "managed") {
        if let Some(prefix) = selected.bin_dir.parent() {
            env.insert("VIRTUAL_ENV".into(), prefix.display().to_string());
        }
    }
    if selected.source == "conda" {
        if let Some(prefix) = selected.bin_dir.parent() {
            env.insert("CONDA_PREFIX".into(), prefix.display().to_string());
        }
    }
    Some(env)
}

pub fn detect_python_environments(workspace: &Path, home: &Path) -> Vec<PythonCandidate> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut add = |path: PathBuf, source: &str, label: String| {
        let path = path.canonicalize().unwrap_or(path).to_path_buf();
        if !path.is_file() || !seen.insert(path.clone()) {
            return;
        }
        let version = python_version(&path).unwrap_or_else(|| "unknown version".into());
        candidates.push(PythonCandidate {
            label,
            source: source.to_string(),
            executable: path,
            version,
            package_manager: package_manager(),
        });
    };

    if let Some(prefix) = std::env::var_os("CONDA_PREFIX") {
        let prefix = PathBuf::from(prefix);
        add(
            python_in_prefix(&prefix),
            "conda",
            format!("Conda · {}", prefix.display()),
        );
    }
    if let Some(prefix) = std::env::var_os("VIRTUAL_ENV") {
        let prefix = PathBuf::from(prefix);
        add(
            python_in_prefix(&prefix),
            "virtualenv",
            format!("Virtualenv · {}", prefix.display()),
        );
    }
    for prefix in [workspace.join(".venv"), workspace.join(".env")] {
        add(
            python_in_prefix(&prefix),
            "virtualenv",
            format!("Workspace venv · {}", prefix.display()),
        );
    }

    if let Some(home_dir) = dirs_home() {
        let pyenv = home_dir.join(".pyenv").join("versions");
        add_pyenv_children(&pyenv, &mut add);
        for dirname in ["miniconda3", "anaconda3", "mambaforge", "miniforge3"] {
            add_conda_children(&home_dir.join(dirname).join("envs"), &mut add);
        }
    }

    for command in ["python3", "python"] {
        if let Ok(path) = which::which(command) {
            add(path, "path", format!("PATH · {command}"));
        }
    }
    // Keep the managed interpreter visible when it already exists, even if
    // the current shell's PATH no longer contains the old environment.
    let managed = python_in_prefix(&managed_venv_path(home));
    add(managed, "managed", "AutoReport managed venv".into());

    candidates
}

pub fn config_for_candidate(candidate: &PythonCandidate) -> PythonConfig {
    PythonConfig {
        source: candidate.source.clone(),
        executable: candidate.executable.clone(),
        package_manager: candidate.package_manager.clone(),
        label: candidate.label.clone(),
    }
}

pub fn config_for_custom(path: PathBuf) -> Result<PythonConfig> {
    let executable = path
        .canonicalize()
        .with_context(|| format!("Python executable does not exist: {}", path.display()))?;
    if !executable.is_file() {
        return Err(anyhow!(
            "Python path is not a file: {}",
            executable.display()
        ));
    }
    let version = python_version(&executable).ok_or_else(|| {
        anyhow!(
            "{} is not a runnable Python interpreter",
            executable.display()
        )
    })?;
    Ok(PythonConfig {
        source: "path".into(),
        executable,
        package_manager: package_manager(),
        label: format!("Custom Python · {version}"),
    })
}

pub fn ensure_python_environment(home: &Path, mut config: PythonConfig) -> Result<PythonConfig> {
    if config.source == "managed" {
        let venv = managed_venv_path(home);
        let python = python_in_prefix(&venv);
        if !python.is_file() {
            std::fs::create_dir_all(home)
                .with_context(|| format!("creating {}", home.display()))?;
            if which::which("uv").is_ok() {
                run_command("uv", ["venv", "--seed", &venv.display().to_string()])?;
                config.package_manager = "uv".into();
            } else {
                let base = detect_python_environments(Path::new("."), home)
                    .into_iter()
                    .find(|candidate| candidate.source != "managed")
                    .map(|candidate| candidate.executable)
                    .or_else(|| which::which("python3").ok())
                    .or_else(|| which::which("python").ok())
                    .ok_or_else(|| anyhow!("no system Python found to create the managed venv"))?;
                run_command_path(&base, ["-m", "venv", &venv.display().to_string()])?;
                config.package_manager = "pip".into();
            }
        }
        config.executable = python;
        config.label = "AutoReport managed venv".into();
    }
    python_version(&config.executable).ok_or_else(|| {
        anyhow!(
            "Python interpreter is not runnable: {}",
            config.executable.display()
        )
    })?;
    Ok(config)
}

pub fn snapshot(home: &Path) -> EnvironmentSnapshot {
    let python = load_environment(home)
        .ok()
        .flatten()
        .and_then(|config| config.python)
        .map(|python| {
            let version = python_version(&python.executable);
            ToolStatus {
                ready: version.is_some(),
                detail: version
                    .map(|version| {
                        format!(
                            "{} · {} · {} · source={} · package_manager={}",
                            python.label,
                            python.executable.display(),
                            version,
                            python.source,
                            python.package_manager,
                        )
                    })
                    .unwrap_or_else(|| format!("missing · {}", python.executable.display())),
            }
        })
        .unwrap_or_else(|| ToolStatus {
            ready: false,
            detail: "not configured".into(),
        });
    let latex = tool_status(&["pdflatex", "latexmk"], "LaTeX");
    let typst = tool_status(&["typst"], "Typst");
    let mineru_cli = which::which("mineru-open-api").ok();
    let mineru_key = ["MINERU_API_KEY", "MINERU_TOKEN", "MINERU_OPEN_API_KEY"]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()))
        || mineru_config_has_token();
    let mineru = ToolStatus {
        ready: mineru_cli.is_some() && mineru_key,
        detail: match (mineru_cli, mineru_key) {
            (Some(path), true) => format!("{} · authenticated", path.display()),
            (Some(path), false) => format!("{} · API key missing", path.display()),
            (None, _) => "mineru-open-api not found".into(),
        },
    };
    EnvironmentSnapshot {
        system: format!("{} / {}", std::env::consts::OS, std::env::consts::ARCH),
        shell: std::env::var("SHELL")
            .or_else(|_| std::env::var("ComSpec"))
            .unwrap_or_else(|_| "unknown shell".into()),
        python,
        latex,
        typst,
        mineru,
    }
}

fn mineru_config_has_token() -> bool {
    let Some(home) = dirs_home() else {
        return false;
    };
    let path = home.join(".mineru").join("config.yaml");
    mineru_config_has_token_at(&path)
}

fn mineru_config_has_token_at(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    contents.lines().any(|line| {
        line.trim_start()
            .strip_prefix("token:")
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub fn render_context(home: &Path) -> String {
    let snapshot = snapshot(home);
    format!(
        "## Local Environment\n- system: {}\n- shell: {}\n- Python: {} ({})\n- LaTeX: {} ({})\n- Typst: {} ({})\n- MinerU open API: {} ({})",
        snapshot.system,
        snapshot.shell,
        readiness(snapshot.python.ready),
        snapshot.python.detail,
        readiness(snapshot.latex.ready),
        snapshot.latex.detail,
        readiness(snapshot.typst.ready),
        snapshot.typst.detail,
        readiness(snapshot.mineru.ready),
        snapshot.mineru.detail,
    )
}

fn readiness(ready: bool) -> &'static str {
    if ready { "ready" } else { "not ready" }
}

fn tool_status(commands: &[&str], label: &str) -> ToolStatus {
    for command in commands {
        if let Ok(path) = which::which(command) {
            let version = Command::new(&path)
                .arg("--version")
                .output()
                .ok()
                .map(|output| first_line(&output.stdout, &output.stderr));
            return ToolStatus {
                ready: true,
                detail: version
                    .filter(|version| !version.is_empty())
                    .map(|version| format!("{} · {version}", path.display()))
                    .unwrap_or_else(|| path.display().to_string()),
            };
        }
    }
    ToolStatus {
        ready: false,
        detail: format!("{label} executable not found"),
    }
}

fn python_version(path: &Path) -> Option<String> {
    let output = Command::new(path).arg("--version").output().ok()?;
    let version = first_line(&output.stdout, &output.stderr);
    (!version.is_empty()).then_some(version)
}

fn first_line(stdout: &[u8], stderr: &[u8]) -> String {
    let bytes = if stdout.is_empty() { stderr } else { stdout };
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn preferred_package_manager() -> String {
    if which::which("uv").is_ok() {
        "uv".into()
    } else {
        "pip".into()
    }
}

fn package_manager() -> String {
    preferred_package_manager()
}

fn python_in_prefix(prefix: &Path) -> PathBuf {
    if cfg!(windows) {
        prefix.join("Scripts").join("python.exe")
    } else {
        let python = prefix.join("bin").join("python");
        if python.is_file() {
            python
        } else {
            prefix.join("bin").join("python3")
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}

fn add_pyenv_children<F>(root: &Path, add: &mut F)
where
    F: FnMut(PathBuf, &str, String),
{
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            add(
                python_in_prefix(&path),
                "pyenv",
                format!("pyenv · {}", entry.file_name().to_string_lossy()),
            );
        }
    }
}

fn add_conda_children<F>(root: &Path, add: &mut F)
where
    F: FnMut(PathBuf, &str, String),
{
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            add(
                python_in_prefix(&path),
                "conda",
                format!("Conda · {}", entry.file_name().to_string_lossy()),
            );
        }
    }
}

fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {program}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| anyhow!("{program} exited with {status}"))
}

fn run_command_path<const N: usize>(program: &Path, args: [&str; N]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("running {}", program.display()))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| anyhow!("{} exited with {status}", program.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn environment_roundtrips_below_global_home() {
        let dir = tempdir().unwrap();
        let config = EnvironmentConfig {
            python: Some(PythonConfig {
                source: "path".into(),
                executable: PathBuf::from("/tmp/python"),
                package_manager: "uv".into(),
                label: "custom".into(),
            }),
        };
        save_environment(dir.path(), &config).unwrap();
        assert_eq!(load_environment(dir.path()).unwrap(), Some(config));
    }

    #[test]
    fn context_reports_tool_readiness_without_secret_values() {
        let dir = tempdir().unwrap();
        let context = render_context(dir.path());
        assert!(context.contains("Local Environment"));
        assert!(!context.contains("API key configured"));
    }

    #[test]
    fn mineru_config_token_detection_requires_a_nonempty_token() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");

        std::fs::write(&config, "token: configured-token\n").unwrap();
        assert!(mineru_config_has_token_at(&config));

        std::fs::write(&config, "token:   \n").unwrap();
        assert!(!mineru_config_has_token_at(&config));

        std::fs::write(&config, "api_key: configured-token\n").unwrap();
        assert!(!mineru_config_has_token_at(&config));
    }

    #[test]
    fn selected_python_is_translated_to_child_process_environment() {
        let Some(python) = which::which("python3")
            .or_else(|_| which::which("python"))
            .ok()
        else {
            return;
        };
        let dir = tempdir().unwrap();
        save_environment(
            dir.path(),
            &EnvironmentConfig {
                python: Some(PythonConfig {
                    source: "path".into(),
                    executable: python.clone(),
                    package_manager: "pip".into(),
                    label: "test python".into(),
                }),
            },
        )
        .unwrap();
        let selected = selected_python_environment(dir.path()).unwrap();
        let child_env = selected_python_process_environment(dir.path()).unwrap();
        assert_eq!(child_env["AUTOREPORT_PYTHON"], python.display().to_string());
        assert!(child_env["PATH"].starts_with(&selected.bin_dir.display().to_string()));
    }
}
