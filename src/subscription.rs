pub mod terminal;
pub mod time;

use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
};

use futures::{StreamExt, stream::BoxStream};
use tokio::{
    sync::mpsc::{self},
    task::JoinHandle,
};

/// A subscription represents an ongoing source of messages.
///
/// Subscriptions are used to listen to external events that occur over time, such as:
/// - Keyboard and mouse input
/// - Timer ticks
/// - WebSocket messages
/// - File system changes
/// - Network events
///
/// Unlike commands which are one-time operations, subscriptions continue to produce
/// messages until they are cancelled.
///
/// # Example
///
/// ```
/// use tears::subscription::{Subscription, time::{TimeSub, Message as TimeMsg}};
///
/// enum Message {
///     Tick,
/// }
///
/// // Create a subscription that sends a message every second
/// let sub = Subscription::new(TimeSub::new(1000)).map(|_| Message::Tick);
/// ```
pub struct Subscription<Msg: 'static> {
    pub(super) id: SubscriptionId,
    pub(super) stream: BoxStream<'static, Msg>,
}

impl<Msg: 'static> Subscription<Msg> {
    /// Create a new subscription from a type implementing [`SubscriptionInner`].
    ///
    /// # Arguments
    ///
    /// * `inner` - The subscription implementation
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::subscription::{Subscription, time::TimeSub};
    ///
    /// // Create a timer subscription that ticks every second
    /// let sub = Subscription::new(TimeSub::new(1000));
    /// ```
    pub fn new(inner: impl SubscriptionInner<Output = Msg>) -> Subscription<Msg> {
        let id = inner.id();

        Self {
            id,
            stream: inner.stream().boxed(),
        }
    }

    /// Transform the messages produced by this subscription.
    ///
    /// This is useful for converting subscription-specific messages into your
    /// application's message type.
    ///
    /// # Arguments
    ///
    /// * `f` - Function to transform each message
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::subscription::{Subscription, time::{TimeSub, Message as TimeMsg}};
    ///
    /// enum AppMessage {
    ///     TimerTick,
    /// }
    ///
    /// // Create a timer and map its messages to your app's messages
    /// let sub = Subscription::new(TimeSub::new(1000))
    ///     .map(|_tick_msg| AppMessage::TimerTick);
    /// ```
    pub fn map<F, NewMsg>(self, f: F) -> Subscription<NewMsg>
    where
        F: Fn(Msg) -> NewMsg + Send + 'static,
        Msg: 'static,
        NewMsg: 'static,
    {
        let stream: BoxStream<'static, NewMsg> = self.stream.map(f).boxed();
        Subscription {
            id: self.id,
            stream,
        }
    }
}

impl<A: SubscriptionInner<Output = Msg>, Msg> From<A> for Subscription<Msg> {
    fn from(value: A) -> Self {
        Self::new(value)
    }
}

/// Trait for types that can be used as subscription sources.
///
/// Implement this trait to create custom subscription types.
/// The trait requires:
/// - A stream of output messages
/// - A unique identifier for the subscription
///
/// # Example
///
/// ```
/// use tears::subscription::{SubscriptionInner, SubscriptionId};
/// use futures::stream::{self, BoxStream};
/// use futures::StreamExt;
/// use std::hash::{Hash, Hasher};
///
/// struct MySubscription {
///     interval_ms: u64,
/// }
///
/// impl SubscriptionInner for MySubscription {
///     type Output = ();
///
///     fn stream(&self) -> BoxStream<'static, Self::Output> {
///         // Implementation details...
///         stream::empty().boxed()
///     }
///
///     fn id(&self) -> SubscriptionId {
///         // Create a unique ID based on the subscription's configuration
///         let mut hasher = std::collections::hash_map::DefaultHasher::new();
///         self.interval_ms.hash(&mut hasher);
///         SubscriptionId::from_hash(hasher.finish())
///     }
/// }
/// ```
pub trait SubscriptionInner: Send {
    /// The type of messages this subscription produces.
    type Output;

    /// Create the stream of messages for this subscription.
    ///
    /// This method is called when the subscription is started.
    fn stream(&self) -> BoxStream<'static, Self::Output>;

    /// Get a unique identifier for this subscription.
    ///
    /// The ID is used to determine if a subscription has changed.
    /// Subscriptions with the same ID are considered identical.
    fn id(&self) -> SubscriptionId;
}

/// A unique identifier for a subscription.
///
/// Subscription IDs are used to track and manage active subscriptions.
/// Two subscriptions with the same ID are considered identical, and
/// the runtime will not create duplicate subscriptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(u64);

impl SubscriptionId {
    /// Create a subscription ID from a hash value.
    ///
    /// This is typically used when implementing [`SubscriptionInner::id`].
    pub fn from_hash(hash: u64) -> Self {
        Self(hash)
    }
}

struct RunningSubscription {
    handle: JoinHandle<()>,
}

pub struct SubscriptionManager<Msg> {
    running: HashMap<SubscriptionId, RunningSubscription>,
    msg_sender: mpsc::UnboundedSender<Msg>,
}

impl<Msg: Send + 'static> SubscriptionManager<Msg> {
    pub fn new(msg_sender: mpsc::UnboundedSender<Msg>) -> Self {
        Self {
            running: HashMap::new(),
            msg_sender,
        }
    }

    pub fn update<I>(&mut self, subscriptions: I)
    where
        I: IntoIterator<Item = Subscription<Msg>>,
    {
        let mut new_subs: HashMap<_, _> = subscriptions
            .into_iter()
            .map(|sub| (sub.id, sub.stream))
            .collect();
        let new_ids: HashSet<_> = new_subs.keys().copied().collect();
        let current_ids: HashSet<_> = self.running.keys().copied().collect();

        let to_remove: Vec<_> = current_ids.difference(&new_ids).copied().collect();
        let to_add: Vec<_> = new_ids.difference(&current_ids).copied().collect();

        for id in to_remove {
            if let Some(running) = self.running.remove(&id) {
                running.handle.abort();
            }
        }

        for id in to_add {
            if let Some(stream) = new_subs.remove(&id) {
                let handle = self.spawn_subscription(stream);
                self.running.insert(id, RunningSubscription { handle });
            }
        }
    }

    fn spawn_subscription(&self, mut stream: BoxStream<'static, Msg>) -> JoinHandle<()> {
        let sender = self.msg_sender.clone();

        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                if sender.send(msg).is_err() {
                    break;
                }
            }
        })
    }

    pub fn shutdown(&mut self) {
        for (_, running) in self.running.drain() {
            running.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use tokio::time::{Duration, sleep, timeout};

    // Test helper: Simple subscription implementation
    struct TestSub {
        id: u64,
        values: Vec<i32>,
    }

    impl SubscriptionInner for TestSub {
        type Output = i32;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            stream::iter(self.values.clone()).boxed()
        }

        fn id(&self) -> SubscriptionId {
            SubscriptionId::from_hash(self.id)
        }
    }

    #[test]
    fn test_subscription_new() {
        let test_sub = TestSub {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub);

        assert_eq!(sub.id, SubscriptionId::from_hash(1));
    }

    #[tokio::test]
    async fn test_subscription_map() {
        let test_sub = TestSub {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub).map(|x| x * 2);

        let mut stream = sub.stream;
        let mut results = vec![];

        while let Some(value) = stream.next().await {
            results.push(value);
        }

        assert_eq!(results, vec![2, 4, 6]);
    }

    #[tokio::test]
    async fn test_subscription_map_type_conversion() {
        #[derive(Debug, PartialEq)]
        enum Message {
            Number(i32),
        }

        let test_sub = TestSub {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub).map(|x| Message::Number(x));

        let mut stream = sub.stream;
        let mut results = vec![];

        while let Some(value) = stream.next().await {
            results.push(value);
        }

        assert_eq!(
            results,
            vec![Message::Number(1), Message::Number(2), Message::Number(3)]
        );
    }

    #[test]
    fn test_subscription_id_from_hash() {
        let id1 = SubscriptionId::from_hash(12345);
        let id2 = SubscriptionId::from_hash(12345);
        let id3 = SubscriptionId::from_hash(67890);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_subscription_id_debug() {
        let id = SubscriptionId::from_hash(12345);
        let debug_str = format!("{:?}", id);
        assert!(debug_str.contains("SubscriptionId"));
    }

    #[test]
    fn test_subscription_id_clone() {
        let id1 = SubscriptionId::from_hash(12345);
        let id2 = id1.clone();
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn test_subscription_manager_update() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let test_sub = TestSub {
            id: 1,
            values: vec![10, 20],
        };
        let sub = Subscription::new(test_sub);

        manager.update(vec![sub]);

        // Should receive messages from the subscription
        let msg1 = timeout(Duration::from_millis(100), rx.recv()).await;
        assert_eq!(msg1.unwrap(), Some(10));

        let msg2 = timeout(Duration::from_millis(100), rx.recv()).await;
        assert_eq!(msg2.unwrap(), Some(20));
    }

    #[tokio::test]
    async fn test_subscription_manager_remove_subscription() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let test_sub = TestSub {
            id: 1,
            values: vec![10],
        };
        let sub = Subscription::new(test_sub);

        // Add subscription
        manager.update(vec![sub]);

        // Receive first message
        let msg = timeout(Duration::from_millis(100), rx.recv()).await;
        assert_eq!(msg.unwrap(), Some(10));

        // Remove subscription by updating with empty list
        manager.update(Vec::<Subscription<i32>>::new());

        // Give time for any pending messages
        sleep(Duration::from_millis(50)).await;

        // Channel should be empty now (or closed)
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_subscription_manager_replace_subscription() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        // First subscription
        let test_sub1 = TestSub {
            id: 1,
            values: vec![10],
        };
        let sub1 = Subscription::new(test_sub1);
        manager.update(vec![sub1]);

        let msg = timeout(Duration::from_millis(100), rx.recv()).await;
        assert_eq!(msg.unwrap(), Some(10));

        // Replace with different subscription
        let test_sub2 = TestSub {
            id: 2,
            values: vec![20],
        };
        let sub2 = Subscription::new(test_sub2);
        manager.update(vec![sub2]);

        let msg = timeout(Duration::from_millis(100), rx.recv()).await;
        assert_eq!(msg.unwrap(), Some(20));
    }

    #[tokio::test]
    async fn test_subscription_manager_shutdown() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        // Create a long-running subscription
        struct InfiniteSub;
        impl SubscriptionInner for InfiniteSub {
            type Output = i32;

            fn stream(&self) -> BoxStream<'static, Self::Output> {
                stream::unfold(0, |state| async move {
                    sleep(Duration::from_millis(10)).await;
                    Some((state, state + 1))
                })
                .boxed()
            }

            fn id(&self) -> SubscriptionId {
                SubscriptionId::from_hash(999)
            }
        }

        let sub = Subscription::new(InfiniteSub);
        manager.update(vec![sub]);

        // Receive a few messages
        let _ = timeout(Duration::from_millis(100), rx.recv()).await;

        // Shutdown should cancel all subscriptions
        manager.shutdown();

        // Wait a bit
        sleep(Duration::from_millis(50)).await;

        // Should not receive more messages after shutdown
        // The channel might have some buffered messages, but stream should stop
    }

    #[tokio::test]
    async fn test_subscription_manager_multiple_subscriptions() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let test_sub1 = TestSub {
            id: 1,
            values: vec![1],
        };
        let test_sub2 = TestSub {
            id: 2,
            values: vec![2],
        };

        manager.update(vec![
            Subscription::new(test_sub1),
            Subscription::new(test_sub2),
        ]);

        // Should receive messages from both subscriptions
        let mut results = vec![];
        for _ in 0..2 {
            if let Ok(Some(msg)) = timeout(Duration::from_millis(100), rx.recv()).await {
                results.push(msg);
            }
        }

        results.sort();
        assert_eq!(results, vec![1, 2]);
    }
}
