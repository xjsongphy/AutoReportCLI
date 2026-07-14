//! AutoReportCLI — entry point.
//!
//! Resolves the main/sub model bindings, ensures the workspace folder layout exists,
//! spins up the loop manager (one persistent agent loop per agent type), then
//! runs the codex-style TUI.

use anyhow::Result;
use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use autoreport_core::bus::Bus;
use autoreport_core::config;
use autoreport_core::config::Settings;
use autoreport_core::provider::build_provider;
use autoreport_runtime::LoopManager;
use autoreport_tui::Tui;
use autoreport_tui::config_update::{ConfigScreen, Outcome};
use autoreport_tui::model_migration::ModelScreen;

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.verbose {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    }

    let workspace = match cli.workspace {
        Some(p) => p,
        None => std::env::current_dir()?,
    };

    // 1) Make sure the required project folders exist.
    config::ensure_workspace(&workspace)?;

    // 2) Load config.
    let mut settings = config::load_settings(&workspace)?;

    // 3) Sync the two upstream repositories (cc-switch presets + skills), like
    //    AutoReport does on startup. Best-effort: network failure keeps the
    //    existing cache and continues. `--no-sync` skips the fetch; use the
    //    existing cache instead. `--sync-presets` forces a full fetch + exits.
    let should_sync =
        cli.sync_presets || (!cli.no_sync && !autoreport_core::sync::cache_is_warm(&workspace));
    if should_sync {
        let report =
            autoreport_core::sync::sync_all(&workspace, std::time::Duration::from_secs(10)).await;
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

    // Always register providers from the (now possibly refreshed) preset cache.
    // cc-switch's real shape: each entry's `settingsConfig.env` block carries
    // the base URL, auth-token env var, and default model.
    let cfg_dir = autoreport_core::sync::external_dir(&workspace)
        .join("cc-switch")
        .join("src")
        .join("config");
    for file in [
        "claudeProviderPresets.ts",
        "codexProviderPresets.ts",
        "geminiProviderPresets.ts",
        "openaiProviderPresets.ts",
        "opencodeProviderPresets.ts",
        "openclawProviderPresets.ts",
        "hermesProviderPresets.ts",
        "universalProviderPresets.ts",
    ] {
        let path = cfg_dir.join(file);
        if let Ok(body) = std::fs::read_to_string(&path) {
            let kind = autoreport_core::sync::file_kind(file)
                .map(|(k, _)| k)
                .unwrap_or("openai");
            let presets = autoreport_core::sync::parse_presets(&body, kind);
            autoreport_core::sync::register_providers(&mut settings, &presets);
        }
    }

    // API setup is always first: an existing config file with expired/missing
    // credentials must re-open this page just like a first launch does.
    if config::needs_api_config(&settings) {
        match run_wizard(&workspace, settings.clone()) {
            Outcome::Saved => {
                // Re-read the just-written config and continue startup.
                settings = config::load_settings(&workspace)?;
            }
            Outcome::Cancelled => {
                log::info!("config wizard cancelled; continuing with env/config defaults");
            }
        }
    }

    if config::needs_api_config(&settings) {
        anyhow::bail!(
            "no usable API is configured; add an API key in /config or the relevant environment variable"
        );
    }

    // API setup and model selection are deliberately separate. After API
    // configuration is complete, the first-run flow asks for main/sub models.
    if config::needs_model_config(&settings) {
        match run_model_wizard(&workspace, settings.clone()) {
            Outcome::Saved => settings = config::load_settings(&workspace)?,
            Outcome::Cancelled => {}
        }
    }

    let (main_api, main_model) = config::resolve_model(&settings, &settings.models.main, "main")?;
    let (sub_api, sub_model) = config::resolve_model(&settings, &settings.models.sub, "sub")?;
    let main_provider = build_provider(main_api, main_model)?;
    let sub_provider = build_provider(sub_api, sub_model)?;
    let provider_id = format!("main: {} · sub: {}", main_provider.id(), sub_provider.id());

    log::info!("workspace: {}", workspace.display());
    log::info!("{}", provider_id);

    // 3) Start the agent loops (one per type, all persistent).
    let bus = Bus::new();
    let sandbox =
        autoreport_sandboxing::SandboxSpec::new(settings.sandbox_mode, settings.sandbox_network);
    let mut manager = LoopManager::new(
        &workspace,
        main_provider,
        sub_provider,
        bus.clone(),
        settings.agents.clone(),
        sandbox,
    );
    manager.start()?;

    // 4) Run the codex-style TUI.
    let manager = Arc::new(manager);
    let tui = Tui::new(manager, bus, workspace, provider_id);
    tui.run().await?;

    Ok(())
}

/// Open the full-screen model-selection wizard after API setup.
fn run_model_wizard(workspace: &std::path::Path, settings: Settings) -> Outcome {
    enable_raw_mode().ok();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Outcome::Cancelled;
        }
    };
    let mut screen = ModelScreen::new(settings, workspace.to_path_buf());
    let outcome = screen
        .run_fullscreen(&mut terminal)
        .unwrap_or(Outcome::Cancelled);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    outcome
}

/// Open the full-screen config wizard. Owns terminal setup/teardown.
fn run_wizard(workspace: &std::path::Path, settings: Settings) -> Outcome {
    enable_raw_mode().ok();
    let _ = execute!(io::stdout(), EnterAlternateScreen);
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(_) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
            return Outcome::Cancelled;
        }
    };
    let mut screen = ConfigScreen::new(settings, workspace.to_path_buf());
    let outcome = screen
        .run_fullscreen(&mut terminal)
        .unwrap_or(Outcome::Cancelled);
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    outcome
}
