//! HTTP User-Agent helper.
//!
//! Mirrors codex's `get_codex_user_agent` (login/src/auth/default_client.rs):
//! the application identity plus OS metadata plus the detected terminal token,
//! so every outbound provider request (OpenAI-compatible, OpenAI Responses,
//! Anthropic, preset sync) carries one consistent, identifiable UA instead of
//! reqwest's default.

use os_info;

/// Returns the application User-Agent string for outbound HTTP requests.
///
/// Format: `AutoReportCLI/<version> (<os_type> <os_version>; <arch>) <terminal-token>`,
/// e.g. `AutoReportCLI/0.1.2 (Macos 14.6.1; aarch64) Apple_Terminal/2.14`.
pub fn app_user_agent() -> String {
    let build_version = env!("CARGO_PKG_VERSION");
    let os = os_info::get();
    format!(
        "AutoReportCLI/{build_version} ({} {}; {}) {}",
        os.os_type(),
        os.version(),
        os.architecture().unwrap_or("unknown"),
        autoreport_terminal_detection::user_agent(),
    )
}

/// Builds a `reqwest::Client` pre-stamped with [`app_user_agent`].
///
/// Falls back to a default client only if the builder fails to construct (e.g. a
/// TLS-backend init error), matching the defensive pattern already used in
/// `sync.rs`.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(app_user_agent())
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}
