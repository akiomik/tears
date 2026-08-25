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

pub(crate) use core::{Subscription, SubscriptionId, SubscriptionSource};

#[cfg(test)]
mod tests {
    use super::*;

    use color_eyre::eyre::Result;
    use futures::{StreamExt, stream, stream::BoxStream};

    use crate::subscription::mock::MockSource;

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
}
