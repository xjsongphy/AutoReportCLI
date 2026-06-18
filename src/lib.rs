//! AutoReportCLI library crate. The binary in `main.rs` is a thin wrapper over
//! these modules, which keeps them unit-testable.

#![allow(dead_code)]

pub mod bus;
pub mod bundled;
pub mod codex_render;
pub mod config;
pub mod file_search;
pub mod provider;
pub mod prompts;
pub mod runtime;
pub mod skills;
pub mod taskboard;
pub mod tools;
pub mod tui;
pub mod types;
