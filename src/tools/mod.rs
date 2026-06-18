//! Tool system. Each tool implements [`Tool`]; a [`ToolRegistry`] exposes
//! definitions to the model and dispatches calls. Per-agent write isolation is
//! enforced inside the file tools via the assigned `write_dir`.

pub mod apply_patch;
pub mod exec_tool;
pub mod file_tools;
pub mod manifest;
pub mod registry;
pub mod skill_tool;
pub mod task_tools;

pub use registry::ToolRegistry;
