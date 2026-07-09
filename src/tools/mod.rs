//! Tool system. Each tool implements [`Tool`]; a [`ToolRegistry`] exposes
//! definitions to the model and dispatches calls. Per-agent write isolation is
//! enforced inside the shared filesystem guards used by `exec` and
//! `apply_patch`.

pub mod apply_patch;
pub mod codex_shell;
pub mod exec_tool;
pub mod file_tools;
pub mod list_dir;
pub mod manifest;
pub mod registry;
pub mod task_tools;

pub use registry::ToolRegistry;
