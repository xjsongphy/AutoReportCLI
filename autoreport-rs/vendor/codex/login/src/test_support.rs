//! Test-only helpers exposed for cross-crate integration tests.
//!
//! Production code should receive an [`AuthRouteConfig`](crate::AuthRouteConfig) adapted from the
//! application's resolved HTTP client factory instead of depending on this module.

use crate::AuthRouteConfig;
use autoreport_http_client::HttpClientFactory;
use autoreport_http_client::OutboundProxyPolicy;

/// Returns auth routing that preserves the transport's built-in proxy behavior.
pub fn transport_default_auth_route_config() -> AuthRouteConfig {
    AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
        OutboundProxyPolicy::ReqwestDefault,
    ))
}

/// Skip tests that require unrestricted network access when the sandbox marks
/// network access as disabled, matching the upstream Codex test convention.
#[macro_export]
macro_rules! skip_if_no_network {
    () => {{
        if ::std::env::var("CODEX_SANDBOX_NETWORK_DISABLED").is_ok() {
            println!("Skipping test because network access is disabled in the sandbox.");
            return;
        }
    }};
}
