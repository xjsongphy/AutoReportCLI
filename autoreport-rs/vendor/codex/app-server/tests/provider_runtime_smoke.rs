//! Offline protocol smoke tests for the AutoReport provider-runtime server.
//!
//! This file deliberately exercises only the protocol boundary.  It neither
//! starts an app-server nor invokes a provider, so it cannot contact OpenAI or
//! any other network service.

use autoreport_app_server::provider_dispatch::PROVIDER_RUNTIME_METHODS;
use autoreport_app_server_protocol::ClientRequest;
use autoreport_app_server_protocol::JSONRPCRequest;
use serde_json::Value;
use serde_json::json;

fn assert_request_round_trip(raw: Value, expected_method: &str) {
    let request: JSONRPCRequest =
        serde_json::from_value(raw).expect("request must decode as JSON-RPC");
    let typed = ClientRequest::try_from(request).expect("request must decode as a typed method");

    assert_eq!(typed.method_name(), expected_method);
    let serialized = serde_json::to_value(&typed).expect("typed request must serialize");
    assert_eq!(serialized["method"], expected_method);
    let reparsed: ClientRequest =
        serde_json::from_value(serialized).expect("serialized typed request must decode");
    assert_eq!(reparsed, typed);
}

#[test]
fn provider_runtime_request_shapes_round_trip_offline() {
    assert_request_round_trip(
        json!({
            "id": 1,
            "method": "initialize",
            "params": {
                "clientInfo": {
                    "name": "autoreport-smoke",
                    "version": "0.1.0"
                }
            }
        }),
        "initialize",
    );

    assert_request_round_trip(
        json!({
            "id": 2,
            "method": "thread/start",
            "params": {
                "model": "provider-model",
                "modelProvider": "local-provider",
                "cwd": "/workspace"
            }
        }),
        "thread/start",
    );

    assert_request_round_trip(
        json!({
            "id": 3,
            "method": "turn/start",
            "params": {
                "threadId": "thread-provider-smoke",
                "input": [{
                    "type": "text",
                    "text": "Use the configured provider."
                }]
            }
        }),
        "turn/start",
    );

    assert_request_round_trip(
        json!({
            "id": 4,
            "method": "turn/interrupt",
            "params": {
                "threadId": "thread-provider-smoke",
                "turnId": "turn-provider-smoke"
            }
        }),
        "turn/interrupt",
    );
}

#[test]
fn provider_runtime_allowlist_excludes_unimplemented_codex_surfaces() {
    for method in [
        "mcp/list",
        "mcp/server/oauth/login",
        "login",
        "account/login",
        "image/generate",
        "thread/realtime/start",
        "thread/realtime/appendSpeech",
    ] {
        assert!(
            !PROVIDER_RUNTIME_METHODS.contains(&method),
            "{method} must not be advertised or registered by AutoReport"
        );
    }
}
