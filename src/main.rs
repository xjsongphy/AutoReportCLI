//! AutoReportCLI — entry point.
//!
//! Resolves the active provider, ensures the workspace folder layout exists,
//! spins up the loop manager (one persistent agent loop per agent type), then
//! runs the codex-style TUI.

use anyhow::{Result, anyhow};
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

use autoreport_cli::bus::Bus;
use autoreport_cli::config;
use autoreport_cli::config::Settings;
use autoreport_cli::config_ui::{ConfigScreen, Outcome};
use autoreport_cli::provider::build_provider;
use autoreport_cli::runtime::LoopManager;
use autoreport_cli::tui::Tui;

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

    /// Override the active provider key from the config.
    #[arg(long, value_name = "KEY")]
    provider: Option<String>,

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
    if let Some(key) = &cli.provider {
        settings.active_provider = Some(key.clone());
    }

    // 3) Sync the two upstream repositories (cc-switch presets + skills), like
    //    AutoReport does on startup. Best-effort: network failure keeps the
    //    existing cache and continues. `--no-sync` skips the fetch; use the
    //    existing cache instead. `--sync-presets` forces a full fetch + exits.
    let should_sync =
        cli.sync_presets || (!cli.no_sync && !autoreport_cli::sync::cache_is_warm(&workspace));
    if should_sync {
        let report =
            autoreport_cli::sync::sync_all(&workspace, std::time::Duration::from_secs(10)).await;
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
    let cfg_dir = autoreport_cli::sync::external_dir(&workspace)
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
            let kind = autoreport_cli::sync::file_kind(file)
                .map(|(k, _)| k)
                .unwrap_or("openai");
            let presets = autoreport_cli::sync::parse_presets(&body, kind);
            autoreport_cli::sync::register_providers(&mut settings, &presets);
        }
    }

    // First-run wizard: no config file and no resolvable provider key.
    if config::needs_config(&workspace, &settings) {
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

    let active_key = settings
        .active_provider
        .clone()
        .or_else(|| settings.providers.keys().next().cloned())
        .ok_or_else(|| {
            anyhow!(
                "no provider configured. Set an API key env var (e.g. ANTHROPIC_API_KEY) or \
                 create autoreport.config.yaml with a providers entry."
            )
        })?;
    let provider_cfg = settings
        .providers
        .get(&active_key)
        .ok_or_else(|| anyhow!("provider '{active_key}' not found in config"))?;
    let provider = build_provider(provider_cfg)?;
    let provider_id = provider.id().to_string();

    log::info!("workspace: {}", workspace.display());
    log::info!("active provider: {}", provider_id);

    // 3) Start the agent loops (one per type, all persistent).
    let bus = Bus::new();
    let mut manager = LoopManager::new(&workspace, provider, bus.clone(), settings.agents.clone());
    manager.start()?;

    // 4) Run the codex-style TUI.
    let manager = Arc::new(manager);
    let tui = Tui::new(manager, bus, workspace, provider_id);
    tui.run().await?;

    Ok(())
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
