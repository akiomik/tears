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
//! # Feature Flag
//!
//! This module is only available when the `ws` feature is enabled:
//!
//! ```toml
//! [dependencies]
//! tears = { version = "0.7", features = ["ws"] }
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
//! tears = { version = "0.7", features = ["ws", "native-tls"] }
//! ```

use std::hash::{DefaultHasher, Hash, Hasher};

use futures::stream::{BoxStream, SplitSink};
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
        sender: mpsc::UnboundedSender<WebSocketCommand>,
    },
    /// Disconnected from the WebSocket server (normal closure)
    Disconnected,
    /// A message received from the WebSocket server
    Received(Message),
    /// An error occurred (connection failure or communication error)
    Error { error: String },
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
/// use tears::subscription::{Subscription, websocket::{WebSocket, WebSocketMessage, WebSocketCommand}};
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

impl WebSocket {
    /// Handle a single command and send the message through WebSocket
    async fn handle_command(
        cmd: WebSocketCommand,
        write: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
        msg_tx: &mpsc::UnboundedSender<WebSocketMessage>,
    ) {
        let result = match cmd {
            WebSocketCommand::SendText(text) => write.send(Message::Text(text.into())).await,
            WebSocketCommand::SendBinary(data) => write.send(Message::Binary(data.into())).await,
            WebSocketCommand::Close(frame) => write.send(Message::Close(frame)).await,
        };

        if let Err(e) = result {
            let _ = msg_tx.send(WebSocketMessage::Error {
                error: e.to_string(),
            });
        }
    }

    /// Main subscription loop that processes incoming messages and outgoing commands
    async fn run_subscription_loop(
        url: String,
        msg_tx: mpsc::UnboundedSender<WebSocketMessage>,
        mut cmd_rx: mpsc::UnboundedReceiver<WebSocketCommand>,
        cmd_tx: mpsc::UnboundedSender<WebSocketCommand>,
    ) {
        // Attempt to connect to WebSocket server
        let ws_stream = match connect_async(&url).await {
            Ok((stream, _)) => stream,
            Err(e) => {
                let _ = msg_tx.send(WebSocketMessage::Error {
                    error: format!("Connection failed: {e}"),
                });
                return;
            }
        };

        // Notify that connection was successful and provide command sender
        if msg_tx
            .send(WebSocketMessage::Connected { sender: cmd_tx })
            .is_err()
        {
            // Receiver dropped, exit early
            return;
        }

        let (mut write, mut read) = ws_stream.split();

        loop {
            tokio::select! {
                // Handle incoming messages from WebSocket server
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Close(_))) => {
                            // Server sent close frame - normal disconnection
                            let _ = msg_tx.send(WebSocketMessage::Disconnected);
                            break;
                        }
                        Some(Ok(message)) => {
                            // Regular message (Text, Binary, Ping, Pong)
                            if msg_tx.send(WebSocketMessage::Received(message)).is_err() {
                                // Receiver dropped, exit loop
                                break;
                            }
                        }
                        Some(Err(e)) => {
                            // Communication error
                            let _ = msg_tx.send(WebSocketMessage::Error {
                                error: e.to_string(),
                            });
                            break;
                        }
                        None => {
                            // Connection closed unexpectedly
                            let _ = msg_tx.send(WebSocketMessage::Disconnected);
                            break;
                        }
                    }
                }
                // Handle outgoing commands
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(WebSocketCommand::Close(frame)) => {
                            // User requested close - send close frame and notify
                            let _ = write.send(Message::Close(frame)).await;
                            let _ = msg_tx.send(WebSocketMessage::Disconnected);
                            break;
                        }
                        Some(cmd) => {
                            Self::handle_command(cmd, &mut write, &msg_tx).await;
                        }
                        None => {
                            // Command channel closed, treat as disconnection
                            let _ = msg_tx.send(WebSocketMessage::Disconnected);
                            break;
                        }
                    }
                }
            }
        }

        // Clean shutdown: close the write half
        let _ = write.close().await;
    }
}

impl SubscriptionSource for WebSocket {
    type Output = WebSocketMessage;

    fn stream(&self) -> BoxStream<'static, WebSocketMessage> {
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();

        let url = self.url.clone();

        tokio::spawn(async move {
            // Run the main subscription loop (will send Connected on success)
            Self::run_subscription_loop(url, msg_tx, cmd_rx, cmd_tx).await;
        });

        stream::unfold(msg_rx, |mut rx| async move {
            let msg = rx.recv().await?;
            Some((msg, rx))
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
