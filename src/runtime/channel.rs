//! A runtime-owned mpsc channel that is either unbounded (the default delivery
//! mode) or bounded to a configured capacity (RFC 0006 bounded mode).
//!
//! tokio's unbounded and bounded mpsc halves are distinct types with no shared
//! trait, so this module wraps them in one enum pair reused for both the shared
//! application-message channel and each keyed command's private channel. The
//! producer path is uniform — [`Sender::send`] awaits capacity in bounded mode
//! (INV-L2: never drops; the producer waits) and completes without suspending
//! in unbounded mode (INV-L6: senders never wait).

use std::num::NonZeroUsize;
use std::task::{Context, Poll};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::{SendError, TryRecvError};

/// Sending half of a runtime-owned channel.
///
/// `pub` rather than `pub(crate)` only to satisfy `clippy::redundant_pub_crate`:
/// the enclosing `runtime` module is `pub(crate)`, so this type's effective
/// reachability is capped at the crate regardless (see `frame_rate`).
pub enum Sender<T> {
    Unbounded(mpsc::UnboundedSender<T>),
    Bounded(mpsc::Sender<T>),
}

/// Receiving half of a runtime-owned channel.
///
/// `pub` for the same crate-capped reason as [`Sender`].
pub enum Receiver<T> {
    Unbounded(mpsc::UnboundedReceiver<T>),
    Bounded(mpsc::Receiver<T>),
}

/// Builds a channel pair from an optional capacity.
///
/// `None` constructs the same `mpsc::unbounded_channel` as before, so the
/// default delivery mode is structurally unchanged (INV-L6). `Some(n)`
/// constructs a bounded channel of exactly `n` slots (INV-L1).
pub fn channel<T>(capacity: Option<NonZeroUsize>) -> (Sender<T>, Receiver<T>) {
    capacity.map_or_else(
        || {
            let (tx, rx) = mpsc::unbounded_channel();
            (Sender::Unbounded(tx), Receiver::Unbounded(rx))
        },
        |capacity| {
            let (tx, rx) = mpsc::channel(capacity.get());
            (Sender::Bounded(tx), Receiver::Bounded(rx))
        },
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Unbounded(tx) => Self::Unbounded(tx.clone()),
            Self::Bounded(tx) => Self::Bounded(tx.clone()),
        }
    }
}

impl<T> Sender<T> {
    /// Sends `value`, awaiting capacity in bounded mode and completing without
    /// suspending in unbounded mode.
    ///
    /// The unbounded arm has no `.await`: `UnboundedSender::send` returns
    /// immediately, so the caller's `.await` resolves on the first poll and the
    /// producer task is never suspended (INV-L6). The bounded arm awaits a
    /// permit, so an overloaded producer waits rather than dropping (INV-L2).
    pub async fn send(&self, value: T) -> Result<(), SendError<T>> {
        match self {
            Self::Unbounded(tx) => tx.send(value),
            Self::Bounded(tx) => tx.send(value).await,
        }
    }

    /// Non-awaiting send used only by tests to inject into the (default,
    /// unbounded) shared channel synchronously. The runtime's own producer path
    /// always uses [`send`](Self::send) so its backpressure and no-drop
    /// behavior is exercised as in production.
    #[cfg(test)]
    pub fn try_send(&self, value: T) -> Result<(), mpsc::error::TrySendError<T>> {
        match self {
            Self::Unbounded(tx) => tx
                .send(value)
                .map_err(|SendError(value)| mpsc::error::TrySendError::Closed(value)),
            Self::Bounded(tx) => tx.try_send(value),
        }
    }

    /// Wraps an existing unbounded sender.
    ///
    /// Used by the `bench-internals` `BenchSubscriptionManager` wrapper (RFC
    /// 0007 §2.3: bench-internals gains no config-related items, and the
    /// subscription bench keeps measuring the unbounded path) and by
    /// `SubscriptionManager`'s own tests, which drive the unbounded forwarding
    /// path. Excluded from normal library builds, where nothing constructs a
    /// `Sender` this way.
    #[cfg(any(test, feature = "bench-internals"))]
    pub const fn from_unbounded(tx: mpsc::UnboundedSender<T>) -> Self {
        Self::Unbounded(tx)
    }
}

impl<T> Receiver<T> {
    /// Polls for the next value, delegating to the underlying receiver.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        match self {
            Self::Unbounded(rx) => rx.poll_recv(cx),
            Self::Bounded(rx) => rx.poll_recv(cx),
        }
    }

    /// Attempts to receive without waiting.
    pub fn try_recv(&mut self) -> Result<T, TryRecvError> {
        match self {
            Self::Unbounded(rx) => rx.try_recv(),
            Self::Bounded(rx) => rx.try_recv(),
        }
    }

    /// Number of messages currently buffered in the channel.
    pub fn len(&self) -> usize {
        match self {
            Self::Unbounded(rx) => rx.len(),
            Self::Bounded(rx) => rx.len(),
        }
    }

    /// Whether every sender has been dropped.
    pub fn is_closed(&self) -> bool {
        match self {
            Self::Unbounded(rx) => rx.is_closed(),
            Self::Bounded(rx) => rx.is_closed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::num::NonZeroUsize;

    use futures::task::noop_waker_ref;

    use super::*;

    fn cap(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("capacity must be non-zero")
    }

    fn noop_context() -> Context<'static> {
        Context::from_waker(noop_waker_ref())
    }

    /// Polls a fresh `send` future exactly once and asserts it resolved without
    /// suspending, returning the send result.
    #[allow(
        clippy::panic,
        reason = "a suspended send is the failure this helper reports"
    )]
    fn send_now<T>(sender: &Sender<T>, value: T) -> Result<(), SendError<T>> {
        let fut = sender.send(value);
        futures::pin_mut!(fut);
        match fut.poll(&mut noop_context()) {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("send should complete without suspending"),
        }
    }

    // INV-L6: the default (unbounded) path never waits — a send resolves on the
    // first poll no matter how many messages are already queued undrained. This
    // is the behavioral complement to the structural review of `send`'s
    // unbounded arm (which contains no `.await`).
    #[test]
    fn unbounded_send_never_suspends() {
        let (tx, _rx) = channel::<i32>(None);

        // Far past any bounded capacity would allow: still never suspends.
        for value in 0..10_000 {
            send_now(&tx, value).expect("unbounded receiver is open");
        }
    }

    // INV-L1/INV-L2/INV-L3: a bounded channel buffers at most its capacity, a
    // send past capacity waits (rather than dropping) until a slot frees, and
    // the full scripted sequence drains back in FIFO order with nothing lost.
    #[test]
    fn bounded_send_waits_at_capacity_and_drops_nothing() {
        let (tx, mut rx) = channel::<i32>(Some(cap(2)));

        // Two sends fill the two slots without suspending.
        send_now(&tx, 1).expect("slot available");
        send_now(&tx, 2).expect("slot available");

        // The third send has no slot: it must wait, not drop.
        let third = tx.send(3);
        futures::pin_mut!(third);
        assert!(
            third.as_mut().poll(&mut noop_context()).is_pending(),
            "a send past capacity must wait, not complete or drop"
        );

        // Freeing one slot lets exactly the waiting send through.
        assert_eq!(rx.try_recv(), Ok(1));
        assert!(
            third.as_mut().poll(&mut noop_context()).is_ready(),
            "the waiting send completes once a slot frees"
        );

        // The whole scripted sequence drained back, in order, nothing lost:
        // this is the bounded-FIFO (INV-L3) and no-drop (INV-L2) regression.
        assert_eq!(rx.try_recv(), Ok(2));
        assert_eq!(rx.try_recv(), Ok(3));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    // Smoke/regression only, NOT the INV-L9 proof: two independently bounded
    // channels do not share capacity — filling one leaves the other free. The
    // pool-absence proof is the structural review of the channel-construction
    // site (no shared permit/semaphore); the behavioral gate is the
    // `keyed_isolation` harness scenario added later. A shared pool larger than
    // `2 × capacity` would still pass this test, which is why it cannot prove
    // isolation on its own.
    #[test]
    fn independent_bounded_channels_do_not_share_capacity() {
        let (tx_a, _rx_a) = channel::<i32>(Some(cap(1)));
        let (tx_b, mut rx_b) = channel::<i32>(Some(cap(1)));

        // Saturate A and leave its next send pending.
        send_now(&tx_a, 1).expect("A slot available");
        let a_next = tx_a.send(2);
        futures::pin_mut!(a_next);
        assert!(a_next.as_mut().poll(&mut noop_context()).is_pending());

        // B is unaffected by A's saturation.
        send_now(&tx_b, 10).expect("B has its own slot");
        assert_eq!(rx_b.try_recv(), Ok(10));
    }
}
