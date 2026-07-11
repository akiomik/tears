//! WebSocket subscription for real-time bidirectional communication.
//!
//! This module provides the [`WebSocket`] subscription source for establishing
//! WebSocket connections, receiving messages from WebSocket servers, and sending
//! messages to them.
//!
//! # Design Pattern: Stream-based Bidirectional Communication
//!
//! WebSocket uses the **stream-based bidirectional** pattern. The subscription
//! manages a long-lived connection and provides an `mpsc::UnboundedSender` for
//! immediate send operations. This design reflects the real-time, streaming
//! nature of WebSocket communication.
//!
//! For more details on why this pattern is used instead of `Command`-based
//! sending, see the "Design Philosophy" section in the [`subscription`](crate::subscription)
//! module documentation.
//!
//! # Disconnection and reconnection
//!
//! The subscription's stream ends when the connection closes (server close,
//! transport error, etc.). While the application keeps returning this
//! subscription from [`Application::subscriptions`](crate::Application::subscriptions),
//! the runtime restarts the finished stream on the next re-evaluation — i.e. it
//! reconnects automatically after the next message. To stop reconnecting, drop
//! the subscription from the returned set once you observe
//! [`WebSocketMessage::Disconnected`]. See the "Restart of finished
//! subscriptions" note on [`Application::subscriptions`](crate::Application::subscriptions).
//!
//! # Feature Flag
//!
//! This module is only available when the `ws` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.9", features = ["ws"] }
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
//! tears = { version = "0.9", features = ["ws", "native-tls"] }
//! ```

use std::hash::{DefaultHasher, Hash, Hasher};

use futures::stream::{BoxStream, SplitSink, SplitStream};
use futures::{SinkExt as _, StreamExt as _, stream};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use super::{SubscriptionId, SubscriptionSource};

/// Commands that can be sent to the WebSocket connection.
#[derive(Debug, Clone)]
pub enum WebSocketCommand {
    /// Send a text message
    SendText(String),
    /// Send a binary message
    SendBinary(Vec<u8>),
    /// Close the WebSocket connection with an optional close frame
    Close(Option<CloseFrame>),
}

/// Messages emitted by the WebSocket subscription.
#[derive(Debug, Clone)]
pub enum WebSocketMessage {
    /// Successfully connected to the WebSocket server, provides command sender
    Connected {
        /// Sends commands (text/binary messages, close requests) to the connection.
        sender: mpsc::UnboundedSender<WebSocketCommand>,
    },
    /// Disconnected from the WebSocket server (normal closure)
    Disconnected,
    /// A message received from the WebSocket server
    Received(Message),
    /// An error occurred (connection failure or communication error)
    Error {
        /// A description of the connection failure or communication error.
        error: String,
    },
}

/// A WebSocket subscription that connects to a WebSocket server and provides
/// bidirectional communication.
///
/// This subscription establishes a WebSocket connection to the specified URL and streams
/// incoming messages. It also provides a command sender through the [`WebSocketMessage::Connected`]
/// message, which allows the application to send messages back to the server.
///
/// ## Connection Behavior
///
/// - Connection is attempted asynchronously when the subscription starts
/// - If the connection fails, an error message is emitted
/// - Once connected, a `Connected` message is emitted with a command sender
/// - Both incoming and outgoing messages are handled in the same connection
///
/// ## Message Flow
///
/// 1. Subscription starts → Connection attempt begins
/// 2. On success → `WebSocketMessage::Connected` is emitted with a command sender
/// 3. Application stores the `sender` and can now send messages
/// 4. Incoming messages are emitted as `WebSocketMessage::Received`
/// 5. On normal disconnection → `WebSocketMessage::Disconnected` is emitted
/// 6. On connection failure or communication error → `WebSocketMessage::Error` is emitted
///
/// ## Disconnection Handling
///
/// The subscription distinguishes between normal disconnection and errors:
/// - `Disconnected`: Server closed connection gracefully, user requested close, or connection ended normally
/// - `Error`: Connection failed, network error, or protocol violation
///
/// ## Example
///
/// ```rust,no_run
/// use tears::Subscription;
/// use tears::subscription::websocket::{WebSocket, WebSocketMessage, WebSocketCommand};
/// use tokio_tungstenite::tungstenite::Message;
/// use tokio::sync::mpsc;
///
/// enum AppMessage {
///     WebSocketConnected(mpsc::UnboundedSender<WebSocketCommand>),
///     WebSocketDisconnected,
///     WebSocketReceived(String),
///     WebSocketError(String),
/// }
///
/// struct App {
///     ws_sender: Option<mpsc::UnboundedSender<WebSocketCommand>>,
/// }
///
/// impl App {
///     fn update(&mut self, msg: AppMessage) {
///         match msg {
///             AppMessage::WebSocketConnected(sender) => {
///                 self.ws_sender = Some(sender);
///                 // Successfully connected, can now send messages
///             }
///             AppMessage::WebSocketDisconnected => {
///                 self.ws_sender = None;
///                 // Connection closed normally
///             }
///             AppMessage::WebSocketReceived(text) => {
///                 // Handle received message
///             }
///             AppMessage::WebSocketError(error) => {
///                 // Handle connection failure or communication error
///             }
///         }
///     }
///
///     fn send_message(&self, text: String) {
///         if let Some(sender) = &self.ws_sender {
///             let _ = sender.send(WebSocketCommand::SendText(text));
///         }
///     }
/// }
///
/// // Create a WebSocket subscription
/// let ws_sub = Subscription::new(WebSocket::new("wss://example.com/socket"))
///     .map(|msg| match msg {
///         WebSocketMessage::Connected { sender } => AppMessage::WebSocketConnected(sender),
///         WebSocketMessage::Disconnected => AppMessage::WebSocketDisconnected,
///         WebSocketMessage::Received(Message::Text(text)) => AppMessage::WebSocketReceived(text.to_string()),
///         WebSocketMessage::Error { error } => AppMessage::WebSocketError(error),
///         _ => AppMessage::WebSocketError("Unexpected message".to_string()),
///     });
/// ```
///
/// ## Performance Considerations
///
/// - Each unique URL creates a separate WebSocket connection
/// - Connections are maintained as long as the subscription is active
/// - The command sender can be cloned and shared across different parts of the application
/// - The WebSocket read loop runs inline inside the `stream::unfold` future, so the server
///   is only read from when the consumer polls `stream.next()`.  For typical TUI applications
///   (low message rate, fast `update()`) this is transparent.  If a server sends messages at
///   a very high rate and the consumer is slower than the arrival rate, TCP receive-window
///   pressure will propagate back to the server.  Applications that need to decouple
///   consumption speed from network read speed should add their own buffering layer.
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

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsRead = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

enum WsStreamState {
    Connecting {
        url: String,
        cmd_tx: mpsc::UnboundedSender<WebSocketCommand>,
        cmd_rx: mpsc::UnboundedReceiver<WebSocketCommand>,
    },
    Running {
        write: WsSink,
        read: WsRead,
        cmd_rx: mpsc::UnboundedReceiver<WebSocketCommand>,
    },
    Done,
}

impl SubscriptionSource for WebSocket {
    type Output = WebSocketMessage;

    #[allow(clippy::too_many_lines)]
    fn stream(&self) -> BoxStream<'static, WebSocketMessage> {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        stream::unfold(
            WsStreamState::Connecting {
                url: self.url.clone(),
                cmd_tx,
                cmd_rx,
            },
            |state| async move {
                match state {
                    WsStreamState::Connecting { url, cmd_tx, cmd_rx } => {
                        let trace_url = trace_url(&url);
                        tracing::debug!(
                            target: "tears::subscription::websocket",
                            url = %trace_url,
                            "websocket connecting"
                        );
                        match connect_async(&url).await {
                            Ok((ws, _)) => {
                                tracing::debug!(
                                    target: "tears::subscription::websocket",
                                    url = %trace_url,
                                    "websocket connected"
                                );
                                let (write, read) = ws.split();
                                Some((
                                    WebSocketMessage::Connected { sender: cmd_tx },
                                    WsStreamState::Running { write, read, cmd_rx },
                                ))
                            }
                            Err(e) => {
                                tracing::debug!(
                                    target: "tears::subscription::websocket",
                                    url = %trace_url,
                                    error = %e,
                                    "websocket connection failed"
                                );
                                Some((
                                    WebSocketMessage::Error {
                                        error: format!("Connection failed: {e}"),
                                    },
                                    WsStreamState::Done,
                                ))
                            }
                        }
                    }
                    // FIXME(#103): when the outer task is aborted (e.g. via
                    // SubscriptionManager::shutdown() calling handle.abort()),
                    // this future is dropped at the select! await point and
                    // WsStreamState::Running is dropped synchronously.  The TCP
                    // connection closes via the OS on TcpStream drop, but no
                    // WebSocket-level Message::Close frame is sent.  Rust has no
                    // async Drop, so close-frame delivery on abort requires an
                    // explicit cooperative shutdown signal (e.g. a
                    // CancellationToken checked in the select! below) combined
                    // with SubscriptionManager::shutdown() providing a grace
                    // period before calling handle.abort().
                    //
                    // This is a limitation, not a bug: the OS still closes the
                    // TCP connection, so the server detects the disconnect.  A
                    // clean close is achievable today by having the app send
                    // WebSocketCommand::Close before quitting; a framework-level
                    // guarantee would need the cooperative shutdown above.
                    WsStreamState::Running {
                        mut write,
                        mut read,
                        mut cmd_rx,
                    } => loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(Message::Close(_))) => {
                                        tracing::debug!(target: "tears::subscription::websocket", reason = "server_close", "websocket disconnected");
                                        let _ = write.close().await;
                                        break Some((WebSocketMessage::Disconnected, WsStreamState::Done));
                                    }
                                    Some(Ok(message)) => {
                                        // NOTE: on a Ping, tungstenite auto-queues
                                        // a Pong but does not flush it within this
                                        // read(); the queued Pong is written on the
                                        // next read()/flush().  Because this task
                                        // re-polls immediately after yielding, the
                                        // Pong is normally prompt.  It can lag only
                                        // if the task is aborted or the executor
                                        // stalls before the next poll.  Flushing
                                        // here would close that narrow window but
                                        // would couple read progress to write
                                        // backpressure on every Ping, so we don't.
                                        tracing::trace!(
                                            target: "tears::subscription::websocket",
                                            message_type = trace_message_type(&message),
                                            message_size = trace_message_size(&message),
                                            "websocket message received"
                                        );
                                        break Some((
                                            WebSocketMessage::Received(message),
                                            WsStreamState::Running { write, read, cmd_rx },
                                        ));
                                    }
                                    Some(Err(e)) => {
                                        tracing::debug!(
                                            target: "tears::subscription::websocket",
                                            error = %e,
                                            "websocket read failed"
                                        );
                                        let _ = write.close().await;
                                        break Some((
                                            WebSocketMessage::Error { error: e.to_string() },
                                            WsStreamState::Done,
                                        ));
                                    }
                                    None => {
                                        tracing::debug!(target: "tears::subscription::websocket", reason = "read_stream_ended", "websocket disconnected");
                                        break Some((WebSocketMessage::Disconnected, WsStreamState::Done));
                                    }
                                }
                            }
                            cmd = cmd_rx.recv() => {
                                match cmd {
                                    Some(WebSocketCommand::Close(frame)) => {
                                        tracing::debug!(target: "tears::subscription::websocket", reason = "close_command", "websocket disconnecting");
                                        let _ = write.send(Message::Close(frame)).await;
                                        let _ = write.close().await;
                                        break Some((WebSocketMessage::Disconnected, WsStreamState::Done));
                                    }
                                    Some(WebSocketCommand::SendText(text)) => {
                                        tracing::trace!(
                                            target: "tears::subscription::websocket",
                                            message_type = "text",
                                            message_size = text.len(),
                                            "websocket message sending"
                                        );
                                        if let Err(e) = write.send(Message::Text(text.into())).await {
                                            tracing::debug!(
                                                target: "tears::subscription::websocket",
                                                error = %e,
                                                message_type = "text",
                                                "websocket write failed"
                                            );
                                            // Report the error but stay Running: the read half may
                                            // still deliver buffered frames (e.g. a server Close).
                                            break Some((
                                                WebSocketMessage::Error { error: e.to_string() },
                                                WsStreamState::Running { write, read, cmd_rx },
                                            ));
                                        }
                                    }
                                    Some(WebSocketCommand::SendBinary(data)) => {
                                        tracing::trace!(
                                            target: "tears::subscription::websocket",
                                            message_type = "binary",
                                            message_size = data.len(),
                                            "websocket message sending"
                                        );
                                        if let Err(e) = write.send(Message::Binary(data.into())).await {
                                            tracing::debug!(
                                                target: "tears::subscription::websocket",
                                                error = %e,
                                                message_type = "binary",
                                                "websocket write failed"
                                            );
                                            break Some((
                                                WebSocketMessage::Error { error: e.to_string() },
                                                WsStreamState::Running { write, read, cmd_rx },
                                            ));
                                        }
                                    }
                                    None => {
                                        // Application dropped the command sender.
                                        tracing::debug!(target: "tears::subscription::websocket", reason = "command_sender_dropped", "websocket disconnected");
                                        let _ = write.close().await;
                                        break Some((WebSocketMessage::Disconnected, WsStreamState::Done));
                                    }
                                }
                            }
                        }
                    },
                    WsStreamState::Done => None,
                }
            },
        )
        .boxed()
    }

    fn id(&self) -> SubscriptionId {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        SubscriptionId::of::<Self>(hasher.finish())
    }
}

const fn trace_message_type(message: &Message) -> &'static str {
    match message {
        Message::Text(_) => "text",
        Message::Binary(_) => "binary",
        Message::Ping(_) => "ping",
        Message::Pong(_) => "pong",
        Message::Close(_) => "close",
        Message::Frame(_) => "frame",
    }
}

fn trace_message_size(message: &Message) -> usize {
    match message {
        Message::Text(text) => text.len(),
        Message::Binary(data) | Message::Ping(data) | Message::Pong(data) => data.len(),
        Message::Close(_) => 0,
        Message::Frame(frame) => frame.payload().len(),
    }
}

fn trace_url(url: &str) -> String {
    let base = url.split_once(['?', '#']).map_or(url, |(base, _)| base);

    let Some((scheme, rest)) = base.split_once("://") else {
        return base.to_string();
    };

    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, ""), |(authority, path)| (authority, path));

    let Some((_, host)) = authority.split_once('@') else {
        return base.to_string();
    };

    if path.is_empty() {
        format!("{scheme}://<redacted>@{host}")
    } else {
        format!("{scheme}://<redacted>@{host}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

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

    #[test]
    fn test_trace_url_redacts_userinfo_only_in_authority() {
        assert_eq!(
            trace_url("wss://user:password@example.com/socket?token=secret#fragment"),
            "wss://<redacted>@example.com/socket"
        );
        assert_eq!(
            trace_url("wss://example.com/@user/feed?token=secret"),
            "wss://example.com/@user/feed"
        );
    }

    #[tokio::test]
    async fn test_stream_emits_error_on_connection_failure() {
        // Use an invalid URL that will fail to connect
        let ws = WebSocket::new("ws://localhost:1");
        let mut stream = ws.stream();

        // First and only message should be Error due to connection failure
        assert!(matches!(
            stream.next().await,
            Some(WebSocketMessage::Error { .. }),
        ));
    }

    #[test]
    fn test_message_variants() {
        // Test that all message variants can be constructed and matched
        let (tx, _rx) = mpsc::unbounded_channel();

        // Test Connected variant with sender
        matches!(
            WebSocketMessage::Connected { sender: tx },
            WebSocketMessage::Connected { .. }
        );

        // Test Disconnected variant
        matches!(
            WebSocketMessage::Disconnected,
            WebSocketMessage::Disconnected
        );

        // Test Received variant
        matches!(
            WebSocketMessage::Received(Message::Text("test".into())),
            WebSocketMessage::Received(_)
        );

        // Test Error variant
        matches!(
            WebSocketMessage::Error {
                error: "test".to_string()
            },
            WebSocketMessage::Error { .. }
        );
    }
}
