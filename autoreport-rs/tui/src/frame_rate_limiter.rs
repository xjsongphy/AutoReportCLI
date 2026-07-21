//! Codex's frame-rate limiter used by `FrameRequester`.

use std::time::{Duration, Instant};

pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);

#[derive(Debug, Default)]
pub(super) struct FrameRateLimiter {
    last_emitted_at: Option<Instant>,
}

impl FrameRateLimiter {
    pub(super) fn clamp_deadline(&self, requested: Instant) -> Instant {
        let Some(last_emitted_at) = self.last_emitted_at else {
            return requested;
        };
        let min_allowed = last_emitted_at
            .checked_add(MIN_FRAME_INTERVAL)
            .unwrap_or(last_emitted_at);
        requested.max(min_allowed)
    }

    pub(super) fn mark_emitted(&mut self, emitted_at: Instant) {
        self.last_emitted_at = Some(emitted_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_is_not_clamped() {
        let now = Instant::now();
        assert_eq!(FrameRateLimiter::default().clamp_deadline(now), now);
    }

    #[test]
    fn subsequent_frame_is_limited_to_120_fps() {
        let now = Instant::now();
        let mut limiter = FrameRateLimiter::default();
        limiter.mark_emitted(now);
        assert_eq!(
            limiter.clamp_deadline(now + Duration::from_millis(1)),
            now + MIN_FRAME_INTERVAL
        );
    }
}
