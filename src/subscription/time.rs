use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;

use futures::stream::BoxStream;
use futures::{StreamExt, stream};
use tokio::time::sleep;

use super::{SubscriptionId, SubscriptionSource};

#[derive(Debug, Clone)]
pub enum Message {
    Tick,
}

/// Time-based subscription
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeSub {
    interval_ms: u64,
}

impl TimeSub {
    pub fn new(interval_ms: u64) -> Self {
        Self { interval_ms }
    }
}

impl SubscriptionSource for TimeSub {
    type Output = Message;

    fn stream(&self) -> BoxStream<'static, Message> {
        let interval_ms = self.interval_ms;
        stream::unfold((), move |_| async move {
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

impl Hash for TimeSub {
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
    fn test_time_sub_new() {
        let sub = TimeSub::new(1000);
        assert_eq!(sub.interval_ms, 1000);
    }

    #[test]
    fn test_time_sub_equality() {
        let sub1 = TimeSub::new(1000);
        let sub2 = TimeSub::new(1000);
        let sub3 = TimeSub::new(2000);

        assert_eq!(sub1, sub2);
        assert_ne!(sub1, sub3);
    }

    #[test]
    fn test_time_sub_id_consistency() {
        let sub1 = TimeSub::new(1000);
        let sub2 = TimeSub::new(1000);

        // Same configuration should produce the same ID
        assert_eq!(sub1.id(), sub2.id());
    }

    #[test]
    fn test_time_sub_id_different_intervals() {
        let sub1 = TimeSub::new(1000);
        let sub2 = TimeSub::new(2000);

        // Different intervals should produce different IDs
        assert_ne!(sub1.id(), sub2.id());
    }

    #[test]
    fn test_time_sub_hash_consistency() {
        let sub1 = TimeSub::new(1000);
        let sub2 = TimeSub::new(1000);

        let mut hasher1 = DefaultHasher::new();
        sub1.hash(&mut hasher1);
        let hash1 = hasher1.finish();

        let mut hasher2 = DefaultHasher::new();
        sub2.hash(&mut hasher2);
        let hash2 = hasher2.finish();

        assert_eq!(hash1, hash2);
    }

    #[tokio::test]
    async fn test_time_sub_stream_produces_ticks() {
        let sub = TimeSub::new(10); // 10ms interval for fast test
        let mut stream = sub.stream();

        // Should receive first tick
        let result = timeout(Duration::from_millis(100), stream.next()).await;
        assert!(result.is_ok());

        if let Ok(Some(Message::Tick)) = result {
            // Expected
        } else {
            panic!("Expected tick message");
        }
    }

    #[tokio::test]
    async fn test_time_sub_stream_multiple_ticks() {
        let sub = TimeSub::new(10); // 10ms interval
        let mut stream = sub.stream();

        // Collect first 3 ticks
        let mut count = 0;
        for _ in 0..3 {
            let result = timeout(Duration::from_millis(100), stream.next()).await;
            if let Ok(Some(Message::Tick)) = result {
                count += 1;
            }
        }

        assert_eq!(count, 3);
    }

    #[test]
    fn test_message_debug() {
        let msg = Message::Tick;
        let debug_str = format!("{:?}", msg);
        assert!(debug_str.contains("Tick"));
    }

    #[test]
    fn test_message_clone() {
        let msg1 = Message::Tick;
        let msg2 = msg1.clone();

        // Both should be Tick
        matches!(msg1, Message::Tick);
        matches!(msg2, Message::Tick);
    }
}
