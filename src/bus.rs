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
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            tx: Arc::new(tx),
            approvals: Arc::new(Mutex::new(HashMap::new())),
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
        self.approvals.lock().await.insert(call_id.to_string(), tx);
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
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
