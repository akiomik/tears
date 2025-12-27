//! Timer subscription for periodic events.
//!
//! This module provides the [`Timer`] subscription source for creating
//! time-based events in your application.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use tokio::time::sleep;

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
        let interval_ms = self.interval_ms;
        stream::unfold((), move |()| async move {
            let interval = Duration::from_millis(interval_ms);
            sleep(interval).await;
            Some((Message::Tick, ()))
        })
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
    fn test_timer_equality() {
        let timer1 = Timer::new(1000);
        let timer2 = Timer::new(1000);
        let timer3 = Timer::new(2000);

        assert_eq!(timer1, timer2);
        assert_ne!(timer1, timer3);
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

    #[tokio::test]
    async fn test_timer_stream_produces_ticks() {
        let timer = Timer::new(10); // 10ms interval for fast test
        let mut stream = timer.stream();

        // Should receive first tick
        let result = timeout(Duration::from_millis(100), stream.next()).await;
        assert!(result.is_ok());

        if matches!(result, Ok(Some(Message::Tick))) {
            // Expected
        } else {
            panic!("Expected tick message");
        }
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

    #[test]
    fn test_message_debug() {
        let msg = Message::Tick;
        let debug_str = format!("{msg:?}");
        assert!(debug_str.contains("Tick"));
    }

    #[test]
    fn test_message_clone() {
        let msg1 = Message::Tick;
        let msg2 = msg1;

        // Both should be Tick
        matches!(msg1, Message::Tick);
        matches!(msg2, Message::Tick);
    }
}
