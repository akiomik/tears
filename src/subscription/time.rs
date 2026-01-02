//! Timer subscription for periodic events.
//!
//! This module provides the [`Timer`] subscription source for creating
//! time-based events in your application.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::time::MissedTickBehavior;
use tokio::time::interval;
use tokio_stream::wrappers::IntervalStream;

use super::{SubscriptionId, SubscriptionSource};

/// Messages produced by the [`Timer`] subscription.
#[derive(Debug, Clone)]
pub enum Message {
    /// A timer tick has occurred.
    Tick,
}

/// A timer subscription that emits tick messages at regular intervals.
///
/// This is useful for creating animations, periodic updates, or any time-based
/// behavior in your application.
///
/// ## Implementation Details
///
/// Uses `tokio::time::interval` with `MissedTickBehavior::Skip` to provide
/// accurate timing without catching up on missed ticks. This is appropriate
/// for UI applications where maintaining a consistent tick rate is more
/// important than processing every tick.
///
/// ## Performance
///
/// The timer uses Tokio's efficient interval mechanism with drift correction,
/// making it suitable for high frame rates (e.g., 60 FPS or higher).
///
/// # Example
///
/// ```rust
/// use tears::subscription::{Subscription, time::{Timer, Message as TimeMsg}};
///
/// enum AppMessage {
///     Tick,
/// }
///
/// // Create a timer that ticks every second (1000ms)
/// let sub = Subscription::new(Timer::new(1000))
///     .map(|_| AppMessage::Tick);
///
/// // For 60 FPS animations (approximately 16.67ms per frame)
/// let animation_timer = Subscription::new(Timer::new(16))
///     .map(|_| AppMessage::Tick);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    interval_ms: u64,
}

impl Timer {
    /// Create a new timer with the specified interval.
    ///
    /// # Arguments
    ///
    /// * `interval_ms` - The interval between ticks in milliseconds
    ///
    /// # Example
    ///
    /// ```
    /// use tears::subscription::time::Timer;
    ///
    /// // Tick every second
    /// let timer = Timer::new(1000);
    ///
    /// // Tick every 16ms (approximately 60 FPS)
    /// let fast_timer = Timer::new(16);
    /// ```
    #[must_use]
    pub const fn new(interval_ms: u64) -> Self {
        Self { interval_ms }
    }
}

impl SubscriptionSource for Timer {
    type Output = Message;

    fn stream(&self) -> BoxStream<'static, Message> {
        // NOTE: Using Skip behavior to drop missed ticks rather than trying to catch up.
        // This is appropriate for UI applications where we want
        // to maintain a consistent tick rate rather than processing old ticks.
        let duration = Duration::from_millis(self.interval_ms);
        let mut interval = interval(duration);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        IntervalStream::new(interval)
            .skip(1) // Skip the first immediate tick
            .map(|_| Message::Tick)
            .boxed()
    }

    fn id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        self.interval_ms.hash(&mut hasher);
        SubscriptionId::of::<Self>(hasher.finish())
    }
}

impl Hash for Timer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.interval_ms.hash(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use tokio::time::{Duration, timeout};

    #[test]
    fn test_timer_new() {
        let timer = Timer::new(1000);
        assert_eq!(timer.interval_ms, 1000);
    }

    #[test]
    fn test_timer_id_consistency() {
        let timer1 = Timer::new(1000);
        let timer2 = Timer::new(1000);

        // Same configuration should produce the same ID
        assert_eq!(timer1.id(), timer2.id());
    }

    #[test]
    fn test_timer_id_different_intervals() {
        let timer1 = Timer::new(1000);
        let timer2 = Timer::new(2000);

        // Different intervals should produce different IDs
        assert_ne!(timer1.id(), timer2.id());
    }

    #[test]
    fn test_timer_hash_consistency() {
        let timer1 = Timer::new(1000);
        let timer2 = Timer::new(1000);

        let mut hasher1 = DefaultHasher::new();
        timer1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        timer2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_timer_hash_different_intervals() {
        let timer1 = Timer::new(1000);
        let timer2 = Timer::new(2000);

        let mut hasher1 = DefaultHasher::new();
        timer1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        timer2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        assert_ne!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_timer_stream_produces_ticks() {
        let timer = Timer::new(10); // 10ms interval for fast test
        let mut stream = timer.stream();

        // Should receive first tick after interval
        let result = timeout(Duration::from_millis(100), stream.next()).await;
        assert!(matches!(result, Ok(Some(Message::Tick))));
    }

    #[tokio::test]
    async fn test_timer_stream_multiple_ticks() {
        let timer = Timer::new(10); // 10ms interval
        let mut stream = timer.stream();

        // Collect first 3 ticks
        let mut count = 0;
        for _ in 0..3 {
            let result = timeout(Duration::from_millis(100), stream.next()).await;
            if matches!(result, Ok(Some(Message::Tick))) {
                count += 1;
            }
        }

        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_timer_interval_accuracy() {
        use tokio::time::Instant;

        let timer = Timer::new(50); // 50ms interval
        let mut stream = timer.stream();

        let start = Instant::now();

        // Wait for first tick
        let result = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(matches!(result, Ok(Some(Message::Tick))));

        let first_tick = start.elapsed();

        // Wait for second tick
        let result = timeout(Duration::from_millis(300), stream.next()).await;
        assert!(matches!(result, Ok(Some(Message::Tick))));

        let second_tick = start.elapsed();

        // NOTE: The interval should be accurate within a reasonable margin.
        // With tokio::time::interval, we expect better accuracy than sleep-based approach.
        // First tick should be around 50ms, second around 100ms.
        // Wide margins to account for CI environments and system load.
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
        use tokio::time::Instant;

        let timer = Timer::new(100); // 100ms interval
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
            matches!(result, Ok(Some(Message::Tick))),
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
        let fast_timer = Timer::new(20);
        let slow_timer = Timer::new(200);

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
