//! Mock subscription source for testing.
//!
//! This module provides controllable subscription sources that emit values on demand,
//! enabling deterministic testing without real I/O or time dependencies.
//!
//! # Basic Usage
//!
//! ```
//! use tears::subscription::{Subscription, mock::MockSource};
//!
//! let mock = MockSource::<i32>::new();
//! let subscription = Subscription::new(mock.clone());
//!
//! // Emit values from the mock (requires at least one receiver)
//! # #[cfg(not(test))] // Skip in doctest
//! let _ = mock.emit(42);
//! # #[cfg(not(test))]
//! let _ = mock.emit(100);
//! ```
//!
//! # Testing Applications
//!
//! `MockSource` is designed to be shared between your application and test code:
//!
//! ```no_run
//! use tears::prelude::*;
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
//! #[tokio::test]
//! async fn test_my_app() -> color_eyre::Result<()> {
//!     let mock = MockSource::new();
//!
//!     // Pass mock to app
//!     let (mut app, _) = MyApp::new(mock.clone());
//!
//!     // Emit events from test
//!     mock.emit(())?;
//!
//!     // Manually call update for unit testing
//!     app.update(());
//!
//!     assert_eq!(app.count, 1);
//!     Ok(())
//! }
//! ```
//!
//! # Dynamic Subscriptions
//!
//! Test subscriptions that change based on application state:
//!
//! ```
//! # use tears::subscription::{Subscription, mock::MockSource};
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

use crate::BoxStream;
use crate::subscription::{SubscriptionId, SubscriptionSource};
use futures::StreamExt;
use tokio::sync::broadcast;

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
    id: SubscriptionId,
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
    /// Panics if the system time is before [`std::time::UNIX_EPOCH`] (should never happen on modern systems).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        // Use a random ID for each instance
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        // Use current timestamp + type name for uniqueness
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("System time before UNIX_EPOCH")
            .as_nanos()
            .hash(&mut hasher);
        std::any::type_name::<T>().hash(&mut hasher);

        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            sender: tx,
            id: SubscriptionId::of::<Self>(hasher.finish()),
        }
    }

    /// Creates a new mock subscription source with default capacity (100).
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

    fn stream(&self) -> BoxStream<'static, Self::Output> {
        let rx = self.sender.subscribe();
        Box::pin(
            tokio_stream::wrappers::BroadcastStream::new(rx)
                .filter_map(|result| async move { result.ok() }),
        )
    }

    fn id(&self) -> SubscriptionId {
        self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::Subscription;
    use futures::StreamExt;

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
