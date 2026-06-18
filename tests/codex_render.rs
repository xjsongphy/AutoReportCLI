//! Smoke test for the vendored codex markdown renderer (exercises syntect
//! highlighting for a fenced code block).

use autoreport_cli::codex_render::markdown_render;

#[test]
fn renders_markdown_and_code_via_codex() {
    let input = "# Title\n\nSome **bold** and `code`.\n\n```rust\nlet x = 1;\n```\n\n- a\n- b\n";
    let text = markdown_render::render_markdown_text(input);
    let joined: String = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.to_string())
        .collect();
    assert!(joined.contains("Title"));
    assert!(joined.contains("let x = 1;"));
    assert!(joined.contains("bold"));
}

#[test]
fn empty_input_does_not_panic() {
    let _ = markdown_render::render_markdown_text("");
}
