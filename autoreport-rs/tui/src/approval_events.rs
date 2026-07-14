//! Approval-modal decisions for the terminal application.

use crate::app::Tui;
use crate::app_state::SysKind;
use crossterm::event::KeyEvent;

impl Tui {
    /// Resolve the approval request at the front of the shared queue.
    pub(crate) fn handle_approval_key(&mut self, key: KeyEvent) {
        let decision = match key.code {
            crossterm::event::KeyCode::Enter
            | crossterm::event::KeyCode::Char('y')
            | crossterm::event::KeyCode::Char('Y') => {
                autoreport_core::policy::ReviewDecision::Approved
            }
            crossterm::event::KeyCode::Char('a') | crossterm::event::KeyCode::Char('A') => {
                autoreport_core::policy::ReviewDecision::ApprovedForSession
            }
            crossterm::event::KeyCode::Esc
            | crossterm::event::KeyCode::Char('n')
            | crossterm::event::KeyCode::Char('N') => {
                autoreport_core::policy::ReviewDecision::Denied
            }
            _ => return,
        };
        let Some(request) = self.pending_approvals.pop_front() else {
            return;
        };
        let label = match decision {
            autoreport_core::policy::ReviewDecision::Approved => "approved",
            autoreport_core::policy::ReviewDecision::ApprovedForSession => "approved for session",
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
