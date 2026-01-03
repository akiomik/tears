//! Subscriptions for handling ongoing event sources.
//!
//! Subscriptions represent streams of events that your application can listen to,
//! such as terminal input, timers, or any custom event source.
//!
//! # Built-in Subscriptions
//!
//! - [`terminal::TerminalEvents`] - Terminal input events (keyboard, mouse, resize)
//! - [`time::Timer`] - Timer ticks at regular intervals
//! - [`signal::Signal`] (Unix) - Unix signals (SIGINT, SIGTERM, etc.)
//! - `signal::CtrlC` (Windows) - Ctrl+C events
//! - `signal::CtrlBreak` (Windows) - Ctrl+Break events
#![cfg_attr(
    feature = "ws",
    doc = "- [`websocket::WebSocket`] - WebSocket connections (requires `ws` feature)"
)]
#![cfg_attr(
    not(feature = "ws"),
    doc = "- `websocket::WebSocket` - WebSocket connections (requires `ws` feature)"
)]
//!
//!
//! # Creating Custom Subscriptions
//!
//! Implement the [`SubscriptionSource`] trait to create your own subscription types:
//!
//! ```
//! use tears::subscription::{SubscriptionSource, SubscriptionId, Subscription};
//! use tears::BoxStream;
//! use futures::{StreamExt, stream};
//! use std::hash::{Hash, Hasher};
//!
//! struct MySubscription {
//!     id: u64,
//! }
//!
//! impl SubscriptionSource for MySubscription {
//!     type Output = String;
//!
//!     fn stream(&self) -> BoxStream<'static, Self::Output> {
//!         stream::once(async { "Hello".to_string() }).boxed()
//!     }
//!
//!     fn id(&self) -> SubscriptionId {
//!         SubscriptionId::of::<Self>(self.id)
//!     }
//! }
//!
//! // Use it in your application
//! enum Message {
//!     MyEvent(String),
//! }
//!
//! let sub = Subscription::new(MySubscription { id: 1 })
//!     .map(Message::MyEvent);
//! ```

pub mod signal;
pub mod terminal;
pub mod time;
#[cfg(feature = "ws")]
pub mod websocket;

use std::{
    any::TypeId,
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
/// use tears::subscription::{Subscription, time::{Timer, Message as TimeMsg}};
///
/// enum Message {
///     Tick,
/// }
///
/// // Create a subscription that sends a message every second
/// let sub = Subscription::new(Timer::new(1000)).map(|_| Message::Tick);
/// ```
pub struct Subscription<Msg: 'static> {
    pub(super) id: SubscriptionId,
    pub(super) spawn: Box<dyn FnOnce() -> BoxStream<'static, Msg> + Send>,
}

impl<Msg: 'static> Subscription<Msg> {
    /// Create a new subscription from a type implementing [`SubscriptionSource`].
    ///
    /// # Arguments
    ///
    /// * `source` - The subscription implementation
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::subscription::{Subscription, time::Timer};
    ///
    /// // Create a timer subscription that ticks every second
    /// let sub = Subscription::new(Timer::new(1000));
    /// ```
    #[must_use]
    pub fn new(source: impl SubscriptionSource<Output = Msg> + 'static) -> Self {
        let id = source.id();

        Self {
            id,
            spawn: Box::new(move || source.stream().boxed()),
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
    /// use tears::subscription::{Subscription, time::{Timer, Message as TimeMsg}};
    ///
    /// enum AppMessage {
    ///     TimerTick,
    /// }
    ///
    /// // Create a timer and map its messages to your app's messages
    /// let sub = Subscription::new(Timer::new(1000))
    ///     .map(|_tick_msg| AppMessage::TimerTick);
    /// ```
    #[must_use]
    pub fn map<F, NewMsg>(self, f: F) -> Subscription<NewMsg>
    where
        F: Fn(Msg) -> NewMsg + Send + 'static,
        Msg: 'static,
        NewMsg: 'static,
    {
        let spawn = self.spawn;
        Subscription {
            id: self.id,
            spawn: Box::new(move || {
                let stream = spawn();
                stream.map(f).boxed()
            }),
        }
    }
}

impl<A: SubscriptionSource<Output = Msg> + 'static, Msg> From<A> for Subscription<Msg> {
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
/// use tears::subscription::{SubscriptionSource, SubscriptionId};
/// use tears::BoxStream;
/// use futures::{StreamExt, stream};
/// use std::hash::{Hash, Hasher};
///
/// struct MySubscription {
///     interval_ms: u64,
/// }
///
/// impl SubscriptionSource for MySubscription {
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
///         SubscriptionId::of::<Self>(hasher.finish())
///     }
/// }
/// ```
pub trait SubscriptionSource: Send {
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
///
/// The ID includes both type information and a hash value to prevent
/// collisions between different subscription types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId {
    type_id: TypeId,
    hash: u64,
}

impl SubscriptionId {
    /// Create a subscription ID from a type and a hash value.
    ///
    /// This ensures that subscriptions of different types cannot collide
    /// even if they have the same hash value.
    ///
    /// This is typically used when implementing [`SubscriptionSource::id`].
    ///
    /// # Type Parameters
    ///
    /// * `T` - The type of the subscription source, typically `Self`
    ///
    /// # Arguments
    ///
    /// * `hash` - A hash value representing the subscription's configuration
    ///
    /// # Examples
    ///
    /// ```
    /// use tears::subscription::SubscriptionId;
    /// use std::hash::{Hash, Hasher};
    /// use std::collections::hash_map::DefaultHasher;
    ///
    /// struct MySubscription {
    ///     interval_ms: u64,
    /// }
    ///
    /// impl MySubscription {
    ///     fn compute_id(&self) -> SubscriptionId {
    ///         let mut hasher = DefaultHasher::new();
    ///         self.interval_ms.hash(&mut hasher);
    ///         SubscriptionId::of::<Self>(hasher.finish())
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn of<T: 'static>(hash: u64) -> Self {
        Self {
            type_id: TypeId::of::<T>(),
            hash,
        }
    }
}

struct RunningSubscription {
    handle: JoinHandle<()>,
}

/// Manages the lifecycle of active subscriptions.
///
/// This is used internally by the runtime to start, stop, and update subscriptions
/// as the application state changes. You typically don't need to interact with this
/// type directly.
pub struct SubscriptionManager<Msg> {
    running: HashMap<SubscriptionId, RunningSubscription>,
    msg_sender: mpsc::UnboundedSender<Msg>,
}

impl<Msg: Send + 'static> SubscriptionManager<Msg> {
    /// Create a new subscription manager.
    ///
    /// # Arguments
    ///
    /// * `msg_sender` - Channel sender for forwarding subscription messages
    #[must_use]
    pub fn new(msg_sender: mpsc::UnboundedSender<Msg>) -> Self {
        Self {
            running: HashMap::new(),
            msg_sender,
        }
    }

    /// Update the set of active subscriptions.
    ///
    /// This method performs a diff between the current subscriptions and the new ones:
    /// - Subscriptions that are no longer present will be cancelled
    /// - New subscriptions will be started
    /// - Subscriptions with the same ID will continue running
    ///
    /// # Arguments
    ///
    /// * `subscriptions` - The new set of subscriptions to run
    pub fn update<I>(&mut self, subscriptions: I)
    where
        I: IntoIterator<Item = Subscription<Msg>>,
    {
        // NOTE: Store stream spawners instead of streams to avoid creating
        // streams unnecessarily. This is important for subscriptions like
        // TerminalEvents where creating the stream has side effects.
        let mut new_subs: HashMap<_, _> = subscriptions
            .into_iter()
            .map(|sub| (sub.id, sub.spawn))
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
            if let Some(spawn) = new_subs.remove(&id) {
                // Only call the spawner when we actually need to start the subscription
                let stream = spawn();
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

    /// Shut down all active subscriptions.
    ///
    /// This cancels all running subscription tasks. Called automatically
    /// when the runtime shuts down.
    pub fn shutdown(&mut self) {
        for (_, running) in self.running.drain() {
            running.handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use color_eyre::eyre::Result;
    use futures::stream;
    use tokio::time::{Duration, sleep, timeout};

    // Test helper: Simple subscription implementation
    struct TestSource {
        id: u64,
        values: Vec<i32>,
    }

    impl SubscriptionSource for TestSource {
        type Output = i32;

        fn stream(&self) -> BoxStream<'static, Self::Output> {
            stream::iter(self.values.clone()).boxed()
        }

        fn id(&self) -> SubscriptionId {
            SubscriptionId::of::<Self>(self.id)
        }
    }

    #[test]
    fn test_subscription_new() {
        let test_sub = TestSource {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub);

        assert_eq!(sub.id, SubscriptionId::of::<TestSource>(1));
    }

    #[tokio::test]
    async fn test_subscription_map() {
        let test_sub = TestSource {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub).map(|x| x * 2);

        let mut stream = (sub.spawn)();
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

        let test_sub = TestSource {
            id: 1,
            values: vec![1, 2, 3],
        };
        let sub = Subscription::new(test_sub).map(Message::Number);

        let mut stream = (sub.spawn)();
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
    fn test_subscription_id_of() {
        let id1 = SubscriptionId::of::<i32>(12345);
        let id2 = SubscriptionId::of::<i32>(12345);
        let id3 = SubscriptionId::of::<i32>(67890);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_subscription_id_different_types() {
        // Same hash value but different types should produce different IDs
        let id_i32 = SubscriptionId::of::<i32>(12345);
        let id_u64 = SubscriptionId::of::<u64>(12345);
        let id_string = SubscriptionId::of::<String>(12345);

        assert_ne!(id_i32, id_u64);
        assert_ne!(id_i32, id_string);
        assert_ne!(id_u64, id_string);
    }

    #[tokio::test]
    async fn test_subscription_manager_update() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let test_sub = TestSource {
            id: 1,
            values: vec![10, 20],
        };
        let sub = Subscription::new(test_sub);

        manager.update(vec![sub]);

        // Should receive messages from the subscription
        let msg1 = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg1, Some(10));

        let msg2 = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg2, Some(20));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_remove_subscription() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let test_sub = TestSource {
            id: 1,
            values: vec![10],
        };
        let sub = Subscription::new(test_sub);

        // Add subscription
        manager.update(vec![sub]);

        // Receive first message
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(10));

        // Remove subscription by updating with empty list
        manager.update(Vec::<Subscription<i32>>::new());

        // Give time for any pending messages
        sleep(Duration::from_millis(50)).await;

        // Channel should be empty now (or closed)
        assert!(rx.try_recv().is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_replace_subscription() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        // First subscription
        let test_sub1 = TestSource {
            id: 1,
            values: vec![10],
        };
        let sub1 = Subscription::new(test_sub1);
        manager.update(vec![sub1]);

        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(10));

        // Replace with different subscription
        let test_sub2 = TestSource {
            id: 2,
            values: vec![20],
        };
        let sub2 = Subscription::new(test_sub2);
        manager.update(vec![sub2]);

        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(20));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_shutdown() {
        // Create a long-running subscription
        struct InfiniteSub;
        impl SubscriptionSource for InfiniteSub {
            type Output = i32;

            fn stream(&self) -> BoxStream<'static, Self::Output> {
                stream::unfold(0, |state| async move {
                    sleep(Duration::from_millis(10)).await;
                    Some((state, state + 1))
                })
                .boxed()
            }

            fn id(&self) -> SubscriptionId {
                SubscriptionId::of::<Self>(999)
            }
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

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

        let test_sub1 = TestSource {
            id: 1,
            values: vec![1],
        };
        let test_sub2 = TestSource {
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

        results.sort_unstable();
        assert_eq!(results, vec![1, 2]);
    }
}
