//! Configuration: schema, YAML loading, env-var fallback, and workspace
//! folder auto-initialization.
//!
//! Mirrors AutoReport's `config/` package (Pydantic Settings + YAML) but in
//! plain serde. Working directory is always the run directory.

pub mod loader;
pub mod schema;

pub use loader::{
    ensure_workspace, load_settings, needs_api_config, needs_config, needs_model_config,
    resolve_api_key, resolve_model, save_settings,
};
pub use schema::{AgentDefaults, ModelAssignments, ModelConfig, Settings};
