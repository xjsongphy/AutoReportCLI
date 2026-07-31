//! Provider-runtime request recognition for the AutoReport app-server.
//!
//! This module intentionally knows only the small RPC surface implemented by
//! AutoReport's provider runtime. The JSON-RPC transport owns handling of
//! methods outside this allowlist.

use autoreport_app_server_protocol::InitializeParams;
use autoreport_app_server_protocol::JSONRPCRequest;
use autoreport_app_server_protocol::RequestId;
use autoreport_app_server_protocol::ThreadListParams;
use autoreport_app_server_protocol::ThreadReadParams;
use autoreport_app_server_protocol::ThreadStartParams;
use autoreport_app_server_protocol::TurnInterruptParams;
use autoreport_app_server_protocol::TurnStartParams;
use autoreport_app_server_protocol::TurnSteerParams;
use serde_json::Value;

/// The complete app-server request surface implemented by AutoReport.
pub const PROVIDER_RUNTIME_METHODS: &[&str] = &[
    INITIALIZE_METHOD,
    THREAD_START_METHOD,
    THREAD_READ_METHOD,
    THREAD_LIST_METHOD,
    TURN_START_METHOD,
    TURN_STEER_METHOD,
    TURN_INTERRUPT_METHOD,
];

pub const INITIALIZE_METHOD: &str = "initialize";
pub const THREAD_START_METHOD: &str = "thread/start";
pub const THREAD_READ_METHOD: &str = "thread/read";
pub const THREAD_LIST_METHOD: &str = "thread/list";
pub const TURN_START_METHOD: &str = "turn/start";
pub const TURN_STEER_METHOD: &str = "turn/steer";
pub const TURN_INTERRUPT_METHOD: &str = "turn/interrupt";

/// Exact protocol parameter types accepted by the provider runtime.
pub type ProviderInitializeParams = InitializeParams;
pub type ProviderThreadStartParams = ThreadStartParams;
pub type ProviderThreadReadParams = ThreadReadParams;
pub type ProviderThreadListParams = ThreadListParams;
pub type ProviderTurnStartParams = TurnStartParams;
pub type ProviderTurnSteerParams = TurnSteerParams;
pub type ProviderTurnInterruptParams = TurnInterruptParams;

/// A supported request after its protocol parameters have been decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderRequest {
    Initialize {
        id: RequestId,
        params: ProviderInitializeParams,
    },
    ThreadStart {
        id: RequestId,
        params: ProviderThreadStartParams,
    },
    ThreadRead {
        id: RequestId,
        params: ProviderThreadReadParams,
    },
    ThreadList {
        id: RequestId,
        params: ProviderThreadListParams,
    },
    TurnStart {
        id: RequestId,
        params: ProviderTurnStartParams,
    },
    TurnSteer {
        id: RequestId,
        params: ProviderTurnSteerParams,
    },
    TurnInterrupt {
        id: RequestId,
        params: ProviderTurnInterruptParams,
    },
}

impl ProviderRequest {
    pub fn id(&self) -> &RequestId {
        match self {
            Self::Initialize { id, .. }
            | Self::ThreadStart { id, .. }
            | Self::ThreadRead { id, .. }
            | Self::ThreadList { id, .. }
            | Self::TurnStart { id, .. }
            | Self::TurnSteer { id, .. }
            | Self::TurnInterrupt { id, .. } => id,
        }
    }
}

/// Returns whether `method` belongs to AutoReport's provider-runtime surface.
pub fn is_provider_runtime_method(method: &str) -> bool {
    PROVIDER_RUNTIME_METHODS.contains(&method)
}

/// Decodes a supported JSON-RPC request.
///
/// `Ok(None)` deliberately means that the method is outside AutoReport's
/// provider-runtime surface.  The caller can then leave it to its general
/// JSON-RPC dispatch layer without this module naming or handling it.
pub fn parse_provider_request(
    request: JSONRPCRequest,
) -> serde_json::Result<Option<ProviderRequest>> {
    let id = request.id;
    let params = request.params.unwrap_or_else(empty_object);

    let parsed = match request.method.as_str() {
        INITIALIZE_METHOD => ProviderRequest::Initialize {
            id,
            params: serde_json::from_value(params)?,
        },
        THREAD_START_METHOD => ProviderRequest::ThreadStart {
            id,
            params: serde_json::from_value(params)?,
        },
        THREAD_READ_METHOD => ProviderRequest::ThreadRead {
            id,
            params: serde_json::from_value(params)?,
        },
        THREAD_LIST_METHOD => ProviderRequest::ThreadList {
            id,
            params: serde_json::from_value(params)?,
        },
        TURN_START_METHOD => ProviderRequest::TurnStart {
            id,
            params: serde_json::from_value(params)?,
        },
        TURN_STEER_METHOD => ProviderRequest::TurnSteer {
            id,
            params: serde_json::from_value(params)?,
        },
        TURN_INTERRUPT_METHOD => ProviderRequest::TurnInterrupt {
            id,
            params: serde_json::from_value(params)?,
        },
        _ => return Ok(None),
    };

    Ok(Some(parsed))
}

/// Typed decoding helpers for callers that already selected a supported method.
pub fn parse_initialize_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderInitializeParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_thread_start_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderThreadStartParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_thread_read_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderThreadReadParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_thread_list_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderThreadListParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_turn_start_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderTurnStartParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_turn_steer_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderTurnSteerParams> {
    serde_json::from_value(params_or_empty(request))
}

pub fn parse_turn_interrupt_params(
    request: &JSONRPCRequest,
) -> serde_json::Result<ProviderTurnInterruptParams> {
    serde_json::from_value(params_or_empty(request))
}

fn params_or_empty(request: &JSONRPCRequest) -> Value {
    request.params.clone().unwrap_or_else(empty_object)
}

fn empty_object() -> Value {
    Value::Object(Default::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoreport_app_server_protocol::ClientInfo;
    use serde_json::json;

    fn request(method: &str, params: Option<Value>) -> JSONRPCRequest {
        JSONRPCRequest {
            id: RequestId::Integer(7),
            method: method.to_owned(),
            params,
            trace: None,
        }
    }

    #[test]
    fn allowlist_contains_only_provider_runtime_methods() {
        assert_eq!(
            PROVIDER_RUNTIME_METHODS,
            [
                "initialize",
                "thread/start",
                "thread/read",
                "thread/list",
                "turn/start",
                "turn/steer",
                "turn/interrupt",
            ]
        );
        assert!(is_provider_runtime_method("turn/start"));
        assert!(!is_provider_runtime_method("unsupported/method"));
    }

    #[test]
    fn parses_supported_initialize_request() {
        let parsed = parse_provider_request(request(
            INITIALIZE_METHOD,
            Some(json!({
                "clientInfo": { "name": "test-client", "version": "1.0" }
            })),
        ))
        .expect("initialize params should parse")
        .expect("initialize is supported");

        assert_eq!(parsed.id(), &RequestId::Integer(7));
        assert!(matches!(
            parsed,
            ProviderRequest::Initialize {
                params: InitializeParams {
                    client_info: ClientInfo { name, version, .. },
                    ..
                },
                ..
            } if name == "test-client" && version == "1.0"
        ));
    }

    #[test]
    fn parses_empty_params_for_thread_list() {
        let parsed = parse_provider_request(request(THREAD_LIST_METHOD, None))
            .expect("empty thread/list params should parse")
            .expect("thread/list is supported");
        assert!(matches!(parsed, ProviderRequest::ThreadList { .. }));
    }

    #[test]
    fn leaves_unknown_methods_to_the_caller() {
        let parsed = parse_provider_request(request("unsupported/method", Some(json!({}))))
            .expect("unknown methods are not parsed here");
        assert!(parsed.is_none());
    }
}
