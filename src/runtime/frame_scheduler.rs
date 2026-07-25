//! Frame scheduling for runtime work.
//!
//! This module combines the frame timer with [`PendingWork`], so it owns both
//! *when* the runtime may process a frame and *whether* there is any frame work
//! to do. The runtime still owns what a frame means: draining pending flags,
//! rendering, and re-evaluating subscriptions.

use std::future::pending;
use std::time::Duration;

use tokio::time::{Instant, sleep_until};

use crate::time_util::next_anchor_phase_deadline;

use super::frame_rate::FrameRate;
use super::pending_work::PendingWork;

/// Schedules runtime frame work at the configured frame rate.
///
/// The scheduler parks while there is no pending work, so the runtime's event
/// loop remains event-driven while idle instead of waking at the frame rate to
/// do nothing.
pub(super) struct FrameScheduler {
    period: Duration,
    /// The frame cadence, sampled on the first
    /// [`next_work_frame`](Self::next_work_frame) await — the polling
    /// runtime's clock — never at construction, which may run outside that
    /// runtime (`Runtime` construction precedes `block_on`).
    schedule: Option<FrameSchedule>,
    pub(super) pending: PendingWork,
}

/// The frame cadence: the phase-defining anchor and the next frame deadline.
#[derive(Clone, Copy)]
struct FrameSchedule {
    anchor: Instant,
    next_deadline: Instant,
}

impl FrameScheduler {
    /// Creates a frame scheduler with the given target frame rate.
    pub(super) fn new(frame_rate: FrameRate) -> Self {
        Self {
            // `frame_duration` divides a one-second `Duration` directly so
            // the period is exact (e.g. 60 FPS -> 16.667ms) instead of
            // truncating to whole milliseconds.
            period: frame_rate.frame_duration(),
            schedule: None,
            pending: PendingWork::new(),
        }
    }

    /// Returns the scheduler's frame period.
    #[cfg(test)]
    pub(super) const fn frame_period(&self) -> Duration {
        self.period
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

        // Frame deadlines are anchor-phase boundaries (`time_util`), not
        // `tokio::time::interval` ticks: `MissedTickBehavior::Skip` engages
        // only once a tick is late past a fixed margin, so at sub-margin
        // frame periods (above roughly 200 FPS) a stall whose missed
        // deadlines sit within that margin replayed them one frame tick per
        // missed period — the defect RFC 0009 §4.2 removed from `Timer`. A
        // deadline already in the past resolves immediately (a re-enabled
        // frame adds no render latency), and completing a frame then
        // advances to the first anchor-phase boundary strictly after now:
        // at most one immediate frame per stall, cadence preserved.
        let FrameSchedule {
            anchor,
            next_deadline,
        } = *self.schedule.get_or_insert_with(|| {
            let anchor = Instant::now();
            FrameSchedule {
                anchor,
                // The first frame is due immediately, as `interval`'s
                // first tick was.
                next_deadline: anchor,
            }
        });
        sleep_until(next_deadline).await;
        self.schedule = Some(FrameSchedule {
            anchor,
            next_deadline: next_anchor_phase_deadline(anchor, self.period, Instant::now()),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::num::NonZeroU32;
    use std::pin::pin;

    use tokio::time::{Duration, Instant, advance, timeout};

    use crate::poll_util::noop_context;

    use super::*;

    fn frame_rate(value: u32) -> FrameRate {
        FrameRate::new(NonZeroU32::new(value).expect("frame rate must be non-zero"))
            .expect("frame rate must be valid")
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

    // The sub-margin non-catch-up check: at 500 FPS (2 ms) a 5 ms stall
    // leaves every missed deadline inside tokio's `MissedTickBehavior::Skip`
    // lateness margin, exactly where the old interval-based scheduler
    // replayed one tick per missed period (RFC 0009 §4.2's forbidden
    // catch-up burst; a longer stall pushes the lateness past the margin,
    // where Skip works and either implementation passes).
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn stalled_frames_do_not_replay_a_catch_up_burst() {
        let mut scheduler = FrameScheduler::new(frame_rate(500));

        // First frame: due immediately; anchors the cadence.
        scheduler.record_redraw(true);
        scheduler.next_work_frame().await;
        let anchor = Instant::now();

        // Stall past the 2 ms and 4 ms deadlines with work still pending.
        scheduler.record_redraw(true);
        advance(Duration::from_millis(5)).await;

        // Exactly one immediate late frame ...
        scheduler.next_work_frame().await;
        assert_eq!(
            Instant::now(),
            anchor + Duration::from_millis(5),
            "one frame fires immediately after a stall"
        );

        // ... and the next frame waits for the 6 ms anchor-phase boundary:
        // a Skip-based scheduler replays the missed 4 ms deadline
        // immediately instead.
        scheduler.record_redraw(true);
        scheduler.next_work_frame().await;
        assert_eq!(
            Instant::now(),
            anchor + Duration::from_millis(6),
            "missed frame deadlines must not replay as a catch-up burst"
        );
    }

    // Cancel safety: the runtime's select! loop drops this future whenever
    // another branch wins; a pending poll must not move the deadline. An
    // implementation deriving the deadline from the await's own start time
    // ("now + period" per call) drifts here and fails.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn dropping_a_pending_frame_future_preserves_the_deadline() {
        let mut scheduler = FrameScheduler::new(frame_rate(250));

        scheduler.record_redraw(true);
        scheduler.next_work_frame().await;
        let anchor = Instant::now();

        // 1 ms into the 4 ms frame period, poll the frame future exactly
        // once (registering its sleep) and drop it.
        scheduler.record_redraw(true);
        advance(Duration::from_millis(1)).await;
        {
            let fut = pin!(scheduler.next_work_frame());
            assert!(
                fut.poll(&mut noop_context()).is_pending(),
                "mid-period frame future starts pending"
            );
        }

        // Re-awaiting completes at the original 4 ms deadline, not 1 ms +
        // one period.
        scheduler.next_work_frame().await;
        assert_eq!(
            Instant::now(),
            anchor + Duration::from_millis(4),
            "the deadline survives dropping a pending frame future"
        );
    }

    // Post-stall cadence resumes on the anchor's phase: after the late frame
    // the next boundary can be less than one period away and still fires.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn post_stall_cadence_resumes_on_the_anchor_phase() {
        let mut scheduler = FrameScheduler::new(frame_rate(250));

        scheduler.record_redraw(true);
        scheduler.next_work_frame().await;
        let anchor = Instant::now();

        // Stall to 18 ms past the anchor (between the 16 ms and 20 ms
        // boundaries), then take the one late frame.
        scheduler.record_redraw(true);
        advance(Duration::from_millis(18)).await;
        scheduler.next_work_frame().await;

        // The next frame is due at the 20 ms anchor-phase boundary — 2 ms
        // later — not a full 4 ms period after the late frame.
        scheduler.record_redraw(true);
        scheduler.next_work_frame().await;
        assert_eq!(
            Instant::now(),
            anchor + Duration::from_millis(20),
            "cadence resumes on the anchor's phase, not reset to now + period"
        );
    }
}
