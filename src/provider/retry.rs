//! Request-level retry with jittered exponential backoff.
//!
//! Ported from codex `codex-client/src/retry.rs` (`RetryPolicy`, `backoff`,
//! `run_with_retry`). We retry the *request establishment* — connection /
//! timeout failures and HTTP 429 / 5xx responses — because that is where the
//! overwhelming majority of transient errors occur. Mid-stream retry (re-issuing
//! a request after SSE bytes have started flowing) is intentionally NOT done
//! here; it requires partially-consumed-response bookkeeping and is far rarer.
//!
//! Defaults match codex: `request_max_retries = 4`, base delay 1 s.

use anyhow::{Result, anyhow};
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

/// Default maximum attempts (1 initial try + 3 retries), matching codex's
/// `DEFAULT_REQUEST_MAX_RETRIES`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 4;
/// Base delay for the first retry; doubles each attempt (codex uses a 1 s base).
pub const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);

/// Jittered exponential backoff (codex `backoff`). `attempt` is 0-indexed: the
/// first retry (attempt 0) sleeps `base`, then 2×base, 4×base, …, each with a
/// ±10 % jitter so concurrent retries don't synchronize. The jitter is derived
/// from a nanosecond clock rather than a PRNG to avoid pulling in `rand` for a
/// single call site.
pub fn backoff(base: Duration, attempt: u32) -> Duration {
    let exp = 2u64.saturating_pow(attempt);
    let raw = (base.as_millis() as u64).saturating_mul(exp);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter = 0.9 + (nanos % 200) as f64 / 1000.0; // 0.9..=1.0999
    Duration::from_millis((raw as f64 * jitter) as u64)
}

/// Whether a `reqwest` send error is worth retrying (timeout or connection
/// failure), mirroring codex `RetryOn::retry_transport`.
fn is_retryable_send_err(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

/// Whether an HTTP status is worth retrying (codex `retry_429 || retry_5xx`).
fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.is_server_error()
}

/// POST with retry. `build` is called to construct a fresh `RequestBuilder` for
/// each attempt (the body is not reusable after `send`). On a non-success
/// status the response body is consumed for the error message. Returns the
/// successful response or the last error.
pub async fn post_with_retry<F, Fut>(
    build: F,
    id: &str,
    max_attempts: u32,
    base_delay: Duration,
) -> Result<reqwest::Response>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut attempt: u32 = 0;
    loop {
        let resp = match build().await {
            Ok(r) => r,
            Err(e) if attempt + 1 < max_attempts && is_retryable_send_err(&e) => {
                log::warn!(
                    "{id} request failed ({e}); retry {}/{}",
                    attempt + 1,
                    max_attempts - 1
                );
                sleep(backoff(base_delay, attempt)).await;
                attempt += 1;
                continue;
            }
            Err(e) => return Err(anyhow::Error::from(e).context(format!("{id} request failed"))),
        };
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // Non-success: read the body once for the error message, then decide.
        let text = resp.text().await.unwrap_or_default();
        if attempt + 1 < max_attempts && is_retryable_status(status) {
            log::warn!(
                "{id} {status} (retry {}/{}): {}",
                attempt + 1,
                max_attempts - 1,
                text.chars().take(200).collect::<String>()
            );
            sleep(backoff(base_delay, attempt)).await;
            attempt += 1;
            continue;
        }
        return Err(anyhow!("{} error {status}: {text}", id));
    }
}
