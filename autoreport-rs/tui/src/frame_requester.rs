//! Direct port of Codex's frame draw scheduler.

use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc};

use crate::frame_rate_limiter::FrameRateLimiter;

#[derive(Clone, Debug)]
pub(crate) struct FrameRequester {
    frame_schedule_tx: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    pub(crate) fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(FrameScheduler::new(rx, draw_tx).run());
        Self {
            frame_schedule_tx: tx,
        }
    }

    pub(crate) fn schedule_frame(&self) {
        let _ = self.frame_schedule_tx.send(Instant::now());
    }

    pub(crate) fn schedule_frame_in(&self, dur: Duration) {
        let _ = self.frame_schedule_tx.send(Instant::now() + dur);
    }
}

struct FrameScheduler {
    receiver: mpsc::UnboundedReceiver<Instant>,
    draw_tx: broadcast::Sender<()>,
    rate_limiter: FrameRateLimiter,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn coalesces_immediate_requests() {
        let (draw_tx, mut draw_rx) = broadcast::channel(4);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame();
        requester.schedule_frame();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), draw_rx.recv())
                .await
                .is_ok()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1), draw_rx.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn delayed_request_waits_until_deadline() {
        let (draw_tx, mut draw_rx) = broadcast::channel(4);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame_in(Duration::from_millis(20));
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(10)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(1), draw_rx.recv())
                .await
                .is_err()
        );
        tokio::time::advance(Duration::from_millis(11)).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), draw_rx.recv())
                .await
                .is_ok()
        );
    }
}

impl FrameScheduler {
    fn new(receiver: mpsc::UnboundedReceiver<Instant>, draw_tx: broadcast::Sender<()>) -> Self {
        Self {
            receiver,
            draw_tx,
            rate_limiter: FrameRateLimiter::default(),
        }
    }

    async fn run(mut self) {
        const ONE_YEAR: Duration = Duration::from_secs(60 * 60 * 24 * 365);
        let mut next_deadline: Option<Instant> = None;
        loop {
            let target = next_deadline.unwrap_or_else(|| Instant::now() + ONE_YEAR);
            let deadline = tokio::time::sleep_until(target.into());
            tokio::pin!(deadline);

            tokio::select! {
                draw_at = self.receiver.recv() => {
                    let Some(draw_at) = draw_at else { break; };
                    let draw_at = self.rate_limiter.clamp_deadline(draw_at);
                    next_deadline = Some(next_deadline.map_or(draw_at, |cur| cur.min(draw_at)));
                }
                _ = &mut deadline => {
                    if next_deadline.is_some() {
                        next_deadline = None;
                        self.rate_limiter.mark_emitted(target);
                        let _ = self.draw_tx.send(());
                    }
                }
            }
        }
    }
}
