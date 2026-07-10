//! Integration tests for WebSocket connection lifecycle.
//!
//! Covers three scenarios:
//!
//! 1. **Stream drop** — the message stream is dropped while the application
//!    still holds the command sender.
//!
//! 2. **Outer task abort** — the task that drives the stream (as
//!    `SubscriptionManager::shutdown()` does via `handle.abort()`) is
//!    aborted.  The old channel-based design kept the TCP connection alive in
//!    a separate `run_subscription_loop` background task; if the Tokio
//!    runtime tore down before that task was scheduled, the connection was
//!    never closed.  The new inline `stream::unfold` design holds the
//!    connection handles inside the stream future itself, so aborting the
//!    outer task drops them synchronously.
//!
//! 3. **Protocol error close frame** — when `read.next()` returns
//!    `Some(Err(e))`, the stream must call `write.close()` before
//!    transitioning to `Done` so that a WebSocket-level close frame is sent
//!    to the server.

#![cfg(feature = "ws")]

use std::time::Duration;

use futures::StreamExt;
use tears::subscription::SubscriptionSource;
use tears::subscription::websocket::{WebSocket, WebSocketMessage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
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

    // Cancel the subscription by dropping the consumer stream.
    // The connection state (write/read) lives inside the stream::unfold
    // future, so the drop releases the TCP handles synchronously.
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

#[tokio::test]
async fn websocket_closes_connection_when_outer_task_is_aborted() {
    // Regression test: simulate what SubscriptionManager::shutdown() does.
    // Previously, stream() spawned a background `run_subscription_loop` task
    // that held the TCP handles.  When the outer consumer task was aborted,
    // that background task needed to be scheduled to react — but during Tokio
    // runtime teardown it might never get that chance.  The new inline design
    // holds the connection inside the stream::unfold future, so aborting the
    // outer task drops the handles synchronously and closes the connection
    // without any background task involvement.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local listener");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept tcp");
        let mut ws = accept_async(tcp).await.expect("accept websocket");
        loop {
            match ws.next().await {
                Some(Ok(msg)) if msg.is_close() => return true,
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return true,
            }
        }
    });

    let url = format!("ws://{addr}");
    let ws = WebSocket::new(url);

    // Spawn the stream inside a separate task, mirroring SubscriptionManager.
    let (connected_tx, connected_rx) = oneshot::channel::<()>();
    let mut connected_tx = Some(connected_tx);
    let mut stream = ws.stream();
    let task = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            if matches!(msg, WebSocketMessage::Connected { .. })
                && let Some(tx) = connected_tx.take()
            {
                let _ = tx.send(());
            }
        }
    });

    timeout(Duration::from_secs(2), connected_rx)
        .await
        .expect("connected within timeout")
        .expect("connected signal received");

    // Abort the outer task (as SubscriptionManager::shutdown() does).
    task.abort();
    let _ = task.await;

    let observed_close = timeout(Duration::from_secs(2), server).await;
    assert!(
        matches!(observed_close, Ok(Ok(true))),
        "server should observe the websocket connection closing after the outer task is aborted"
    );
}

#[tokio::test]
async fn websocket_sends_close_frame_after_read_error() {
    // Regression test for the Some(Err(e)) path in WsStreamState::Running.
    // Previously write.close() was not called on the error path, so the
    // server observed a raw TCP drop instead of a WebSocket-level close frame.
    //
    // The server claims a 1 GiB TEXT payload in the frame header only.
    // tungstenite rejects this with Err(Capacity(FrameTooLong {...})) while
    // parsing the extended-length field — no payload is transferred.  Unlike
    // RSV-bit / opcode violations, Capacity errors do not trigger tungstenite's
    // auto-close path, so the write half is still available for write.close().
    // After the fix the stream calls write.close() before transitioning to Done,
    // sending a Close frame (opcode 0x88) back to the server.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local listener");
    let addr = listener.local_addr().expect("local addr");

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept tcp");

        // Complete the WebSocket handshake, then unwrap to the raw TcpStream.
        let ws = accept_async(tcp).await.expect("accept websocket");
        let mut raw = ws.into_inner();

        // Claim a 1 GiB TEXT payload (0x40000000 bytes) without actually
        // sending any payload bytes.  tungstenite's default max_frame_size is
        // 16 MiB (16 << 20); reading the extended-length header field reveals
        // the size before any payload is transferred, so tungstenite
        // immediately returns Err(Capacity(FrameTooLong { ... })).
        //
        // Unlike RSV-bit / opcode violations, Capacity errors do NOT trigger
        // tungstenite's auto-close path, so the write half remains open.
        // This is therefore the most reliable way to exercise the
        // Some(Err(e)) branch where our write.close() call makes the
        // difference.
        //
        // Frame: 0x81 0x7F = FIN+TEXT, extended-64-bit-length.
        // Extended length bytes (big-endian u64): 0x0000000040000000 = 1 GiB.
        raw.write_all(&[0x81, 0x7F, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00])
            .await
            .expect("send oversized frame header");

        // Keep the connection open and wait for the client's close frame.
        // A WebSocket close frame always starts with 0x88 (FIN + CLOSE opcode).
        let mut buf = [0u8; 16];
        let n = timeout(Duration::from_secs(2), raw.read(&mut buf))
            .await
            .expect("timed out waiting for client close frame")
            .unwrap_or(0);

        n > 0 && buf[0] == 0x88
    });

    let url = format!("ws://{addr}");
    let ws = WebSocket::new(url);
    let mut stream = ws.stream();

    // Wait for the Connected message and keep the sender alive.  Dropping the
    // sender before the next stream.next() call causes cmd_rx.recv() to return
    // None, which would emit Disconnected and bypass the Some(Err(e)) path.
    let _sender = timeout(Duration::from_secs(2), async {
        loop {
            match stream.next().await {
                Some(WebSocketMessage::Connected { sender }) => return Some(sender),
                Some(_) => {}
                None => return None,
            }
        }
    })
    .await
    .expect("connect timeout")
    .expect("stream should yield Connected before the invalid frame arrives");

    // The oversized-frame error from the read path should surface as Error.
    let item = timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("error item timeout");
    assert!(
        matches!(item, Some(WebSocketMessage::Error { .. })),
        "expected Error on capacity violation, got: {item:?}"
    );

    // The server must have received the WebSocket close frame sent by
    // write.close() before the stream transitioned to Done.
    let saw_close_frame = timeout(Duration::from_secs(2), server)
        .await
        .expect("server task timeout")
        .expect("server task panicked");
    assert!(
        saw_close_frame,
        "client must send a WebSocket close frame (0x88) after a read error; \
         write.close() was not called on the Some(Err(e)) path"
    );
}
