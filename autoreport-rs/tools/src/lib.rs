//! Tool system. Each tool implements [`Tool`]; a [`ToolRegistry`] exposes
//! definitions to the model and dispatches calls. Per-agent write isolation is
//! enforced by the OS sandbox for `exec` and the shared filesystem guards for
//! `apply_patch`.

pub mod apply_patch;
pub mod codex_shell;
pub mod exec_tool;
pub mod file_tools;
pub mod manifest;
pub mod registry;
pub mod request_user_input;
pub mod task_tools;

pub use registry::{ToolExecutionContext, ToolRegistry};
