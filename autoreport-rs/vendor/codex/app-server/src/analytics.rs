//! Minimal disabled-mode port of codex's `AnalyticsEventsClient`.
//!
//! codex's full analytics client (codex-analytics) sends usage events to a
//! cloud backend and depends on codex-login/codex-plugin/codex-state — codex's
//! account system, which we do not have. codex itself runs the client in
//! "disabled" mode (queue = None) whenever that backend is absent, in which
//! case every `track_*` method is a no-op early-return.
//!
//! We always run disabled (no codex cloud), so this module ports exactly that
//! disabled-mode surface with codex's identical method signatures (verbatim),
//! just with no-op bodies. The enabled path cannot function without codex's
//! account backend, so this is the faithful port for our context.

use autoreport_app_server_protocol::{
    ClientResponsePayload, RequestId, ServerNotification, ServerRequest, ServerResponse,
};
use autoreport_codex_protocol::request_permissions::RequestPermissionsResponse;

/// Disabled-mode analytics client. All `track_*` calls are no-ops, matching
/// codex's `AnalyticsEventsClient::disabled()` behavior. Signatures mirror
/// codex verbatim so call sites are unchanged.
#[derive(Clone)]
pub struct AnalyticsEventsClient;

impl AnalyticsEventsClient {
    pub fn disabled() -> Self {
        Self
    }

    pub fn track_response(
        &self,
        _connection_id: u64,
        _request_id: RequestId,
        _response: ClientResponsePayload,
    ) {
    }

    pub fn track_response_with_thread_originator(
        &self,
        _connection_id: u64,
        _request_id: RequestId,
        _response: ClientResponsePayload,
        _thread_originator: String,
    ) {
    }

    pub fn track_server_request(&self, _connection_id: u64, _request: ServerRequest) {}

    pub fn track_server_response(&self, _completed_at_ms: u64, _response: ServerResponse) {}

    pub fn track_server_request_aborted(&self, _completed_at_ms: u64, _request_id: RequestId) {}

    pub fn track_notification(&self, _notification: ServerNotification) {}

    pub fn track_effective_permissions_approval_response(
        &self,
        _completed_at_ms: u64,
        _request_id: RequestId,
        _response: RequestPermissionsResponse,
    ) {
    }
}
