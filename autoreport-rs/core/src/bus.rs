//! Message bus — a broadcast channel carrying [`BusMessage`]s to every
//! subscriber (agent loops and the TUI). Each subscriber filters by
//! `agent_type`. This is the async pub/sub spine that replaces the Python
//! `MessageBus`/`MessageBus` typed dispatch.
//!
//! Also hosts the approval request/reply broker: agents publish an
//! [`BusMessage::ApprovalRequest`] (broadcast, so the TUI sees it from any of
//! the 5 agents regardless of which is focused) and park on a oneshot. The TUI
//! resolves a decision by `call_id` via [`Bus::resolve_approval`]. This broker
//! is the one piece of glue our architecture adds over codex's app-server
//! request/response transport — the overlay/queue/label/decision UI itself is
//! ported from codex's `approval_overlay.rs`.

use crate::policy::ReviewDecision;
use crate::request_user_input::RequestUserInputResponse;
use crate::types::BusMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, oneshot};

const CHANNEL_CAPACITY: usize = 2048;

#[derive(Clone)]
pub struct Bus {
    tx: Arc<broadcast::Sender<BusMessage>>,
    /// Pending approval requests awaiting a TUI decision, keyed by `call_id`.
    /// This is the reply half of the approval flow (codex's analog is its
    /// app-server `RequestId` resolution).
    approvals: Arc<Mutex<HashMap<String, oneshot::Sender<ReviewDecision>>>>,
    /// Pending Codex-compatible user-input requests.
    user_inputs: Arc<Mutex<HashMap<String, oneshot::Sender<RequestUserInputResponse>>>>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx: Arc::new(tx),
            approvals: Arc::new(Mutex::new(HashMap::new())),
            user_inputs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Subscribe a new receiver. Each call is independent.
    pub fn subscribe(&self) -> broadcast::Receiver<BusMessage> {
        self.tx.subscribe()
    }

    /// Broadcast a message to all subscribers.
    pub fn publish(&self, msg: BusMessage) {
        // send errors only when there are no receivers, which is harmless.
        let _ = self.tx.send(msg);
    }

    /// Register a pending approval and return the receiver the requesting
    /// agent awaits. The caller must then `publish(BusMessage::ApprovalRequest)`
    /// with the same `call_id` so the TUI can surface it.
    pub async fn register_approval(&self, call_id: &str) -> oneshot::Receiver<ReviewDecision> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.approvals.lock().await;
        // `HashMap::insert` overwrites a duplicate `call_id` (retry, race,
        // restarted agent) and returns the stale sender, which we drop here —
        // dropping it closes that oneshot, so the prior awaiter observes a
        // cancellation (`recv()` → `None`) instead of blocking forever. This
        // mirrors codex `session::insert_pending_approval` + the caller's
        // collision `warn!` (`codex-rs/core/src/session/mod.rs`).
        if let Some(_stale) = pending.insert(call_id.to_string(), tx) {
            log::warn!("overwriting existing pending approval for call_id: {call_id}");
        }
        rx
    }

    /// Deliver a user decision to the agent awaiting `call_id`. Returns
    /// `false` if no pending request exists for that id (e.g. already resolved
    /// or cancelled).
    pub async fn resolve_approval(&self, call_id: &str, decision: ReviewDecision) -> bool {
        let mut pending = self.approvals.lock().await;
        if let Some(tx) = pending.remove(call_id) {
            let _ = tx.send(decision);
            true
        } else {
            false
        }
    }

    pub async fn register_user_input(
        &self,
        call_id: &str,
    ) -> oneshot::Receiver<RequestUserInputResponse> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.user_inputs.lock().await;
        pending.remove(call_id);
        pending.insert(call_id.to_string(), tx);
        rx
    }

    pub async fn resolve_user_input(
        &self,
        call_id: &str,
        response: RequestUserInputResponse,
    ) -> bool {
        let mut pending = self.user_inputs.lock().await;
        if let Some(tx) = pending.remove(call_id) {
            let _ = tx.send(response);
            true
        } else {
            false
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approval_broker_round_trips_a_decision() {
        let bus = Bus::new();
        let rx = bus.register_approval("call-1").await;
        // Unknown id resolves to false and leaves the receiver pending.
        assert!(
            !bus.resolve_approval("unknown", ReviewDecision::Denied)
                .await
        );
        // Resolving the registered id delivers the decision.
        assert!(
            bus.resolve_approval("call-1", ReviewDecision::Approved)
                .await
        );
        assert_eq!(rx.await.unwrap(), ReviewDecision::Approved);
        // A second resolve is a no-op (already consumed).
        assert!(!bus.resolve_approval("call-1", ReviewDecision::Denied).await);
    }

    #[tokio::test]
    async fn approval_request_is_broadcast_to_subscribers() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        bus.publish(BusMessage::ApprovalRequest {
            agent_type: crate::types::AgentType::Theory,
            call_id: "c".into(),
            command: "cat Data/x.csv".into(),
            cwd: None,
            summary: crate::policy::summarize_command(&["cat".into(), "Data/x.csv".into()]),
            reason: None,
        });
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, BusMessage::ApprovalRequest { .. }));
    }

    #[tokio::test]
    async fn user_input_broker_round_trips_response() {
        let bus = Bus::new();
        let rx = bus.register_user_input("question-1").await;
        let mut answers = HashMap::new();
        answers.insert(
            "q".to_string(),
            crate::request_user_input::RequestUserInputAnswer {
                answers: vec!["yes".to_string()],
            },
        );
        assert!(
            bus.resolve_user_input("question-1", RequestUserInputResponse { answers },)
                .await
        );
        assert_eq!(rx.await.unwrap().answers["q"].answers, ["yes"]);
    }
}
