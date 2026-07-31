//! AutoReport's provider-backed app-server boundary.
//!
//! The protocol and stdio framing are reused as transport infrastructure;
//! request semantics are implemented by AutoReport's own provider/runtime
//! adapter. Only the provider-runtime methods declared below are exposed.

pub mod provider_dispatch;
pub mod provider_runtime_server;
pub mod provider_transport_runner;
pub mod runtime_adapter;
