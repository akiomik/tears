//! Subscriptions for handling ongoing event sources.
//!
//! Subscriptions represent streams of events that your application can listen to,
//! such as terminal input, timers, or any custom event source.
//!
//! # Design Philosophy
//!
//! tears distinguishes between three types of subscriptions based on their
//! communication patterns. Understanding these patterns helps you choose the
//! right approach for your use case.
//!
//! ## 1. Unidirectional Event Sources
//!
//! These subscriptions only emit events without any need for sending data back.
//!
//! **Examples**: [`terminal::TerminalEvents`], [`time::Timer`], [`signal::Signal`]
//!
//! **Characteristics**:
//! - Simple event streaming from external sources
//! - No application-to-source communication needed
//! - Only implement [`SubscriptionSource::stream`]
//!
//! ```rust
//! use tears::subscription::{Subscription, time::Timer};
//!
//! # enum Message { Tick }
//! // Timer only emits tick events
//! let timer = Subscription::new(Timer::new(1000)) // 1000ms = 1 second
//!     .map(|_| Message::Tick);
//! ```
//!
//! ## 2. Stream-based Bidirectional Communication
//!
//! These subscriptions manage long-lived connections where both receiving and
//! sending happen continuously within a single stream.
//!
//! **Examples**: `websocket::WebSocket`, future gRPC streams
//!
//! **Characteristics**:
//! - Long-lived connection managed by the subscription
//! - Sending is part of the ongoing communication stream
//! - Provides `mpsc::Sender` (or similar handle) for immediate send operations
//! - The subscription itself is the side effect; sending through it is control
//!
//! **Design Rationale**: In stream-based protocols, sending data is not a new
//! side effect but rather a control operation on an existing connection. The
//! connection itself is managed by the subscription, and sending through it is
//! immediate and stream-like in nature. Using `Command` for each send would be
//! unnatural for real-time applications like chat or live feeds.
//!
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::{Subscription, websocket::{WebSocket, WebSocketMessage, WebSocketCommand}};
//! use tokio::sync::mpsc;
//!
//! # enum Message {
//! #     WebSocket(WebSocketMessage),
//! #     SendText(String),
//! # }
//! struct App {
//!     ws_sender: Option<mpsc::UnboundedSender<WebSocketCommand>>,
//! }
//!
//! impl Application for App {
//! #   type Message = Message;
//! #   type Flags = ();
//! #   fn new(_: ()) -> (Self, Command<Message>) { (Self { ws_sender: None }, Command::none()) }
//! #   fn view(&self, _: &mut ratatui::Frame) {}
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::WebSocket(WebSocketMessage::Connected { sender }) => {
//!                 self.ws_sender = Some(sender);
//!                 Command::none()
//!             }
//!             Message::SendText(text) => {
//!                 // Immediate send - natural for stream-based communication
//!                 if let Some(sender) = &self.ws_sender {
//!                     let _ = sender.send(WebSocketCommand::SendText(text));
//!                 }
//!                 Command::none()
//!             }
//!             _ => Command::none()
//!         }
//!     }
//!
//!     fn subscriptions(&self) -> Vec<Subscription<Message>> {
//!         vec![
//!             Subscription::new(WebSocket::new("wss://example.com"))
//!                 .map(Message::WebSocket)
//!         ]
//!     }
//! }
//! ```
//!
//! ## 3. Transaction-based Operations
//!
//! These represent discrete request/response cycles where each operation is
//! a distinct side effect.
//!
//! **Examples**: HTTP queries/mutations, database operations, file I/O
//!
//! **Characteristics**:
//! - Discrete request/response cycles
//! - Each request is a new side effect that should be initiated explicitly
//! - Operations return [`Command`](crate::Command) to maintain TEA principles
//! - State may be shared via `Arc<Client>` for connection pooling or caching
//!
//! **Design Rationale**: Transaction-based operations have a clear beginning
//! (the request) and end (the response). Each request is a new side effect that
//! should be explicitly initiated and tracked. Using `Command` makes these
//! operations testable, composable, and allows for future features like
//! cancellation and retry.
//!
//! ```rust,ignore
//! use tears::prelude::*;
//! use tears::subscription::http::query::{Query, QueryError};
//! use tears::subscription::http::QueryClient;
//! use std::sync::Arc;
//! use futures::future::BoxFuture;
//!
//! # enum Message {
//! #     LoadUser(String),
//! #     UserLoaded(Result<String, QueryError>),
//! # }
//! # fn fetch_user_from_api() -> BoxFuture<'static, Result<String, QueryError>> {
//! #     Box::pin(async { Ok("user".to_string()) })
//! # }
//! struct App {
//!     query_client: Arc<QueryClient>,
//! }
//!
//! impl Application for App {
//! #   type Message = Message;
//! #   type Flags = ();
//! #   fn new(_: ()) -> (Self, Command<Message>) {
//! #       (Self { query_client: Arc::new(QueryClient::new()) }, Command::none())
//! #   }
//! #   fn view(&self, _: &mut ratatui::Frame) {}
//!     fn update(&mut self, msg: Message) -> Command<Message> {
//!         match msg {
//!             Message::LoadUser(id) => {
//!                 // Returns Command - natural for transaction-based operations
//!                 Query::new(self.query_client.clone())
//!                     .fetch(id, fetch_user_from_api, Message::UserLoaded)
//!             }
//!             _ => Command::none()
//!         }
//!     }
//! }
//! ```
//!
//! ## Choosing the Right Pattern
//!
//! - **Unidirectional**: Use when you only need to receive events (timers, signals, input)
//! - **Stream-based**: Use for real-time, continuous bidirectional communication (WebSocket, gRPC streams)
//! - **Transaction-based**: Use for discrete operations with clear start/end (HTTP, database, files)
//!
//! The key distinction between stream-based and transaction-based is:
//! - Stream-based: Sending is **control** of an ongoing connection
//! - Transaction-based: Each operation is a **new side effect** to initiate
//!
//! # Built-in Subscriptions
//!
//! - [`terminal::TerminalEvents`] - Terminal input events (keyboard, mouse, resize)
//! - [`time::Timer`] - Timer ticks at regular intervals
//! - [`signal::Signal`] (Unix) - Unix signals (SIGINT, SIGTERM, etc.)
//! - `signal::CtrlC` (Windows) - Ctrl+C events
//! - `signal::CtrlBreak` (Windows) - Ctrl+Break events
//! - [`mock::MockSource`] - Controllable mock for testing
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
//!
//! # Testing
//!
//! Use [`mock::MockSource`] for deterministic testing without real I/O:
//!
//! ```no_run
//! use tears::subscription::mock::MockSource;
//! use tears::subscription::Subscription;
//! use futures::StreamExt;
//!
//! #[tokio::test]
//! async fn test_example() {
//!     // Create a controllable mock
//!     let mock = MockSource::<i32>::new();
//!
//!     // Create a subscription and spawn its stream (creates a receiver)
//!     let subscription = Subscription::new(mock.clone());
//!     let mut stream = (subscription.spawn)();
//!
//!     // Control events from your test
//!     mock.emit(42).expect("should emit");
//!
//!     // Receive the value
//!     let value = stream.next().await;
//!     assert_eq!(value, Some(42));
//! }
//! ```
//!
//! See the [`mock`] module documentation for complete testing examples.

#[cfg(feature = "http")]
pub mod http;
pub mod mock;
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
    use crate::subscription::mock::MockSource;
    use color_eyre::eyre::Result;
    use tokio::time::{Duration, sleep, timeout};

    #[test]
    fn test_subscription_new() {
        use crate::subscription::mock::MockSource;

        let mock = MockSource::<i32>::new();
        let sub = Subscription::new(mock);

        // Should have correct ID type
        assert_eq!(sub.id.type_id, TypeId::of::<MockSource<i32>>());
    }

    #[tokio::test]
    async fn test_subscription_map() -> Result<()> {
        use crate::subscription::mock::MockSource;

        let mock = MockSource::new();
        let sub = Subscription::new(mock.clone()).map(|x: i32| x * 2);

        let mut stream = (sub.spawn)();

        // Emit values
        mock.emit(1)?;
        mock.emit(2)?;
        mock.emit(3)?;

        // Collect mapped values
        let mut results = vec![];
        for _ in 0..3 {
            if let Some(value) = stream.next().await {
                results.push(value);
            }
        }

        assert_eq!(results, vec![2, 4, 6]);
        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_map_type_conversion() -> Result<()> {
        use crate::subscription::mock::MockSource;

        #[derive(Debug, PartialEq)]
        enum Message {
            Number(i32),
        }

        let mock = MockSource::new();
        let sub = Subscription::new(mock.clone()).map(Message::Number);

        let mut stream = (sub.spawn)();

        // Emit values
        mock.emit(1)?;
        mock.emit(2)?;
        mock.emit(3)?;

        // Collect mapped values
        let mut results = vec![];
        for _ in 0..3 {
            if let Some(value) = stream.next().await {
                results.push(value);
            }
        }

        assert_eq!(
            results,
            vec![Message::Number(1), Message::Number(2), Message::Number(3)]
        );
        Ok(())
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
    async fn test_subscription_manager_basic_update() -> Result<()> {
        use crate::subscription::mock::MockSource;

        // Test basic subscription update functionality
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();
        let sub = Subscription::new(mock.clone());

        manager.update(vec![sub]);
        sleep(Duration::from_millis(10)).await;

        // Emit values
        mock.emit(10)?;
        mock.emit(20)?;

        // Should receive messages from the subscription
        let msg1 = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg1, Some(10));

        let msg2 = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg2, Some(20));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_shutdown() {
        use futures::stream;

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
    async fn test_subscription_manager_multiple_subscriptions() -> Result<()> {
        use crate::subscription::mock::MockSource;

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock1 = MockSource::new();
        let mock2 = MockSource::new();

        manager.update(vec![
            Subscription::new(mock1.clone()),
            Subscription::new(mock2.clone()),
        ]);
        sleep(Duration::from_millis(10)).await;

        // Emit from both subscriptions
        mock1.emit(1)?;
        mock2.emit(2)?;

        // Should receive messages from both subscriptions
        let mut results = vec![];
        for _ in 0..2 {
            if let Ok(Some(msg)) = timeout(Duration::from_millis(100), rx.recv()).await {
                results.push(msg);
            }
        }

        results.sort_unstable();
        assert_eq!(results, vec![1, 2]);
        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_starts_when_enabled() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();

        // Initially no subscriptions
        manager.update(Vec::<Subscription<i32>>::new());
        sleep(Duration::from_millis(10)).await;

        // Enable subscription
        manager.update(vec![Subscription::new(mock.clone())]);
        sleep(Duration::from_millis(10)).await;

        // Emit event
        mock.emit(42)?;

        // Should receive the event
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(42));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_stops_when_disabled() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();

        // Start with subscription enabled
        manager.update(vec![Subscription::new(mock.clone())]);
        sleep(Duration::from_millis(10)).await;

        // Emit event - should be received
        mock.emit(1)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(1));

        // Disable subscription
        manager.update(Vec::<Subscription<i32>>::new());
        sleep(Duration::from_millis(10)).await;

        // Emit event - should NOT be received
        let _ = mock.emit(2); // May fail if no receivers
        sleep(Duration::from_millis(10)).await;

        // Channel should be empty
        assert!(rx.try_recv().is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_changes_based_on_state() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock1 = MockSource::new();
        let mock2 = MockSource::new();

        // Start with subscription 1
        manager.update(vec![Subscription::new(mock1.clone())]);
        sleep(Duration::from_millis(10)).await;

        mock1.emit(100)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(100));

        // Switch to subscription 2
        manager.update(vec![Subscription::new(mock2.clone())]);
        sleep(Duration::from_millis(10)).await;

        // mock1 should no longer work (no receivers)
        let _ = mock1.emit(200);

        // mock2 should work
        mock2.emit(300)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(300));

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_multiple_changes() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();

        // Enable
        manager.update(vec![Subscription::new(mock.clone())]);
        sleep(Duration::from_millis(10)).await;
        mock.emit(1)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(1)
        );

        // Disable
        manager.update(Vec::<Subscription<i32>>::new());
        sleep(Duration::from_millis(10)).await;

        // Re-enable
        manager.update(vec![Subscription::new(mock.clone())]);
        sleep(Duration::from_millis(10)).await;
        mock.emit(2)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(2)
        );

        // Disable again
        manager.update(Vec::<Subscription<i32>>::new());
        sleep(Duration::from_millis(10)).await;

        // Re-enable again
        manager.update(vec![Subscription::new(mock.clone())]);
        sleep(Duration::from_millis(10)).await;
        mock.emit(3)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(3)
        );

        Ok(())
    }
}
