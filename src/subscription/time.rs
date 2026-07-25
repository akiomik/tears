//! Timer subscription for periodic events.
//!
//! This module provides the [`Timer`] subscription source for creating
//! time-based events in your application.

use std::num::NonZeroU64;
use std::time::Duration;

use futures::StreamExt;
use futures::stream::{self, BoxStream};
use tokio::time::{Instant, sleep_until};

use super::SubscriptionSource;

/// Events produced by the [`Timer`] subscription.
#[derive(Debug, Clone)]
pub enum TimerEvent {
    /// A timer tick has occurred.
    Tick,
}

/// A timer subscription that emits tick messages at regular intervals.
///
/// This is useful for creating animations, periodic updates, or any time-based
/// behavior in your application.
///
/// ## Timing semantics
///
/// The semantics of record are RFC 0009 §4.2
/// (`docs/rfcs/0009-clock-di.md`):
///
/// - **Anchor.** The timer's time anchor is fixed at its stream's first
///   poll — read from the clock of the runtime that polls it, so a stream
///   built outside that runtime ([`SubscriptionSource::stream`] on another
///   thread, say) anchors correctly anyway; [`Timer::new`] and `stream()`
///   itself store or build, never anchor. The first tick becomes
///   deliverable one full interval after that anchor — there is no tick at
///   or before the anchor instant.
/// - **No catch-up.** However many interval boundaries elapse while a tick
///   goes untaken (a busy runtime, a long delay), exactly one tick becomes
///   deliverable — never a burst of missed ticks.
/// - **Post-miss cadence.** Taking a tick sets the next deadline to the
///   first anchor-phase boundary strictly after that moment: the cadence is
///   drift-corrected against the anchor, not reset to "now + interval", so
///   after a late tick the next one can arrive less than one interval
///   later.
///
/// Dropping missed ticks instead of replaying them is appropriate for UI
/// applications, where holding a consistent tick rate matters more than
/// processing every scheduled tick; the drift-corrected cadence keeps the
/// timer suitable for high frame rates (e.g. 60 FPS or higher).
///
/// # Example
///
/// ```rust
/// use std::num::NonZeroU64;
/// use tears::Subscription;
/// use tears::subscription::time::Timer;
///
/// enum AppMessage {
///     Tick,
/// }
///
/// // Create a timer that ticks every second (1000ms)
/// let sub = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")))
///     .map(|_| AppMessage::Tick);
///
/// // For 60 FPS animations (approximately 16.67ms per frame)
/// let animation_timer = Subscription::new(Timer::new(NonZeroU64::new(16).expect("non-zero")))
///     .map(|_| AppMessage::Tick);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    interval_ms: NonZeroU64,
}

impl Timer {
    /// Create a new timer with the specified interval.
    ///
    /// # Arguments
    ///
    /// * `interval_ms` - The non-zero interval between ticks in milliseconds
    ///
    /// # Example
    ///
    /// ```
    /// use tears::subscription::time::Timer;
    /// use std::num::NonZeroU64;
    ///
    /// // Tick every second
    /// let timer = Timer::new(NonZeroU64::new(1000).expect("timer interval must be non-zero"));
    ///
    /// // Tick every 16ms (approximately 60 FPS)
    /// let fast_timer = Timer::new(NonZeroU64::new(16).expect("timer interval must be non-zero"));
    /// ```
    #[must_use]
    pub const fn new(interval_ms: NonZeroU64) -> Self {
        Self { interval_ms }
    }
}

impl SubscriptionSource for Timer {
    type Output = TimerEvent;
    type Key = NonZeroU64;

    fn stream(&self) -> BoxStream<'static, TimerEvent> {
        let interval = Duration::from_millis(self.interval_ms.get());
        tracing::trace!(
            target: "tears::subscription::time",
            interval_ms = self.interval_ms.get(),
            "timer stream created"
        );
        // The anchor is sampled on the stream's first poll (RFC 0009 §4.2),
        // never here at construction: `stream()` may legally run outside the
        // runtime that will poll the stream (for example on a plain thread
        // while the poller is a paused test runtime), and an anchor read from
        // that ambient clock can disagree with the clock `sleep_until`
        // measures against — firing a construction-instant tick or never
        // firing at all. The first poll, by definition, happens on the
        // polling runtime's clock. Every deadline is an anchor-phase
        // boundary; `tokio::time::interval` is deliberately not used, since
        // its `MissedTickBehavior::Skip` engages only once a tick is late by
        // more than a fixed margin, so at sub-margin intervals it replays a
        // catch-up burst the RFC forbids.
        stream::unfold(None, move |state: Option<(Instant, Instant)>| async move {
            let (anchor, deadline) = state.unwrap_or_else(|| {
                let anchor = Instant::now();
                (anchor, anchor + interval)
            });
            sleep_until(deadline).await;
            let next = next_anchor_phase_deadline(anchor, interval, Instant::now());
            Some((TimerEvent::Tick, Some((anchor, next))))
        })
        .boxed()
    }

    fn key(&self) -> Self::Key {
        self.interval_ms
    }
}

/// The first anchor-phase boundary strictly after `now`: the smallest
/// `anchor + k * interval` with `k >= 1` that lies after `now` (RFC 0009
/// §4.2 post-miss cadence).
fn next_anchor_phase_deadline(anchor: Instant, interval: Duration, now: Instant) -> Instant {
    let elapsed_intervals = now.duration_since(anchor).as_nanos() / interval.as_nanos();
    let offset_nanos = interval
        .as_nanos()
        .saturating_mul(elapsed_intervals + 1)
        .try_into()
        .unwrap_or(u64::MAX);
    anchor + Duration::from_nanos(offset_nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::task::Poll;
    use std::thread;

    use tokio::time::{Duration, Instant, advance, timeout};

    use crate::poll_util::noop_context;

    fn timer(interval_ms: u64) -> Timer {
        Timer::new(NonZeroU64::new(interval_ms).expect("timer interval must be non-zero"))
    }

    /// Polls the stream exactly once with the canonical no-op waker, so the
    /// executor never idles and the paused clock never auto-advances
    /// (RFC 0009 §3.2's non-idling controller). Pending and end-of-stream
    /// both read as `None`; the timer stream never ends, so the ambiguity is
    /// moot here.
    fn poll_once(stream: &mut BoxStream<'static, TimerEvent>) -> Option<TimerEvent> {
        match stream.as_mut().poll_next(&mut noop_context()) {
            Poll::Ready(item) => item,
            Poll::Pending => None,
        }
    }

    #[test]
    fn test_timer_new() {
        let interval_ms = NonZeroU64::new(1000).expect("timer interval must be non-zero");
        let timer = Timer::new(interval_ms);
        assert_eq!(timer.interval_ms, interval_ms);
    }

    #[test]
    fn test_timer_id_consistency() {
        let timer1 = timer(1000);
        let timer2 = timer(1000);

        // Same configuration should produce the same ID
        assert_eq!(timer1.key(), timer2.key());
    }

    #[test]
    fn test_timer_id_different_intervals() {
        let timer1 = timer(1000);
        let timer2 = timer(2000);

        // Different intervals should produce different IDs
        assert_ne!(timer1.key(), timer2.key());
    }

    // Paused-clock contract tests (RFC 0009 §4.2, INV-C3). The 1 ms and 2 ms
    // intervals fall inside Tokio's `MissedTickBehavior::Skip` lateness
    // margin, so an implementation leaning on Skip replays a catch-up burst
    // and fails them. The real-time tests further down remain non-normative
    // smoke checks.

    #[tokio::test(start_paused = true)]
    async fn test_timer_paused_first_tick_one_interval_after_anchor() {
        // INV-C3 (a) and (b): no tick before `anchor + interval`, first tick
        // ready once virtual now reaches it.
        let mut stream = timer(2).stream();

        assert!(
            poll_once(&mut stream).is_none(),
            "the first poll anchors and yields nothing"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            poll_once(&mut stream).is_none(),
            "no tick before the first deadline"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "first tick ready one interval after the anchor"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_timer_paused_no_catch_up_burst() {
        // INV-C3 (c): one advance spanning several interval boundaries makes
        // exactly one tick ready, followed by pending until the next
        // anchor-phase deadline.
        let mut stream = timer(1).stream();
        assert!(
            poll_once(&mut stream).is_none(),
            "the first poll anchors and yields nothing"
        );

        advance(Duration::from_micros(5500)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "one tick ready after the multi-interval advance"
        );
        assert!(
            poll_once(&mut stream).is_none(),
            "a second tick would be a catch-up burst"
        );

        // The next anchor-phase boundary (6 ms) is only 0.5 ms away.
        advance(Duration::from_micros(500)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "cadence resumes at the next anchor-phase boundary"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_timer_paused_post_miss_cadence_preserves_phase() {
        // INV-C3 (d): after a tick delivered late at 3.5 ms on a 1 ms timer,
        // the next tick fires at the 4 ms anchor-phase boundary, not at
        // "now + interval" (4.5 ms).
        let mut stream = timer(1).stream();
        assert!(
            poll_once(&mut stream).is_none(),
            "the first poll anchors and yields nothing"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "on-time first tick"
        );

        advance(Duration::from_micros(2500)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "late tick at 3.5 ms"
        );

        advance(Duration::from_micros(400)).await;
        assert!(
            poll_once(&mut stream).is_none(),
            "no tick before the 4 ms boundary"
        );

        advance(Duration::from_micros(100)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "tick at the preserved 4 ms anchor-phase boundary, not 4.5 ms"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_timer_paused_anchor_is_first_poll() {
        // INV-C3 (e): time advanced before the stream's first poll — whether
        // before or after `Timer::new` and `stream()` — does not consume the
        // first interval: the anchor is the first poll, and the first
        // deadline is one interval after it. An implementation anchoring at
        // `Timer::new` or at the `stream()` call would already have a tick
        // ready at the first poll below.
        let source = timer(2);
        advance(Duration::from_millis(10)).await;

        let mut stream = source.stream();
        advance(Duration::from_millis(10)).await;

        assert!(
            poll_once(&mut stream).is_none(),
            "pre-first-poll time must not count against the first interval"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            poll_once(&mut stream).is_none(),
            "one interval has not elapsed since the anchor"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "first deadline is first_poll_time + interval"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn test_timer_stream_built_outside_the_runtime_anchors_at_first_poll() {
        // INV-C3 (e), cross-context half: a stream built outside the polling
        // runtime's clock context (a plain thread reads the real clock)
        // still anchors on the polling runtime's virtual clock at its first
        // poll. A construction-time anchor reads the ambient clock instead
        // and fires immediately or never against the virtual clock.
        let mut stream = thread::spawn(|| timer(2).stream())
            .join()
            .expect("stream construction off-runtime should not panic");

        assert!(
            poll_once(&mut stream).is_none(),
            "no tick at the first poll"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            poll_once(&mut stream).is_none(),
            "no tick before one interval after the first poll"
        );

        advance(Duration::from_millis(1)).await;
        assert!(
            matches!(poll_once(&mut stream), Some(TimerEvent::Tick)),
            "first tick one interval after the first poll, on the polling runtime's clock"
        );
    }

    #[tokio::test]
    async fn test_timer_stream_produces_ticks() {
        let timer = timer(10); // 10ms interval for fast test
        let mut stream = timer.stream();

        // Should receive first tick after interval
        let result = timeout(Duration::from_millis(100), stream.next()).await;
        assert!(matches!(result, Ok(Some(TimerEvent::Tick))));
    }

    #[tokio::test]
    async fn test_timer_stream_multiple_ticks() {
        let timer = timer(10); // 10ms interval
        let mut stream = timer.stream();

        // Collect first 3 ticks
        let mut count = 0;
        for _ in 0..3 {
            let result = timeout(Duration::from_millis(100), stream.next()).await;
            if matches!(result, Ok(Some(TimerEvent::Tick))) {
                count += 1;
            }
        }

        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_timer_interval_accuracy() {
        let timer = timer(50); // 50ms interval
        let mut stream = timer.stream();

        let start = Instant::now();

        // Wait for first tick
        let result = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(matches!(result, Ok(Some(TimerEvent::Tick))));

        let first_tick = start.elapsed();

        // Wait for second tick
        let result = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(matches!(result, Ok(Some(TimerEvent::Tick))));

        let second_tick = start.elapsed();

        // NOTE: The interval should be accurate within a reasonable margin:
        // deadlines are anchor-phase boundaries, so lateness on one tick does
        // not drift the cadence. First tick should be around 50ms, second
        // around 100ms. Wide margins to account for CI environments and
        // system load.
        assert!(
            first_tick >= Duration::from_millis(30) && first_tick <= Duration::from_millis(100),
            "First tick was {first_tick:?}, expected between 30-100ms",
        );
        assert!(
            second_tick >= Duration::from_millis(80) && second_tick <= Duration::from_millis(150),
            "Second tick was {second_tick:?}, expected between 80-150ms",
        );
    }

    #[tokio::test]
    async fn test_timer_no_immediate_tick() {
        let timer = timer(100); // 100ms interval
        let mut stream = timer.stream();

        let start = Instant::now();

        // First tick should NOT be immediate (should wait for interval)
        // Use a reasonable timeout that accounts for CI environment delays
        let result = timeout(Duration::from_millis(70), stream.next()).await;
        assert!(
            result.is_err(),
            "Timer should not tick immediately (within 70ms)"
        );

        // But should arrive after the interval (with generous timeout for CI)
        let result = timeout(Duration::from_millis(200), stream.next()).await;
        assert!(
            matches!(result, Ok(Some(TimerEvent::Tick))),
            "Timer should tick after interval"
        );

        let elapsed = start.elapsed();
        // Relaxed assertion for CI environments
        assert!(
            elapsed >= Duration::from_millis(70),
            "Timer ticked too early: {elapsed:?}",
        );
    }

    #[tokio::test]
    async fn test_timer_different_intervals() {
        let fast_timer = timer(20);
        let slow_timer = timer(200);

        let mut fast_stream = fast_timer.stream();
        let mut slow_stream = slow_timer.stream();

        // Fast timer should tick first (with generous timeout for CI)
        let fast_result = timeout(Duration::from_millis(100), fast_stream.next()).await;
        let slow_result = timeout(Duration::from_millis(100), slow_stream.next()).await;

        assert!(
            fast_result.is_ok(),
            "Fast timer (20ms) should tick within 100ms"
        );
        assert!(
            slow_result.is_err(),
            "Slow timer (200ms) should not tick within 100ms"
        );
    }
}
