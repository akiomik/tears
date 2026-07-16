//! Subscriptions for handling ongoing event sources.
//!
//! Subscriptions represent streams of events like terminal input, timers,
//! or WebSocket connections.
//!
//! # Overview
//!
//! tears supports three patterns for different communication needs:
//! - **Unidirectional**: Events only (timers, signals, input)
//! - **Stream-based**: Bidirectional real-time communication (WebSocket, gRPC)
//! - **Transaction-based**: Discrete operations (HTTP, database, files)
//!
//! # Examples
//!
//! ## Unidirectional (Timer)
//!
//! ```rust
//! use std::num::NonZeroU64;
//! use tears::Subscription;
//! use tears::subscription::time::Timer;
//!
//! # enum Message { Tick }
//! let timer = Subscription::new(Timer::new(NonZeroU64::new(1000).expect("non-zero")))
//!     .map(|_| Message::Tick);
//! ```
//!
//! ## Stream-based (WebSocket)
//!
//! ```rust,ignore
//! use tears::subscription::websocket::{WebSocket, WebSocketMessage, WebSocketCommand};
//! use tokio::sync::mpsc;
//!
//! struct App {
//!     ws_sender: Option<mpsc::UnboundedSender<WebSocketCommand>>,
//! }
//!
//! // Store sender on connection, use it to send messages immediately
//! ```
//!
//! ## Transaction-based (HTTP)
//!
//! ```rust,ignore
//! use tears::subscription::http::Query;
//!
//! // In update():
//! Query::new(client).fetch(id, fetch_fn, Message::UserLoaded)
//! ```
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
//! Implement the [`SubscriptionSource`] trait to
//! create your own subscription types:
//!
//! ```
//! use tears::{Subscription, SubscriptionSource};
//! use tears::BoxStream;
//! use futures::{StreamExt, stream};
//!
//! struct MySubscription {
//!     id: u64,
//! }
//!
//! impl SubscriptionSource for MySubscription {
//!     type Output = String;
//!     type Key = u64;
//!
//!     fn stream(&self) -> BoxStream<'static, Self::Output> {
//!         stream::once(async { "Hello".to_string() }).boxed()
//!     }
//!
//!     fn key(&self) -> Self::Key {
//!         self.id
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
//! ```
//! use tears::SubscriptionSource;
//! use tears::subscription::mock::MockSource;
//! use futures::StreamExt;
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! // Create a controllable mock
//! let mock = MockSource::<i32>::new();
//!
//! // Call the SubscriptionSource trait method directly to get a stream
//! let mut stream = mock.stream();
//!
//! // Control events from your test
//! mock.emit(42).expect("should emit");
//!
//! // Receive the value
//! let value = stream.next().await;
//! assert_eq!(value, Some(42));
//! # }
//! ```
//!
//! See the [`mock`] module documentation for complete testing examples.

pub(crate) mod core;
#[cfg(any(feature = "http", feature = "loom-core"))]
pub mod http;
pub mod mock;
#[cfg(not(all(feature = "loom-core", test)))]
pub mod signal;
pub mod terminal;
pub mod time;
#[cfg(feature = "ws")]
pub mod websocket;

#[cfg(test)]
use std::any::TypeId;
use std::{
    collections::{HashMap, HashSet},
    panic::AssertUnwindSafe,
};

use futures::{FutureExt, StreamExt, stream::BoxStream};
use tokio::{
    sync::mpsc::{self},
    task::JoinHandle,
};

pub(crate) use core::{Subscription, SubscriptionId, SubscriptionSource};

struct RunningSubscription {
    handle: JoinHandle<()>,
}

/// Manages the lifecycle of active subscriptions.
///
/// Internal to the runtime; not part of the public API.
pub(crate) struct SubscriptionManager<Msg> {
    running: HashMap<SubscriptionId, RunningSubscription>,
    msg_sender: mpsc::UnboundedSender<Msg>,
}

impl<Msg: Send + 'static> SubscriptionManager<Msg> {
    /// Create a new subscription manager.
    #[must_use]
    pub(crate) fn new(msg_sender: mpsc::UnboundedSender<Msg>) -> Self {
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
    /// - Subscriptions whose tasks have finished will be restarted if still present
    /// - Subscriptions with the same ID and a running task will continue unchanged
    ///
    /// # Arguments
    ///
    /// * `subscriptions` - The new set of subscriptions to run
    pub(crate) fn update<I>(&mut self, subscriptions: I)
    where
        I: IntoIterator<Item = Subscription<Msg>>,
    {
        // NOTE: Store stream spawners instead of streams to avoid creating
        // streams unnecessarily. This is important for subscriptions like
        // TerminalEvents where creating the stream has side effects.
        let mut new_subs = Vec::new();
        let mut new_ids = HashSet::new();

        for Subscription { id, spawn } in subscriptions {
            // Keep the first subscription for a given ID. Subscriptions with
            // the same ID are considered identical, and preserving the first
            // one avoids making duplicate IDs silently "last one wins".
            if new_ids.insert(id.clone()) {
                new_subs.push((id, spawn));
            } else {
                tracing::warn!(target: "tears::subscription", subscription_id = ?id, "duplicate subscription ignored");
            }
        }

        // Discard entries whose tasks have already finished so they are treated
        // as absent: restarted if still requested, silently dropped otherwise.
        let mut finished_ids = HashSet::new();
        self.running.retain(|id, rs| {
            let finished = rs.handle.is_finished();
            if finished {
                finished_ids.insert(id.clone());
            }
            !finished
        });

        self.running.retain(|id, running| {
            let keep = new_ids.contains(id);
            if !keep {
                running.handle.abort();
                tracing::debug!(target: "tears::subscription", "subscription stopped");
            }
            keep
        });

        for (id, spawn) in new_subs {
            if !self.running.contains_key(&id) {
                let restarted = finished_ids.contains(&id);
                // Only call the spawner when we actually need to start the subscription
                let stream = spawn();
                let handle = self.spawn_subscription(stream);
                self.running.insert(id, RunningSubscription { handle });
                tracing::debug!(target: "tears::subscription", restarted, "subscription started");
            }
        }
    }

    fn spawn_subscription(&self, mut stream: BoxStream<'static, Msg>) -> JoinHandle<()> {
        let sender = self.msg_sender.clone();

        tokio::spawn(async move {
            // Catch panics in the subscription's stream so a bug in a source is
            // logged instead of vanishing into a detached task.
            let result = AssertUnwindSafe(async move {
                while let Some(msg) = stream.next().await {
                    if sender.send(msg).is_err() {
                        break;
                    }
                }
            })
            .catch_unwind()
            .await;

            if result.is_err() {
                tracing::error!(target: "tears::subscription", "subscription task panicked");
            }
        })
    }

    /// Shut down all active subscriptions.
    ///
    /// This cancels all running subscription tasks. Called automatically
    /// when the runtime shuts down.
    pub(crate) fn shutdown(&mut self) {
        self.abort_running();
        self.running.clear();
    }
}

impl<Msg> SubscriptionManager<Msg> {
    /// Abort every running subscription task without removing the map entries.
    ///
    /// Shared by [`shutdown`](SubscriptionManager::shutdown) and [`Drop`], so a
    /// manager that is dropped without a clean shutdown (e.g. during a panic
    /// unwind) still cancels its tasks instead of detaching them.
    fn abort_running(&self) {
        for running in self.running.values() {
            running.handle.abort();
        }
    }
}

impl<Msg> Drop for SubscriptionManager<Msg> {
    fn drop(&mut self) {
        // `JoinHandle` does not abort on drop (it detaches), so a manager that
        // is dropped without `shutdown()` — for instance while unwinding from a
        // panic — would otherwise leak its subscription tasks.
        self.abort_running();
    }
}

/// Bench-only wrapper exposing [`SubscriptionManager`]'s reconciliation hot
/// path to `benches/subscription.rs`.
///
/// `benches/subscription.rs` compiles as a separate crate that only sees the
/// public API, so it cannot name `SubscriptionManager` directly once that
/// type is crate-private. This wrapper delegates to the real implementation
/// and exists solely to keep that benchmark isolating
/// `SubscriptionManager::update`'s cost from the rest of the runtime. Gated
/// behind the `bench-internals` feature, which is not part of the public API
/// and carries no semver guarantees; do not enable it for normal builds.
#[cfg(feature = "bench-internals")]
#[doc(hidden)]
pub struct BenchSubscriptionManager<Msg>(SubscriptionManager<Msg>);

#[cfg(feature = "bench-internals")]
#[doc(hidden)]
impl<Msg: Send + 'static> BenchSubscriptionManager<Msg> {
    #[must_use]
    pub fn new(msg_sender: mpsc::UnboundedSender<Msg>) -> Self {
        Self(SubscriptionManager::new(msg_sender))
    }

    pub fn update<I>(&mut self, subscriptions: I)
    where
        I: IntoIterator<Item = Subscription<Msg>>,
    {
        self.0.update(subscriptions);
    }

    pub fn shutdown(&mut self) {
        self.0.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::hash::{Hash, Hasher};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Poll;

    use color_eyre::eyre::Result;
    use futures::stream;
    use tokio::time::{Duration, sleep, timeout};

    use crate::subscription::mock::MockSource;
    use crate::test_support::{TraceRecorder, wait_until};

    struct OneshotSource {
        value: i32,
    }

    impl SubscriptionSource for OneshotSource {
        type Output = i32;
        type Key = ();

        fn stream(&self) -> BoxStream<'static, i32> {
            let v = self.value;
            stream::once(async move { v }).boxed()
        }

        fn key(&self) -> Self::Key {}
    }

    async fn assert_completed_oneshot_subscription_restarts() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        // Start the first one-shot subscription.
        manager.update(vec![Subscription::new(OneshotSource { value: 1 })]);
        let first = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(first, Some(1));

        wait_until(
            || {
                manager
                    .running
                    .values()
                    .all(|running| running.handle.is_finished())
            },
            "one-shot subscription should finish before restart",
        )
        .await;

        // Update with the same subscription ID. Because the previous task
        // finished, it must be restarted rather than silently skipped.
        manager.update(vec![Subscription::new(OneshotSource { value: 2 })]);
        let second = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(second, Some(2), "finished subscription should be restarted");

        Ok(())
    }

    async fn wait_for_mock_receivers<T: Clone + 'static>(mock: &MockSource<T>, expected: usize) {
        wait_until(
            || mock.receiver_count() == expected,
            "mock receiver count should reach the expected value",
        )
        .await;
    }

    #[test]
    fn test_subscription_new() {
        let mock = MockSource::<i32>::new();
        let sub = Subscription::new(mock);

        // Should have correct ID type
        assert_eq!(sub.id.source_type_id, TypeId::of::<MockSource<i32>>());
    }

    #[tokio::test]
    async fn test_subscription_map() -> Result<()> {
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
    fn test_subscription_id_is_structural() {
        struct Source(u64);
        impl SubscriptionSource for Source {
            type Output = ();
            type Key = u64;
            fn stream(&self) -> BoxStream<'static, ()> {
                stream::empty().boxed()
            }
            fn key(&self) -> Self::Key {
                self.0
            }
        }
        let id1 = Subscription::new(Source(12345)).id;
        let id2 = Subscription::new(Source(12345)).id;
        let id3 = Subscription::new(Source(67890)).id;

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_subscription_id_different_source_types() {
        struct I32Source;
        struct U64Source;
        impl SubscriptionSource for I32Source {
            type Output = ();
            type Key = u64;
            fn stream(&self) -> BoxStream<'static, ()> {
                stream::empty().boxed()
            }
            fn key(&self) -> Self::Key {
                12345
            }
        }
        impl SubscriptionSource for U64Source {
            type Output = ();
            type Key = u64;
            fn stream(&self) -> BoxStream<'static, ()> {
                stream::empty().boxed()
            }
            fn key(&self) -> Self::Key {
                12345
            }
        }
        let id_i32 = Subscription::new(I32Source).id;
        let id_u64 = Subscription::new(U64Source).id;
        let id_string = Subscription::new(MockSource::<String>::new()).id;

        assert_ne!(id_i32, id_u64);
        assert_ne!(id_i32, id_string);
        assert_ne!(id_u64, id_string);
    }

    #[tokio::test]
    async fn test_subscription_manager_basic_update() -> Result<()> {
        // Test basic subscription update functionality
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();
        let sub = Subscription::new(mock.clone());

        manager.update(vec![sub]);
        wait_for_mock_receivers(&mock, 1).await;

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
        // Create a long-running subscription
        struct InfiniteSub;
        impl SubscriptionSource for InfiniteSub {
            type Output = i32;
            type Key = ();

            fn stream(&self) -> BoxStream<'static, Self::Output> {
                stream::unfold(0, |state| async move {
                    sleep(Duration::from_millis(10)).await;
                    Some((state, state + 1))
                })
                .boxed()
            }

            fn key(&self) -> Self::Key {}
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let sub = Subscription::new(InfiniteSub);
        manager.update(vec![sub]);

        // Receive a few messages
        let _ = timeout(Duration::from_millis(100), rx.recv()).await;

        // Shutdown should cancel all subscriptions
        manager.shutdown();

        assert_eq!(manager.running.len(), 0);
    }

    #[tokio::test]
    async fn test_drop_aborts_running_subscriptions() {
        // A guard that records, via its `Drop`, that the task's future was
        // dropped. The runtime only drops an aborted task's future, so the flag
        // flipping to `true` proves the task was cancelled rather than detached.
        struct AbortGuard(Arc<AtomicBool>);
        impl Drop for AbortGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        // A subscription whose stream owns the guard and never yields, so its
        // task stays parked until it is aborted.
        struct ParkedSource {
            started: Arc<AtomicBool>,
            aborted: Arc<AtomicBool>,
        }
        impl SubscriptionSource for ParkedSource {
            type Output = i32;
            type Key = ();

            fn stream(&self) -> BoxStream<'static, i32> {
                let started = self.started.clone();
                let guard = AbortGuard(self.aborted.clone());
                stream::poll_fn(move |_cx| {
                    started.store(true, Ordering::SeqCst);
                    let _keep_alive = &guard;
                    Poll::Pending
                })
                .boxed()
            }

            fn key(&self) -> Self::Key {}
        }

        let started = Arc::new(AtomicBool::new(false));
        let aborted = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::unbounded_channel();

        {
            let mut manager = SubscriptionManager::new(tx);
            manager.update(vec![Subscription::new(ParkedSource {
                started: started.clone(),
                aborted: aborted.clone(),
            })]);

            wait_until(
                || started.load(Ordering::SeqCst),
                "subscription task should start before the manager is dropped",
            )
            .await;
            assert!(
                !aborted.load(Ordering::SeqCst),
                "task should still be running before the manager is dropped"
            );
            // `manager` is dropped here.
        }

        wait_until(
            || aborted.load(Ordering::SeqCst),
            "dropping the manager should abort running subscription tasks",
        )
        .await;
    }

    #[tokio::test]
    async fn test_subscription_manager_multiple_subscriptions() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock1 = MockSource::new();
        let mock2 = MockSource::new();

        manager.update(vec![
            Subscription::new(mock1.clone()),
            Subscription::new(mock2.clone()),
        ]);
        wait_for_mock_receivers(&mock1, 1).await;
        wait_for_mock_receivers(&mock2, 1).await;

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
    async fn duplicate_subscriptions_keep_the_first_and_warn() {
        let recorder = TraceRecorder::new()
            .with_target("tears::subscription")
            .with_level(tracing::Level::WARN);
        let _guard = recorder.set_default();

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);
        let first = MockSource::<i32>::new();
        let duplicate = first.clone();

        manager.update(vec![
            Subscription::new(first.clone()),
            Subscription::new(duplicate.clone()),
        ]);

        wait_for_mock_receivers(&first, 1).await;
        assert_eq!(
            duplicate.receiver_count(),
            1,
            "only the first spawner should run"
        );
        assert_eq!(
            recorder.event_count(),
            1,
            "the ignored duplicate should warn"
        );
    }

    #[tokio::test]
    async fn hash_colliding_subscriptions_start_and_map_independently() -> Result<()> {
        #[derive(Eq, PartialEq)]
        struct CollisionKey(u8);

        impl Hash for CollisionKey {
            fn hash<H: Hasher>(&self, state: &mut H) {
                0_u8.hash(state);
            }
        }

        struct CollidingSource {
            key: CollisionKey,
            value: u8,
        }

        impl SubscriptionSource for CollidingSource {
            type Output = u8;
            type Key = CollisionKey;

            fn stream(&self) -> BoxStream<'static, Self::Output> {
                let value = self.value;
                stream::once(async move { value }).boxed()
            }

            fn key(&self) -> Self::Key {
                CollisionKey(self.key.0)
            }
        }

        #[derive(Debug, Eq, PartialEq)]
        enum Message {
            First(u8),
            Second(u8),
        }

        let recorder = TraceRecorder::new()
            .with_target("tears::subscription")
            .with_level(tracing::Level::WARN);
        let _guard = recorder.set_default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        manager.update(vec![
            Subscription::new(CollidingSource {
                key: CollisionKey(1),
                value: 10,
            })
            .map(Message::First),
            Subscription::new(CollidingSource {
                key: CollisionKey(2),
                value: 20,
            })
            .map(Message::Second),
        ]);

        let first = timeout(Duration::from_millis(100), rx.recv()).await?;
        let second = timeout(Duration::from_millis(100), rx.recv()).await?;
        let messages = [first, second];

        assert!(messages.contains(&Some(Message::First(10))));
        assert!(messages.contains(&Some(Message::Second(20))));
        assert_eq!(
            recorder.event_count(),
            0,
            "hash collisions are not duplicates"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_starts_new_subscriptions_in_input_order() {
        struct OrderedStart {
            id: u64,
            started: Arc<Mutex<Vec<u64>>>,
        }
        impl SubscriptionSource for OrderedStart {
            type Output = ();
            type Key = u64;
            fn stream(&self) -> BoxStream<'static, ()> {
                self.started
                    .lock()
                    .expect("started order mutex should not be poisoned")
                    .push(self.id);
                stream::empty().boxed()
            }
            fn key(&self) -> Self::Key {
                self.id
            }
        }

        fn recording_subscription(id: u64, started: Arc<Mutex<Vec<u64>>>) -> Subscription<()> {
            Subscription::new(OrderedStart { id, started })
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);
        let started = Arc::new(Mutex::new(Vec::new()));

        manager.update(vec![
            recording_subscription(1, started.clone()),
            recording_subscription(2, started.clone()),
            recording_subscription(3, started.clone()),
            recording_subscription(4, started.clone()),
        ]);

        assert_eq!(
            *started
                .lock()
                .expect("started order mutex should not be poisoned"),
            vec![1, 2, 3, 4]
        );
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_starts_when_enabled() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();

        // Initially no subscriptions
        manager.update(Vec::<Subscription<i32>>::new());

        // Enable subscription
        manager.update(vec![Subscription::new(mock.clone())]);
        wait_for_mock_receivers(&mock, 1).await;

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
        wait_for_mock_receivers(&mock, 1).await;

        // Emit event - should be received
        mock.emit(1)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(1));

        // Disable subscription
        manager.update(Vec::<Subscription<i32>>::new());
        wait_for_mock_receivers(&mock, 0).await;

        // Emit event - should NOT be received
        assert!(mock.emit(2).is_err());

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
        wait_for_mock_receivers(&mock1, 1).await;

        mock1.emit(100)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(100));

        // Switch to subscription 2
        manager.update(vec![Subscription::new(mock2.clone())]);
        wait_for_mock_receivers(&mock1, 0).await;
        wait_for_mock_receivers(&mock2, 1).await;

        // mock1 should no longer work (no receivers)
        let _ = mock1.emit(200);

        // mock2 should work
        mock2.emit(300)?;
        let msg = timeout(Duration::from_millis(100), rx.recv()).await?;
        assert_eq!(msg, Some(300));

        Ok(())
    }

    #[tokio::test]
    async fn test_completed_subscription_is_restarted() -> Result<()> {
        assert_completed_oneshot_subscription_restarts().await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_subscription_started_tracing_marks_restarts() -> Result<()> {
        let recorder = TraceRecorder::new()
            .with_target("tears::subscription")
            .with_level(tracing::Level::DEBUG);
        let _guard = recorder.set_default();

        assert_completed_oneshot_subscription_restarts().await?;

        assert_eq!(
            recorder.bool_values("restarted"),
            vec![false, true],
            "subscription start tracing should distinguish initial starts from restarts"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_completed_subscription_cleaned_up_when_not_requested() -> Result<()> {
        struct OneshotSource;

        impl SubscriptionSource for OneshotSource {
            type Output = i32;
            type Key = ();

            fn stream(&self) -> BoxStream<'static, i32> {
                stream::once(async move { 42 }).boxed()
            }

            fn key(&self) -> Self::Key {}
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        // Start and let the task finish.
        manager.update(vec![Subscription::new(OneshotSource)]);
        let _ = timeout(Duration::from_millis(100), rx.recv()).await?;
        wait_until(
            || {
                manager
                    .running
                    .values()
                    .all(|running| running.handle.is_finished())
            },
            "one-shot subscription should finish before cleanup",
        )
        .await;

        // Update without any subscriptions — the stale map entry must be removed.
        manager.update(Vec::<Subscription<i32>>::new());

        assert_eq!(
            manager.running.len(),
            0,
            "dead entry for a no-longer-requested subscription should be removed"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_subscription_manager_subscription_multiple_changes() -> Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut manager = SubscriptionManager::new(tx);

        let mock = MockSource::new();

        // Enable
        manager.update(vec![Subscription::new(mock.clone())]);
        wait_for_mock_receivers(&mock, 1).await;
        mock.emit(1)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(1)
        );

        // Disable
        manager.update(Vec::<Subscription<i32>>::new());
        wait_for_mock_receivers(&mock, 0).await;

        // Re-enable
        manager.update(vec![Subscription::new(mock.clone())]);
        wait_for_mock_receivers(&mock, 1).await;
        mock.emit(2)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(2)
        );

        // Disable again
        manager.update(Vec::<Subscription<i32>>::new());
        wait_for_mock_receivers(&mock, 0).await;

        // Re-enable again
        manager.update(vec![Subscription::new(mock.clone())]);
        wait_for_mock_receivers(&mock, 1).await;
        mock.emit(3)?;
        assert_eq!(
            timeout(Duration::from_millis(100), rx.recv()).await?,
            Some(3)
        );

        Ok(())
    }
}
