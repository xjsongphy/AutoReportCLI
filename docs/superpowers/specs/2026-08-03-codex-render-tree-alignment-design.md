# Codex render-tree and viewport alignment

## Goal

Make AutoReport's conversation history and composer follow the parent Codex
TUI at the code-ownership level, not only by matching a screenshot. Reuse the
parent render-tree and terminal-history semantics while keeping AutoReport's
runtime, event bus, agent model, and provider-specific screens intact.

## Current structural gaps

- AutoReport's `Tui` directly assembles transcript, status, pending input,
  composer, completion menus, agent selection, and approval surfaces.
- Completion state is outside `ChatComposer`, and `/agent` plus approval views
  are painted after the chat render, so they can cover transcript rows.
- `prepare_chat_viewport` bottom-aligns the viewport on every frame instead of
  preserving it and changing it only for insertion, resize, or a clear.
- The local history insertion helper does not account for wrapped rows or
  update the viewport after insertion.

## Design

### Bottom-pane ownership

Introduce a local `BottomPane` renderable boundary matching Codex's
`BottomPane::as_renderable_with_composer_right_reserve` contract. It owns the
ordered children for status/details, pending-input previews, and the composer.
Completion menus, `/agent`, approval, and user-input interactions are active
bottom-pane views: an active view replaces the ordinary bottom pane and gets
its own desired height, render area, cursor behavior, and key routing. The
main app no longer paints these views over the completed chat frame.

Keep the existing AutoReport event handlers initially, exposing narrow methods
from the boundary rather than changing provider/runtime events. Move popup
state toward `ChatComposer` so popup lifecycle and input routing have the same
owner as the textarea; the existing slash/mention catalogs remain adapters.

### Chat render tree

`chatwidget::rendering` will contain only the flexible active transcript and
one bottom-pane child, with the same one-row top inset as Codex. The bottom
pane's desired height, rendering, and cursor position must come from the same
renderable. No independent popup or picker height reservation is permitted in
`app_view`.

### History and viewport lifecycle

Adapt the local terminal/history helpers to Codex's lifecycle:

1. A normal draw preserves the current viewport origin.
2. History insertion pre-wraps lines, counts the actual rows, scrolls only the
   history region when necessary, updates the viewport area, and notifies the
   terminal of inserted rows.
3. Resize/reflow is the only normal draw-time geometry adjustment.
4. Clearing for `/new` clears pending history and terminal scrollback, resets
   the viewport origin to the top, and resets transcript insertion state before
   the next frame.

The local `Cell` data model remains in this change; its renderer is kept as the
AutoReport adapter until a separate migration can make every cell implement
Codex's dynamic `HistoryCell` trait without changing runtime behavior.

## Non-goals

- Replacing AutoReport's event bus or provider/model configuration flow.
- Importing Codex's full dependency graph or copying unrelated app-server
  behavior.
- Reworking the existing tool execution data model.

## Verification

- Render tests prove the composer background is limited to its allocated rows.
- Render tests prove an active menu, `/agent` picker, or approval view does not
  paint over the transcript area.
- History insertion tests cover wrapped rows and a viewport whose top is zero.
- `/new` tests prove scrollback and viewport reset state before the next draw.
- Run `cargo test -p autoreport-tui --lib`, `cargo check -p autoreport-tui`,
  and `git diff --check`.
