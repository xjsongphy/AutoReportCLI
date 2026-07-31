//! Stdio transport for AutoReport's provider-backed app-server runtime.
//!
//! This module deliberately owns only transport lifecycle and JSON-RPC
//! response forwarding. Request semantics remain in `ProviderRuntimeServer`.

use crate::provider_runtime_server::ProviderRuntimeServer;
use anyhow::Context;
use autoreport_app_server_protocol::JSONRPCMessage;
use autoreport_app_server_transport::{
    OutgoingError, OutgoingMessage, OutgoingResponse, QueuedOutgoingMessage, TransportEvent,
    start_stdio_connection,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::debug;

const TRANSPORT_EVENT_CAPACITY: usize = 64;

/// Serve the provider runtime over the process's stdin and stdout.
///
/// The only supported inbound operation is a JSON-RPC request handled by
/// [`ProviderRuntimeServer`]. Notifications and peer responses are ignored,
/// because this server never issues JSON-RPC requests of its own. The function
/// returns after stdin reaches EOF or the transport closes.
pub async fn serve_stdio(server: Arc<ProviderRuntimeServer>) -> anyhow::Result<()> {
    let (transport_event_tx, mut transport_events) =
        mpsc::channel::<TransportEvent>(TRANSPORT_EVENT_CAPACITY);
    let (initialize_client_name_tx, _initialize_client_name_rx) = oneshot::channel();
    let mut stdio_handles: Vec<JoinHandle<()>> = Vec::with_capacity(2);

    start_stdio_connection(
        transport_event_tx.clone(),
        &mut stdio_handles,
        initialize_client_name_tx,
    )
    .await
    .context("failed to start stdio transport")?;

    let mut writers = HashMap::new();
    while let Some(event) = transport_events.recv().await {
        match event {
            TransportEvent::ConnectionOpened {
                connection_id,
                writer,
                ..
            } => {
                writers.insert(connection_id, writer);
            }
            TransportEvent::ConnectionClosed { connection_id } => {
                writers.remove(&connection_id);
                // This runner starts exactly one stdio connection. Its closure
                // therefore ends the serving lifetime.
                break;
            }
            TransportEvent::IncomingMessage {
                connection_id,
                message: JSONRPCMessage::Request(request),
            } => {
                let Some(writer) = writers.get(&connection_id) else {
                    debug!(%connection_id, "dropping request from closed connection");
                    continue;
                };
                let Some(response) = server.handle_request(request).await else {
                    debug!(%connection_id, "dropping request outside provider runtime surface");
                    continue;
                };
                let Some(outgoing) = outgoing_message_from_jsonrpc(response) else {
                    // `handle_request` is constrained to response/error, but do
                    // not let a future implementation turn this adapter into a
                    // source of unsolicited protocol traffic.
                    debug!(%connection_id, "provider runtime returned a non-response message");
                    continue;
                };
                if writer
                    .send(QueuedOutgoingMessage::new(outgoing))
                    .await
                    .is_err()
                {
                    debug!(%connection_id, "stdio writer is unavailable");
                }
            }
            TransportEvent::IncomingMessage { .. } => {
                // The provider runtime is server-only: it neither consumes
                // notifications nor awaits responses from a client.
            }
        }
    }

    // Drop every writer so the stdout task can drain and exit. The stdin task
    // has already sent ConnectionClosed on EOF; awaiting both tasks prevents
    // detached stdio work from surviving the server lifetime.
    writers.clear();
    drop(transport_event_tx);
    for handle in stdio_handles {
        let _ = handle.await;
    }
    Ok(())
}

fn outgoing_message_from_jsonrpc(message: JSONRPCMessage) -> Option<OutgoingMessage> {
    match message {
        JSONRPCMessage::Response(response) => Some(OutgoingMessage::Response(OutgoingResponse {
            id: response.id,
            result: response.result,
        })),
        JSONRPCMessage::Error(error) => Some(OutgoingMessage::Error(OutgoingError {
            id: error.id,
            error: error.error,
        })),
        JSONRPCMessage::Request(_) | JSONRPCMessage::Notification(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::outgoing_message_from_jsonrpc;
    use autoreport_app_server_protocol::{
        JSONRPCError, JSONRPCErrorError, JSONRPCMessage, JSONRPCResponse, RequestId,
    };
    use autoreport_app_server_transport::{OutgoingMessage, OutgoingResponse};
    use serde_json::json;

    #[test]
    fn converts_jsonrpc_response_for_the_transport() {
        let outgoing = outgoing_message_from_jsonrpc(JSONRPCMessage::Response(JSONRPCResponse {
            id: RequestId::Integer(7),
            result: json!({ "thread": "thread-1" }),
        }));

        let Some(OutgoingMessage::Response(response)) = outgoing else {
            panic!("expected outgoing JSON-RPC response");
        };
        assert_eq!(
            response,
            OutgoingResponse {
                id: RequestId::Integer(7),
                result: json!({ "thread": "thread-1" }),
            }
        );
    }

    #[test]
    fn converts_jsonrpc_error_for_the_transport() {
        let outgoing = outgoing_message_from_jsonrpc(JSONRPCMessage::Error(JSONRPCError {
            id: RequestId::String("request-1".to_owned()),
            error: JSONRPCErrorError {
                code: -32602,
                message: "invalid params".to_owned(),
                data: Some(json!({ "field": "input" })),
            },
        }));

        let Some(OutgoingMessage::Error(error)) = outgoing else {
            panic!("expected outgoing JSON-RPC error");
        };
        assert_eq!(error.id, RequestId::String("request-1".to_owned()));
        assert_eq!(error.error.code, -32602);
        assert_eq!(error.error.message, "invalid params");
        assert_eq!(error.error.data, Some(json!({ "field": "input" })));
    }
}
