//! Message bus — a broadcast channel carrying [`BusMessage`]s to every
//! subscriber (agent loops and the TUI). Each subscriber filters by
//! `agent_type`. This is the async pub/sub spine that replaces the Python
//! `MessageBus`/`MessageBus` typed dispatch.

use crate::types::BusMessage;
use std::sync::Arc;
use tokio::sync::broadcast;

const CHANNEL_CAPACITY: usize = 2048;

#[derive(Clone)]
pub struct Bus {
    tx: Arc<broadcast::Sender<BusMessage>>,
}

impl Bus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx: Arc::new(tx) }
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
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
