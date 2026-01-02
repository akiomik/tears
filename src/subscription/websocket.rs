//! WebSocket subscription for real-time communication.
//!
//! This module provides the [`WebSocket`] subscription source for establishing
//! WebSocket connections and receiving messages from WebSocket servers.
//!
//! # Feature Flag
//!
//! This module is only available when the `ws` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.4", features = ["ws"] }
//! ```
//!
//! ## TLS Support
//!
//! For secure WebSocket connections (wss://), you need to enable one of the TLS features:
//!
//! - `native-tls` - Uses the platform's native TLS implementation
//! - `rustls` - Uses rustls with ring crypto provider and native root certificates
//! - `rustls-tls-webpki-roots` - Uses rustls with ring crypto provider and webpki root certificates
//!
//! Example:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.4", features = ["ws", "native-tls"] }
//! ```

use std::hash::{DefaultHasher, Hash, Hasher};

use futures::StreamExt as _;
use futures::stream::BoxStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

use super::{SubscriptionId, SubscriptionSource};

/// A WebSocket subscription that connects to a WebSocket server and emits received messages.
///
/// This subscription establishes a WebSocket connection to the specified URL and streams
/// incoming messages. Connection errors are silently filtered out, and only successfully
/// received messages are emitted.
///
/// ## Connection Behavior
///
/// - Connection is attempted asynchronously when the subscription starts
/// - If the connection fails, no messages are emitted (the stream ends gracefully)
/// - Only incoming messages are streamed; this is a read-only subscription
/// - Message parsing errors are filtered out
///
/// ## Message Types
///
/// The subscription emits [`Message`] types from `tungstenite`, which can be:
/// - `Message::Text` - UTF-8 text messages
/// - `Message::Binary` - Binary data messages
/// - `Message::Ping` / `Message::Pong` - WebSocket control frames
/// - `Message::Close` - Connection close frames
///
/// ## Example
///
/// ```rust,no_run
/// use tears::subscription::{Subscription, websocket::WebSocket};
/// use tokio_tungstenite::tungstenite::Message;
///
/// enum AppMessage {
///     WebSocketMessage(String),
///     WebSocketBinary(Vec<u8>),
///     WebSocketControl,
/// }
///
/// // Create a WebSocket subscription
/// let ws_sub = Subscription::new(WebSocket::new("wss://example.com/socket"))
///     .map(|msg| match msg {
///         Message::Text(text) => AppMessage::WebSocketMessage(text.to_string()),
///         Message::Binary(data) => AppMessage::WebSocketBinary(data.to_vec()),
///         _ => AppMessage::WebSocketControl,
///     });
/// ```
///
/// ## Performance Considerations
///
/// - Each unique URL creates a separate WebSocket connection
/// - Connections are maintained as long as the subscription is active
/// - Consider using connection pooling for multiple subscriptions to the same server
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WebSocket {
    url: String,
}

impl WebSocket {
    /// Creates a new WebSocket subscription for the specified URL.
    ///
    /// # Arguments
    ///
    /// * `url` - The WebSocket URL to connect to (e.g., `wss://example.com/socket`)
    ///
    /// # Example
    ///
    /// ```rust
    /// use tears::subscription::websocket::WebSocket;
    ///
    /// let ws = WebSocket::new("wss://echo.websocket.org");
    /// ```
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

impl SubscriptionSource for WebSocket {
    type Output = Message;

    fn stream(&self) -> BoxStream<'static, Message> {
        let url = self.url.clone();
        futures::stream::once(async move { connect_async(&url).await.ok() })
            .filter_map(|opt| async { opt })
            .flat_map(|(ws_stream, _)| {
                let (_, read) = ws_stream.split();
                read.filter_map(|msg| async { msg.ok() })
            })
            .boxed()
    }

    fn id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        SubscriptionId::of::<Self>(hasher.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_new() {
        let ws = WebSocket::new("wss://example.com");
        assert_eq!(ws.url, "wss://example.com");
    }

    #[test]
    fn test_ws_id_consistency() {
        let ws1 = WebSocket::new("wss://example.com");
        let ws2 = WebSocket::new("wss://example.com");

        // Same configuration should produce the same ID
        assert_eq!(ws1.id(), ws2.id());
    }

    #[test]
    fn test_ws_id_different_urls() {
        let ws1 = WebSocket::new("wss://example.com");
        let ws2 = WebSocket::new("wss://different.com");

        // Different urls should produce different IDs
        assert_ne!(ws1.id(), ws2.id());
    }
}
