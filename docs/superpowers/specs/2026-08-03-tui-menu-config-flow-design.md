# Codex-style menu and provider configuration flow

## Scope

Align the AutoReport TUI with the parent Codex implementation for completion
menus, `/new` viewport reset, and the two-page provider/model setup flow.

## Design

### Composer menus

Slash and file completion are rendered as one bottom-pane child, mirroring
Codex's `ChatComposer::layout_areas_with_textarea_right_reserve` and
`ActivePopup`. The vertical order is input surface, then popup. While a popup
is active, it owns the footer slot, so the normal status/footer line is not
rendered below the input. The popup area is reserved in the bottom-pane
desired height and is cleared before drawing, preventing transcript rows from
showing through or being overwritten.

### New-session reset

`/new` clears the in-memory transcript, pending history/scrollback state,
queued inputs, and popup/overlay state. Before the next draw it clears the
terminal scrollback and resets the inline viewport to the top, matching Codex's
`clear_terminal_ui` and `reset_transcript_state_after_clear` behavior.

### Provider/model setup

The provider page keeps a small action menu only:

1. Add from preset
2. Add custom provider
3. Use configured provider
4. Continue to model assignment

The preset page is entered from the first action and renders only cached
preset templates, grouped by provider kind with indented, fixed-width columns.
The configured-provider page is entered from the third action and renders
only providers already present in `settings.providers`, by display name; it
never displays the preset catalog.

Selecting either a preset or an existing provider binds the selected provider
to both Main and Sub agents. The model page therefore contains no provider
picker: it only edits the Main and shared Sub-agent model names. Custom
provider editing remains available through the second action, then returns to
the same shared-provider model assignment flow.

## Verification

Add unit coverage for popup geometry and footer ownership, viewport reset
state, grouped/indented preset rows, configured-provider filtering, and the
invariant that Main/Sub share the selected provider.
