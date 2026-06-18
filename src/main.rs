//! AutoReportCLI — entry point.
//!
//! Resolves the active provider, ensures the workspace folder layout exists,
//! spins up the loop manager (one persistent agent loop per agent type), then
//! runs the codex-style TUI.

use anyhow::{anyhow, Result};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;

use autoreport_cli::bus::Bus;
use autoreport_cli::config;
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

    // 2) Load config + pick the active provider.
    let mut settings = config::load_settings(&workspace)?;
    if let Some(key) = &cli.provider {
        settings.active_provider = Some(key.clone());
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
    let tui = Tui::new(manager, bus, workspace.display().to_string(), provider_id);
    tui.run().await?;

    Ok(())
}
