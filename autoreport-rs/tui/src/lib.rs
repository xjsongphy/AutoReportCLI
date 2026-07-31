//! Terminal UI, rendering, and editor-context integration.

pub mod app;
pub(crate) mod app_command;
pub(crate) mod app_event;
pub(crate) mod app_input;
pub(crate) mod app_state;
pub(crate) mod app_view;
pub(crate) mod approval_events;
pub(crate) mod bottom_pane;
pub(crate) mod chatwidget;
pub(crate) mod clipboard_copy;
pub mod color;
pub mod config_update;
pub mod custom_terminal;
pub mod diff_render;
pub mod environment_setup;
pub mod file_search;
pub(crate) mod frame_rate_limiter;
pub(crate) mod frame_requester;
pub mod highlight;
pub(crate) mod history_cell;
pub mod ide_context;
pub(crate) mod insert_history;
pub(crate) mod line_truncation;
pub mod line_utils;
pub mod markdown_render;
pub mod model_migration;
pub(crate) mod motion;
pub(crate) mod multi_agents;
pub(crate) mod pager_overlay;
pub(crate) mod render;
pub(crate) mod request_user_input_events;
pub(crate) mod selection_list;
pub(crate) mod shimmer;
pub mod slash_command;
pub(crate) mod style;
pub(crate) mod terminal_hyperlinks;
pub mod terminal_palette;
#[cfg(test)]
pub(crate) mod test_support;
pub mod utils_string;
pub mod workspace_confirm;
pub mod wrapping;

pub use app::Tui;
pub(crate) mod terminal_probe;
