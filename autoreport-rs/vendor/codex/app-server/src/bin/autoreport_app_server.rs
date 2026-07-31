//! Stdio entry point for AutoReport's provider-backed app-server runtime.
//!
//! Configuration, credentials, and model selection come solely from
//! AutoReport's local configuration. This binary deliberately has no login,
//! cloud, or extension bootstrap path.

use anyhow::{Context, Result, bail};
use autoreport_app_server::provider_runtime_server::ProviderRuntimeServer;
use autoreport_app_server::provider_transport_runner::serve_stdio;
use autoreport_app_server::runtime_adapter::RuntimeSessionRegistry;
use autoreport_core::bus::Bus;
use autoreport_core::config;
use autoreport_core::provider::build_provider;
use autoreport_runtime::LoopManager;
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("creating AutoReport app-server runtime")?
        .block_on(run())
}

async fn run() -> Result<()> {
    let workspace = parse_workspace()?;
    let autoreport_home = config::find_autoreport_home()?;
    config::ensure_autoreport_home(&autoreport_home)?;
    let settings = config::load_settings(&autoreport_home)?;
    config::ensure_workspace(&workspace)?;

    let (main_config, main_model) =
        config::resolve_model(&settings, &settings.models.main, "main")?;
    let (sub_config, sub_model) = config::resolve_model(&settings, &settings.models.sub, "sub")?;
    let main_provider = build_provider(main_config, main_model)?;
    let sub_provider = build_provider(sub_config, sub_model)?;

    let bus = Bus::new();
    let sandbox =
        autoreport_sandboxing::SandboxSpec::new(settings.sandbox_mode, settings.sandbox_network);
    let mut manager = LoopManager::new(
        &workspace,
        &autoreport_home,
        main_provider,
        sub_provider,
        bus,
        settings.agents,
        sandbox,
    );
    manager.start().await?;

    let server = Arc::new(ProviderRuntimeServer::new(
        Arc::new(RuntimeSessionRegistry::default()),
        Arc::new(manager),
        autoreport_home,
        workspace,
    ));
    serve_stdio(server).await
}

fn parse_workspace() -> Result<PathBuf> {
    let mut args = std::env::args_os().skip(1);
    let mut workspace = None;

    while let Some(arg) = args.next() {
        if arg == "--workspace" {
            let value = args.next().context("--workspace requires a directory")?;
            if workspace.replace(PathBuf::from(value)).is_some() {
                bail!("--workspace may only be specified once");
            }
        } else if let Some(value) = arg
            .to_str()
            .and_then(|arg| arg.strip_prefix("--workspace="))
        {
            if value.is_empty() {
                bail!("--workspace requires a directory");
            }
            if workspace.replace(PathBuf::from(value)).is_some() {
                bail!("--workspace may only be specified once");
            }
        } else if arg == "--help" || arg == "-h" {
            println!("Usage: autoreport-app-server [--workspace DIR]");
            std::process::exit(0);
        } else {
            bail!("unknown argument: {}", arg.to_string_lossy());
        }
    }

    workspace
        .map(Ok)
        .unwrap_or_else(std::env::current_dir)
        .context("resolving workspace directory")
}
