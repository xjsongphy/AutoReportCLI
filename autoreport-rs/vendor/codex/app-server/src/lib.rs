//! AutoReport app-server: a backend that speaks codex's app-server-protocol
//! over the vendored transport, so plugins / graphical frontends / IDEs can
//! drive the (fixed-agent) runtime. Ported from codex's `app-server` crate,
//! minimal changes at the codex-core → our-runtime boundary.

pub mod analytics;
pub mod connection_rpc_gate;
pub mod error_code;
pub mod outgoing_message;
pub mod request_serialization;
pub mod server_request_error;
