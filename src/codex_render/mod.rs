//! Codex display stack, vendored from `codex-rs/tui` (markdown_render.rs,
//! wrapping.rs, render/highlight.rs, render/line_utils.rs). These are codex's
//! stable rendering routines, adapted only for import paths and stubbed
//! helpers. Kept verbatim otherwise so behaviour matches codex.

pub mod codex_utils_string;
pub mod color;
pub mod highlight;
pub mod line_utils;
pub mod markdown_render;
pub mod terminal_palette;
pub mod wrapping;
