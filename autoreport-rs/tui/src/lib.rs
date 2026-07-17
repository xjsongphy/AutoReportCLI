//! Terminal UI, rendering, and editor-context integration.

pub mod app;
pub(crate) mod app_command;
pub(crate) mod app_event;
pub(crate) mod app_input;
pub(crate) mod app_state;
pub(crate) mod app_view;
pub(crate) mod approval_events;
pub(crate) mod chatwidget;
pub mod color;
pub mod config_update;
pub mod diff_render;
pub mod file_search;
pub mod highlight;
pub mod ide_context;
pub mod line_utils;
pub mod markdown_render;
pub mod model_migration;
pub mod slash_command;
pub mod terminal_palette;
pub mod utils_string;
pub mod workspace_confirm;
pub mod wrapping;

pub use app::Tui;
