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
use tokio::time::Instant;

use super::load::{self, Channel, LoadObserver};

/// Sending half of a runtime-owned channel.
///
/// `pub` rather than `pub(crate)` only to satisfy `clippy::redundant_pub_crate`:
/// the enclosing `runtime` module is `pub(crate)`, so this type's effective
/// reachability is capped at the crate regardless.
pub enum Sender<T> {
    Unbounded(mpsc::UnboundedSender<T>),
    Bounded {
        tx: mpsc::Sender<T>,
        /// Observability context every bounded sender carries: the
        /// capacity-wait event and the `blocked` gauge (RFC 0006 §4.4). Test
        /// senders built via [`channel`] carry a throwaway [`LoadObserver`]
        /// shared with no runtime, so the type never expresses a "bounded but
        /// unobserved" state that INV-L13 forbids.
        obs: SendObs,
    },
}

/// The channel label and shared observer a bounded sender carries so it can
/// emit the capacity-wait event and maintain the `blocked` gauge (RFC 0006
/// §4.4).
#[derive(Clone)]
pub struct SendObs {
    channel: Channel,
    observer: LoadObserver,
}

/// Receiving half of a runtime-owned channel.
///
/// `pub` for the same crate-capped reason as [`Sender`].
pub enum Receiver<T> {
    Unbounded(mpsc::UnboundedReceiver<T>),
    Bounded(mpsc::Receiver<T>),
}

/// Builds a channel pair from an optional capacity, for tests.
///
/// `None` constructs the same `mpsc::unbounded_channel` as before, so the
/// default delivery mode is structurally unchanged (INV-L6). `Some(n)`
/// constructs a bounded channel of exactly `n` slots (INV-L1). The bounded
/// sender carries a throwaway [`LoadObserver`] shared with no runtime, so its
/// `blocked` gauge and capacity-wait events reach no aggregate — but they still
/// fire to an installed `tracing` subscriber (emission is gated by the
/// subscriber, not the observer). Every runtime-owned channel is built through
/// [`channel_observed`] with the real observer.
///
/// The bounded label is fixed to [`Channel::Data`]; a test that asserts on
/// the capacity-wait event's `channel` field (e.g. a keyed case) must use
/// [`channel_observed`] with the intended label instead.
#[cfg(test)]
pub fn channel<T>(capacity: Option<NonZeroUsize>) -> (Sender<T>, Receiver<T>) {
    build(
        capacity,
        SendObs {
            channel: Channel::Data,
            observer: LoadObserver::default(),
        },
    )
}

/// Builds a channel pair whose bounded sender emits the capacity-wait event and
/// maintains the `blocked` gauge under the given `channel` label (RFC 0006
/// §4.4). An unbounded channel (`None` capacity) never blocks, so the observer
/// is inert and dropped.
pub fn channel_observed<T>(
    capacity: Option<NonZeroUsize>,
    channel: Channel,
    observer: LoadObserver,
) -> (Sender<T>, Receiver<T>) {
    build(capacity, SendObs { channel, observer })
}

fn build<T>(capacity: Option<NonZeroUsize>, obs: SendObs) -> (Sender<T>, Receiver<T>) {
    capacity.map_or_else(
        || {
            let (tx, rx) = mpsc::unbounded_channel();
            (Sender::Unbounded(tx), Receiver::Unbounded(rx))
        },
        |capacity| {
            let (tx, rx) = mpsc::channel(capacity.get());
            (Sender::Bounded { tx, obs }, Receiver::Bounded(rx))
        },
    )
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Unbounded(tx) => Self::Unbounded(tx.clone()),
            Self::Bounded { tx, obs } => Self::Bounded {
                tx: tx.clone(),
                obs: obs.clone(),
            },
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
    ///
    /// The bounded arm first attempts a non-blocking send; only when that finds
    /// the channel full — the first unready attempt — does it wait, and it is
    /// exactly that path that raises the `blocked` gauge for the wait's duration
    /// and emits the capacity-wait event on acceptance (RFC 0006 §4.4). An
    /// immediately accepted send stays silent. The `blocked` gauge is held by a
    /// guard, so a send aborted mid-wait (cancellation) still lowers it.
    pub async fn send(&self, value: T) -> Result<(), SendError<T>> {
        match self {
            Self::Unbounded(tx) => tx.send(value),
            Self::Bounded { tx, obs } => match tx.try_send(value) {
                Ok(()) => Ok(()),
                Err(mpsc::error::TrySendError::Closed(value)) => Err(SendError(value)),
                Err(mpsc::error::TrySendError::Full(value)) => {
                    let started = Instant::now();
                    let _blocked = obs.observer.track_blocked();
                    let accepted = tx.send(value).await;
                    // Emit the capacity-wait event only on acceptance; a send
                    // that failed (channel closed) was never accepted.
                    if accepted.is_ok() {
                        load::capacity_wait(obs.channel, started.elapsed());
                    }
                    accepted
                }
            },
        }
    }

    /// Non-awaiting send used only by tests to inject into the (default,
    /// unbounded) data lane synchronously. The runtime's own producer path
    /// always uses [`send`](Self::send) so its backpressure and no-drop
    /// behavior is exercised as in production.
    #[cfg(test)]
    pub fn try_send(&self, value: T) -> Result<(), mpsc::error::TrySendError<T>> {
        match self {
            Self::Unbounded(tx) => tx
                .send(value)
                .map_err(|SendError(value)| mpsc::error::TrySendError::Closed(value)),
            Self::Bounded { tx, .. } => tx.try_send(value),
        }
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
    ///
    /// Test-only: the kernel holds a sender clone for its whole lifetime, so
    /// production has no closed lane to observe — that is the ownership
    /// invariant `lane` documents, and this is what asserts it.
    #[cfg(test)]
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

    use super::*;
    use crate::noop_waker::noop_context;

    fn cap(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("capacity must be non-zero")
    }

    /// Polls a fresh `send` future exactly once and asserts it resolved without
    /// suspending, returning the send result.
    #[expect(
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

    // Capacity-wait event (RFC 0006 §4.4, INV-L13): a bounded send that finds
    // the channel full waits, and on acceptance emits exactly one capacity-wait
    // event naming the blocking `channel` and a `wait_us` measured against the
    // pausable clock. An immediately accepted send emits nothing. The clock is
    // paused so the reported wait equals the scripted 5ms exactly.
    #[tokio::test(start_paused = true)]
    async fn bounded_send_emits_capacity_wait_on_acceptance() {
        use tokio::task::yield_now;
        use tokio::time::{Duration, advance};

        use crate::test_support::TraceRecorder;

        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let (tx, mut rx) =
            channel_observed::<i32>(Some(cap(1)), Channel::Data, LoadObserver::default());

        // Fills the only slot; accepted immediately, so it fires no event.
        tx.send(1).await.expect("first send fits the empty slot");

        // The second send has no slot: it blocks. Let it reach the await, hold
        // it blocked across a scripted 5ms, then free a slot so it is accepted.
        let sender = tx.clone();
        let blocked = tokio::spawn(async move { sender.send(2).await });
        yield_now().await;
        advance(Duration::from_millis(5)).await;
        assert_eq!(rx.try_recv(), Ok(1), "freeing a slot unblocks the send");
        blocked
            .await
            .expect("blocked send task joins")
            .expect("the send is accepted once a slot frees");

        assert_eq!(
            recorder.str_values("channel"),
            vec!["data".to_owned()],
            "exactly one capacity-wait event, naming the data lane"
        );
        let waits = recorder.u64_values("wait_us");
        assert_eq!(
            waits.len(),
            1,
            "the immediately accepted first send fires no capacity-wait event, so this row \
             records exactly one wait: {waits:?}"
        );
        let wait = waits
            .only()
            .expect("the count above admits exactly one reading");
        assert!(
            wait >= 5_000,
            "wait_us reflects the ~5ms blocked interval, got {wait}"
        );

        // The `blocked` gauge rose while the send waited and fell once it was
        // accepted (RFC 0006 §4.4) — the accepted-send counterpart to the
        // abort-decrement below.
        let blocked = recorder.u64_values("blocked");
        assert!(
            blocked.contains(1),
            "blocked rose while the send waited: {blocked:?}"
        );
        assert_eq!(
            recorder.current_u64("blocked"),
            Some(0),
            "blocked fell once the send was accepted (arrival-order log: {blocked:?})"
        );
    }

    // INV-L13 (unbounded mode is silent): an unbounded channel never waits, so
    // its observed sender emits no capacity-wait event and never touches the
    // `blocked` gauge — `blocked` stays 0 in unbounded mode by construction (the
    // observer is not even attached to an unbounded sender).
    #[tokio::test]
    async fn unbounded_observed_channel_emits_no_load_events() {
        use crate::test_support::TraceRecorder;

        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let (tx, _rx) = channel_observed::<i32>(None, Channel::Data, LoadObserver::default());
        for value in 0..1_000 {
            tx.send(value)
                .await
                .expect("the unbounded receiver is open");
        }

        assert_eq!(
            recorder.event_count(),
            0,
            "unbounded mode fires no capacity-wait and no blocked-gauge events"
        );
        assert!(recorder.str_values("channel").is_empty());
        assert!(recorder.u64_values("blocked").is_empty());
    }

    // `blocked` gauge (RFC 0006 §4.4): a producer that begins awaiting capacity
    // raises `blocked`, and aborting that producer mid-wait lowers it again —
    // the decrement does not depend on the send ever being accepted, so no
    // capacity-wait event fires. This is the cancellation-abort decrement the
    // DoD requires.
    #[tokio::test(flavor = "current_thread")]
    async fn blocked_gauge_falls_when_a_blocked_send_is_aborted() {
        use tokio::task::yield_now;

        use crate::test_support::TraceRecorder;

        let recorder = TraceRecorder::new().with_target("tears::runtime::load");
        let _guard = recorder.set_default();

        let (tx, _rx) =
            channel_observed::<i32>(Some(cap(1)), Channel::Data, LoadObserver::default());
        tx.send(1).await.expect("first send fills the only slot");

        // The second send blocks (no slot). Abort its task before any slot frees.
        let sender = tx.clone();
        let blocked = tokio::spawn(async move { sender.send(2).await });
        yield_now().await;
        blocked.abort();
        let _ = blocked.await;
        yield_now().await;

        let blocked_values = recorder.u64_values("blocked");
        assert!(
            blocked_values.contains(1),
            "blocked rose while the send waited: {blocked_values:?}"
        );
        assert_eq!(
            recorder.current_u64("blocked"),
            Some(0),
            "aborting the blocked send lowered blocked (arrival-order log: {blocked_values:?})"
        );
        assert!(
            recorder.str_values("channel").is_empty(),
            "no capacity-wait event fires: the send was never accepted"
        );
    }
}
