//! Status-line item vocabulary copied from Codex's `status_line_setup.rs`.

// Vendored vocabulary from codex's status_line_setup.rs. Only `ModelName` and
// `CurrentDir` are constructed in our TUI today; the remaining variants are
// config-driven items consumed by codex's chatwidget/status_surfaces.rs
// (status_line_value_for_item), which we have not ported. Keep the full set so
// user configs that work upstream don't silently break.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StatusLineItem {
    ModelName,
    ModelWithReasoning,
    Reasoning,
    CurrentDir,
    ProjectRoot,
    GitBranch,
    PullRequestNumber,
    BranchChanges,
    Status,
    Permissions,
    ApprovalMode,
    ContextRemaining,
    ContextUsed,
    FiveHourLimit,
    WeeklyLimit,
    CodexVersion,
    ContextWindowSize,
    UsedTokens,
    TotalInputTokens,
    TotalOutputTokens,
    SessionId,
    FastMode,
    RawOutput,
    ThreadTitle,
    WorkspaceHeadline,
    TaskProgress,
}
