//! AutoReportCLI — entry point.
//!
//! Resolves the main/sub model bindings, ensures the workspace folder layout exists,
//! spins up the loop manager (one persistent agent loop per agent type), then
//! runs the codex-style TUI.

use anyhow::Result;
use autoreport_tui::custom_terminal::Terminal;
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use autoreport_core::bus::Bus;
use autoreport_core::config;
use autoreport_core::config::Settings;
use autoreport_core::environment;
use autoreport_core::provider::build_provider;
use autoreport_runtime::LoopManager;
use autoreport_tui::Tui;
use autoreport_tui::config_update::Outcome;
use autoreport_tui::configuration_flow::ConfigurationFlow;
use autoreport_tui::environment_setup::EnvironmentScreen;
use autoreport_tui::workspace_confirm::{WorkspaceOutcome, WorkspaceScreen};

#[derive(Parser, Debug)]
#[command(
    name = "autoreport",
    version,
    about = "AutoReportCLI — codex-style multi-agent CLI for automated physics experiment reports"
)]
struct Cli {
    /// Workspace directory (defaults to the current working directory).
    #[arg(long, value_name = "DIR")]
    workspace: Option<PathBuf>,

    /// Force a full re-sync of the cc-switch presets and skills repos, then exit.
    #[arg(long)]
    sync_presets: bool,

    /// Skip the startup repository sync (use cached skills/presets only).
    #[arg(long)]
    no_sync: bool,

    /// Increase logging verbosity.
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    // Apply OS-level process hardening as the very first thing (mirrors codex's
    // `responses-api-proxy`): deny debugger attach (PT_DENY_ATTACH on macOS),
    // scrub DYLD_* env, drop core dumps. Our binary holds API keys in memory,
    // so this narrows the local-attach exfil surface. Best-effort; never fatal.
    autoreport_process_hardening::pre_main_hardening();
    dispatch_windows_sandbox_wrapper();
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

#[cfg(target_os = "windows")]
fn dispatch_windows_sandbox_wrapper() {
    use std::ffi::OsStr;

    if std::env::args_os().nth(1).as_deref()
        == Some(OsStr::new(
            autoreport_windows_sandbox::AUTOREPORT_WINDOWS_SANDBOX_ARG1,
        ))
    {
        autoreport_windows_sandbox::run_windows_sandbox_wrapper_main();
    }
}

#[cfg(not(target_os = "windows"))]
fn dispatch_windows_sandbox_wrapper() {}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    }

    // Codex keeps the session cwd absolute all the way into the TUI. Do the
    // same for AutoReport so the header, file index, agent loops, and footer
    // all describe one stable directory, while the renderer can consistently
    // shorten paths below $HOME to `~/...`.
    let workspace = match cli.workspace {
        Some(p) if p.is_absolute() => p,
        Some(p) => std::env::current_dir()?.join(p),
        None => std::env::current_dir()?,
    };

    let autoreport_home = config::find_autoreport_home()?;
    config::ensure_autoreport_home(&autoreport_home)?;
    // Load config without touching the selected workspace. Report-layout
    // creation is deferred until after explicit consent.
    let mut settings = config::load_settings(&autoreport_home)?;

    // 3) Sync the two upstream repositories (cc-switch presets + skills), like
    //    AutoReport does on startup. Best-effort: network failure keeps the
    //    existing cache and continues. `--no-sync` skips the fetch; use the
    //    existing cache instead. `--sync-presets` forces a full fetch + exits.
    let should_sync = cli.sync_presets
        || (!cli.no_sync && !autoreport_core::sync::cache_is_warm(&autoreport_home));
    if should_sync {
        let report =
            autoreport_core::sync::sync_all(&autoreport_home, std::time::Duration::from_secs(10))
                .await;
        if report.total() > 0 {
            eprintln!(
                "synced {} preset(s) and {} skill(s) from cc-switch + skills repos",
                report.presets_fetched,
                report.skills_fetched.len()
            );
        } else if !report.errors.is_empty() {
            eprintln!(
                "repo sync unavailable (using cache); {} fetch(es) failed",
                report.errors.len()
            );
        }
    }

    if cli.sync_presets {
        return Ok(());
    }

    // One shared flow keeps provider edits and Main/Sub assignments in a
    // single in-memory draft, both on first start and under `/model`.
    if config::needs_api_config(&settings) || config::needs_model_config(&settings) {
        match run_wizard(&autoreport_home, settings.clone()) {
            Outcome::Saved | Outcome::Continue => {
                // Re-read the just-written config and continue startup.
                settings = config::load_settings(&autoreport_home)?;
            }
            Outcome::Cancelled | Outcome::Quit => {
                log::info!("config wizard cancelled; continuing with env/config defaults");
            }
        }
    }

    if config::needs_api_config(&settings) {
        anyhow::bail!(
            "no usable API is configured; add an API key in /model or the relevant environment variable"
        );
    }

    // Local tool and Python setup follows API/model selection and precedes the
    // workspace trust gate, so the selected interpreter is available to every
    // agent before its first prompt is assembled.
    if environment::needs_python_config(&autoreport_home)? {
        match run_environment_wizard(&autoreport_home, &workspace) {
            Outcome::Saved | Outcome::Continue => {}
            Outcome::Cancelled | Outcome::Quit => return Ok(()),
        }
    }

    // The selected directory is user data. Ask before creating the report
    // layout only when the standard layout is not already present. Existing
    // complete workspaces can proceed directly without another confirmation.
    if !config::workspace_is_complete(&workspace) {
        match run_workspace_confirmation(&workspace) {
            WorkspaceOutcome::Confirmed => {}
            WorkspaceOutcome::Cancelled => return Ok(()),
        }
    }

    config::ensure_workspace(&workspace)?;

    // Report language is project-scoped. Existing unambiguous projects are
    // upgraded silently; new or ambiguous projects use the language stage of
    // the environment wizard.
    if autoreport_core::project::load_project_config(&autoreport_home, &workspace)?.is_none() {
        match autoreport_core::project::infer_report_language(&workspace) {
            autoreport_core::project::ReportLanguageInference::Latex => {
                autoreport_core::project::save_project_config(
                    &autoreport_home,
                    &workspace,
                    &autoreport_core::project::ProjectConfig {
                        report_language: autoreport_core::project::ReportLanguage::Latex,
                    },
                )?;
            }
            autoreport_core::project::ReportLanguageInference::Typst => {
                autoreport_core::project::save_project_config(
                    &autoreport_home,
                    &workspace,
                    &autoreport_core::project::ProjectConfig {
                        report_language: autoreport_core::project::ReportLanguage::Typst,
                    },
                )?;
            }
            autoreport_core::project::ReportLanguageInference::Empty
            | autoreport_core::project::ReportLanguageInference::Ambiguous => {
                match run_environment_wizard(&autoreport_home, &workspace) {
                    Outcome::Saved | Outcome::Continue => {}
                    Outcome::Cancelled | Outcome::Quit => return Ok(()),
                }
            }
        }
    }
    let language = autoreport_core::project::selected_report_language(&autoreport_home, &workspace)
        .unwrap_or(autoreport_core::project::ReportLanguage::Latex);
    let prepared = autoreport_core::project::prepare_report_resources(
        &workspace,
        &autoreport_home,
        language,
        autoreport_core::project::MaterializePolicy::CreateMissingOnly,
    )?;
    if !prepared.failed.is_empty() {
        log::warn!(
            "report resources incomplete: {}",
            prepared.failed.join("; ")
        );
    }

    let (main_api, main_model) = config::resolve_model(&settings, &settings.models.main, "main")?;
    let (sub_api, sub_model) = config::resolve_model(&settings, &settings.models.sub, "sub")?;
    let main_provider = build_provider(main_api, main_model)?;
    let sub_provider = build_provider(sub_api, sub_model)?;
    // Codex's session header receives the selected model slug, not the
    // provider transport id. The latter includes prefixes such as
    // `openai-responses/` and was the source of the header overflow shown in
    // the original UI. Keep provider ids for diagnostics only.
    let main_provider_id = main_provider.id().to_string();
    let sub_provider_id = sub_provider.id().to_string();
    let provider_id = format!("main: {main_provider_id} · sub: {sub_provider_id}");

    log::info!("workspace: {}", workspace.display());
    log::info!("{}", provider_id);

    // 3) Start the agent loops (one per type, all persistent).
    let bus = Bus::new();
    let sandbox =
        autoreport_sandboxing::SandboxSpec::new(settings.sandbox_mode, settings.sandbox_network);
    let mut manager = LoopManager::new(
        &workspace,
        &autoreport_home,
        main_provider,
        sub_provider,
        bus.clone(),
        settings.agents.clone(),
        sandbox,
    );
    manager.start().await?;

    // 4) Run the codex-style TUI.
    let manager = Arc::new(manager);
    let tui = Tui::new(
        manager,
        bus,
        autoreport_home,
        workspace,
        main_model.to_string(),
        sub_model.to_string(),
    );
    tui.run().await?;

    Ok(())
}

/// Open the global Python/local-tool environment page after model selection.
fn run_environment_wizard(home: &std::path::Path, workspace: &std::path::Path) -> Outcome {
    enable_raw_mode().ok();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::with_options(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Outcome::Cancelled;
        }
    };
    let mut screen = if !environment::needs_python_config(home).unwrap_or(true) {
        EnvironmentScreen::language_only(home.to_path_buf(), workspace.to_path_buf())
    } else {
        EnvironmentScreen::new(home.to_path_buf(), workspace.to_path_buf())
    };
    let outcome = screen
        .run_fullscreen(&mut terminal)
        .unwrap_or(Outcome::Cancelled);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    outcome
}

/// Open the full-screen config wizard. Owns terminal setup/teardown.
fn run_wizard(home: &std::path::Path, settings: Settings) -> Outcome {
    enable_raw_mode().ok();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::with_options(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Outcome::Cancelled;
        }
    };
    let presets = autoreport_core::sync::load_presets(home);
    let mut screen = ConfigurationFlow::new(settings, home.to_path_buf(), presets);
    let outcome = screen
        .run_fullscreen(&mut terminal)
        .unwrap_or(Outcome::Cancelled);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    outcome
}

/// Ask for explicit permission before AutoReport initializes the selected
/// workspace. Terminal setup/teardown is kept local to the startup page so a
/// declined prompt leaves the user's terminal in a clean state.
fn run_workspace_confirmation(workspace: &std::path::Path) -> WorkspaceOutcome {
    enable_raw_mode().ok();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::with_options(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return WorkspaceOutcome::Cancelled;
        }
    };
    let mut screen = WorkspaceScreen::new(workspace.to_path_buf());
    let outcome = screen
        .run_fullscreen(&mut terminal)
        .unwrap_or(WorkspaceOutcome::Cancelled);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    outcome
}
