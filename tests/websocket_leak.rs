//! Integration test for the WebSocket connection leak.
//!
//! Reproduces the scenario where a WebSocket subscription is cancelled
//! (its message stream is dropped) while the application still holds the
//! command sender obtained from `WebSocketMessage::Connected`.
//!
//! With the buggy implementation the inner connection task stays parked on
//! `read.next().await` (the server is silent) and `cmd_rx.recv().await`
//! (the sender is still alive), so the underlying connection is never closed.
//! After the fix, dropping the stream must terminate the connection promptly.

#![cfg(feature = "ws")]

use std::time::Duration;

use futures::StreamExt;
use tears::subscription::SubscriptionSource;
use tears::subscription::websocket::{WebSocket, WebSocketMessage};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::accept_async;

#[tokio::test]
async fn websocket_closes_connection_when_stream_dropped() {
    // Start a local WebSocket server that accepts one connection and then
    // stays silent, watching for the client to close the connection.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local listener");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept tcp");
        let mut ws = accept_async(tcp).await.expect("accept websocket");

        // Stay silent and wait until the client side closes the connection.
        // Returns `true` once a close / EOF / error is observed.
        loop {
            match ws.next().await {
                Some(Ok(msg)) if msg.is_close() => return true,
                Some(Ok(_)) => {} // ignore pings/pongs/etc.
                // Connection reset (Err) or EOF (None) both count as closed.
                Some(Err(_)) | None => return true,
            }
        }
    });

    let url = format!("ws://{addr}");

    // Connect via the tears WebSocket subscription source.
    let ws = WebSocket::new(url);
    let mut stream = ws.stream();

    // Wait for the Connected message and keep the command sender alive,
    // mirroring how a real application stores it to send messages later.
    let sender = timeout(Duration::from_secs(2), async {
        loop {
            match stream.next().await {
                Some(WebSocketMessage::Connected { sender }) => break Some(sender),
                Some(_) => {}
                None => break None,
            }
        }
    })
    .await
    .expect("should connect within timeout")
    .expect("stream should yield Connected before ending");

    // Cancel the subscription by dropping the consumer stream, while still
    // holding the command sender (so cmd_rx stays open).
    drop(stream);

    // The server must observe the connection closing promptly.
    let observed_close = timeout(Duration::from_secs(2), server).await;

    // Keep the command sender alive until after the assertion so that the
    // leak can only be resolved by detecting the dropped message receiver.
    drop(sender);

    assert!(
        matches!(observed_close, Ok(Ok(true))),
        "server should observe the websocket connection closing after the stream is dropped \
         (connection leaked)"
    );
}
