//! Frame scheduling for runtime work.
//!
//! This module combines the frame timer with [`PendingWork`], so it owns both
//! *when* the runtime may process a frame and *whether* there is any frame work
//! to do. The runtime still owns what a frame means: draining pending flags,
//! rendering, and re-evaluating subscriptions.

use std::future::pending;

use tokio::time::{Interval, MissedTickBehavior, interval};

use super::frame_rate::FrameRate;
use super::pending_work::PendingWork;

/// Schedules runtime frame work at the configured frame rate.
///
/// The scheduler parks while there is no pending work, so the runtime's event
/// loop remains event-driven while idle instead of waking at the frame rate to
/// do nothing.
pub(super) struct FrameScheduler {
    interval: Interval,
    pub(super) pending: PendingWork,
}

impl FrameScheduler {
    /// Creates a frame scheduler with the given target frame rate.
    pub(super) fn new(frame_rate: FrameRate) -> Self {
        // Divide a one-second `Duration` directly so the period is exact (e.g.
        // 60 FPS -> 16.667ms) instead of truncating to whole milliseconds.
        let frame_duration = frame_rate.frame_duration();
        let mut interval = interval(frame_duration);
        // Skip missed frames rather than trying to catch up.
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        Self {
            interval,
            pending: PendingWork::new(),
        }
    }

    /// Returns the scheduler's frame period.
    #[cfg(test)]
    pub(super) fn frame_period(&self) -> std::time::Duration {
        self.interval.period()
    }

    /// Records whether a processed command requested a redraw.
    pub(super) const fn record_redraw(&mut self, requested: bool) {
        self.pending.record_redraw(requested);
    }

    /// Marks that subscriptions may have changed and should be re-evaluated.
    pub(super) const fn mark_subscriptions_dirty(&mut self) {
        self.pending.mark_subscriptions_dirty();
    }

    /// Whether a redraw or subscription re-evaluation is pending.
    pub(super) const fn has_pending_work(&self) -> bool {
        self.pending.has_pending_work()
    }

    /// Returns whether a redraw is pending and clears the flag.
    pub(super) const fn take_redraw(&mut self) -> bool {
        self.pending.take_redraw()
    }

    /// Returns whether subscriptions need re-evaluation and clears the flag.
    pub(super) const fn take_subscriptions_dirty(&mut self) -> bool {
        self.pending.take_subscriptions_dirty()
    }

    /// Resolves at the next frame boundary, but only when work is pending.
    ///
    /// If no work is pending, this future parks forever. In the runtime's
    /// `select!` loop that is intentional: another branch processes a message,
    /// marks work pending, the loop iterates, and this future is recreated so it
    /// re-checks the pending flags before awaiting the interval.
    ///
    /// This relies on the runtime's current single-task ownership invariant:
    /// only the runtime task mutates [`PendingWork`] while handling another
    /// `select!` branch. A future parked here does not register a wake for later
    /// pending-flag changes. If pending work is ever allowed to be marked from a
    /// different task, this implementation would risk losing that wake and must
    /// be replaced with an explicitly notified design.
    pub(super) async fn next_work_frame(&mut self) {
        if !self.has_pending_work() {
            pending::<()>().await;
        }

        self.interval.tick().await;
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::{Duration, Instant, timeout};

    use super::*;

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::try_new(value).expect("frame rate must be valid")
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn next_work_frame_parks_while_idle() {
        let mut scheduler = FrameScheduler::new(frame_rate(60));
        scheduler.take_redraw();

        let result = timeout(Duration::from_secs(1), scheduler.next_work_frame()).await;

        assert!(
            result.is_err(),
            "an idle scheduler must park instead of waking on the interval"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn next_work_frame_is_ready_immediately_after_idle_work_arrives() {
        let mut scheduler = FrameScheduler::new(frame_rate(60));

        // Consume the initial redraw frame and drain it so the scheduler reaches
        // the idle state under test.
        scheduler.next_work_frame().await;
        scheduler.take_redraw();

        let result = timeout(Duration::from_secs(1), scheduler.next_work_frame()).await;
        assert!(result.is_err(), "scheduler should be parked while idle");

        scheduler.mark_subscriptions_dirty();
        let before = Instant::now();
        timeout(Duration::from_secs(1), scheduler.next_work_frame())
            .await
            .expect("elapsed interval should make the re-armed frame ready");

        assert_eq!(
            Instant::now(),
            before,
            "re-arming after idle should not wait an extra frame period"
        );
    }
}
