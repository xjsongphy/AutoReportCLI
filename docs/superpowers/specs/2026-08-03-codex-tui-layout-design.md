# Codex-style TUI layout alignment

## Goal

Align AutoReportCLI's terminal UI with the existing Codex TUI implementation, especially the composer surface, bottom-pane geometry, tool-call spacing, and tool-call colors. Reuse the implementation in `../codex/codex-rs/tui` wherever the local runtime model permits, and keep AutoReport-specific behavior behind small adapters.

## Scope and constraints

- Preserve the existing AutoReport runtime, five-agent model, tool registry, configuration screens, and event model.
- Preserve unrelated worktree changes; this work touches only the TUI layout/rendering path and its focused tests.
- Do not create a second layout system for the composer. Height calculation, rendering, and cursor placement must share the same render tree.
- Keep the terminal behavior represented by the supplied Codex screenshots: a bottom composer surface, transcript above it, stable breathing rows, and visually distinct tool rows.

## Design

### Composer and viewport

Use the local `Renderable`/`FlexRenderable` boundary as the Codex equivalent of `ChatWidget::as_renderable()`. The chat render tree is:

1. flexible transcript;
2. active status/details row;
3. pending-input preview, when present;
4. composer;
5. active `/` or `@` completion content.

The same tree supplies `desired_height`, paints the frame, and locates the cursor. The composer applies `user_message_style()` only to its own allocated rectangle. The outer app must not independently reserve or clear the same rows, and popup rows must be part of the bottom-pane height exactly once.

### History and tool calls

Keep AutoReport's `ToolEntry` as the data source, but align the rendering boundary with Codex's history cells. Each tool call owns its header, wrapped command continuation gutter, output block, and terminal status. Adjacent cells are separated by the cell/render-tree spacing rule rather than ad-hoc outer blank lines. Command/status/output spans retain Codex's dim, accent, and error distinctions; no renderer should flatten all tool output into one unstyled paragraph.

### Compatibility boundary

The expected changes are limited to the existing local equivalents of Codex's `app_view`, chat rendering, composer, style, and history-cell modules. AutoReport-specific slash commands, mentions, agent labels, and provider/runtime messages remain local. When an upstream type cannot be imported directly, add a narrow conversion/helper instead of reimplementing upstream layout behavior.

## Verification

- Unit-render the composer at empty, multiline, and narrow widths and assert that the background style is confined to the composer rectangle.
- Assert that the render tree's desired height equals the rows it paints and that cursor coordinates remain inside the composer.
- Render consecutive tool calls and wrapped commands with `TestBackend`; assert the blank separator, continuation gutter, and distinct styles.
- Run `cargo test -p autoreport-tui` and `cargo check --workspace`.
- Inspect a 120x30 runtime screenshot and compare the composer/transcript boundary and tool-call presentation with the supplied Codex references.
