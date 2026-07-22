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
use crate::types::ApprovalRequestPayload;
use crate::types::BusMessage;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, oneshot};

const CHANNEL_CAPACITY: usize = 2048;

/// One pending approval: the reply oneshot plus the displayable payload. Kept
/// in insertion order in a `Vec` (approvals are rare) so the TUI can reconcile
/// its display queue in a stable order after a broadcast lag.
struct PendingApproval {
    call_id: String,
    tx: Option<oneshot::Sender<ReviewDecision>>,
    payload: ApprovalRequestPayload,
}

#[derive(Clone)]
pub struct Bus {
    tx: Arc<broadcast::Sender<BusMessage>>,
    /// Pending approval requests awaiting a TUI decision, in insertion order.
    /// This is the non-lossy source of truth: the broadcast
    /// [`BusMessage::ApprovalRequest`] is only an instant-delivery fast path, so
    /// a receiver that lags (or subscribed late) reconciles from this list.
    approvals: Arc<Mutex<Vec<PendingApproval>>>,
    /// Pending Codex-compatible user-input requests.
    user_inputs: Arc<Mutex<HashMap<String, oneshot::Sender<RequestUserInputResponse>>>>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx: Arc::new(tx),
            approvals: Arc::new(Mutex::new(Vec::new())),
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
    /// with the same payload so the TUI surfaces it immediately. The payload is
    /// retained here as the non-lossy source of truth for reconcile-after-lag.
    pub async fn register_approval(
        &self,
        payload: ApprovalRequestPayload,
    ) -> oneshot::Receiver<ReviewDecision> {
        let (tx, rx) = oneshot::channel();
        let mut pending = self.approvals.lock().await;
        // A duplicate `call_id` (retry, race, restarted agent) replaces the
        // stale entry; dropping its sender closes that oneshot, so the prior
        // awaiter observes a cancellation (`recv()` → `None`) instead of
        // blocking forever. Mirrors codex `session::insert_pending_approval` +
        // the caller's collision `warn!`.
        if let Some(pos) = pending.iter().position(|e| e.call_id == payload.call_id) {
            log::warn!("overwriting existing pending approval for call_id: {}", payload.call_id);
            pending[pos] = PendingApproval {
                call_id: payload.call_id.clone(),
                tx: Some(tx),
                payload,
            };
        } else {
            pending.push(PendingApproval {
                call_id: payload.call_id.clone(),
                tx: Some(tx),
                payload,
            });
        }
        rx
    }

    /// Snapshot of every still-pending approval payload, in insertion order.
    /// The TUI calls this at startup and after any broadcast `Lagged(_)` to
    /// rebuild its display queue, so no request can be lost to a lagging
    /// receiver (which would otherwise deadlock the awaiting agent).
    pub async fn pending_approvals(&self) -> Vec<ApprovalRequestPayload> {
        self.approvals
            .lock()
            .await
            .iter()
            .map(|e| e.payload.clone())
            .collect()
    }

    /// Deliver a user decision to the agent awaiting `call_id`. Returns
    /// `false` if no pending request exists for that id (e.g. already resolved
    /// or cancelled).
    pub async fn resolve_approval(&self, call_id: &str, decision: ReviewDecision) -> bool {
        let mut pending = self.approvals.lock().await;
        if let Some(pos) = pending.iter().position(|e| e.call_id == call_id) {
            if let Some(entry) = pending.get_mut(pos) {
                if let Some(tx) = entry.tx.take() {
                    let _ = tx.send(decision);
                }
            }
            pending.remove(pos);
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
        let rx = bus.register_approval(payload("call-1", "cat x")).await;
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
            payload: payload("c", "cat Data/x.csv"),
        });
        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, BusMessage::ApprovalRequest { .. }));
    }

    #[tokio::test]
    async fn pending_approvals_snapshots_in_insertion_order() {
        // Reconcile source of truth: a request registered BEFORE the TUI
        // subscribed (broadcast publish would have no receiver) is still
        // recoverable via `pending_approvals`, and ordering is stable.
        let bus = Bus::new();
        bus.register_approval(payload("a", "ls")).await;
        bus.register_approval(payload("b", "rm -rf /tmp/x")).await;
        let snap = bus.pending_approvals().await;
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].call_id, "a");
        assert_eq!(snap[1].call_id, "b");
        // Resolving one removes only it; the other stays in order.
        bus.resolve_approval("a", ReviewDecision::Approved).await;
        let snap = bus.pending_approvals().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].call_id, "b");
    }

    /// Build a minimal approval payload for tests.
    fn payload(call_id: &str, command: &str) -> ApprovalRequestPayload {
        let argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
        ApprovalRequestPayload {
            agent_type: crate::types::AgentType::Theory,
            call_id: call_id.to_string(),
            command: command.to_string(),
            cwd: None,
            summary: crate::policy::summarize_command(&argv),
            reason: None,
        }
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
