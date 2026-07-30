//! Mock subscription source for testing.
//!
//! This module provides controllable subscription sources that emit values on demand,
//! enabling deterministic testing without real I/O or time dependencies.
//!
//! # Basic Usage
//!
//! ```
//! use tears::Subscription;
//! use tears::subscription::mock::MockSource;
//!
//! let mock = MockSource::<i32>::new();
//! let subscription = Subscription::new(mock.clone());
//!
//! // Emit values from the mock (requires at least one receiver)
//! let _ = mock.emit(42);
//! let _ = mock.emit(100);
//! ```
//!
//! # Testing Applications
//!
//! `MockSource` is designed to be shared between your application and test code:
//!
//! ```
//! use tears::prelude::*;
//! use tears::SubscriptionSource;
//! use tears::subscription::mock::MockSource;
//! # use ratatui::Frame;
//!
//! struct MyApp {
//!     count: u32,
//!     mock: MockSource<()>,
//! }
//!
//! impl Application for MyApp {
//!     type Message = ();
//!     type Flags = MockSource<()>;
//!
//!     fn new(mock: MockSource<()>) -> (Self, Command<()>) {
//!         (Self { count: 0, mock }, Command::none())
//!     }
//!
//!     fn update(&mut self, _: ()) -> Command<()> {
//!         self.count += 1;
//!         Command::none()
//!     }
//!
//!     fn view(&self, _frame: &mut Frame<'_>) {}
//!
//!     fn subscriptions(&self) -> Vec<Subscription<()>> {
//!         // Use the mock in subscriptions
//!         vec![Subscription::new(self.mock.clone())]
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> color_eyre::Result<()> {
//! let mock = MockSource::new();
//!
//! // Pass mock to app
//! let (mut app, _) = MyApp::new(mock.clone());
//!
//! // Emit requires at least one receiver; normally the runtime subscribes
//! // via `app.subscriptions()`, but here we hold a stream directly.
//! let _stream = mock.stream();
//!
//! // Emit events from test
//! mock.emit(())?;
//!
//! // Manually call update for unit testing
//! app.update(());
//!
//! assert_eq!(app.count, 1);
//! # Ok(())
//! # }
//! ```
//!
//! # Dynamic Subscriptions
//!
//! Test subscriptions that change based on application state:
//!
//! ```
//! # use tears::Subscription;
//! # use tears::subscription::mock::MockSource;
//! struct App {
//!     enabled: bool,
//!     mock: MockSource<i32>,
//! }
//!
//! impl App {
//!     fn subscriptions(&self) -> Vec<Subscription<i32>> {
//!         if self.enabled {
//!             vec![Subscription::new(self.mock.clone())]
//!         } else {
//!             vec![]
//!         }
//!     }
//! }
//!
//! // Test can verify subscription behavior changes with state
//! let mock = MockSource::new();
//! let app = App { enabled: true, mock: mock.clone() };
//! assert_eq!(app.subscriptions().len(), 1);
//!
//! let app = App { enabled: false, mock: mock.clone() };
//! assert_eq!(app.subscriptions().len(), 0);
//! ```

use std::sync::atomic::{AtomicU64, Ordering};

use futures::StreamExt;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

use crate::BoxStream;
use crate::subscription::SubscriptionSource;

static NEXT_MOCK_SOURCE_ID: AtomicU64 = AtomicU64::new(1);

/// A mock subscription source that emits values on demand.
///
/// This is primarily intended for testing the framework itself, but can also be used
/// by users to test custom subscription implementations or application logic.
///
/// Uses a broadcast channel internally, so it can be cloned and shared between
/// the test code and application's `subscriptions()` method.
#[derive(Debug, Clone)]
pub struct MockSource<T: Clone> {
    sender: broadcast::Sender<T>,
    key: u64,
}

impl<T: Clone + 'static> MockSource<T> {
    /// Creates a new mock subscription source.
    ///
    /// # Arguments
    ///
    /// * `capacity` - Maximum number of buffered messages (defaults to 100 if using `new()`)
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero, or if the process-wide key space is
    /// exhausted (the allocator counter has reached `u64::MAX`): the key is
    /// exact identity, so keys are never reused within a process
    /// (RFC 0005 §8.3).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            sender: tx,
            // The key is exact identity and must be unique within the process
            // (RFC 0005 §8.3), so the allocator fails before it can reuse a
            // value: `checked_add` leaves the counter saturated at `u64::MAX`,
            // making this and every later allocation panic instead of wrapping
            // into reuse.
            key: NEXT_MOCK_SOURCE_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .expect(
                    "MockSource key space exhausted; keys are never reused \
                     within a process (RFC 0005 §8.3)",
                ),
        }
    }

    /// Creates a new mock subscription source with default capacity (100).
    ///
    /// # Panics
    ///
    /// Panics if the process-wide key space is exhausted (the allocator
    /// counter has reached `u64::MAX`): the key is exact identity, so keys are
    /// never reused within a process (RFC 0005 §8.3).
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(100)
    }

    /// Emits a value from the subscription.
    ///
    /// # Errors
    ///
    /// Returns an error if there are no active receivers.
    pub fn emit(&self, value: T) -> Result<usize, broadcast::error::SendError<T>> {
        self.sender.send(value)
    }

    /// Returns the number of active receivers.
    #[must_use]
    pub fn receiver_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl<T: Clone + 'static> Default for MockSource<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone + Send + 'static> SubscriptionSource for MockSource<T> {
    type Output = T;
    type Key = u64;

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let rx = self.sender.subscribe();
        Box::pin(BroadcastStream::new(rx).filter_map(|result| async move { result.ok() }))
    }

    fn key(&self) -> Self::Key {
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::subscription::Subscription;

    #[test]
    fn test_mock_source_creation() {
        let mock = MockSource::<i32>::new();
        assert_eq!(mock.receiver_count(), 0);
    }

    #[test]
    fn test_emit() {
        let mock = MockSource::<i32>::new();

        // No receivers yet
        assert!(mock.emit(42).is_err());

        // Subscribe
        let _rx = mock.sender.subscribe();
        assert_eq!(mock.receiver_count(), 1);

        // Now emit works
        assert_eq!(mock.emit(42).expect("should emit to receiver"), 1);
        assert_eq!(mock.emit(100).expect("should emit to receiver"), 1);
    }

    #[test]
    fn test_clone() {
        let mock1 = MockSource::<i32>::new();
        let mock2 = mock1.clone();

        // Same underlying channel
        let _rx = mock1.sender.subscribe();
        assert_eq!(mock2.receiver_count(), 1);
    }

    #[tokio::test]
    async fn test_stream_receives_values() {
        let mock = MockSource::<i32>::new();

        // Create subscription and get stream
        let sub = Subscription::new(mock.clone());
        let mut stream = (sub.spawn)();

        // Emit values
        mock.emit(1).expect("should emit to stream");
        mock.emit(2).expect("should emit to stream");
        mock.emit(3).expect("should emit to stream");

        // Collect values
        let mut values = Vec::new();
        for _ in 0..3 {
            if let Some(value) = stream.next().await {
                values.push(value);
            }
        }

        assert_eq!(values, vec![1, 2, 3]);
    }
}
