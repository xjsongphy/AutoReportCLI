//! Configuration: Codex-style global home, TOML loading, env-var fallback,
//! loading and workspace folder auto-initialization.
//!
//! Mirrors Codex's global `config.toml` + `auth.json` layout. Working directory
//! remains the selected report project and only report artifacts are written there.

pub mod loader;
pub mod schema;

pub use loader::{
    ensure_autoreport_home, ensure_workspace, find_autoreport_home, load_settings,
    needs_api_config, needs_model_config, resolve_api_key, resolve_model, save_settings,
    workspace_is_complete, workspace_state_dir,
};
pub use schema::{AgentDefaults, ModelAssignments, ModelConfig, Settings};
