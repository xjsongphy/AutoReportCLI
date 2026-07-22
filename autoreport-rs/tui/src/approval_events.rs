//! Approval-modal decisions for the terminal application.

use crate::app::Tui;
use crate::app_state::SysKind;
use crossterm::event::KeyEvent;

impl Tui {
    /// Resolve the approval request at the front of the shared queue. Keymap
    /// follows codex's default `ApprovalKeymap` (`codex-rs/tui/src/keymap.rs`):
    /// `y` approve · `a` approve-for-session · `p` approve-and-persist-prefix
    /// · `d` deny · `Esc`/`n` decline · `c` cancel. `Enter` is kept as an extra
    /// approve synonym. Our 4-variant `ReviewDecision` folds codex's
    /// deny/decline/cancel into `Denied`.
    pub(crate) fn handle_approval_key(&mut self, key: KeyEvent) {
        use autoreport_core::policy::ReviewDecision;
        let decision = match key.code {
            // Approval.approve
            crossterm::event::KeyCode::Char('y')
            | crossterm::event::KeyCode::Char('Y')
            | crossterm::event::KeyCode::Enter => ReviewDecision::Approved,
            // Approval.approve_for_session
            crossterm::event::KeyCode::Char('a') | crossterm::event::KeyCode::Char('A') => {
                ReviewDecision::ApprovedForSession
            }
            // Approval.approve_for_prefix (persist a narrow allow rule)
            crossterm::event::KeyCode::Char('p') | crossterm::event::KeyCode::Char('P') => {
                ReviewDecision::ApprovedAndPersisted
            }
            // Approval.deny / Approval.decline (Esc, n) / Approval.cancel (c)
            crossterm::event::KeyCode::Char('d')
            | crossterm::event::KeyCode::Char('D')
            | crossterm::event::KeyCode::Char('c')
            | crossterm::event::KeyCode::Char('C')
            | crossterm::event::KeyCode::Char('n')
            | crossterm::event::KeyCode::Char('N')
            | crossterm::event::KeyCode::Esc => ReviewDecision::Denied,
            _ => return,
        };
        let Some(request) = self.pending_approvals.pop_front() else {
            return;
        };
        let label = match decision {
            autoreport_core::policy::ReviewDecision::Approved => "approved",
            autoreport_core::policy::ReviewDecision::ApprovedForSession => "approved for session",
            autoreport_core::policy::ReviewDecision::ApprovedAndPersisted => {
                "approved and saved as a rule"
            }
            autoreport_core::policy::ReviewDecision::Denied => "denied",
        };
        let bus = self.bus.clone();
        let call_id = request.call_id;
        tokio::spawn(async move {
            let _ = bus.resolve_approval(&call_id, decision).await;
        });
        self.system(&format!("command {label}"), SysKind::Info);
    }
}
