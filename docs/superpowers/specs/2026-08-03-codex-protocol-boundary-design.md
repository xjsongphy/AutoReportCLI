# Codex protocol boundary for AutoReport's five loops

## Status

Approved direction from the user: retain AutoReport's five independent loops and provider implementations, but make the TUI/runtime boundary use Codex's app-server-style event and command model instead of exposing `BusMessage` to the TUI.

## Problem

The TUI currently consumes `autoreport_core::types::BusMessage` directly in `Tui::apply_bus`. User input, interrupts, compaction, approvals, and user-input prompts also call `LoopManager` or `Bus` directly. This makes every new TUI feature require AutoReport-specific reducer and rendering glue, even though the visible surface is being migrated toward Codex's `AppEvent`, `AppCommand`, `ChatWidget`, `BottomPane`, and `HistoryCell` structure.

The repository already contains an AutoReport app-server crate and the Codex app-server protocol crate, but the current implementation only exposes one Main-backed session, returns synthetic turn ids, and does not deliver a live notification stream to the TUI.

## Goals

1. Make the TUI-facing runtime contract Codex-shaped: typed app-server requests, notifications, server requests, ids, and lifecycle events.
2. Represent each AutoReport `AgentType` as an independently addressable thread/session while preserving the existing five loops.
3. Keep provider selection and provider implementations below the runtime boundary. Main/Sub provider choices remain configuration concerns.
4. Reuse Codex TUI event dispatch, history insertion, active-cell lifecycle, approvals, user-input requests, and thread switching with the smallest AutoReport-specific surface possible.
5. Remove direct `BusMessage` consumption and direct `LoopManager` calls from the TUI.

## Non-goals

- Replacing Anthropic, OpenAI Chat, or OpenAI Responses provider implementations.
- Implementing Codex account, cloud, MCP, plugin, image, or realtime features that AutoReport does not support.
- Making AutoReport's external CLI support every Codex app-server method immediately.
- Making the five loops share one conversation. Each loop remains an independent conversation/thread.

## Chosen architecture

Use an in-process app-server boundary. The TUI keeps Codex's conceptual layers, while an AutoReport runtime session host implements the subset of the Codex app-server protocol needed by this product.

```text
Codex-shaped TUI
  AppEvent / AppCommand / ChatWidget / HistoryCell
              │
              │ typed in-process app-server session
              ▼
AutoReport runtime session host
  ThreadId → AgentType → AgentLoop → LLMProvider
```

The app-server protocol is the canonical TUI contract. `BusMessage` may remain temporarily inside the runtime for loop-to-loop coordination and task-board signaling, but it must not cross into `autoreport-rs/tui`. Provider chunks are normalized once inside `AgentLoop`/the session host into canonical turn/item notifications; this is a stable runtime boundary, not a TUI-specific adapter.

## Thread and provider mapping

`RuntimeSessionRegistry` will register one session per active AutoReport loop. A session record contains:

- stable `ThreadId`;
- `AgentType` metadata;
- workspace and current model metadata;
- an `Arc<AgentLoop>` handle;
- the session's event/request channels.

The five fixed agents remain `AgentType::ALL`. The TUI's agent picker selects a `ThreadId`; it does not need to know which provider is underneath. `/new` and clear operations affect the focused thread unless a command explicitly targets all threads.

The existing `LoopManager` keeps `main_provider` and `sub_provider`, and continues constructing each loop with the selected provider. The app-server layer receives model/provider metadata only to report thread state and route a turn; it does not load credentials or implement provider calls.

## Event and command model

Introduce a typed internal session stream matching the Codex event lifecycle:

- thread/session started, resumed, cleared, or failed;
- turn started, steered, interrupted, completed, or failed;
- user message item created;
- assistant message and reasoning item deltas/completion;
- tool call item started, output delta/completion, and failure;
- plan/collaboration items;
- approval and user-input server requests;
- status and runtime metrics updates.

Every event carries the identifiers needed to correlate it:

- `ThreadId` for the owning loop/session;
- `TurnId` for the current user turn;
- `ItemId` for messages, reasoning, and tools;
- provider/tool `call_id` where the provider exposes one;
- app-server `RequestId` for request/response and approval/input replies.

The TUI sends typed commands through one app-event path. The first supported command set is:

- start/clear/resume focused thread;
- start user turn;
- steer active turn;
- interrupt active turn;
- compact focused thread;
- resolve command/file/permissions approval;
- answer user-input request.

The existing direct calls in `app_input.rs`, `app_event.rs`, `app_command.rs`, and `approval_events.rs` are moved behind this command path.

## History ownership

The TUI will stop building the main transcript from `Vec<Cell>` as its primary storage. Runtime notifications are reduced into Codex-style dynamic `HistoryCell` instances and an active mutable cell, with finalized cells inserted into scrollback using the existing Codex insertion lifecycle.

AutoReport's persisted `ResponseItem` history remains the runtime persistence format. On thread replay, the app-server/session layer emits replay events or supplies typed history items; the TUI creates the same `HistoryCell` families it creates for live events. This preserves provider/runtime history without making the render tree understand provider-specific payloads.

## Approval and user-input lifecycle

Approvals and `request_user_input` become app-server server requests:

1. The loop emits a request with `ThreadId`, `TurnId`, `ItemId`, and `RequestId`.
2. The TUI renders it as the active `BottomPane` view.
3. The TUI sends a typed response command.
4. The session host routes the response to the correct loop and request waiter.

The current oneshot broker can be used internally during the transition, but its public boundary must be the typed request/response channel rather than `Bus::resolve_approval` or `Bus::resolve_user_input` called from TUI code.

## Migration phases

### Phase 1: canonical ids and session registry

- Extend the runtime session record to map all five `AgentType` values.
- Unify the generated session/conversation id with the app-server `ThreadId`.
- Add per-turn and per-item ids at loop boundaries.
- Add contract tests for five-thread registration, routing, and provider metadata isolation.

### Phase 2: live typed event stream

- Add a session event channel and app-server notification types for the supported lifecycle.
- Emit events from `AgentLoop` at turn, message, reasoning, tool, plan, report, and status boundaries.
- Keep `BusMessage` for internal coordination only; add tests proving event ordering and correlation.

### Phase 3: TUI event/command ownership

- Port the Codex-style `AppEvent`/`AppCommand` dispatcher boundary.
- Replace TUI `BusMessage` receive/reduction with the typed session event stream.
- Route submit, interrupt, steer, compact, clear, approval, and user-input replies through commands.
- Make agent switching select a thread/session rather than only changing a local enum.

### Phase 4: history and active-cell lifecycle

- Replace the TUI's primary `Vec<Cell>` transcript path with dynamic `HistoryCell` storage and active-cell updates.
- Keep AutoReport-specific history cell constructors only for report/delegation/planning content that has no direct Codex counterpart.
- Remove `TranscriptHistoryCell` as an aggregate adapter once live/replay events produce real cells.

### Phase 5: remove obsolete TUI coupling

- Remove `BusMessage` imports from `autoreport-rs/tui`.
- Remove direct `LoopManager` and `Bus` calls from TUI input/event modules.
- Keep the runtime bus only where it is still needed for loop-to-loop coordination.
- Run full TUI, runtime, app-server, and protocol tests.

## Error and compatibility behavior

- Unknown or unsupported app-server methods return typed protocol errors; they are not silently treated as successful operations.
- A loop/provider failure completes the affected turn with an error event and leaves other threads available.
- Lost or delayed event delivery must not leave approval or user-input requests unresolved. Request state remains owned by the session host until a typed response or cancellation is observed.
- Existing rollout files remain readable. Replay converts persisted `ResponseItem` entries into the new typed history path without rewriting old files.

## Acceptance criteria

1. TUI has no direct `BusMessage` consumer.
2. TUI has no direct `LoopManager::submit`, `interrupt`, `compact`, or approval-resolution call.
3. `/agent` can switch among five independently running threads.
4. A turn can be traced from command to `TurnStarted`, item deltas, tool requests/results, and `TurnCompleted` using ids.
5. Approval and user-input prompts round-trip through typed server requests and responses.
6. Provider implementations remain unchanged except for event emission hooks needed to expose lifecycle data.
7. Existing Codex-style menu, composer, history, scrollback, and tool rendering tests continue to pass, with new protocol contract tests covering the five-loop runtime.

