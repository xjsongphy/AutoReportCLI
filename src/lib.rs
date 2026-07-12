//! AutoReportCLI library crate. The binary in `main.rs` is a thin wrapper over
//! these modules, which keeps them unit-testable.

#![allow(dead_code)]

pub mod bundled;
pub mod bus;
pub mod codex_render;
pub mod config;
pub mod config_ui;
pub mod diff_render;
pub mod file_search;
pub mod ide_context;
pub mod prompts;
pub mod provider;
pub mod rollout;
pub mod runtime;
pub mod skills;
pub mod sync;
pub mod taskboard;
pub mod tools;
pub mod tui;
pub mod types;
