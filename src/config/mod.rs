//! Configuration: schema, YAML loading, env-var fallback, and workspace
//! folder auto-initialization.
//!
//! Mirrors AutoReport's `config/` package (Pydantic Settings + YAML) but in
//! plain serde. Working directory is always the run directory.

pub mod loader;
pub mod schema;

pub use loader::{ensure_workspace, load_settings, needs_config, resolve_api_key, save_settings};
pub use schema::{AgentDefaults, Settings};
