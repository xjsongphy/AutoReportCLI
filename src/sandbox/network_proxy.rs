//! Network-proxy surface used by the sandbox backends.
//!
//! The sandbox seatbelt/bwrap policy generators accept an optional managed
//! network proxy (`NetworkProxy`) and a portable `ManagedNetworkSandboxContext`
//! so codex can inject allow-rules for a local MITM proxy's loopback ports and
//! unix sockets. AutoReportCLI does not run that proxy, so the sandbox is always
//! invoked with `network: None` and `managed_network: None`.
//!
//! To keep the codex `seatbelt.rs`/`manager.rs` sources vendored *verbatim*
//! (their signatures reference these types), this module provides:
//!
//! - `ManagedNetworkSandboxContext`, `PROXY_URL_ENV_KEYS`,
//!   `proxy_url_env_value`, `has_proxy_url_env_vars` — copied verbatim from
//!   `codex-rs/network-proxy/src/proxy.rs`. These are self-contained env
//!   utilities and are exercised by the seatbelt policy generator.
//! - `NetworkProxy` — an opaque boundary type matching codex's public method
//!   interface. Its methods mirror `codex-network-proxy::NetworkProxy` but are
//!   **never reached** in AutoReportCLI (the caller always passes `None`). They
//!   exist only so the verbatim seatbelt signatures type-check. Building the
//!   real managed-proxy engine (rustls MITM, ~14k LOC) is out of scope.

use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;

use crate::sandbox::absolute_path::AbsolutePathBuf;

/// Portable managed-network facts needed by an operating-system sandbox.
///
/// Verbatim from `codex-rs/network-proxy/src/proxy.rs`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedNetworkSandboxContext {
    /// Loopback proxy ports that sandboxed commands may connect to.
    #[serde(default)]
    pub loopback_ports: Vec<u16>,
    /// Whether the command may bind local sockets and exchange loopback traffic.
    #[serde(default)]
    pub allow_local_binding: bool,
}

/// Verbatim from `codex-rs/network-proxy/src/proxy.rs`.
pub const PROXY_URL_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "WS_PROXY",
    "WSS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "YARN_HTTP_PROXY",
    "YARN_HTTPS_PROXY",
    "NPM_CONFIG_HTTP_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
    "NPM_CONFIG_PROXY",
    "BUNDLE_HTTP_PROXY",
    "BUNDLE_HTTPS_PROXY",
    "PIP_PROXY",
    "DOCKER_HTTP_PROXY",
    "DOCKER_HTTPS_PROXY",
];

/// Verbatim from `codex-rs/network-proxy/src/proxy.rs`.
pub fn proxy_url_env_value<'a>(
    env: &'a HashMap<String, String>,
    canonical_key: &str,
) -> Option<&'a str> {
    if let Some(value) = env.get(canonical_key) {
        return Some(value.as_str());
    }
    let lower_key = canonical_key.to_ascii_lowercase();
    env.get(lower_key.as_str()).map(String::as_str)
}

/// Verbatim from `codex-rs/network-proxy/src/proxy.rs`.
pub fn has_proxy_url_env_vars(env: &HashMap<String, String>) -> bool {
    PROXY_URL_ENV_KEYS
        .iter()
        .any(|key| proxy_url_env_value(env, key).is_some_and(|value| !value.trim().is_empty()))
}

/// Opaque boundary type matching the public method interface of codex's
/// `codex-network-proxy::NetworkProxy`.
///
/// The real `NetworkProxy` owns a rustls MITM proxy engine. AutoReportCLI never
/// constructs one — the sandbox backends receive `network: None` — so the
/// methods below are unreachable in practice and return empty/default values.
/// They exist solely so the verbatim `seatbelt.rs`/`manager.rs` signatures
/// (which take `Option<&NetworkProxy>`) compile unchanged.
#[derive(Clone, Debug)]
pub struct NetworkProxy {
    _private: (),
}

impl PartialEq for NetworkProxy {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for NetworkProxy {}

impl NetworkProxy {
    /// Whether all unix-domain sockets are allowed. Unreachable: callers pass `None`.
    pub fn dangerously_allow_all_unix_sockets(&self) -> bool {
        false
    }

    /// Path to the managed-MITM CA trust bundle, if a managed proxy is active.
    /// Unreachable: callers pass `None`. Mirrors codex's
    /// `NetworkProxy::managed_mitm_ca_trust_bundle_path`.
    pub fn managed_mitm_ca_trust_bundle_path(&self) -> Option<AbsolutePathBuf> {
        None
    }

    /// Allowed unix-socket paths. Unreachable: callers pass `None`.
    pub fn allow_unix_sockets(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether local binding is allowed. Unreachable: callers pass `None`.
    pub fn allow_local_binding(&self) -> bool {
        false
    }

    /// Apply proxy env vars for an environment id. Unreachable: callers pass `None`.
    pub fn apply_to_env_for_optional_environment(
        &self,
        _env: &mut HashMap<String, String>,
        _environment_id: Option<&str>,
    ) -> Result<(), NetworkProxyEnvError> {
        Ok(())
    }
}

/// Error type returned by [`NetworkProxy::apply_to_env_for_optional_environment`].
#[derive(Debug, thiserror::Error)]
pub enum NetworkProxyEnvError {
    #[error("{0}")]
    Other(String),
}
